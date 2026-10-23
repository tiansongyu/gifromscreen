//! One active render, one replaceable request and one completed image.
//! Pixel I/O, effects, transition generation, resize and color conversion stay
//! on the worker; the UI only plans immutable metadata and uploads ready images.

use std::sync::{
    Arc, Condvar, Mutex, PoisonError,
    atomic::{AtomicU64, Ordering},
};

use super::{
    ActiveProject, CancellationToken, EditorPreview, EditorPreviewCache, EditorPreviewError,
    FrameGeometryPlan, FrameId, PathBuf, PreparedPreview, PresentationTransitionStep,
    PreviewCacheKey, PreviewRasterRole, PreviewRenderPlan, RgbaSurface, Transition,
    checked_rgba_byte_len, downsample_preview_cancellable, egui,
    invalidate_stale_project_revisions, preview_raster_shape, render_transition,
    transition_step_progress, validate_native_extent, validate_preview_bounds,
};

#[derive(Clone, PartialEq, Eq)]
struct RequestKey {
    root: PathBuf,
    preview: PreviewCacheKey,
}

impl RequestKey {
    fn same_view(&self, other: &Self) -> bool {
        self.root == other.root
            && self.preview.project_id == other.preview.project_id
            && self.preview.revision == other.preview.revision
            && self.preview.max_size == other.preview.max_size
            && self.preview.native_pixels == other.preview.native_pixels
    }
}

enum PresentationPlan {
    Frame(Box<PreviewRenderPlan>),
    Transition {
        transition: Transition,
        from: Box<PreviewRenderPlan>,
        to: Box<PreviewRenderPlan>,
        step: PresentationTransitionStep,
    },
}

fn frame_plan(
    project: &ActiveProject,
    frame_id: FrameId,
) -> Result<PreviewRenderPlan, EditorPreviewError> {
    let frame = project
        .manifest()
        .timeline
        .frames
        .iter()
        .find(|frame| frame.id == frame_id)
        .ok_or(EditorPreviewError::FrameNotFound { frame_id })?;
    let time = project
        .manifest()
        .timeline
        .frame_start(frame_id)
        .ok_or(EditorPreviewError::FrameTimeOverflow { frame_id })?;
    PreviewRenderPlan::new(project, frame, time)
}

impl PresentationPlan {
    fn new(
        project: &ActiveProject,
        frame_id: FrameId,
        step: Option<PresentationTransitionStep>,
    ) -> Result<Self, EditorPreviewError> {
        let Some(step) = step else {
            return frame_plan(project, frame_id).map(|plan| Self::Frame(Box::new(plan)));
        };
        let transition = project
            .manifest()
            .timeline
            .transitions
            .iter()
            .find(|transition| {
                transition.id == step.transition_id
                    && transition.from_frame == step.from_frame
                    && transition.to_frame == step.to_frame
            })
            .ok_or(EditorPreviewError::TransitionNotFound {
                transition_id: step.transition_id,
            })?;
        transition_step_progress(transition, step.step).map_err(|source| {
            EditorPreviewError::TransitionRender {
                transition_id: step.transition_id,
                step: step.step,
                source,
            }
        })?;
        Ok(Self::Transition {
            transition: transition.clone(),
            from: Box::new(frame_plan(project, step.from_frame)?),
            to: Box::new(frame_plan(project, step.to_frame)?),
            step,
        })
    }

    fn validate_native(&self, max_side: usize, max_bytes: usize) -> Result<(), EditorPreviewError> {
        let plan = match self {
            Self::Frame(plan) | Self::Transition { from: plan, .. } => plan,
        };
        let frame_id = plan.clip.id;
        let descriptor = &plan.descriptors[&plan.clip.asset_id];
        let (size, _) = preview_raster_shape(
            &descriptor.kind,
            PreviewRasterRole::Frame { frame_id },
            descriptor.id,
        )?;
        let size = FrameGeometryPlan::new(&plan.clip, size)
            .map_err(|reason| EditorPreviewError::GeometryPlan { frame_id, reason })?
            .output_size();
        validate_native_extent([size.width.get(), size.height.get()], max_side, max_bytes)
    }
}

struct RenderTask {
    key: RequestKey,
    generation: u64,
    plan: PresentationPlan,
    render_limit: usize,
    preview_limit: usize,
}

struct PreparedImage {
    rendered_size: [u32; 2],
    preview_size: [u32; 2],
    image: egui::ColorImage,
}

impl PreparedImage {
    fn new(prepared: &PreparedPreview) -> Result<Self, EditorPreviewError> {
        let [width, height] = prepared.preview_size;
        let size = [
            usize::try_from(width)
                .map_err(|_| EditorPreviewError::PreviewByteLengthOverflow { width, height })?,
            usize::try_from(height)
                .map_err(|_| EditorPreviewError::PreviewByteLengthOverflow { width, height })?,
        ];
        Ok(Self {
            rendered_size: prepared.rendered_size,
            preview_size: prepared.preview_size,
            image: egui::ColorImage::from_rgba_unmultiplied(size, &prepared.rgba),
        })
    }
}

struct RenderResult {
    key: RequestKey,
    generation: u64,
    image: Result<PreparedImage, String>,
}

#[derive(Default)]
struct Mailbox {
    pending: Option<RenderTask>,
    completed: Option<RenderResult>,
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    mailbox: Mutex<Mailbox>,
    ready: Condvar,
    generation: AtomicU64,
}

struct RenderCancellation {
    shared: Arc<Shared>,
    generation: u64,
}

impl CancellationToken for RenderCancellation {
    fn is_cancelled(&self) -> bool {
        self.shared.generation.load(Ordering::Acquire) != self.generation
    }
}

struct Worker {
    shared: Arc<Shared>,
}

impl Worker {
    fn start(context: egui::Context) -> Result<Self, String> {
        let shared = Arc::new(Shared::default());
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("editor-preview".to_owned())
            .spawn(move || render_loop(&worker_shared, &context))
            .map_err(|error| format!("Could not start preview renderer: {error}"))?;
        Ok(Self { shared })
    }

    fn cancel(&self) {
        let mut mailbox = self
            .shared
            .mailbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        mailbox.pending = None;
        mailbox.completed = None;
    }

    fn submit(&self, mut task: RenderTask) {
        let mut mailbox = self
            .shared
            .mailbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        task.generation = self.shared.generation.load(Ordering::Acquire);
        mailbox.pending = Some(task);
        self.shared.ready.notify_one();
    }

    fn take_completed(&self) -> Option<RenderResult> {
        self.shared
            .mailbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .completed
            .take()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
        self.shared
            .mailbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stopped = true;
        self.shared.ready.notify_one();
        // No UI join. A filesystem read already in progress can return later;
        // the worker owns immutable metadata and observes cancellation then.
    }
}

struct Endpoints {
    key: RequestKey,
    transition: Transition,
    from: RgbaSurface,
    to: RgbaSurface,
}

fn prepare_task(
    task: &RenderTask,
    endpoints: &mut Option<Endpoints>,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedImage, EditorPreviewError> {
    let prepared = match &task.plan {
        PresentationPlan::Frame(plan) => {
            *endpoints = None;
            plan.prepare(
                task.key.preview.max_size,
                task.render_limit,
                task.preview_limit,
                cancellation,
            )?
        }
        PresentationPlan::Transition {
            transition,
            from,
            to,
            step,
        } => {
            if !endpoints.as_ref().is_some_and(|cached| {
                cached.key.same_view(&task.key) && cached.transition == *transition
            }) {
                *endpoints = None;
                // Keep the two endpoints plus output within the existing
                // transition surface budget; never retain the previous pair.
                let limit = task.render_limit / 3;
                let from = from.render(limit, cancellation)?;
                let to = to.render(limit, cancellation)?;
                *endpoints = Some(Endpoints {
                    key: task.key.clone(),
                    transition: transition.clone(),
                    from,
                    to,
                });
            }
            let cached = endpoints.as_ref().expect("prepared transition endpoints");
            let map_error = |source| EditorPreviewError::TransitionRender {
                transition_id: transition.id,
                step: step.step,
                source,
            };
            let progress = transition_step_progress(transition, step.step).map_err(map_error)?;
            let surface = render_transition(
                &cached.from,
                &cached.to,
                &transition.kind,
                progress,
                cancellation,
            )
            .map_err(map_error)?;
            downsample_preview_cancellable(
                surface,
                task.key.preview.max_size,
                task.preview_limit,
                cancellation,
            )?
        }
    };
    if cancellation.is_cancelled() {
        return Err(EditorPreviewError::Cancelled);
    }
    PreparedImage::new(&prepared)
}

fn render_loop(shared: &Arc<Shared>, context: &egui::Context) {
    let mut endpoints = None;
    loop {
        let task = {
            let mut mailbox = shared
                .mailbox
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            while mailbox.pending.is_none() && !mailbox.stopped {
                mailbox = shared
                    .ready
                    .wait(mailbox)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if mailbox.stopped {
                return;
            }
            mailbox.pending.take().expect("pending preview")
        };
        let cancellation = RenderCancellation {
            shared: Arc::clone(shared),
            generation: task.generation,
        };
        if cancellation.is_cancelled() {
            continue;
        }
        let image =
            prepare_task(&task, &mut endpoints, &cancellation).map_err(|error| error.to_string());
        if cancellation.is_cancelled() {
            endpoints = None;
            continue;
        }
        let mut mailbox = shared
            .mailbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !cancellation.is_cancelled() && !mailbox.stopped {
            mailbox.completed = Some(RenderResult {
                key: task.key,
                generation: task.generation,
                image,
            });
            drop(mailbox);
            context.request_repaint();
        }
    }
}

#[derive(Default)]
pub(super) struct AsyncPreviewRenderer {
    worker: Option<Worker>,
    target: Option<RequestKey>,
    scheduled: Option<RequestKey>,
    failure: Option<String>,
}

impl AsyncPreviewRenderer {
    /// Returns true when cached textures belong to a different project root.
    fn select_target(&mut self, key: &RequestKey, playback: bool) -> bool {
        if self.target.as_ref() == Some(key) {
            return false;
        }
        let same_view = self.target.as_ref().is_some_and(|old| old.same_view(key));
        if !same_view || !playback {
            if let Some(worker) = &self.worker {
                worker.cancel();
            }
            self.scheduled = None;
        }
        let changed_root = self.target.as_ref().is_some_and(|old| old.root != key.root);
        self.target = Some(key.clone());
        self.failure = None;
        changed_root
    }
}

impl EditorPreviewCache {
    /// None means pixels are being prepared. A new selection cancels stale
    /// rendering; playback keeps the active render and replaces queued work so
    /// expensive frames still make progress instead of being cancelled forever.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn request_presentation_preview(
        &mut self,
        project: &ActiveProject,
        frame_id: FrameId,
        transition: Option<PresentationTransitionStep>,
        context: &egui::Context,
        max_size: [u32; 2],
        native_pixels: bool,
        playback: bool,
    ) -> Result<Option<EditorPreview>, String> {
        let max_size = if native_pixels {
            [u32::MAX; 2]
        } else {
            max_size
        };
        validate_preview_bounds(max_size).map_err(|error| error.to_string())?;
        let key = RequestKey {
            root: project.layout().root.clone(),
            preview: PreviewCacheKey {
                project_id: project.manifest().project_id,
                revision: project.manifest().revision,
                frame_id,
                transition,
                max_size,
                native_pixels,
            },
        };
        if self.asynchronous.select_target(&key, playback) {
            self.cache.retain(|_| false);
        }
        invalidate_stale_project_revisions(
            &mut self.cache,
            key.preview.project_id,
            key.preview.revision,
        );
        let requested_ready = self.poll_prepared_preview(&key, context)?;
        if let Some(preview) = requested_ready.or_else(|| self.cache.get(&key.preview).cloned()) {
            if native_pixels {
                validate_native_extent(
                    preview.rendered_size,
                    context.input(|input| input.max_texture_side),
                    self.cache.max_bytes,
                )
                .map_err(|error| error.to_string())?;
            }
            return Ok(Some(preview));
        }
        let renderer = &mut self.asynchronous;
        if let Some(error) = &renderer.failure {
            return Err(error.clone());
        }
        if renderer.scheduled.as_ref() != Some(&key) {
            let plan = PresentationPlan::new(project, frame_id, transition)
                .map_err(|error| error.to_string())?;
            if native_pixels {
                plan.validate_native(
                    context.input(|input| input.max_texture_side),
                    self.cache.max_bytes,
                )
                .map_err(|error| error.to_string())?;
            }
            if renderer.worker.is_none() {
                renderer.worker = Some(Worker::start(context.clone())?);
            }
            renderer
                .worker
                .as_ref()
                .expect("started preview worker")
                .submit(RenderTask {
                    key: key.clone(),
                    generation: 0,
                    plan,
                    render_limit: self.render_surface_limit_bytes,
                    preview_limit: self.cache.max_bytes,
                });
            renderer.scheduled = Some(key);
        }
        Ok(None)
    }

    fn poll_prepared_preview(
        &mut self,
        key: &RequestKey,
        context: &egui::Context,
    ) -> Result<Option<EditorPreview>, String> {
        let renderer = &mut self.asynchronous;
        let Some(worker) = &renderer.worker else {
            return Ok(None);
        };
        let Some(completed) = worker.take_completed() else {
            return Ok(None);
        };
        if completed.generation != worker.shared.generation.load(Ordering::Acquire)
            || !completed.key.same_view(key)
        {
            return Ok(None);
        }
        if renderer.scheduled.as_ref() == Some(&completed.key) {
            renderer.scheduled = None;
        }
        let image = match completed.image {
            Ok(image) => image,
            Err(error) => {
                if completed.key == *key {
                    renderer.failure = Some(error);
                }
                return Ok(None);
            }
        };
        validate_native_extent(
            image.preview_size,
            context.input(|input| input.max_texture_side),
            self.cache.max_bytes,
        )
        .map_err(|error| error.to_string())?;
        let bytes = checked_rgba_byte_len(image.preview_size).map_err(|error| error.to_string())?;
        let preview = EditorPreview {
            texture: context.load_texture(
                format!(
                    "editor-preview-{}-{}-{}-{:?}-{}",
                    completed.key.preview.project_id,
                    completed.key.preview.revision,
                    completed.key.preview.frame_id,
                    completed.key.preview.transition,
                    completed.key.preview.native_pixels
                ),
                image.image,
                if key.preview.native_pixels {
                    egui::TextureOptions::NEAREST
                } else {
                    egui::TextureOptions::LINEAR
                },
            ),
            rendered_size: image.rendered_size,
            preview_size: image.preview_size,
        };
        let requested = (completed.key == *key).then(|| preview.clone());
        self.cache.insert(completed.key.preview, preview, bytes);
        Ok(requested)
    }

    pub(crate) fn retry_presentation_preview(&mut self) {
        if let Some(worker) = &self.asynchronous.worker {
            worker.cancel();
        }
        self.asynchronous.scheduled = None;
        self.asynchronous.failure = None;
    }

    /// A bounded-cache fallback while current pixels are pending. The caller must disable
    /// editing while a different requested frame is still being prepared.
    pub(crate) fn latest_ready_presentation(
        &self,
    ) -> Option<(FrameId, Option<PresentationTransitionStep>, EditorPreview)> {
        let target = self.asynchronous.target.as_ref()?;
        self.cache
            .entries
            .iter()
            .find(|entry| {
                entry.key.project_id == target.preview.project_id
                    && entry.key.revision == target.preview.revision
                    && entry.key.max_size == target.preview.max_size
                    && entry.key.native_pixels == target.preview.native_pixels
            })
            .map(|entry| {
                (
                    entry.key.frame_id,
                    entry.key.transition,
                    entry.value.clone(),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_preview::tests::{project_with_frame, transition_project};
    use crate::editor_preview::{
        NeverCancel, fs, render_transition_surface, resize_nearest_rgba_cancellable,
    };
    use gif_from_screen_domain::{
        ClipTransform, DurationUs, EditCommand, FrameDurationChange, PhysicalSize, TransitionKind,
    };

    fn cache_without_thread() -> (EditorPreviewCache, Arc<Shared>) {
        let shared = Arc::new(Shared::default());
        let mut cache = EditorPreviewCache::with_limits(4, 4096, 4096);
        cache.asynchronous.worker = Some(Worker {
            shared: Arc::clone(&shared),
        });
        (cache, shared)
    }

    fn take_task(shared: &Shared) -> RenderTask {
        shared.mailbox.lock().unwrap().pending.take().unwrap()
    }

    fn complete(shared: &Shared, task: RenderTask) {
        let cancellation = RenderCancellation {
            shared: Arc::new(Shared::default()),
            generation: 0,
        };
        let image =
            prepare_task(&task, &mut None, &cancellation).map_err(|error| error.to_string());
        shared.mailbox.lock().unwrap().completed = Some(RenderResult {
            key: task.key,
            generation: task.generation,
            image,
        });
    }

    fn request(
        cache: &mut EditorPreviewCache,
        project: &ActiveProject,
        frame: FrameId,
        step: Option<PresentationTransitionStep>,
        playback: bool,
    ) -> Result<Option<EditorPreview>, String> {
        cache.request_presentation_preview(
            project,
            frame,
            step,
            &egui::Context::default(),
            [64, 64],
            false,
            playback,
        )
    }

    #[test]
    fn ui_request_never_loads_pixels_and_worker_errors_can_be_retried() {
        let pixels = [20, 40, 60, 128];
        let (_directory, project, frame, asset) = project_with_frame(
            &pixels,
            PhysicalSize::new(1, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        let path = project.assets().asset_path(asset);
        fs::remove_file(&path).unwrap();
        let (mut cache, shared) = cache_without_thread();
        assert!(
            request(&mut cache, &project, frame, None, false)
                .unwrap()
                .is_none()
        );
        complete(&shared, take_task(&shared));
        assert!(
            request(&mut cache, &project, frame, None, false)
                .err()
                .unwrap()
                .contains("could not inspect frame asset")
        );
        fs::write(path, pixels).unwrap();
        // An error does not reschedule every GUI frame or repeatedly read disk.
        assert!(request(&mut cache, &project, frame, None, false).is_err());
        assert!(shared.mailbox.lock().unwrap().pending.is_none());
        cache.retry_presentation_preview();
        assert!(
            request(&mut cache, &project, frame, None, false)
                .unwrap()
                .is_none()
        );
        complete(&shared, take_task(&shared));
        assert_eq!(
            request(&mut cache, &project, frame, None, false)
                .unwrap()
                .unwrap()
                .rendered_size,
            [1, 1]
        );
    }

    #[test]
    fn superseded_revision_cannot_upload_or_replace_current_preview() {
        let (_directory, mut project, frame, _) = project_with_frame(
            &[1, 2, 3, 255],
            PhysicalSize::new(1, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        let (mut cache, shared) = cache_without_thread();
        request(&mut cache, &project, frame, None, false).unwrap();
        let old_task = take_task(&shared);
        project
            .commit(EditCommand::SetFrameDurations {
                changes: vec![FrameDurationChange {
                    frame_id: frame,
                    duration: DurationUs::new(50_000).unwrap(),
                }],
            })
            .unwrap();
        request(&mut cache, &project, frame, None, false).unwrap();
        complete(&shared, old_task);
        assert!(
            request(&mut cache, &project, frame, None, false)
                .unwrap()
                .is_none()
        );
        assert!(cache.cache.entries.is_empty());
        complete(&shared, take_task(&shared));
        assert!(
            request(&mut cache, &project, frame, None, false)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            cache.cache.entries[0].key.revision,
            project.manifest().revision
        );
    }

    #[test]
    fn asynchronous_native_limits_precede_pixel_io_and_worker_start() {
        let (_directory, project, frame, asset) = project_with_frame(
            &[255; 8],
            PhysicalSize::new(2, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        fs::remove_file(project.assets().asset_path(asset)).unwrap();
        let context = egui::Context::default();
        let mut cache = EditorPreviewCache::with_limits(4, 4, 4096);
        let error = cache
            .request_presentation_preview(&project, frame, None, &context, [64, 64], true, false)
            .err()
            .unwrap();
        assert!(error.contains("above the 4-byte cache limit"));
        assert!(cache.asynchronous.worker.is_none());
        context.input_mut(|input| input.max_texture_side = 1);
        let error = cache
            .request_presentation_preview(&project, frame, None, &context, [64, 64], true, false)
            .err()
            .unwrap();
        assert!(error.contains("device texture-side limit 1"));
        assert!(cache.asynchronous.worker.is_none());
    }

    #[test]
    fn identical_project_ids_from_different_roots_do_not_reuse_textures() {
        let (_first_directory, first, frame, _) = project_with_frame(
            &[255, 0, 0, 255],
            PhysicalSize::new(1, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        let (_second_directory, second, _, _) = project_with_frame(
            &[0, 0, 255, 255],
            PhysicalSize::new(1, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        let (mut cache, shared) = cache_without_thread();
        request(&mut cache, &first, frame, None, false).unwrap();
        complete(&shared, take_task(&shared));
        assert!(
            request(&mut cache, &first, frame, None, false)
                .unwrap()
                .is_some()
        );
        assert!(
            request(&mut cache, &second, frame, None, false)
                .unwrap()
                .is_none()
        );
        assert!(cache.cache.entries.is_empty());
        assert!(cache.latest_ready_presentation().is_none());
    }

    #[test]
    fn playback_coalesces_pending_frames_without_starving_active_render() {
        let (_directory, project, step) = transition_project(TransitionKind::FadeToNext);
        let (mut cache, shared) = cache_without_thread();
        request(&mut cache, &project, step.from_frame, None, true).unwrap();
        let active = take_task(&shared);
        let generation = active.generation;
        request(&mut cache, &project, step.to_frame, None, true).unwrap();
        request(&mut cache, &project, step.from_frame, Some(step), true).unwrap();
        assert_eq!(shared.generation.load(Ordering::Acquire), generation);
        assert_eq!(
            shared
                .mailbox
                .lock()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .key
                .preview
                .transition,
            Some(step)
        );
        complete(&shared, active);
        assert!(
            request(&mut cache, &project, step.from_frame, Some(step), true)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            cache.latest_ready_presentation().unwrap().0,
            step.from_frame
        );
        let task = take_task(&shared);
        let expected = render_transition_surface(&project, step, 4096).unwrap();
        let prepared = prepare_task(&task, &mut None, &NeverCancel).unwrap();
        assert_eq!(
            prepared.image.pixels,
            egui::ColorImage::from_rgba_unmultiplied([2, 1], expected.pixels()).pixels
        );
        complete(&shared, task);
        assert!(
            request(&mut cache, &project, step.from_frame, Some(step), true)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn closing_preview_cancels_active_and_pending_without_a_ui_join() {
        let (_directory, project, frame, _) = project_with_frame(
            &[1, 2, 3, 255],
            PhysicalSize::new(1, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        let (mut cache, shared) = cache_without_thread();
        request(&mut cache, &project, frame, None, false).unwrap();
        let cancellation = RenderCancellation {
            shared: Arc::clone(&shared),
            generation: shared.generation.load(Ordering::Acquire),
        };
        drop(cache);
        assert!(cancellation.is_cancelled());
        let mailbox = shared.mailbox.lock().unwrap();
        assert!(mailbox.stopped);
        assert!(mailbox.pending.is_none());
        assert!(mailbox.completed.is_none());
    }

    #[test]
    fn real_worker_prepares_pixels_and_native_texture_without_ui_rendering() {
        let (_directory, project, frame, _) = project_with_frame(
            &[1, 2, 3, 255, 4, 5, 6, 128],
            PhysicalSize::new(2, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        let context = egui::Context::default();
        let mut cache = EditorPreviewCache::with_limits(4, 4096, 4096);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let preview = loop {
            if let Some(preview) = cache
                .request_presentation_preview(&project, frame, None, &context, [1, 1], true, false)
                .unwrap()
            {
                break preview;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "preview worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert_eq!(preview.preview_size, [2, 1]);
        assert_eq!(
            context
                .tex_manager()
                .read()
                .meta(preview.texture.id())
                .unwrap()
                .options,
            egui::TextureOptions::NEAREST
        );
    }

    #[test]
    fn large_resize_observes_cancellation_between_rows() {
        struct CancelAfterOneRow(std::sync::atomic::AtomicUsize);
        impl CancellationToken for CancelAfterOneRow {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) > 0
            }
        }
        let cancellation = CancelAfterOneRow(std::sync::atomic::AtomicUsize::new(0));
        assert!(matches!(
            resize_nearest_rgba_cancellable(&[255; 4 * 4 * 4], [4, 4], [2, 2], &cancellation),
            Err(EditorPreviewError::Cancelled)
        ));
    }
}
