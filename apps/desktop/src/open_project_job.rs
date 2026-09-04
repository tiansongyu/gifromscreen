#![allow(
    dead_code,
    reason = "the asynchronous project opener precedes its landing-page integration"
)]

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use gif_from_screen_project::{ActiveProject, LockPolicy, OpenedProject, ProjectError};
use thiserror::Error;

const OPEN_PROJECT_THREAD_NAME: &str = "gfs-project-open";
const PROJECT_MANIFEST_NAME: &str = "manifest.json";

/// Observable lifecycle of one background project-open request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OpenProjectJobState {
    /// No request has started, or synchronous startup failed and may be retried.
    #[default]
    Idle,
    /// A named worker is validating and recovering the project.
    Running,
    /// The worker result, including an error, has been collected.
    Finished,
}

#[derive(Clone, Copy, Debug, Default)]
struct OpenProjectJobLifecycle {
    state: OpenProjectJobState,
}

impl OpenProjectJobLifecycle {
    fn begin(&mut self) -> Result<(), OpenProjectJobState> {
        if self.state != OpenProjectJobState::Idle {
            return Err(self.state);
        }
        self.state = OpenProjectJobState::Running;
        Ok(())
    }

    fn start_failed(&mut self) {
        debug_assert_eq!(self.state, OpenProjectJobState::Running);
        self.state = OpenProjectJobState::Idle;
    }

    fn finish(&mut self) {
        debug_assert_eq!(self.state, OpenProjectJobState::Running);
        self.state = OpenProjectJobState::Finished;
    }
}

/// Friendly synchronous path-validation failures reported before a worker is spawned.
#[derive(Debug, Error)]
pub(crate) enum ProjectOpenPathError {
    /// The selected project path does not exist.
    #[error("project path does not exist: {}", path.display())]
    NotFound { path: PathBuf },
    /// The selected path is not a project directory.
    #[error("project path is not a directory: {}", path.display())]
    NotDirectory { path: PathBuf },
    /// Metadata for the selected path could not be inspected.
    #[error("could not inspect project path {}: {source}", path.display())]
    InspectPath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The directory does not contain the required manifest.
    #[error("project directory has no {PROJECT_MANIFEST_NAME}: {}", path.display())]
    MissingManifest { path: PathBuf },
    /// The manifest path exists but is not a regular file.
    #[error("project manifest is not a file: {}", path.display())]
    ManifestNotFile { path: PathBuf },
    /// Manifest metadata could not be inspected.
    #[error("could not inspect project manifest {}: {source}", path.display())]
    InspectManifest {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Failures that prevent an open job from starting.
#[derive(Debug, Error)]
pub(crate) enum OpenProjectJobStartError {
    /// This one-shot job has already started or finished.
    #[error("this project-open job has already started and is currently {state:?}")]
    AlreadyStarted { state: OpenProjectJobState },
    /// The path cannot identify an existing project directory.
    #[error(transparent)]
    InvalidPath(#[from] ProjectOpenPathError),
    /// The named background thread could not be created.
    #[error("could not spawn project-open worker: {0}")]
    Spawn(#[source] io::Error),
}

/// Terminal errors produced by a running project-open job.
#[derive(Debug, Error)]
pub(crate) enum OpenProjectJobError {
    /// Project validation, locking, journal recovery, or asset inspection failed.
    #[error("could not open project: {0}")]
    Project(#[source] Box<ProjectError>),
    /// The worker channel closed without delivering its terminal result.
    #[error("project-open worker exited without reporting a result")]
    WorkerExited,
}

impl From<ProjectError> for OpenProjectJobError {
    fn from(value: ProjectError) -> Self {
        Self::Project(Box::new(value))
    }
}

/// Notification returned by the non-blocking [`OpenProjectJob::drain`] call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpenProjectJobEvent {
    /// A terminal result is now available through `result` or `take_result`.
    Finished,
}

enum WorkerMessage {
    Finished(Result<OpenedProject, ProjectError>),
}

/// UI-owned handle for opening one existing project on a background thread.
///
/// Opening a project cannot currently be cancelled safely because filesystem parsing and lock
/// acquisition are not cooperative operations. Dropping this handle only disconnects the result
/// receiver and never joins the worker on the UI thread. If the worker later opens the project,
/// its failed send drops the complete [`OpenedProject`] on that worker, releasing its lock.
#[derive(Default)]
pub(crate) struct OpenProjectJob {
    lifecycle: OpenProjectJobLifecycle,
    receiver: Option<Receiver<WorkerMessage>>,
    result: Option<Result<OpenedProject, OpenProjectJobError>>,
}

impl OpenProjectJob {
    /// Starts the one-shot background open operation.
    ///
    /// The lightweight path/manifest preflight runs synchronously so common picker mistakes can be
    /// shown immediately. JSON parsing, journal replay, locking, and asset inspection run on the
    /// named worker.
    ///
    /// # Errors
    ///
    /// Returns a typed path error, thread-spawn error, or [`OpenProjectJobStartError::AlreadyStarted`].
    pub(crate) fn start(
        &mut self,
        path: PathBuf,
        lock_policy: LockPolicy,
    ) -> Result<(), OpenProjectJobStartError> {
        self.lifecycle
            .begin()
            .map_err(|state| OpenProjectJobStartError::AlreadyStarted { state })?;
        if let Err(error) = preflight_project_path(&path) {
            self.lifecycle.start_failed();
            return Err(error.into());
        }

        let (sender, receiver) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name(OPEN_PROJECT_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = ActiveProject::open(path, lock_policy);
                send_worker_result(&sender, result);
            });
        if let Err(source) = spawn {
            self.lifecycle.start_failed();
            return Err(OpenProjectJobStartError::Spawn(source));
        }

        self.receiver = Some(receiver);
        self.result = None;
        Ok(())
    }

    /// Returns the current one-shot lifecycle state.
    pub(crate) const fn state(&self) -> OpenProjectJobState {
        self.lifecycle.state
    }

    /// Drains the terminal worker message, if any, without blocking the caller.
    pub(crate) fn drain(&mut self) -> Vec<OpenProjectJobEvent> {
        if self.lifecycle.state != OpenProjectJobState::Running {
            return Vec::new();
        }
        let message = match self.receiver.as_ref().map(Receiver::try_recv) {
            Some(Ok(message)) => message,
            Some(Err(TryRecvError::Empty)) => return Vec::new(),
            Some(Err(TryRecvError::Disconnected)) | None => {
                self.finish(Err(OpenProjectJobError::WorkerExited));
                return vec![OpenProjectJobEvent::Finished];
            }
        };
        match message {
            WorkerMessage::Finished(result) => self.finish(result.map_err(Into::into)),
        }
        vec![OpenProjectJobEvent::Finished]
    }

    /// Borrows the complete terminal result without taking ownership of the project lock.
    pub(crate) fn result(&self) -> Option<&Result<OpenedProject, OpenProjectJobError>> {
        self.result.as_ref()
    }

    /// Takes the complete terminal result for conversion into an editor workspace.
    ///
    /// The successful value retains the active project, journal recovery report, and every asset
    /// issue returned by [`ActiveProject::open`].
    pub(crate) fn take_result(&mut self) -> Option<Result<OpenedProject, OpenProjectJobError>> {
        self.result.take()
    }

    fn finish(&mut self, result: Result<OpenedProject, OpenProjectJobError>) {
        self.lifecycle.finish();
        self.receiver = None;
        self.result = Some(result);
    }
}

impl Drop for OpenProjectJob {
    fn drop(&mut self) {
        // There is deliberately no JoinHandle and no cancellation signal. Disconnecting is enough:
        // Sender::send returns ownership of an unsent OpenedProject, which the worker then drops.
        self.receiver = None;
    }
}

fn preflight_project_path(path: &Path) -> Result<(), ProjectOpenPathError> {
    let metadata = fs::metadata(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProjectOpenPathError::NotFound {
                path: path.to_owned(),
            }
        } else {
            ProjectOpenPathError::InspectPath {
                path: path.to_owned(),
                source,
            }
        }
    })?;
    if !metadata.is_dir() {
        return Err(ProjectOpenPathError::NotDirectory {
            path: path.to_owned(),
        });
    }

    let manifest = path.join(PROJECT_MANIFEST_NAME);
    let metadata = fs::metadata(&manifest).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProjectOpenPathError::MissingManifest {
                path: path.to_owned(),
            }
        } else {
            ProjectOpenPathError::InspectManifest {
                path: manifest.clone(),
                source,
            }
        }
    })?;
    if !metadata.is_file() {
        return Err(ProjectOpenPathError::ManifestNotFile { path: manifest });
    }
    Ok(())
}

fn send_worker_result(sender: &Sender<WorkerMessage>, result: Result<OpenedProject, ProjectError>) {
    if let Err(unsent) = sender.send(WorkerMessage::Finished(result)) {
        // Explicitly drop the SendError and the OpenedProject it owns on this worker thread. This
        // releases a successfully acquired project lock after the UI has discarded its job handle.
        drop(unsent);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        time::{Duration, Instant},
    };

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, ColorSpace, EditCommand,
        PhysicalSize, ProjectId, ProjectManifest, ProjectRevision, RasterEncoding, Rgba,
        UnixTimeMs,
    };
    use gif_from_screen_project::AssetIssue;
    use tempfile::tempdir;

    use super::*;

    fn manifest() -> ProjectManifest {
        ProjectManifest::new(
            ProjectId::from_u128(1),
            "open-job-test",
            UnixTimeMs::new(1),
            Canvas {
                size: PhysicalSize::new(2, 2).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap()
    }

    fn create_project(path: &Path) -> ActiveProject {
        ActiveProject::create(path, manifest()).unwrap()
    }

    fn drain_until_finished(job: &mut OpenProjectJob) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while job.state() != OpenProjectJobState::Finished {
            let _ = job.drain();
            assert!(Instant::now() < deadline, "project-open worker timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn changed_canvas() -> Canvas {
        Canvas {
            size: PhysicalSize::new(3, 3).unwrap(),
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Solid(Rgba {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 255,
            }),
        }
    }

    #[test]
    fn clean_and_replayed_projects_retain_complete_open_reports() {
        let clean_directory = tempdir().unwrap();
        drop(create_project(clean_directory.path()));
        let mut clean_job = OpenProjectJob::default();
        clean_job
            .start(clean_directory.path().to_owned(), LockPolicy::FailIfPresent)
            .unwrap();
        drain_until_finished(&mut clean_job);
        let clean = clean_job.take_result().unwrap().unwrap();
        assert!(clean.journal_recovery.is_clean());
        assert_eq!(clean.journal_recovery.replayed_records, 0);
        assert!(clean.asset_issues.is_empty());
        drop(clean);

        let replayed_directory = tempdir().unwrap();
        let mut active = create_project(replayed_directory.path());
        active
            .commit(EditCommand::SetCanvas {
                canvas: changed_canvas(),
            })
            .unwrap();
        drop(active);
        let mut replayed_job = OpenProjectJob::default();
        replayed_job
            .start(
                replayed_directory.path().to_owned(),
                LockPolicy::FailIfPresent,
            )
            .unwrap();
        drain_until_finished(&mut replayed_job);
        let replayed = replayed_job.result().unwrap().as_ref().unwrap();
        assert_eq!(
            replayed.journal_recovery.snapshot_revision,
            ProjectRevision::ZERO
        );
        assert_eq!(
            replayed.journal_recovery.recovered_revision,
            ProjectRevision::new(1)
        );
        assert_eq!(replayed.journal_recovery.replayed_records, 1);
        assert_eq!(replayed.project.manifest().canvas, changed_canvas());
    }

    #[test]
    fn asset_issues_are_preserved_in_successful_result() {
        let directory = tempdir().unwrap();
        let mut active = create_project(directory.path());
        let asset_id = AssetId::from_digest([9; 32]);
        active
            .commit(EditCommand::RegisterAsset {
                asset: AssetDescriptor {
                    id: asset_id,
                    byte_len: 10,
                    kind: AssetKind::Frame {
                        size: PhysicalSize::new(1, 1).unwrap(),
                        encoding: RasterEncoding::Rgba8,
                    },
                },
            })
            .unwrap();
        fs::write(active.assets().asset_path(asset_id), b"bad").unwrap();
        active.checkpoint_and_compact().unwrap();
        drop(active);

        let mut job = OpenProjectJob::default();
        job.start(directory.path().to_owned(), LockPolicy::FailIfPresent)
            .unwrap();
        drain_until_finished(&mut job);
        let opened = job.result().unwrap().as_ref().unwrap();
        assert!(matches!(
            opened.asset_issues.as_slice(),
            [AssetIssue::LengthMismatch {
                asset_id: found,
                expected: 10,
                actual: 3,
            }] if *found == asset_id
        ));
    }

    #[test]
    fn lock_conflict_is_a_typed_project_error() {
        let directory = tempdir().unwrap();
        let active = create_project(directory.path());
        let mut job = OpenProjectJob::default();
        job.start(directory.path().to_owned(), LockPolicy::FailIfPresent)
            .unwrap();
        drain_until_finished(&mut job);

        assert!(matches!(
            job.result(),
            Some(Err(OpenProjectJobError::Project(source)))
                if matches!(source.as_ref(), ProjectError::AlreadyLocked { .. })
        ));
        drop(active);
    }

    #[test]
    fn lifecycle_rejects_duplicate_running_and_finished_starts() {
        let directory = tempdir().unwrap();
        drop(create_project(directory.path()));
        let path = directory.path().to_owned();
        let mut job = OpenProjectJob::default();
        assert_eq!(job.state(), OpenProjectJobState::Idle);
        job.start(path.clone(), LockPolicy::FailIfPresent).unwrap();
        assert!(matches!(
            job.start(path.clone(), LockPolicy::FailIfPresent),
            Err(OpenProjectJobStartError::AlreadyStarted {
                state: OpenProjectJobState::Running
            })
        ));
        drain_until_finished(&mut job);
        assert!(matches!(
            job.start(path, LockPolicy::FailIfPresent),
            Err(OpenProjectJobStartError::AlreadyStarted {
                state: OpenProjectJobState::Finished
            })
        ));
    }

    #[test]
    fn disconnected_sender_becomes_worker_exited_without_blocking() {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut job = OpenProjectJob::default();
        job.lifecycle.begin().unwrap();
        job.receiver = Some(receiver);

        assert_eq!(job.drain(), [OpenProjectJobEvent::Finished]);
        assert_eq!(job.state(), OpenProjectJobState::Finished);
        assert!(matches!(
            job.result(),
            Some(Err(OpenProjectJobError::WorkerExited))
        ));
    }

    #[test]
    fn failed_send_drops_opened_project_and_releases_its_lock() {
        let directory = tempdir().unwrap();
        drop(create_project(directory.path()));
        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        let (sender, receiver) = mpsc::channel();
        drop(receiver);

        send_worker_result(&sender, Ok(opened));
        let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(reopened.project.manifest().revision, ProjectRevision::ZERO);
    }

    #[test]
    fn path_preflight_is_friendly_and_failed_start_can_retry() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("missing.gfsproj");
        let file = directory.path().join("not-a-project.gfsproj");
        fs::write(&file, b"not a directory").unwrap();
        let empty_directory = directory.path().join("empty.gfsproj");
        fs::create_dir(&empty_directory).unwrap();
        let mut job = OpenProjectJob::default();

        assert!(matches!(
            job.start(missing, LockPolicy::FailIfPresent),
            Err(OpenProjectJobStartError::InvalidPath(
                ProjectOpenPathError::NotFound { .. }
            ))
        ));
        assert_eq!(job.state(), OpenProjectJobState::Idle);
        assert!(matches!(
            job.start(file, LockPolicy::FailIfPresent),
            Err(OpenProjectJobStartError::InvalidPath(
                ProjectOpenPathError::NotDirectory { .. }
            ))
        ));
        assert!(matches!(
            job.start(empty_directory, LockPolicy::FailIfPresent),
            Err(OpenProjectJobStartError::InvalidPath(
                ProjectOpenPathError::MissingManifest { .. }
            ))
        ));
        assert_eq!(job.state(), OpenProjectJobState::Idle);
    }
}
