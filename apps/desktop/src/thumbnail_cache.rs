//! Visible-frame thumbnails rendered off the UI thread with bounded storage.

use std::{
    collections::{BTreeSet, VecDeque},
    ops::Range,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

use eframe::egui;
use gif_from_screen_domain::{FrameId, ProjectId, ProjectRevision, TimeUs};
use gif_from_screen_project::ActiveProject;
use gif_from_screen_render::CancellationToken;

use crate::editor_preview::{
    BoundedLru, EditorPreview, PreparedPreview, PreviewRenderPlan, upload_preview,
};

const CACHE_BYTES: usize = 32 * 1024 * 1024;
const CACHE_ENTRIES: usize = 256;
const MAX_PENDING: usize = 32;
const MAX_THUMBNAIL_EDGE: u32 = 512;
const MAX_THUMBNAIL_BYTES: usize = 512 * 512 * 4;
const RENDER_SURFACE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProjectKey {
    root: PathBuf,
    project_id: ProjectId,
    revision: ProjectRevision,
    max_size: [u32; 2],
}

struct RenderTask {
    generation: u64,
    frame_id: FrameId,
    max_size: [u32; 2],
    plan: PreviewRenderPlan,
}

struct RenderResult {
    generation: u64,
    frame_id: FrameId,
    result: Result<PreparedPreview, String>,
}

#[derive(Default)]
struct WorkQueue {
    tasks: VecDeque<RenderTask>,
    stopped: bool,
}

#[derive(Default)]
struct SharedWork {
    queue: Mutex<WorkQueue>,
    ready: Condvar,
    generation: AtomicU64,
}

struct RenderCancellation {
    shared: Arc<SharedWork>,
    generation: u64,
}

impl CancellationToken for RenderCancellation {
    fn is_cancelled(&self) -> bool {
        self.shared.generation.load(Ordering::Acquire) != self.generation
    }
}

struct Worker {
    shared: Arc<SharedWork>,
    results: Receiver<RenderResult>,
}

impl Worker {
    fn start(context: egui::Context) -> Result<Self, String> {
        let shared = Arc::new(SharedWork::default());
        let worker_shared = Arc::clone(&shared);
        let (sender, results) = mpsc::sync_channel(2);
        thread::Builder::new()
            .name("timeline-thumbnails".to_owned())
            .spawn(move || render_loop(&worker_shared, &sender, &context))
            .map_err(|error| format!("Could not start thumbnail renderer: {error}"))?;
        Ok(Self { shared, results })
    }

    fn replace_view(&self) -> u64 {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let generation = self
            .shared
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        queue.tasks.clear();
        generation
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        queue.tasks.clear();
        queue.stopped = true;
        self.shared.ready.notify_one();
        // No UI-thread join: a slow filesystem read may still be returning.
        // The worker owns only immutable metadata and exits on cancellation;
        // dropping the result receiver also releases a blocked send.
    }
}

fn render_loop(
    shared: &Arc<SharedWork>,
    sender: &SyncSender<RenderResult>,
    context: &egui::Context,
) {
    loop {
        let task = {
            let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            while queue.tasks.is_empty() && !queue.stopped {
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if queue.stopped {
                return;
            }
            queue.tasks.pop_front().expect("nonempty work queue")
        };
        let cancellation = RenderCancellation {
            shared: Arc::clone(shared),
            generation: task.generation,
        };
        if cancellation.is_cancelled() {
            continue;
        }
        let result = task
            .plan
            .prepare(
                task.max_size,
                RENDER_SURFACE_BYTES,
                MAX_THUMBNAIL_BYTES,
                &cancellation,
            )
            .map_err(|error| error.to_string());
        if cancellation.is_cancelled() {
            continue;
        }
        if sender
            .send(RenderResult {
                generation: task.generation,
                frame_id: task.frame_id,
                result,
            })
            .is_err()
        {
            return;
        }
        context.request_repaint();
    }
}

/// Texture LRU and a single lazy background renderer for the virtual filmstrip.
/// No pixels are read or rendered by `set_visible` or `get`.
pub(crate) struct ThumbnailCache {
    project: Option<ProjectKey>,
    visible: Range<usize>,
    generation: u64,
    cache: BoundedLru<FrameId, Result<EditorPreview, String>>,
    pending: BTreeSet<FrameId>,
    worker: Option<Worker>,
}

impl std::fmt::Debug for ThumbnailCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ThumbnailCache")
            .field("project", &self.project)
            .field("visible", &self.visible)
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self {
            project: None,
            visible: 0..0,
            generation: 0,
            cache: BoundedLru::new(CACHE_ENTRIES, CACHE_BYTES),
            pending: BTreeSet::new(),
            worker: None,
        }
    }
}

impl ThumbnailCache {
    /// Schedules only the visible range, which may already include UI overscan.
    /// Scrolling supersedes queued work; revision and size changes evict stale textures.
    pub(crate) fn set_visible(
        &mut self,
        project: &ActiveProject,
        indices: Range<usize>,
        context: &egui::Context,
        max_size: [u32; 2],
    ) {
        let frame_count = project.manifest().timeline.frames.len();
        let visible = indices.start.min(frame_count)..indices.end.min(frame_count);
        let key = ProjectKey {
            root: project.layout().root.clone(),
            project_id: project.manifest().project_id,
            revision: project.manifest().revision,
            max_size,
        };
        let project_changed = self.project.as_ref() != Some(&key);
        let view_changed = project_changed || self.visible != visible;
        if project_changed {
            self.cache.retain(|_| false);
        }
        self.project = Some(key);
        self.visible = visible.clone();
        if visible.is_empty() {
            self.worker = None;
            self.pending.clear();
            return;
        }
        if self.worker.is_none() {
            match Worker::start(context.clone()) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    for frame in &project.manifest().timeline.frames[visible] {
                        self.cache.insert(frame.id, Err(error.clone()), error.len());
                    }
                    return;
                }
            }
        }
        if view_changed {
            self.pending.clear();
            self.generation = self.worker.as_ref().expect("started worker").replace_view();
        }
        self.receive_results(context);
        self.schedule_visible(project, context, max_size);
    }

    pub(crate) fn get(&mut self, frame_id: FrameId) -> Option<Result<EditorPreview, String>> {
        self.cache.get(&frame_id).cloned()
    }

    fn receive_results(&mut self, context: &egui::Context) {
        let worker = self.worker.as_ref().expect("started worker");
        while let Ok(completed) = worker.results.try_recv() {
            if completed.generation != self.generation {
                continue;
            }
            self.pending.remove(&completed.frame_id);
            let bytes = completed
                .result
                .as_ref()
                .map_or_else(String::len, |preview| preview.rgba.len());
            let preview = completed.result.and_then(|prepared| {
                upload_preview(
                    &prepared,
                    context,
                    format!(
                        "timeline-thumbnail-{}-{}",
                        self.generation, completed.frame_id,
                    ),
                )
                .map_err(|error| error.to_string())
            });
            self.cache.insert(completed.frame_id, preview, bytes);
        }
    }

    fn schedule_visible(
        &mut self,
        project: &ActiveProject,
        context: &egui::Context,
        max_size: [u32; 2],
    ) {
        let frames = &project.manifest().timeline.frames;
        let missing: Vec<_> = self
            .visible
            .clone()
            .filter(|&index| {
                let id = frames[index].id;
                !self.pending.contains(&id) && self.cache.get(&id).is_none()
            })
            .take(MAX_PENDING.saturating_sub(self.pending.len()))
            .collect();
        let Some(&first) = missing.first() else {
            return;
        };
        let mut time_us = frames[..first]
            .iter()
            .try_fold(0_u64, |sum, frame| sum.checked_add(frame.duration.get()));
        let mut previous = first;
        let mut tasks = VecDeque::new();
        for index in missing {
            for frame in &frames[previous..index] {
                time_us = time_us.and_then(|sum| sum.checked_add(frame.duration.get()));
            }
            previous = index;
            let frame = &frames[index];
            let plan = if max_size
                .iter()
                .any(|&edge| edge == 0 || edge > MAX_THUMBNAIL_EDGE)
            {
                Err("Thumbnail dimensions must be within 1..=512 pixels.".to_owned())
            } else if let Some(time_us) = time_us {
                PreviewRenderPlan::new(project, frame, TimeUs::new(time_us))
                    .map_err(|error| error.to_string())
            } else {
                Err("Timeline duration overflow while preparing thumbnail.".to_owned())
            };
            match plan {
                Ok(plan) => {
                    self.pending.insert(frame.id);
                    tasks.push_back(RenderTask {
                        generation: self.generation,
                        frame_id: frame.id,
                        max_size,
                        plan,
                    });
                }
                Err(error) => self.cache.insert(frame.id, Err(error.clone()), error.len()),
            }
        }
        let worker = self.worker.as_ref().expect("started worker");
        worker
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tasks
            .extend(tasks);
        worker.shared.ready.notify_one();
        // The worker requests repaint on completion. This also lets errors
        // found while planning update the visible cards immediately.
        context.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gif_from_screen_domain::{
        AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
        ColorSpace, DurationUs, EditCommand, FrameClip, PhysicalSize, ProjectManifest,
        RasterEncoding, UnixTimeMs,
    };
    use gif_from_screen_project::AssetStore;

    use super::*;

    fn project(frame_count: usize) -> (tempfile::TempDir, ActiveProject) {
        let directory = tempfile::tempdir().unwrap();
        let pixels = [10, 20, 30, 255, 40, 50, 60, 255];
        let asset_id = AssetStore::id_for_bytes(&pixels);
        let size = PhysicalSize::new(2, 1).unwrap();
        let mut manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "thumbnails",
            UnixTimeMs::new(0),
            Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        manifest.assets.insert(
            asset_id,
            AssetDescriptor {
                id: asset_id,
                byte_len: 8,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        manifest.timeline.frames = (0..frame_count)
            .map(|index| FrameClip {
                capture_clock: None,
                capture_binding: gif_from_screen_domain::CaptureBinding::Original,
                id: FrameId::from_u128(index as u128 + 1),
                asset_id,
                duration: DurationUs::new(10_000).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata::default(),
                effects: Vec::new(),
            })
            .collect();
        let project = ActiveProject::create(directory.path(), manifest).unwrap();
        project.assets().put(&pixels).unwrap();
        (directory, project)
    }

    fn wait_for(
        cache: &mut ThumbnailCache,
        project: &ActiveProject,
        context: &egui::Context,
        range: Range<usize>,
        size: [u32; 2],
        frame_id: FrameId,
    ) -> Result<EditorPreview, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cache.set_visible(project, range.clone(), context, size);
            if let Some(result) = cache.get(frame_id) {
                return result;
            }
            assert!(Instant::now() < deadline, "thumbnail worker timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn asynchronous_thumbnail_invalidates_revision_size_and_project_path() {
        let (_directory, mut first) = project(1);
        let (_other_directory, mut second) = project(1);
        let context = egui::Context::default();
        let mut cache = ThumbnailCache::default();
        let id = first.manifest().timeline.frames[0].id;
        cache.set_visible(&first, 0..1, &context, [100, 48]);
        assert!(
            cache.get(id).is_none(),
            "set_visible must not render synchronously"
        );
        let initial = wait_for(&mut cache, &first, &context, 0..1, [100, 48], id).unwrap();
        assert_eq!(initial.preview_size, [2, 1]);
        first
            .commit(EditCommand::SetFrameDurations {
                changes: vec![gif_from_screen_domain::FrameDurationChange {
                    frame_id: id,
                    duration: DurationUs::new(20_000).unwrap(),
                }],
            })
            .unwrap();
        cache.set_visible(&first, 0..1, &context, [100, 48]);
        assert!(cache.get(id).is_none());
        let revised = wait_for(&mut cache, &first, &context, 0..1, [100, 48], id).unwrap();
        assert_ne!(initial.texture.id(), revised.texture.id());
        let small = wait_for(&mut cache, &first, &context, 0..1, [1, 1], id).unwrap();
        assert_eq!(small.preview_size, [1, 1]);
        second
            .commit(EditCommand::SetFrameDurations {
                changes: vec![gif_from_screen_domain::FrameDurationChange {
                    frame_id: id,
                    duration: DurationUs::new(20_000).unwrap(),
                }],
            })
            .unwrap();
        assert_eq!(first.manifest().project_id, second.manifest().project_id);
        assert_eq!(first.manifest().revision, second.manifest().revision);
        let other = wait_for(&mut cache, &second, &context, 0..1, [1, 1], id).unwrap();
        assert_ne!(small.texture.id(), other.texture.id());
    }

    #[test]
    fn fifty_thousand_frames_only_schedule_visible_work_and_supersede_scrolled_tasks() {
        let (_directory, project) = project(50_000);
        let context = egui::Context::default();
        let mut cache = ThumbnailCache::default();
        cache.set_visible(&project, 40_000..40_100, &context, [100, 48]);
        assert_eq!(cache.pending.len(), MAX_PENDING);
        assert!(cache.pending.iter().all(|id| {
            (40_000..40_100).any(|index| project.manifest().timeline.frames[index].id == *id)
        }));
        let previous_generation = cache.generation;
        cache.set_visible(&project, 10..15, &context, [100, 48]);
        assert_ne!(cache.generation, previous_generation);
        assert_eq!(cache.pending.len(), 5);
        let id = project.manifest().timeline.frames[10].id;
        wait_for(&mut cache, &project, &context, 10..15, [100, 48], id).unwrap();
        assert!(
            cache
                .get(project.manifest().timeline.frames[40_000].id)
                .is_none()
        );
        cache.set_visible(&project, 0..0, &context, [100, 48]);
        assert!(cache.worker.is_none());
        assert!(cache.pending.is_empty());
    }

    #[test]
    fn missing_pixels_report_an_async_error_without_requeuing_each_repaint() {
        let (_directory, project) = project(1);
        let frame = &project.manifest().timeline.frames[0];
        std::fs::remove_file(project.assets().asset_path(frame.asset_id)).unwrap();
        let context = egui::Context::default();
        let mut cache = ThumbnailCache::default();
        let error = wait_for(&mut cache, &project, &context, 0..1, [100, 48], frame.id)
            .err()
            .expect("missing pixels must produce a thumbnail error");
        assert!(error.contains("could not inspect frame asset"));
        cache.set_visible(&project, 0..1, &context, [100, 48]);
        assert!(cache.pending.is_empty());
    }
}
