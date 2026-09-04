#![allow(
    dead_code,
    reason = "the background export job precedes its editor-toolbar integration"
)]

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use gif_from_screen_application::{
    ProjectExportProgress, ProjectExportSnapshot, ProjectGifExportError, ProjectGifExportOptions,
    ProjectGifExportReport, export_project_snapshot_to_gif,
};
use gif_from_screen_gif::CancellationFlag;
use thiserror::Error;

const EXPORT_THREAD_NAME: &str = "gfs-project-gif-export";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ExportJobState {
    #[default]
    Idle,
    Running,
    Cancelling,
    Finished,
}

#[derive(Clone, Copy, Debug, Default)]
struct ExportJobLifecycle {
    state: ExportJobState,
}

impl ExportJobLifecycle {
    fn begin(&mut self) -> Result<(), ExportJobState> {
        if self.state != ExportJobState::Idle {
            return Err(self.state);
        }
        self.state = ExportJobState::Running;
        Ok(())
    }

    fn spawn_failed(&mut self) {
        debug_assert_eq!(self.state, ExportJobState::Running);
        self.state = ExportJobState::Idle;
    }

    fn cancel(&mut self) -> bool {
        if self.state != ExportJobState::Running {
            return false;
        }
        self.state = ExportJobState::Cancelling;
        true
    }

    fn finish(&mut self) {
        debug_assert!(matches!(
            self.state,
            ExportJobState::Running | ExportJobState::Cancelling
        ));
        self.state = ExportJobState::Finished;
    }
}

#[derive(Debug, Error)]
pub(crate) enum ExportJobStartError {
    #[error("this export job has already started and is currently {state:?}")]
    AlreadyStarted { state: ExportJobState },
    #[error("could not spawn GIF export worker: {0}")]
    Spawn(#[source] io::Error),
}

#[derive(Debug, Error)]
pub(crate) enum ExportJobError {
    #[error("project GIF export failed: {0}")]
    Export(#[source] Box<ProjectGifExportError>),
    #[error("GIF export worker exited without reporting a result")]
    WorkerExited,
}

impl From<ProjectGifExportError> for ExportJobError {
    fn from(value: ProjectGifExportError) -> Self {
        Self::Export(Box::new(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExportJobEvent {
    Progress(ProjectExportProgress),
    Finished,
}

enum WorkerMessage {
    Progress(ProjectExportProgress),
    Finished(Box<Result<ProjectGifExportReport, ProjectGifExportError>>),
}

/// UI-owned handle for one background project-to-GIF export.
///
/// `drain` never blocks. The worker owns only a lock-free export snapshot and
/// reports progress/results through its channel. Dropping this handle requests
/// cooperative cancellation; no thread join occurs on the UI thread.
#[derive(Default)]
pub(crate) struct ExportJob {
    lifecycle: ExportJobLifecycle,
    cancellation: Option<CancellationFlag>,
    receiver: Option<Receiver<WorkerMessage>>,
    latest_progress: Option<ProjectExportProgress>,
    result: Option<Result<ProjectGifExportReport, ExportJobError>>,
}

impl ExportJob {
    pub(crate) fn start(
        &mut self,
        snapshot: ProjectExportSnapshot,
        output: PathBuf,
        options: ProjectGifExportOptions,
    ) -> Result<(), ExportJobStartError> {
        self.lifecycle
            .begin()
            .map_err(|state| ExportJobStartError::AlreadyStarted { state })?;
        let cancellation = CancellationFlag::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name(EXPORT_THREAD_NAME.to_owned())
            .spawn(move || {
                let progress_sender = sender.clone();
                let mut progress = move |update| {
                    let _ = progress_sender.send(WorkerMessage::Progress(update));
                };
                let result = export_project_snapshot_to_gif(
                    &snapshot,
                    output,
                    &options,
                    &worker_cancellation,
                    &mut progress,
                );
                let _ = sender.send(WorkerMessage::Finished(Box::new(result)));
            });
        if let Err(source) = spawn {
            self.lifecycle.spawn_failed();
            return Err(ExportJobStartError::Spawn(source));
        }
        self.cancellation = Some(cancellation);
        self.receiver = Some(receiver);
        self.latest_progress = None;
        self.result = None;
        Ok(())
    }

    pub(crate) const fn state(&self) -> ExportJobState {
        self.lifecycle.state
    }

    pub(crate) const fn latest_progress(&self) -> Option<ProjectExportProgress> {
        self.latest_progress
    }

    /// Requests cancellation once. Repeated calls are idempotent.
    ///
    /// Returns `true` only for the transition from Running to Cancelling.
    pub(crate) fn cancel(&mut self) -> bool {
        let changed = self.lifecycle.cancel();
        if changed && let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
        changed
    }

    /// Drains every currently queued worker message without blocking.
    pub(crate) fn drain(&mut self) -> Vec<ExportJobEvent> {
        if !matches!(
            self.lifecycle.state,
            ExportJobState::Running | ExportJobState::Cancelling
        ) {
            return Vec::new();
        }
        let mut events = Vec::new();
        loop {
            let message = match self
                .receiver
                .as_ref()
                .expect("a live export lifecycle always has a receiver")
                .try_recv()
            {
                Ok(message) => message,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finish(Err(ExportJobError::WorkerExited), &mut events);
                    break;
                }
            };
            match message {
                WorkerMessage::Progress(progress) => {
                    self.latest_progress = Some(progress);
                    events.push(ExportJobEvent::Progress(progress));
                }
                WorkerMessage::Finished(result) => {
                    self.finish(result.map_err(ExportJobError::from), &mut events);
                    break;
                }
            }
        }
        events
    }

    pub(crate) fn result(&self) -> Option<&Result<ProjectGifExportReport, ExportJobError>> {
        self.result.as_ref()
    }

    pub(crate) fn take_result(&mut self) -> Option<Result<ProjectGifExportReport, ExportJobError>> {
        self.result.take()
    }

    fn finish(
        &mut self,
        result: Result<ProjectGifExportReport, ExportJobError>,
        events: &mut Vec<ExportJobEvent>,
    ) {
        self.lifecycle.finish();
        self.receiver = None;
        self.cancellation = None;
        self.result = Some(result);
        events.push(ExportJobEvent::Finished);
    }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use gif_from_screen_application::ProjectFrameSelection;
    use gif_from_screen_domain::{
        AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
        ColorSpace, DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize, ProjectId,
        ProjectManifest, RasterEncoding, UnixTimeMs,
    };
    use gif_from_screen_project::ActiveProject;
    use tempfile::tempdir;

    use super::*;

    fn snapshot(root: &Path, frame_count: usize) -> ProjectExportSnapshot {
        let size = PhysicalSize::new(8, 8).unwrap();
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "export-job-test",
            UnixTimeMs::new(1),
            Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        let mut project = ActiveProject::create(root, manifest).unwrap();
        let mut descriptors = BTreeMap::new();
        let mut clips = Vec::with_capacity(frame_count);
        for index in 0..frame_count {
            let color = u8::try_from(index % 251).unwrap();
            let mut pixels = vec![color; 8 * 8 * 4];
            for alpha in pixels.as_chunks_mut::<4>().0 {
                alpha[3] = 255;
            }
            let asset_id = project.assets().put(&pixels).unwrap();
            descriptors.entry(asset_id).or_insert(AssetDescriptor {
                id: asset_id,
                byte_len: u64::try_from(pixels.len()).unwrap(),
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            });
            clips.push(FrameClip {
                id: FrameId::from_u128(u128::try_from(index + 1).unwrap()),
                asset_id,
                duration: DurationUs::new(10_000).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata::default(),
                effects: Vec::new(),
            });
        }
        let mut commands: Vec<_> = descriptors
            .into_values()
            .map(|asset| EditCommand::RegisterAsset { asset })
            .collect();
        commands.push(EditCommand::InsertFrames {
            index: 0,
            frames: clips,
        });
        project.commit(EditCommand::Compound { commands }).unwrap();
        project.checkpoint_and_compact().unwrap();
        ProjectExportSnapshot::from_active(&project)
    }

    fn drain_until_finished(job: &mut ExportJob) -> Vec<ExportJobEvent> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut events = Vec::new();
        while job.state() != ExportJobState::Finished {
            events.extend(job.drain());
            assert!(Instant::now() < deadline, "export worker timed out");
            thread::sleep(Duration::from_millis(1));
        }
        events
    }

    #[test]
    fn lifecycle_is_pure_idempotent_and_single_start() {
        let mut lifecycle = ExportJobLifecycle::default();
        assert_eq!(lifecycle.state, ExportJobState::Idle);
        assert!(!lifecycle.cancel());
        lifecycle.begin().unwrap();
        assert_eq!(lifecycle.begin(), Err(ExportJobState::Running));
        assert!(lifecycle.cancel());
        assert!(!lifecycle.cancel());
        assert_eq!(lifecycle.begin(), Err(ExportJobState::Cancelling));
        lifecycle.finish();
        assert_eq!(lifecycle.state, ExportJobState::Finished);
        assert!(!lifecycle.cancel());
        assert_eq!(lifecycle.begin(), Err(ExportJobState::Finished));

        let mut failed_spawn = ExportJobLifecycle::default();
        failed_spawn.begin().unwrap();
        failed_spawn.spawn_failed();
        assert_eq!(failed_spawn.state, ExportJobState::Idle);
    }

    #[test]
    fn successful_background_export_reports_monotonic_progress() {
        let directory = tempdir().unwrap();
        let snapshot = snapshot(&directory.path().join("project"), 3);
        let output = directory.path().join("success.gif");
        let mut job = ExportJob::default();
        job.start(
            snapshot.clone(),
            output.clone(),
            ProjectGifExportOptions::default(),
        )
        .unwrap();
        assert!(matches!(
            job.start(snapshot, output.clone(), ProjectGifExportOptions::default()),
            Err(ExportJobStartError::AlreadyStarted {
                state: ExportJobState::Running
            })
        ));

        let events = drain_until_finished(&mut job);
        assert!(output.is_file());
        assert!(matches!(job.result(), Some(Ok(report)) if report.selected_frames == 3));
        let progress: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event {
                ExportJobEvent::Progress(progress) => Some(progress),
                ExportJobEvent::Finished => None,
            })
            .collect();
        assert!(!progress.is_empty());
        assert!(progress.windows(2).all(|pair| {
            pair[0].frames_rendered <= pair[1].frames_rendered
                && pair[0].frames_encoded <= pair[1].frames_encoded
        }));
        assert_eq!(job.latest_progress().unwrap().total_frames, 3);
    }

    #[test]
    fn cancellation_is_idempotent_and_cleans_output() {
        let directory = tempdir().unwrap();
        let snapshot = snapshot(&directory.path().join("project"), 128);
        let output = directory.path().join("cancelled.gif");
        let options = ProjectGifExportOptions {
            frames: ProjectFrameSelection::All,
            ..ProjectGifExportOptions::default()
        };
        let mut job = ExportJob::default();
        job.start(snapshot, output.clone(), options).unwrap();

        assert!(job.cancel());
        assert!(!job.cancel());
        assert_eq!(job.state(), ExportJobState::Cancelling);
        drain_until_finished(&mut job);

        assert!(matches!(
            job.take_result(),
            Some(Err(ExportJobError::Export(source)))
                if matches!(*source, ProjectGifExportError::Cancelled)
        ));
        assert!(!output.exists());
    }

    #[test]
    fn existing_output_is_returned_without_replacing_bytes() {
        let directory = tempdir().unwrap();
        let snapshot = snapshot(&directory.path().join("project"), 1);
        let output = directory.path().join("existing.gif");
        fs::write(&output, b"old GIF").unwrap();
        let mut job = ExportJob::default();
        job.start(snapshot, output.clone(), ProjectGifExportOptions::default())
            .unwrap();

        drain_until_finished(&mut job);

        assert!(matches!(
            job.take_result(),
            Some(Err(ExportJobError::Export(source)))
                if matches!(&*source, ProjectGifExportError::ExistingOutput(path) if path == &output)
        ));
        assert_eq!(fs::read(&output).unwrap(), b"old GIF");
    }

    #[test]
    fn disconnected_worker_is_a_typed_terminal_result() {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut job = ExportJob {
            lifecycle: ExportJobLifecycle {
                state: ExportJobState::Running,
            },
            cancellation: Some(CancellationFlag::default()),
            receiver: Some(receiver),
            latest_progress: None,
            result: None,
        };

        assert_eq!(job.drain(), [ExportJobEvent::Finished]);
        assert_eq!(job.state(), ExportJobState::Finished);
        assert!(matches!(
            job.take_result(),
            Some(Err(ExportJobError::WorkerExited))
        ));
    }
}
