#![allow(
    dead_code,
    reason = "the GIF import job precedes its landing-page picker integration"
)]

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use gif_from_screen_application::{
    DecodedAnimationProjectOptions, PersistDecodedAnimationError, persist_decoded_animation,
};
use gif_from_screen_domain::{FrameId, ProjectId, UnixTimeMs};
use gif_from_screen_media::{DecodeLimits, GifDecodeError, GifDecodeOptions, decode_gif};
use gif_from_screen_project::ActiveProject;
use thiserror::Error;
use uuid::Uuid;

const IMPORT_GIF_THREAD_NAME: &str = "gfs-gif-import";
const PROJECT_EXTENSION: &str = "gfsproj";
const MAX_IMPORT_WIDTH: u16 = 16_384;
const MAX_IMPORT_HEIGHT: u16 = 16_384;
const MAX_IMPORT_FRAMES: usize = 10_000;
const MAX_IMPORT_RGBA_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ImportGifJobState {
    #[default]
    Idle,
    Running,
    Finished,
}

#[derive(Clone, Copy, Debug, Default)]
struct ImportGifLifecycle {
    state: ImportGifJobState,
}

impl ImportGifLifecycle {
    fn begin(&mut self) -> Result<(), ImportGifJobState> {
        if self.state != ImportGifJobState::Idle {
            return Err(self.state);
        }
        self.state = ImportGifJobState::Running;
        Ok(())
    }

    fn start_failed(&mut self) {
        debug_assert_eq!(self.state, ImportGifJobState::Running);
        self.state = ImportGifJobState::Idle;
    }

    fn finish(&mut self) {
        debug_assert_eq!(self.state, ImportGifJobState::Running);
        self.state = ImportGifJobState::Finished;
    }
}

#[derive(Debug, Error)]
pub(crate) enum GifImportPathError {
    #[error("GIF input does not exist: {}", path.display())]
    NotFound { path: PathBuf },
    #[error("GIF input is not a regular file: {}", path.display())]
    NotFile { path: PathBuf },
    #[error("could not inspect GIF input {}: {source}", path.display())]
    InspectInput {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("input filename must end in .gif: {}", path.display())]
    InvalidExtension { path: PathBuf },
    #[error("GIF input has no filename stem for its project directory: {}", path.display())]
    MissingStem { path: PathBuf },
    #[error("target project already exists: {}", path.display())]
    TargetExists { path: PathBuf },
    #[error("could not inspect target project {}: {source}", path.display())]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub(crate) enum ImportGifJobStartError {
    #[error("this GIF import job has already started and is currently {state:?}")]
    AlreadyStarted { state: ImportGifJobState },
    #[error(transparent)]
    InvalidPath(#[from] GifImportPathError),
    #[error("could not spawn GIF import worker: {0}")]
    Spawn(#[source] io::Error),
}

#[derive(Debug, Error)]
pub(crate) enum ImportGifJobError {
    #[error("could not open GIF input {}: {source}", path.display())]
    OpenInput {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not decode GIF input: {0}")]
    Decode(#[source] Box<GifDecodeError>),
    #[error("system clock is before the Unix epoch: {0}")]
    Clock(#[source] SystemTimeError),
    #[error("current Unix timestamp {milliseconds} ms does not fit the project format")]
    TimestampOutOfRange { milliseconds: u128 },
    #[error("target project appeared while GIF import was running: {}", path.display())]
    TargetAppeared { path: PathBuf },
    #[error("could not inspect target project {}: {source}", path.display())]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not persist decoded GIF project: {0}")]
    Persist(#[source] Box<PersistDecodedAnimationError>),
    #[error("GIF import worker exited without reporting a result")]
    WorkerExited,
}

impl From<GifDecodeError> for ImportGifJobError {
    fn from(value: GifDecodeError) -> Self {
        Self::Decode(Box::new(value))
    }
}

impl From<PersistDecodedAnimationError> for ImportGifJobError {
    fn from(value: PersistDecodedAnimationError) -> Self {
        Self::Persist(Box::new(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImportGifJobEvent {
    Finished,
}

enum WorkerMessage {
    Finished(Box<Result<ActiveProject, ImportGifJobError>>),
}

/// One-shot, non-blocking UI handle for a complete GIF decode and project import.
///
/// Decoding and project persistence do not currently expose cooperative
/// cancellation boundaries, so this job intentionally has no `cancel` method.
/// Dropping it disconnects the receiver without joining the worker. If the
/// worker later succeeds, its failed send drops [`ActiveProject`] on the worker
/// thread and releases the project lock.
#[derive(Default)]
pub(crate) struct ImportGifJob {
    lifecycle: ImportGifLifecycle,
    receiver: Option<Receiver<WorkerMessage>>,
    result: Option<Result<ActiveProject, ImportGifJobError>>,
}

impl ImportGifJob {
    pub(crate) fn start(&mut self, input: PathBuf) -> Result<(), ImportGifJobStartError> {
        self.lifecycle
            .begin()
            .map_err(|state| ImportGifJobStartError::AlreadyStarted { state })?;
        let project_path = match preflight_gif_import(&input) {
            Ok(path) => path,
            Err(error) => {
                self.lifecycle.start_failed();
                return Err(error.into());
            }
        };
        let (sender, receiver) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name(IMPORT_GIF_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = import_gif(&input, &project_path);
                send_worker_result(&sender, result);
            });
        if let Err(source) = spawn {
            self.lifecycle.start_failed();
            return Err(ImportGifJobStartError::Spawn(source));
        }
        self.receiver = Some(receiver);
        self.result = None;
        Ok(())
    }

    pub(crate) const fn state(&self) -> ImportGifJobState {
        self.lifecycle.state
    }

    /// GIF decoding/persistence is currently non-cooperative and cannot be cancelled.
    pub(crate) const fn cancellation_supported() -> bool {
        false
    }

    pub(crate) fn drain(&mut self) -> Vec<ImportGifJobEvent> {
        if self.lifecycle.state != ImportGifJobState::Running {
            return Vec::new();
        }
        let message = match self.receiver.as_ref().map(Receiver::try_recv) {
            Some(Ok(message)) => message,
            Some(Err(TryRecvError::Empty)) => return Vec::new(),
            Some(Err(TryRecvError::Disconnected)) | None => {
                self.finish(Err(ImportGifJobError::WorkerExited));
                return vec![ImportGifJobEvent::Finished];
            }
        };
        match message {
            WorkerMessage::Finished(result) => self.finish(*result),
        }
        vec![ImportGifJobEvent::Finished]
    }

    pub(crate) fn result(&self) -> Option<&Result<ActiveProject, ImportGifJobError>> {
        self.result.as_ref()
    }

    pub(crate) fn take_result(&mut self) -> Option<Result<ActiveProject, ImportGifJobError>> {
        self.result.take()
    }

    fn finish(&mut self, result: Result<ActiveProject, ImportGifJobError>) {
        self.lifecycle.finish();
        self.receiver = None;
        self.result = Some(result);
    }
}

impl Drop for ImportGifJob {
    fn drop(&mut self) {
        // There is no safe cancellation signal or JoinHandle. Disconnecting is
        // sufficient because an unsent ActiveProject is returned to and dropped
        // by the worker's Sender::send call.
        self.receiver = None;
    }
}

fn preflight_gif_import(input: &Path) -> Result<PathBuf, GifImportPathError> {
    let metadata = fs::metadata(input).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            GifImportPathError::NotFound {
                path: input.to_owned(),
            }
        } else {
            GifImportPathError::InspectInput {
                path: input.to_owned(),
                source,
            }
        }
    })?;
    if !metadata.is_file() {
        return Err(GifImportPathError::NotFile {
            path: input.to_owned(),
        });
    }
    if input
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("gif"))
    {
        return Err(GifImportPathError::InvalidExtension {
            path: input.to_owned(),
        });
    }
    input
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| GifImportPathError::MissingStem {
            path: input.to_owned(),
        })?;
    let project_path = input.with_extension(PROJECT_EXTENSION);
    match project_path.try_exists() {
        Ok(false) => Ok(project_path),
        Ok(true) => Err(GifImportPathError::TargetExists { path: project_path }),
        Err(source) => Err(GifImportPathError::InspectTarget {
            path: project_path,
            source,
        }),
    }
}

fn import_gif(input: &Path, project_path: &Path) -> Result<ActiveProject, ImportGifJobError> {
    let file = File::open(input).map_err(|source| ImportGifJobError::OpenInput {
        path: input.to_owned(),
        source,
    })?;
    let animation = decode_gif(BufReader::new(file), &gif_import_decode_options())?;
    ensure_target_still_absent(project_path)?;
    let frame_ids = (0..animation.frames().len())
        .map(|_| FrameId::from_u128(Uuid::new_v4().as_u128()))
        .collect();
    let created_at = current_unix_time_ms()?;
    let display_name = input
        .file_name()
        .expect("preflight guarantees an input filename")
        .to_string_lossy()
        .into_owned();
    persist_decoded_animation(
        project_path,
        animation,
        DecodedAnimationProjectOptions {
            project_id: ProjectId::from_u128(Uuid::new_v4().as_u128()),
            frame_ids,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at,
            display_name,
        },
    )
    .map_err(Into::into)
}

fn ensure_target_still_absent(project_path: &Path) -> Result<(), ImportGifJobError> {
    match project_path.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(ImportGifJobError::TargetAppeared {
            path: project_path.to_owned(),
        }),
        Err(source) => Err(ImportGifJobError::InspectTarget {
            path: project_path.to_owned(),
            source,
        }),
    }
}

fn gif_import_decode_options() -> GifDecodeOptions {
    GifDecodeOptions {
        limits: DecodeLimits {
            max_width: MAX_IMPORT_WIDTH,
            max_height: MAX_IMPORT_HEIGHT,
            max_frames: MAX_IMPORT_FRAMES,
            max_total_rgba_bytes: MAX_IMPORT_RGBA_BYTES,
        },
        ..GifDecodeOptions::default()
    }
}

fn current_unix_time_ms() -> Result<UnixTimeMs, ImportGifJobError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(ImportGifJobError::Clock)?
        .as_millis();
    let milliseconds = i64::try_from(milliseconds)
        .map_err(|_| ImportGifJobError::TimestampOutOfRange { milliseconds })?;
    Ok(UnixTimeMs::new(milliseconds))
}

fn send_worker_result(
    sender: &Sender<WorkerMessage>,
    result: Result<ActiveProject, ImportGifJobError>,
) {
    // SendError returns the complete message. Dropping it here drops a
    // successful ActiveProject and therefore releases its lock.
    let _ = sender.send(WorkerMessage::Finished(Box::new(result)));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gif_from_screen_domain::SourceProvenance;
    use gif_from_screen_gif::{
        BuiltinGifEncoder, DeltaMode, EncodeOptions, RgbaFrame, Transparency,
    };
    use gif_from_screen_project::{LockPolicy, ProjectError};
    use tempfile::tempdir;

    use super::*;

    fn write_test_gif(path: &Path) -> [Vec<u8>; 2] {
        let first = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let second = vec![0, 0, 0, 0, 0, 0, 255, 255];
        let frames = vec![
            RgbaFrame::new(2, 1, first.clone(), 10_000).unwrap(),
            RgbaFrame::new(2, 1, second.clone(), 20_000).unwrap(),
        ];
        let mut file = File::create(path).unwrap();
        BuiltinGifEncoder::default()
            .encode_frames(
                frames,
                &mut file,
                &EncodeOptions {
                    merge_duplicate_frames: false,
                    transparency: Transparency::AlphaThreshold(1),
                    delta_mode: DeltaMode::ChangedRectangles,
                    ..EncodeOptions::default()
                },
            )
            .unwrap();
        file.sync_all().unwrap();
        [first, second]
    }

    fn wait_for_finished(job: &mut ImportGifJob) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while job.state() != ImportGifJobState::Finished {
            let _ = job.drain();
            assert!(Instant::now() < deadline, "GIF import worker timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn lifecycle_is_one_shot_but_synchronous_start_failure_can_retry() {
        let mut lifecycle = ImportGifLifecycle::default();
        lifecycle.begin().unwrap();
        assert_eq!(lifecycle.begin(), Err(ImportGifJobState::Running));
        lifecycle.start_failed();
        assert_eq!(lifecycle.state, ImportGifJobState::Idle);
        lifecycle.begin().unwrap();
        lifecycle.finish();
        assert_eq!(lifecycle.state, ImportGifJobState::Finished);
        assert_eq!(lifecycle.begin(), Err(ImportGifJobState::Finished));
        assert!(!ImportGifJob::cancellation_supported());
    }

    #[test]
    fn imports_transparency_and_disposal_as_full_canvas_project_frames() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("animated.gif");
        let expected = write_test_gif(&input);
        let project_path = directory.path().join("animated.gfsproj");
        let mut job = ImportGifJob::default();

        job.start(input.clone()).unwrap();
        assert_eq!(job.state(), ImportGifJobState::Running);
        assert!(!ImportGifJob::cancellation_supported());
        assert!(matches!(
            job.start(input),
            Err(ImportGifJobStartError::AlreadyStarted {
                state: ImportGifJobState::Running
            })
        ));
        wait_for_finished(&mut job);

        let project = job.take_result().unwrap().unwrap();
        assert_eq!(project.layout().root, project_path);
        assert_eq!(project.manifest().timeline.frames.len(), 2);
        assert_eq!(
            project
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [10_000, 20_000]
        );
        for (clip, expected) in project.manifest().timeline.frames.iter().zip(expected) {
            assert_eq!(project.assets().read(clip.asset_id).unwrap(), expected);
        }
        assert!(matches!(
            project.manifest().source_provenance.as_slice(),
            [SourceProvenance::Imported {
                display_name,
                media_type
            }] if display_name == "animated.gif" && media_type == "image/gif"
        ));
        assert!(project.layout().lock.is_file());
    }

    #[test]
    fn malformed_gif_returns_a_typed_decode_error_without_a_project() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("broken.gif");
        fs::write(&input, b"not a GIF").unwrap();
        let mut job = ImportGifJob::default();
        job.start(input.clone()).unwrap();

        wait_for_finished(&mut job);

        assert!(matches!(
            job.take_result(),
            Some(Err(ImportGifJobError::Decode(_)))
        ));
        assert!(!input.with_extension(PROJECT_EXTENSION).exists());
    }

    #[test]
    fn preflight_rejects_bad_paths_extensions_and_existing_target() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("missing.gif");
        assert!(matches!(
            preflight_gif_import(&missing),
            Err(GifImportPathError::NotFound { .. })
        ));

        let folder = directory.path().join("folder.gif");
        fs::create_dir(&folder).unwrap();
        assert!(matches!(
            preflight_gif_import(&folder),
            Err(GifImportPathError::NotFile { .. })
        ));

        let wrong_extension = directory.path().join("animation.png");
        fs::write(&wrong_extension, b"bytes").unwrap();
        assert!(matches!(
            preflight_gif_import(&wrong_extension),
            Err(GifImportPathError::InvalidExtension { .. })
        ));

        let input = directory.path().join("animation.GIF");
        write_test_gif(&input);
        assert_eq!(
            preflight_gif_import(&input).unwrap(),
            directory.path().join("animation.gfsproj")
        );
        fs::create_dir(input.with_extension(PROJECT_EXTENSION)).unwrap();
        let mut job = ImportGifJob::default();
        assert!(matches!(
            job.start(input),
            Err(ImportGifJobStartError::InvalidPath(
                GifImportPathError::TargetExists { .. }
            ))
        ));
        assert_eq!(job.state(), ImportGifJobState::Idle);
    }

    #[test]
    fn receiver_drop_releases_successful_project_lock() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("detached.gif");
        write_test_gif(&input);
        let project_path = input.with_extension(PROJECT_EXTENSION);
        let mut job = ImportGifJob::default();
        job.start(input).unwrap();
        drop(job);

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if project_path.join("manifest.json").is_file() {
                match ActiveProject::open(&project_path, LockPolicy::FailIfPresent) {
                    Ok(opened) => {
                        assert_eq!(opened.project.manifest().timeline.frames.len(), 2);
                        break;
                    }
                    Err(ProjectError::AlreadyLocked { .. }) => {}
                    Err(error) => panic!("completed imported project could not reopen: {error}"),
                }
            }
            assert!(
                Instant::now() < deadline,
                "detached import worker timed out"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn disconnected_worker_becomes_a_typed_terminal_error() {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut job = ImportGifJob {
            lifecycle: ImportGifLifecycle {
                state: ImportGifJobState::Running,
            },
            receiver: Some(receiver),
            result: None,
        };

        assert_eq!(job.drain(), [ImportGifJobEvent::Finished]);
        assert_eq!(job.state(), ImportGifJobState::Finished);
        assert!(matches!(
            job.take_result(),
            Some(Err(ImportGifJobError::WorkerExited))
        ));
    }

    #[test]
    fn decode_limits_are_explicit_and_strictly_bounded() {
        let options = gif_import_decode_options();
        assert_eq!(options.limits.max_width, 16_384);
        assert_eq!(options.limits.max_height, 16_384);
        assert_eq!(options.limits.max_frames, 10_000);
        assert_eq!(options.limits.max_total_rgba_bytes, 512 * 1024 * 1024);
    }
}
