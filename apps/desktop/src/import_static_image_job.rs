#![allow(
    dead_code,
    reason = "the static-image import job precedes its landing-page picker integration"
)]

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use gif_from_screen_application::{
    PersistStaticImageError, StaticImageProjectOptions, persist_decoded_static_image,
};
use gif_from_screen_domain::{FrameId, ProjectId, UnixTimeMs};
use gif_from_screen_media::{
    DecodeLimits, StaticImageDecodeError, StaticImageDecodeOptions, decode_static_image_with_format,
};
use gif_from_screen_project::ActiveProject;
use thiserror::Error;
use uuid::Uuid;

const IMPORT_STATIC_THREAD_NAME: &str = "gfs-static-image-import";
const PROJECT_EXTENSION: &str = "gfsproj";
const MAX_IMPORT_WIDTH: u16 = 16_384;
const MAX_IMPORT_HEIGHT: u16 = 16_384;
const MAX_IMPORT_RGBA_BYTES: u64 = 512 * 1024 * 1024;
const STATIC_FRAME_DURATION_US: u64 = 100_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ImportStaticImageJobState {
    #[default]
    Idle,
    Running,
    Finished,
}

#[derive(Clone, Copy, Debug, Default)]
struct ImportStaticImageLifecycle {
    state: ImportStaticImageJobState,
}

impl ImportStaticImageLifecycle {
    fn begin(&mut self) -> Result<(), ImportStaticImageJobState> {
        if self.state != ImportStaticImageJobState::Idle {
            return Err(self.state);
        }
        self.state = ImportStaticImageJobState::Running;
        Ok(())
    }

    fn start_failed(&mut self) {
        debug_assert_eq!(self.state, ImportStaticImageJobState::Running);
        self.state = ImportStaticImageJobState::Idle;
    }

    fn finish(&mut self) {
        debug_assert_eq!(self.state, ImportStaticImageJobState::Running);
        self.state = ImportStaticImageJobState::Finished;
    }
}

#[derive(Debug, Error)]
pub(crate) enum StaticImageImportPathError {
    #[error("static image input does not exist: {}", path.display())]
    NotFound { path: PathBuf },
    #[error("static image input is not a regular file: {}", path.display())]
    NotFile { path: PathBuf },
    #[error("could not inspect static image input {}: {source}", path.display())]
    InspectInput {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("input extension must be PNG, JPG/JPEG, BMP, or WebP: {}", path.display())]
    InvalidExtension { path: PathBuf },
    #[error("static image has no filename stem for its project directory: {}", path.display())]
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
pub(crate) enum ImportStaticImageJobStartError {
    #[error("this static-image import job has already started and is currently {state:?}")]
    AlreadyStarted { state: ImportStaticImageJobState },
    #[error(transparent)]
    InvalidPath(#[from] StaticImageImportPathError),
    #[error("could not spawn static-image import worker: {0}")]
    Spawn(#[source] io::Error),
}

#[derive(Debug, Error)]
pub(crate) enum ImportStaticImageJobError {
    #[error("could not open static image {}: {source}", path.display())]
    OpenInput {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not decode static image: {0}")]
    Decode(#[source] Box<StaticImageDecodeError>),
    #[error("system clock is before the Unix epoch: {0}")]
    Clock(#[source] SystemTimeError),
    #[error("current Unix timestamp {milliseconds} ms does not fit the project format")]
    TimestampOutOfRange { milliseconds: u128 },
    #[error("target project appeared while static-image import was running: {}", path.display())]
    TargetAppeared { path: PathBuf },
    #[error("could not inspect target project {}: {source}", path.display())]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not persist decoded static-image project: {0}")]
    Persist(#[source] Box<PersistStaticImageError>),
    #[error("static-image import worker exited without reporting a result")]
    WorkerExited,
}

impl From<StaticImageDecodeError> for ImportStaticImageJobError {
    fn from(value: StaticImageDecodeError) -> Self {
        Self::Decode(Box::new(value))
    }
}

impl From<PersistStaticImageError> for ImportStaticImageJobError {
    fn from(value: PersistStaticImageError) -> Self {
        Self::Persist(Box::new(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImportStaticImageJobEvent {
    Finished,
}

enum WorkerMessage {
    Finished(Box<Result<ActiveProject, ImportStaticImageJobError>>),
}

/// One-shot UI handle for bounded static-image decode and project persistence.
///
/// The current decoder and project writer have no cooperative cancellation
/// boundary, so this job deliberately exposes no `cancel` method. Dropping the
/// handle disconnects without blocking the UI. If the worker later succeeds,
/// its failed send drops [`ActiveProject`] on that thread and releases the lock.
#[derive(Default)]
pub(crate) struct ImportStaticImageJob {
    lifecycle: ImportStaticImageLifecycle,
    receiver: Option<Receiver<WorkerMessage>>,
    result: Option<Result<ActiveProject, ImportStaticImageJobError>>,
}

impl ImportStaticImageJob {
    pub(crate) fn start(&mut self, input: PathBuf) -> Result<(), ImportStaticImageJobStartError> {
        self.lifecycle
            .begin()
            .map_err(|state| ImportStaticImageJobStartError::AlreadyStarted { state })?;
        let project_path = match preflight_static_image_import(&input) {
            Ok(path) => path,
            Err(error) => {
                self.lifecycle.start_failed();
                return Err(error.into());
            }
        };
        let (sender, receiver) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name(IMPORT_STATIC_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = import_static_image(&input, &project_path);
                send_worker_result(&sender, result);
            });
        if let Err(source) = spawn {
            self.lifecycle.start_failed();
            return Err(ImportStaticImageJobStartError::Spawn(source));
        }
        self.receiver = Some(receiver);
        self.result = None;
        Ok(())
    }

    pub(crate) const fn state(&self) -> ImportStaticImageJobState {
        self.lifecycle.state
    }

    /// Static-image decoding and persistence cannot currently be cancelled safely.
    pub(crate) const fn cancellation_supported() -> bool {
        false
    }

    pub(crate) fn drain(&mut self) -> Vec<ImportStaticImageJobEvent> {
        if self.lifecycle.state != ImportStaticImageJobState::Running {
            return Vec::new();
        }
        let message = match self.receiver.as_ref().map(Receiver::try_recv) {
            Some(Ok(message)) => message,
            Some(Err(TryRecvError::Empty)) => return Vec::new(),
            Some(Err(TryRecvError::Disconnected)) | None => {
                self.finish(Err(ImportStaticImageJobError::WorkerExited));
                return vec![ImportStaticImageJobEvent::Finished];
            }
        };
        match message {
            WorkerMessage::Finished(result) => self.finish(*result),
        }
        vec![ImportStaticImageJobEvent::Finished]
    }

    pub(crate) fn result(&self) -> Option<&Result<ActiveProject, ImportStaticImageJobError>> {
        self.result.as_ref()
    }

    pub(crate) fn take_result(
        &mut self,
    ) -> Option<Result<ActiveProject, ImportStaticImageJobError>> {
        self.result.take()
    }

    fn finish(&mut self, result: Result<ActiveProject, ImportStaticImageJobError>) {
        self.lifecycle.finish();
        self.receiver = None;
        self.result = Some(result);
    }
}

impl Drop for ImportStaticImageJob {
    fn drop(&mut self) {
        self.receiver = None;
    }
}

fn preflight_static_image_import(input: &Path) -> Result<PathBuf, StaticImageImportPathError> {
    let metadata = fs::metadata(input).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            StaticImageImportPathError::NotFound {
                path: input.to_owned(),
            }
        } else {
            StaticImageImportPathError::InspectInput {
                path: input.to_owned(),
                source,
            }
        }
    })?;
    if !metadata.is_file() {
        return Err(StaticImageImportPathError::NotFile {
            path: input.to_owned(),
        });
    }
    if !has_friendly_static_extension(input) {
        return Err(StaticImageImportPathError::InvalidExtension {
            path: input.to_owned(),
        });
    }
    input
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| StaticImageImportPathError::MissingStem {
            path: input.to_owned(),
        })?;
    let project_path = input.with_extension(PROJECT_EXTENSION);
    match project_path.try_exists() {
        Ok(false) => Ok(project_path),
        Ok(true) => Err(StaticImageImportPathError::TargetExists { path: project_path }),
        Err(source) => Err(StaticImageImportPathError::InspectTarget {
            path: project_path,
            source,
        }),
    }
}

fn has_friendly_static_extension(input: &Path) -> bool {
    input
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "bmp", "webp"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn import_static_image(
    input: &Path,
    project_path: &Path,
) -> Result<ActiveProject, ImportStaticImageJobError> {
    let file = File::open(input).map_err(|source| ImportStaticImageJobError::OpenInput {
        path: input.to_owned(),
        source,
    })?;
    let decoded =
        decode_static_image_with_format(BufReader::new(file), &static_image_decode_options())?;
    ensure_target_still_absent(project_path)?;
    let format = decoded.format();
    let created_at = current_unix_time_ms()?;
    let display_name = input
        .file_name()
        .expect("preflight guarantees an input filename")
        .to_string_lossy()
        .into_owned();
    persist_decoded_static_image(
        project_path,
        decoded.into_animation(),
        format,
        StaticImageProjectOptions {
            project_id: ProjectId::from_u128(Uuid::new_v4().as_u128()),
            frame_id: FrameId::from_u128(Uuid::new_v4().as_u128()),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at,
            display_name,
        },
    )
    .map_err(Into::into)
}

fn ensure_target_still_absent(project_path: &Path) -> Result<(), ImportStaticImageJobError> {
    match project_path.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(ImportStaticImageJobError::TargetAppeared {
            path: project_path.to_owned(),
        }),
        Err(source) => Err(ImportStaticImageJobError::InspectTarget {
            path: project_path.to_owned(),
            source,
        }),
    }
}

fn static_image_decode_options() -> StaticImageDecodeOptions {
    StaticImageDecodeOptions {
        limits: DecodeLimits {
            max_width: MAX_IMPORT_WIDTH,
            max_height: MAX_IMPORT_HEIGHT,
            max_frames: 1,
            max_total_rgba_bytes: MAX_IMPORT_RGBA_BYTES,
        },
        frame_duration_us: NonZeroU64::new(STATIC_FRAME_DURATION_US)
            .expect("static import duration is non-zero"),
    }
}

fn current_unix_time_ms() -> Result<UnixTimeMs, ImportStaticImageJobError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(ImportStaticImageJobError::Clock)?
        .as_millis();
    let milliseconds = i64::try_from(milliseconds)
        .map_err(|_| ImportStaticImageJobError::TimestampOutOfRange { milliseconds })?;
    Ok(UnixTimeMs::new(milliseconds))
}

fn send_worker_result(
    sender: &Sender<WorkerMessage>,
    result: Result<ActiveProject, ImportStaticImageJobError>,
) {
    // SendError owns the complete message, so an unsent ActiveProject is
    // dropped here and releases its project lock on the worker thread.
    let _ = sender.send(WorkerMessage::Finished(Box::new(result)));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gif_from_screen_domain::SourceProvenance;
    use gif_from_screen_project::{LockPolicy, ProjectError};
    use tempfile::tempdir;

    use super::*;

    const PNG_ALPHA: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1, 1, 3,
        0, 0, 0, 206, 236, 237, 201, 0, 0, 0, 6, 80, 76, 84, 69, 0, 255, 0, 255, 0, 0, 209, 155,
        74, 174, 0, 0, 0, 1, 116, 82, 78, 83, 64, 54, 58, 153, 246, 0, 0, 0, 10, 73, 68, 65, 84, 8,
        215, 99, 104, 0, 0, 0, 130, 0, 129, 221, 67, 106, 244, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
        96, 130,
    ];
    const WEBP_ALPHA: &[u8] = &[
        82, 73, 70, 70, 32, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 19, 0, 0, 0, 47, 1, 0, 0, 16,
        15, 48, 255, 251, 31, 15, 250, 15, 7, 21, 136, 232, 127, 0, 0,
    ];
    const BMP_COLOR: &[u8] = &[
        66, 77, 146, 0, 0, 0, 0, 0, 0, 0, 138, 0, 0, 0, 124, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 1, 0,
        24, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255,
        0, 0, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 255, 66, 71, 82, 115, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 179, 101,
        42, 179, 101, 42, 0, 0,
    ];
    const JPEG_COLOR: &[u8] = &[
        255, 216, 255, 224, 0, 16, 74, 70, 73, 70, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 255, 219, 0, 67,
        0, 2, 1, 1, 1, 1, 1, 2, 1, 1, 1, 2, 2, 2, 2, 2, 4, 3, 2, 2, 2, 2, 5, 4, 4, 3, 4, 6, 5, 6,
        6, 6, 5, 6, 6, 6, 7, 9, 8, 6, 7, 9, 7, 6, 6, 8, 11, 8, 9, 10, 10, 10, 10, 10, 6, 8, 11, 12,
        11, 10, 12, 9, 10, 10, 10, 255, 219, 0, 67, 1, 2, 2, 2, 2, 2, 2, 5, 3, 3, 5, 10, 7, 6, 7,
        10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
        10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
        10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 255, 192, 0, 17, 8, 0, 1, 0, 2, 3, 1, 17,
        0, 2, 17, 1, 3, 17, 1, 255, 196, 0, 20, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        8, 255, 196, 0, 20, 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 196, 0, 20,
        1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 255, 196, 0, 20, 17, 1, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 12, 3, 1, 0, 2, 17, 3, 17, 0, 63, 0, 55, 152,
        131, 123, 255, 217,
    ];
    const JPEG_COLOR_IMAGE: &[u8] = &[
        255, 216, 255, 224, 0, 16, 74, 70, 73, 70, 0, 1, 2, 0, 0, 1, 0, 1, 0, 0, 255, 192, 0, 17,
        8, 0, 2, 0, 3, 3, 1, 17, 0, 2, 17, 1, 3, 17, 1, 255, 219, 0, 67, 0, 8, 6, 6, 7, 6, 5, 8, 7,
        7, 7, 9, 9, 8, 10, 12, 20, 13, 12, 11, 11, 12, 25, 18, 19, 15, 20, 29, 26, 31, 30, 29, 26,
        28, 28, 32, 36, 46, 39, 32, 34, 44, 35, 28, 28, 40, 55, 41, 44, 48, 49, 52, 52, 52, 31, 39,
        57, 61, 56, 50, 60, 46, 51, 52, 50, 255, 219, 0, 67, 1, 9, 9, 9, 12, 11, 12, 24, 13, 13,
        24, 50, 33, 28, 33, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
        50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
        50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 255, 196, 0, 31, 0, 0, 1, 5, 1, 1, 1,
        1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 255, 196, 0, 181, 16,
        0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125, 1, 2, 3, 0, 4, 17, 5, 18, 33, 49, 65, 6,
        19, 81, 97, 7, 34, 113, 20, 50, 129, 145, 161, 8, 35, 66, 177, 193, 21, 82, 209, 240, 36,
        51, 98, 114, 130, 9, 10, 22, 23, 24, 25, 26, 37, 38, 39, 40, 41, 42, 52, 53, 54, 55, 56,
        57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88, 89, 90, 99, 100, 101, 102,
        103, 104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 131, 132, 133, 134, 135, 136,
        137, 138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164, 165, 166, 167, 168,
        169, 170, 178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196, 197, 198, 199, 200,
        201, 202, 210, 211, 212, 213, 214, 215, 216, 217, 218, 225, 226, 227, 228, 229, 230, 231,
        232, 233, 234, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 255, 196, 0, 31, 1, 0, 3,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 255, 196,
        0, 181, 17, 0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119, 0, 1, 2, 3, 17, 4, 5, 33, 49,
        6, 18, 65, 81, 7, 97, 113, 19, 34, 50, 129, 8, 20, 66, 145, 161, 177, 193, 9, 35, 51, 82,
        240, 21, 98, 114, 209, 10, 22, 36, 52, 225, 37, 241, 23, 24, 25, 26, 38, 39, 40, 41, 42,
        53, 54, 55, 56, 57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88, 89, 90, 99,
        100, 101, 102, 103, 104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 130, 131, 132,
        133, 134, 135, 136, 137, 138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164,
        165, 166, 167, 168, 169, 170, 178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196,
        197, 198, 199, 200, 201, 202, 210, 211, 212, 213, 214, 215, 216, 217, 218, 226, 227, 228,
        229, 230, 231, 232, 233, 234, 242, 243, 244, 245, 246, 247, 248, 249, 250, 255, 218, 0, 12,
        3, 1, 0, 2, 17, 3, 17, 0, 63, 0, 230, 235, 235, 15, 159, 63, 255, 217,
    ];

    fn wait_for_finished(job: &mut ImportStaticImageJob) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while job.state() != ImportStaticImageJobState::Finished {
            let _ = job.drain();
            assert!(Instant::now() < deadline, "static import worker timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn imported_media_type(project: &ActiveProject) -> &str {
        match project.manifest().source_provenance.as_slice() {
            [SourceProvenance::Imported { media_type, .. }] => media_type,
            provenance => panic!("unexpected provenance: {provenance:?}"),
        }
    }

    #[test]
    fn lifecycle_is_one_shot_and_explicitly_not_cancellable() {
        let mut lifecycle = ImportStaticImageLifecycle::default();
        lifecycle.begin().unwrap();
        assert_eq!(lifecycle.begin(), Err(ImportStaticImageJobState::Running));
        lifecycle.start_failed();
        assert_eq!(lifecycle.state, ImportStaticImageJobState::Idle);
        lifecycle.begin().unwrap();
        lifecycle.finish();
        assert_eq!(lifecycle.begin(), Err(ImportStaticImageJobState::Finished));
        assert!(!ImportStaticImageJob::cancellation_supported());
    }

    #[test]
    fn png_alpha_import_uses_content_detected_mime_not_jpeg_extension() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("misleading.jpg");
        fs::write(&input, PNG_ALPHA).unwrap();
        let mut job = ImportStaticImageJob::default();
        job.start(input.clone()).unwrap();
        assert!(matches!(
            job.start(input.clone()),
            Err(ImportStaticImageJobStartError::AlreadyStarted {
                state: ImportStaticImageJobState::Running
            })
        ));

        wait_for_finished(&mut job);

        let project = job.take_result().unwrap().unwrap();
        assert_eq!(
            project.layout().root,
            input.with_extension(PROJECT_EXTENSION)
        );
        assert_eq!(imported_media_type(&project), "image/png");
        let clip = &project.manifest().timeline.frames[0];
        assert_eq!(clip.duration.get(), STATIC_FRAME_DURATION_US);
        assert_eq!(
            project.assets().read(clip.asset_id).unwrap(),
            [255, 0, 0, 255, 0, 255, 0, 64]
        );
        assert!(matches!(
            project.manifest().source_provenance.as_slice(),
            [SourceProvenance::Imported { display_name, .. }] if display_name == "misleading.jpg"
        ));
    }

    #[test]
    fn jpeg_bmp_and_webp_jobs_persist_content_detected_formats() {
        for (filename, bytes, expected_mime) in [
            ("photo.jpeg", JPEG_COLOR_IMAGE, "image/jpeg"),
            ("color.bmp", BMP_COLOR, "image/bmp"),
            ("alpha.webp", WEBP_ALPHA, "image/webp"),
        ] {
            let directory = tempdir().unwrap();
            let input = directory.path().join(filename);
            fs::write(&input, bytes).unwrap();
            let mut job = ImportStaticImageJob::default();
            job.start(input).unwrap();
            wait_for_finished(&mut job);
            let project = job.take_result().unwrap().unwrap();
            assert_eq!(imported_media_type(&project), expected_mime);
            assert_eq!(project.manifest().timeline.frames.len(), 1);
            assert_eq!(
                project.manifest().timeline.frames[0].duration.get(),
                STATIC_FRAME_DURATION_US
            );
        }
    }

    #[test]
    fn malformed_input_returns_decode_error_without_creating_target() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("broken.png");
        fs::write(&input, b"not an image").unwrap();
        let mut job = ImportStaticImageJob::default();
        job.start(input.clone()).unwrap();
        wait_for_finished(&mut job);

        assert!(matches!(
            job.take_result(),
            Some(Err(ImportStaticImageJobError::Decode(_)))
        ));
        assert!(!input.with_extension(PROJECT_EXTENSION).exists());
    }

    #[test]
    fn path_preflight_filters_extensions_and_rejects_existing_target() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("missing.png");
        assert!(matches!(
            preflight_static_image_import(&missing),
            Err(StaticImageImportPathError::NotFound { .. })
        ));
        let folder = directory.path().join("folder.webp");
        fs::create_dir(&folder).unwrap();
        assert!(matches!(
            preflight_static_image_import(&folder),
            Err(StaticImageImportPathError::NotFile { .. })
        ));
        let unsupported = directory.path().join("image.tiff");
        fs::write(&unsupported, PNG_ALPHA).unwrap();
        assert!(matches!(
            preflight_static_image_import(&unsupported),
            Err(StaticImageImportPathError::InvalidExtension { .. })
        ));
        for extension in ["png", "JPG", "jpeg", "BMP", "webp"] {
            let input = directory
                .path()
                .join(format!("friendly-{extension}.{extension}"));
            fs::write(&input, PNG_ALPHA).unwrap();
            assert_eq!(
                preflight_static_image_import(&input).unwrap(),
                input.with_extension(PROJECT_EXTENSION)
            );
        }

        let input = directory.path().join("existing.png");
        fs::write(&input, PNG_ALPHA).unwrap();
        fs::create_dir(input.with_extension(PROJECT_EXTENSION)).unwrap();
        let mut job = ImportStaticImageJob::default();
        assert!(matches!(
            job.start(input),
            Err(ImportStaticImageJobStartError::InvalidPath(
                StaticImageImportPathError::TargetExists { .. }
            ))
        ));
        assert_eq!(job.state(), ImportStaticImageJobState::Idle);
    }

    #[test]
    fn receiver_drop_after_start_eventually_releases_project_lock() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("detached.png");
        fs::write(&input, PNG_ALPHA).unwrap();
        let project_path = input.with_extension(PROJECT_EXTENSION);
        let mut job = ImportStaticImageJob::default();
        job.start(input).unwrap();
        drop(job);

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if project_path.join("manifest.json").is_file() {
                match ActiveProject::open(&project_path, LockPolicy::FailIfPresent) {
                    Ok(opened) => {
                        assert_eq!(opened.project.manifest().timeline.frames.len(), 1);
                        break;
                    }
                    Err(ProjectError::AlreadyLocked { .. }) => {}
                    Err(error) => panic!("static project could not reopen: {error}"),
                }
            }
            assert!(
                Instant::now() < deadline,
                "detached static import timed out"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn failed_result_send_drops_project_and_disconnected_worker_is_typed() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("unsent.png");
        fs::write(&input, PNG_ALPHA).unwrap();
        let project_path = input.with_extension(PROJECT_EXTENSION);
        let project = import_static_image(&input, &project_path).unwrap();
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        send_worker_result(&sender, Ok(project));
        assert!(!project_path.join("project.lock").exists());

        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut job = ImportStaticImageJob {
            lifecycle: ImportStaticImageLifecycle {
                state: ImportStaticImageJobState::Running,
            },
            receiver: Some(receiver),
            result: None,
        };
        assert_eq!(job.drain(), [ImportStaticImageJobEvent::Finished]);
        assert!(matches!(
            job.take_result(),
            Some(Err(ImportStaticImageJobError::WorkerExited))
        ));
    }

    #[test]
    fn decode_limits_and_single_frame_duration_are_explicit() {
        let options = static_image_decode_options();
        assert_eq!(options.limits.max_width, 16_384);
        assert_eq!(options.limits.max_height, 16_384);
        assert_eq!(options.limits.max_frames, 1);
        assert_eq!(options.limits.max_total_rgba_bytes, 512 * 1024 * 1024);
        assert_eq!(options.frame_duration_us.get(), 100_000);
    }
}
