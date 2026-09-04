#![allow(
    dead_code,
    reason = "the blank-project background job precedes its desktop form integration"
)]

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use gif_from_screen_application::{
    BlankAnimationProjectOptions, CreateBlankAnimationError, create_blank_animation_project,
};
use gif_from_screen_domain::{DurationUs, FrameId, PhysicalSize, ProjectId, Rgba, UnixTimeMs};
use gif_from_screen_project::ActiveProject;
use thiserror::Error;

const BLANK_PROJECT_THREAD_NAME: &str = "gfs-blank-project";
const PROJECT_EXTENSION: &str = "gfsproj";

/// Complete deterministic input for one blank-project creation request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BlankProjectRequest {
    pub(crate) target: PathBuf,
    pub(crate) canvas: PhysicalSize,
    pub(crate) background: Rgba,
    pub(crate) frame_duration: DurationUs,
    pub(crate) frame_limit_bytes: u64,
    pub(crate) project_id: ProjectId,
    pub(crate) frame_id: FrameId,
    pub(crate) app_version: String,
    pub(crate) created_at: UnixTimeMs,
}

/// Observable lifecycle of one background blank-project request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BlankProjectJobState {
    /// No request is active, including after a synchronous start failure.
    #[default]
    Idle,
    /// The worker is validating, allocating, and persisting the project.
    Running,
    /// A terminal result has been received and may be taken by the UI.
    Finished,
}

#[derive(Clone, Copy, Debug, Default)]
struct BlankProjectJobLifecycle {
    state: BlankProjectJobState,
}

impl BlankProjectJobLifecycle {
    fn begin(&mut self) -> Result<(), BlankProjectJobState> {
        if self.state != BlankProjectJobState::Idle {
            return Err(self.state);
        }
        self.state = BlankProjectJobState::Running;
        Ok(())
    }

    fn start_failed(&mut self) {
        debug_assert_eq!(self.state, BlankProjectJobState::Running);
        self.state = BlankProjectJobState::Idle;
    }

    fn finish(&mut self) {
        debug_assert_eq!(self.state, BlankProjectJobState::Running);
        self.state = BlankProjectJobState::Finished;
    }
}

/// Friendly target validation failures reported before spawning a worker.
#[derive(Debug, Error)]
pub(crate) enum BlankProjectPathError {
    /// Blank project directories use a stable, recognizable suffix.
    #[error("blank project target must end in .{PROJECT_EXTENSION}: {}", path.display())]
    InvalidExtension { path: PathBuf },
    /// The destination's parent must already exist.
    #[error("blank project parent directory does not exist: {}", path.display())]
    ParentNotFound { path: PathBuf },
    /// The destination parent cannot be a regular file or special node.
    #[error("blank project parent is not a directory: {}", path.display())]
    ParentNotDirectory { path: PathBuf },
    /// Parent metadata could not be inspected.
    #[error("could not inspect blank project parent {}: {source}", path.display())]
    InspectParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Existing files and directories are never replaced or reused.
    #[error("blank project target already exists: {}", path.display())]
    TargetExists { path: PathBuf },
    /// Target existence could not be determined safely.
    #[error("could not inspect blank project target {}: {source}", path.display())]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Failures that prevent a blank-project job from starting.
#[derive(Debug, Error)]
pub(crate) enum BlankProjectJobStartError {
    /// This one-shot handle has already started or completed a request.
    #[error("this blank-project job has already started and is currently {state:?}")]
    AlreadyStarted { state: BlankProjectJobState },
    /// The requested target is not safe to create.
    #[error(transparent)]
    InvalidPath(#[from] BlankProjectPathError),
    /// The named worker thread could not be spawned.
    #[error("could not spawn blank-project worker: {0}")]
    Spawn(#[source] io::Error),
}

/// Terminal failure produced by a running blank-project worker.
#[derive(Debug, Error)]
pub(crate) enum BlankProjectJobError {
    /// Another filesystem entry appeared after synchronous preflight.
    #[error("blank project target appeared while creation was starting: {}", path.display())]
    TargetAppeared { path: PathBuf },
    /// The worker could no longer inspect the requested target.
    #[error("could not inspect blank project target {}: {source}", path.display())]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The bounded application use case rejected or failed to persist the request.
    #[error("could not create blank animation: {0}")]
    Create(#[source] Box<CreateBlankAnimationError>),
    /// The channel disconnected without a terminal worker message.
    #[error("blank-project worker exited without reporting a result")]
    WorkerExited,
}

impl From<CreateBlankAnimationError> for BlankProjectJobError {
    fn from(value: CreateBlankAnimationError) -> Self {
        Self::Create(Box::new(value))
    }
}

/// Notification returned by the non-blocking [`BlankProjectJob::drain`] call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BlankProjectJobEvent {
    /// A result is available through `result` or `take_result`.
    Finished,
}

enum WorkerMessage {
    Finished(Box<Result<ActiveProject, BlankProjectJobError>>),
}

/// UI-owned one-shot handle for creating a bounded blank animation off-thread.
///
/// Creation is not cooperatively cancellable. Dropping this handle disconnects
/// immediately without joining; a successful unsent result is dropped on the
/// worker and releases its [`ActiveProject`] lock.
#[derive(Default)]
pub(crate) struct BlankProjectJob {
    lifecycle: BlankProjectJobLifecycle,
    receiver: Option<Receiver<WorkerMessage>>,
    result: Option<Result<ActiveProject, BlankProjectJobError>>,
}

impl BlankProjectJob {
    /// Performs lightweight path checks and starts one named worker.
    ///
    /// # Errors
    ///
    /// Returns a typed target, duplicate-start, or thread-spawn error. A
    /// synchronous failure restores `Idle`, so the same handle may retry.
    pub(crate) fn start(
        &mut self,
        request: BlankProjectRequest,
    ) -> Result<(), BlankProjectJobStartError> {
        self.lifecycle
            .begin()
            .map_err(|state| BlankProjectJobStartError::AlreadyStarted { state })?;
        if let Err(error) = preflight_blank_project_target(&request.target) {
            self.lifecycle.start_failed();
            return Err(error.into());
        }

        let (sender, receiver) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name(BLANK_PROJECT_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = create_blank_project(request);
                send_worker_result(&sender, result);
            });
        if let Err(source) = spawn {
            self.lifecycle.start_failed();
            return Err(BlankProjectJobStartError::Spawn(source));
        }
        self.receiver = Some(receiver);
        self.result = None;
        Ok(())
    }

    /// Returns the current one-shot lifecycle state.
    pub(crate) const fn state(&self) -> BlankProjectJobState {
        self.lifecycle.state
    }

    /// Blank creation has no cooperative cancellation boundary.
    pub(crate) const fn cancellation_supported() -> bool {
        false
    }

    /// Polls for a terminal message without blocking the UI thread.
    pub(crate) fn drain(&mut self) -> Vec<BlankProjectJobEvent> {
        if self.lifecycle.state != BlankProjectJobState::Running {
            return Vec::new();
        }
        let message = match self.receiver.as_ref().map(Receiver::try_recv) {
            Some(Ok(message)) => message,
            Some(Err(TryRecvError::Empty)) => return Vec::new(),
            Some(Err(TryRecvError::Disconnected)) | None => {
                self.finish(Err(BlankProjectJobError::WorkerExited));
                return vec![BlankProjectJobEvent::Finished];
            }
        };
        match message {
            WorkerMessage::Finished(result) => self.finish(*result),
        }
        vec![BlankProjectJobEvent::Finished]
    }

    /// Borrows the terminal result while retaining any successful project lock.
    pub(crate) fn result(&self) -> Option<&Result<ActiveProject, BlankProjectJobError>> {
        self.result.as_ref()
    }

    /// Takes the terminal result for conversion into an editor workspace.
    pub(crate) fn take_result(&mut self) -> Option<Result<ActiveProject, BlankProjectJobError>> {
        self.result.take()
    }

    fn finish(&mut self, result: Result<ActiveProject, BlankProjectJobError>) {
        self.lifecycle.finish();
        self.receiver = None;
        self.result = Some(result);
    }
}

impl Drop for BlankProjectJob {
    fn drop(&mut self) {
        self.receiver = None;
    }
}

fn preflight_blank_project_target(target: &Path) -> Result<(), BlankProjectPathError> {
    let valid_extension = target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(PROJECT_EXTENSION));
    if !valid_extension {
        return Err(BlankProjectPathError::InvalidExtension {
            path: target.to_owned(),
        });
    }
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_metadata = fs::metadata(parent).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            BlankProjectPathError::ParentNotFound {
                path: parent.to_owned(),
            }
        } else {
            BlankProjectPathError::InspectParent {
                path: parent.to_owned(),
                source,
            }
        }
    })?;
    if !parent_metadata.is_dir() {
        return Err(BlankProjectPathError::ParentNotDirectory {
            path: parent.to_owned(),
        });
    }
    match target.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(BlankProjectPathError::TargetExists {
            path: target.to_owned(),
        }),
        Err(source) => Err(BlankProjectPathError::InspectTarget {
            path: target.to_owned(),
            source,
        }),
    }
}

fn create_blank_project(
    request: BlankProjectRequest,
) -> Result<ActiveProject, BlankProjectJobError> {
    ensure_target_still_absent(&request.target)?;
    create_blank_animation_project(
        &request.target,
        BlankAnimationProjectOptions {
            project_id: request.project_id,
            frame_id: request.frame_id,
            app_version: request.app_version,
            created_at: request.created_at,
            canvas: request.canvas,
            background: request.background,
            frame_duration: request.frame_duration,
            frame_limit_bytes: request.frame_limit_bytes,
        },
    )
    .map_err(Into::into)
}

fn ensure_target_still_absent(target: &Path) -> Result<(), BlankProjectJobError> {
    match target.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(BlankProjectJobError::TargetAppeared {
            path: target.to_owned(),
        }),
        Err(source) => Err(BlankProjectJobError::InspectTarget {
            path: target.to_owned(),
            source,
        }),
    }
}

fn send_worker_result(
    sender: &Sender<WorkerMessage>,
    result: Result<ActiveProject, BlankProjectJobError>,
) {
    // SendError owns the complete message. Dropping it here releases the lock
    // of an ActiveProject that the UI no longer wants.
    let _ = sender.send(WorkerMessage::Finished(Box::new(result)));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gif_from_screen_application::{CreateBlankAnimationError, DEFAULT_BLANK_FRAME_LIMIT_BYTES};
    use gif_from_screen_domain::{CanvasBackground, ProjectRevision};
    use gif_from_screen_project::{LockPolicy, ProjectError};
    use tempfile::tempdir;

    use super::*;

    fn request(target: PathBuf, background: Rgba) -> BlankProjectRequest {
        BlankProjectRequest {
            target,
            canvas: PhysicalSize::new(2, 1).unwrap(),
            background,
            frame_duration: DurationUs::new(125_000).unwrap(),
            frame_limit_bytes: DEFAULT_BLANK_FRAME_LIMIT_BYTES,
            project_id: ProjectId::from_u128(11),
            frame_id: FrameId::from_u128(22),
            app_version: "blank-job-test".to_owned(),
            created_at: UnixTimeMs::new(33),
        }
    }

    fn wait_for_finished(job: &mut BlankProjectJob) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while job.state() != BlankProjectJobState::Finished {
            let _ = job.drain();
            assert!(Instant::now() < deadline, "blank-project worker timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn transparent_and_solid_jobs_persist_exact_pixels_and_metadata() {
        for (color, expected_canvas) in [
            (
                Rgba {
                    red: 9,
                    green: 8,
                    blue: 7,
                    alpha: 0,
                },
                CanvasBackground::Transparent,
            ),
            (
                Rgba {
                    red: 10,
                    green: 20,
                    blue: 30,
                    alpha: 200,
                },
                CanvasBackground::Solid(Rgba {
                    red: 10,
                    green: 20,
                    blue: 30,
                    alpha: 200,
                }),
            ),
        ] {
            let directory = tempdir().unwrap();
            let target = directory.path().join("blank.gfsproj");
            let mut job = BlankProjectJob::default();
            job.start(request(target.clone(), color)).unwrap();
            assert_eq!(job.state(), BlankProjectJobState::Running);
            wait_for_finished(&mut job);

            let project = job.take_result().unwrap().unwrap();
            let manifest = project.manifest();
            assert_eq!(manifest.project_id, ProjectId::from_u128(11));
            assert_eq!(manifest.revision, ProjectRevision::new(1));
            assert_eq!(manifest.created_at, UnixTimeMs::new(33));
            assert_eq!(manifest.canvas.background, expected_canvas);
            assert_eq!(manifest.timeline.frames[0].id, FrameId::from_u128(22));
            assert_eq!(manifest.timeline.frames[0].duration.get(), 125_000);
            let asset = manifest.timeline.frames[0].asset_id;
            assert_eq!(
                project.assets().read(asset).unwrap(),
                [
                    color.red,
                    color.green,
                    color.blue,
                    color.alpha,
                    color.red,
                    color.green,
                    color.blue,
                    color.alpha,
                ]
            );
            assert_eq!(project.layout().root, target);
        }
    }

    #[test]
    fn canvas_and_allocation_boundaries_are_reported_by_the_worker() {
        let valid_directory = tempdir().unwrap();
        let mut valid = request(
            valid_directory.path().join("edge.gfsproj"),
            Rgba::TRANSPARENT,
        );
        valid.canvas = PhysicalSize::new(u32::from(u16::MAX), 1).unwrap();
        valid.frame_limit_bytes = u64::from(u16::MAX) * 4;
        let mut job = BlankProjectJob::default();
        job.start(valid).unwrap();
        wait_for_finished(&mut job);
        assert!(job.take_result().unwrap().is_ok());

        let oversized_directory = tempdir().unwrap();
        let mut oversized = request(
            oversized_directory.path().join("oversized.gfsproj"),
            Rgba::TRANSPARENT,
        );
        oversized.canvas = PhysicalSize::new(u32::from(u16::MAX) + 1, 1).unwrap();
        let mut job = BlankProjectJob::default();
        job.start(oversized).unwrap();
        wait_for_finished(&mut job);
        assert!(matches!(
            job.take_result(),
            Some(Err(BlankProjectJobError::Create(source)))
                if matches!(
                    source.as_ref(),
                    CreateBlankAnimationError::DimensionsOutOfRange { .. }
                )
        ));

        let bounded_directory = tempdir().unwrap();
        let mut bounded = request(
            bounded_directory.path().join("bounded.gfsproj"),
            Rgba::TRANSPARENT,
        );
        bounded.frame_limit_bytes = 7;
        let mut job = BlankProjectJob::default();
        job.start(bounded).unwrap();
        wait_for_finished(&mut job);
        assert!(matches!(
            job.take_result(),
            Some(Err(BlankProjectJobError::Create(source)))
                if matches!(
                    source.as_ref(),
                    CreateBlankAnimationError::FrameLimitExceeded {
                        required_bytes: 8,
                        limit_bytes: 7
                    }
                )
        ));
    }

    #[test]
    fn path_preflight_never_overwrites_and_synchronous_failures_can_retry() {
        let directory = tempdir().unwrap();
        let invalid = directory.path().join("blank.txt");
        let missing_parent = directory.path().join("missing/blank.gfsproj");
        let parent_file = directory.path().join("parent-file");
        fs::write(&parent_file, b"not a directory").unwrap();
        let child_of_file = parent_file.join("blank.gfsproj");
        let existing = directory.path().join("existing.gfsproj");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("sentinel"), b"keep").unwrap();
        let valid = directory.path().join("retry.gfsproj");
        let mut job = BlankProjectJob::default();

        assert!(matches!(
            job.start(request(invalid, Rgba::TRANSPARENT)),
            Err(BlankProjectJobStartError::InvalidPath(
                BlankProjectPathError::InvalidExtension { .. }
            ))
        ));
        assert_eq!(job.state(), BlankProjectJobState::Idle);
        assert!(matches!(
            job.start(request(missing_parent, Rgba::TRANSPARENT)),
            Err(BlankProjectJobStartError::InvalidPath(
                BlankProjectPathError::ParentNotFound { .. }
            ))
        ));
        assert!(matches!(
            job.start(request(child_of_file, Rgba::TRANSPARENT)),
            Err(BlankProjectJobStartError::InvalidPath(
                BlankProjectPathError::ParentNotDirectory { .. }
            ))
        ));
        assert!(matches!(
            job.start(request(existing.clone(), Rgba::TRANSPARENT)),
            Err(BlankProjectJobStartError::InvalidPath(
                BlankProjectPathError::TargetExists { .. }
            ))
        ));
        assert_eq!(fs::read(existing.join("sentinel")).unwrap(), b"keep");
        assert_eq!(job.state(), BlankProjectJobState::Idle);

        job.start(request(valid, Rgba::TRANSPARENT)).unwrap();
        wait_for_finished(&mut job);
        assert!(job.take_result().unwrap().is_ok());
    }

    #[test]
    fn lifecycle_rejects_duplicate_running_and_finished_starts() {
        let first_directory = tempdir().unwrap();
        let second_directory = tempdir().unwrap();
        let first = request(
            first_directory.path().join("first.gfsproj"),
            Rgba::TRANSPARENT,
        );
        let second = request(
            second_directory.path().join("second.gfsproj"),
            Rgba::TRANSPARENT,
        );
        let mut job = BlankProjectJob::default();
        job.start(first).unwrap();
        assert!(matches!(
            job.start(second.clone()),
            Err(BlankProjectJobStartError::AlreadyStarted {
                state: BlankProjectJobState::Running
            })
        ));
        wait_for_finished(&mut job);
        assert!(matches!(
            job.start(second),
            Err(BlankProjectJobStartError::AlreadyStarted {
                state: BlankProjectJobState::Finished
            })
        ));
        assert!(!BlankProjectJob::cancellation_supported());
    }

    #[test]
    fn receiver_drop_eventually_releases_the_successful_project_lock() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("detached.gfsproj");
        let mut job = BlankProjectJob::default();
        job.start(request(target.clone(), Rgba::TRANSPARENT))
            .unwrap();
        drop(job);

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if target.join("manifest.json").is_file() {
                match ActiveProject::open(&target, LockPolicy::FailIfPresent) {
                    Ok(opened) => {
                        assert_eq!(
                            opened.project.manifest().project_id,
                            ProjectId::from_u128(11)
                        );
                        break;
                    }
                    Err(ProjectError::AlreadyLocked { .. }) => {}
                    Err(error) => panic!("detached blank project could not reopen: {error}"),
                }
            }
            assert!(Instant::now() < deadline, "detached blank job timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn disconnected_worker_is_a_typed_finished_result() {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut job = BlankProjectJob::default();
        job.lifecycle.begin().unwrap();
        job.receiver = Some(receiver);

        assert_eq!(job.drain(), [BlankProjectJobEvent::Finished]);
        assert_eq!(job.state(), BlankProjectJobState::Finished);
        assert!(matches!(
            job.result(),
            Some(Err(BlankProjectJobError::WorkerExited))
        ));
    }

    #[test]
    fn worker_recheck_reports_a_target_that_appeared_after_preflight() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("appeared.gfsproj");
        preflight_blank_project_target(&target).unwrap();
        fs::create_dir(&target).unwrap();

        assert!(matches!(
            ensure_target_still_absent(&target),
            Err(BlankProjectJobError::TargetAppeared { path }) if path == target
        ));
    }
}
