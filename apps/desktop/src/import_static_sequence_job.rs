#![allow(
    dead_code,
    reason = "the static-sequence background job precedes its desktop form integration"
)]

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, BufReader},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use gif_from_screen_application::{
    PersistStaticImageSequenceError, StaticImageSequenceProjectOptions,
    persist_decoded_static_image_sequence,
};
use gif_from_screen_domain::{FrameId, ProjectId, UnixTimeMs};
use gif_from_screen_media::{
    DecodeLimits, LoopBehavior, StaticImageDecodeError, StaticImageDecodeOptions,
    StaticImageSequenceDurationPolicy, StaticImageSequenceError, StaticImageSequenceOptions,
    assemble_static_image_sequence, decode_static_image_with_format,
};
use gif_from_screen_project::ActiveProject;
use thiserror::Error;

const IMPORT_STATIC_SEQUENCE_THREAD_NAME: &str = "gfs-static-sequence-import";
const PROJECT_EXTENSION: &str = "gfsproj";
const MIN_SEQUENCE_FRAMES: usize = 2;

/// Complete deterministic input for one ordered static-image sequence import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImportStaticSequenceRequest {
    /// Source paths in desired timeline order.
    pub(crate) inputs: Vec<PathBuf>,
    /// New editable project directory. Existing entries are never reused.
    pub(crate) target: PathBuf,
    /// Uniform replacement timing or explicit preservation of decoder timing.
    ///
    /// Static files contain no animation delay metadata, so `PreserveDecoded`
    /// retains the positive default assigned by [`StaticImageDecodeOptions`].
    pub(crate) duration_policy: StaticImageSequenceDurationPolicy,
    /// Playback loop metadata attached to the assembled animation.
    pub(crate) loop_behavior: LoopBehavior,
    /// Per-file and complete-sequence decode bounds.
    pub(crate) limits: DecodeLimits,
    /// Stable identity of the created project.
    pub(crate) project_id: ProjectId,
    /// Stable frame identities consumed in source order.
    pub(crate) frame_ids: Vec<FrameId>,
    /// Persisted application version.
    pub(crate) app_version: String,
    /// Injected wall-clock creation time.
    pub(crate) created_at: UnixTimeMs,
    /// One non-empty user-facing label per source path.
    pub(crate) display_names: Vec<String>,
}

/// Observable lifecycle of one sequence-import worker.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ImportStaticSequenceJobState {
    /// No request is active, including after synchronous preflight failure.
    #[default]
    Idle,
    /// The named worker is decoding, assembling, or persisting.
    Running,
    /// A terminal result has been collected for the UI.
    Finished,
}

#[derive(Clone, Copy, Debug, Default)]
struct ImportStaticSequenceLifecycle {
    state: ImportStaticSequenceJobState,
}

impl ImportStaticSequenceLifecycle {
    fn begin(&mut self) -> Result<(), ImportStaticSequenceJobState> {
        if self.state != ImportStaticSequenceJobState::Idle {
            return Err(self.state);
        }
        self.state = ImportStaticSequenceJobState::Running;
        Ok(())
    }

    fn start_failed(&mut self) {
        debug_assert_eq!(self.state, ImportStaticSequenceJobState::Running);
        self.state = ImportStaticSequenceJobState::Idle;
    }

    fn finish(&mut self) {
        debug_assert_eq!(self.state, ImportStaticSequenceJobState::Running);
        self.state = ImportStaticSequenceJobState::Finished;
    }
}

/// Friendly synchronous request/path validation failure.
#[derive(Debug, Error)]
pub(crate) enum StaticSequenceImportPathError {
    /// A sequence requires at least two images.
    #[error("static-image sequence needs at least {minimum} inputs, got {actual}")]
    TooFewInputs { minimum: usize, actual: usize },
    /// Input count must fit the shared frame limit before a worker starts.
    #[error("static-image sequence has {actual} inputs, above the configured limit of {limit}")]
    FrameLimitExceeded { actual: usize, limit: usize },
    /// Every source needs a corresponding provenance label.
    #[error("static-image sequence has {inputs} inputs but {labels} labels")]
    DisplayNameCountMismatch { inputs: usize, labels: usize },
    /// Every source needs one stable frame identity.
    #[error("static-image sequence has {inputs} inputs but {frame_ids} frame ids")]
    FrameIdCountMismatch { inputs: usize, frame_ids: usize },
    /// The reserved nil project identity is never persisted.
    #[error("static-image sequence project id must not be nil")]
    NilProjectId,
    /// A frame identity may not use the reserved nil value.
    #[error("static-image sequence frame id {image_index} must not be nil")]
    NilFrameId { image_index: usize },
    /// Frame identities must remain unique in the new timeline.
    #[error("static-image sequence frame id {frame_id} is duplicated at index {image_index}")]
    DuplicateFrameId {
        image_index: usize,
        frame_id: FrameId,
    },
    /// Duplicate detection could not reserve its bounded identity set.
    #[error("could not reserve {requested} static-image sequence frame identities")]
    FrameIdSetAllocationFailed { requested: usize },
    /// Persisted application metadata requires a visible version.
    #[error("static-image sequence application version must not be empty")]
    EmptyAppVersion,
    /// Provenance labels must contain visible text.
    #[error("static-image sequence label {image_index} must not be empty")]
    EmptyDisplayName { image_index: usize },
    /// A finite animation repeat count must be positive.
    #[error("static-image sequence finite loop count must be positive")]
    ZeroFiniteLoopCount,
    /// One source path does not exist.
    #[error("static-image sequence input {image_index} does not exist: {}", path.display())]
    InputNotFound { image_index: usize, path: PathBuf },
    /// Every source must be a regular file.
    #[error("static-image sequence input {image_index} is not a regular file: {}", path.display())]
    InputNotFile { image_index: usize, path: PathBuf },
    /// Source metadata could not be read safely.
    #[error("could not inspect static-image sequence input {image_index} {}: {source}", path.display())]
    InspectInput {
        image_index: usize,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Extensions are a friendly filter; codecs still detect actual content.
    #[error(
        "static-image sequence input {image_index} must end in PNG, JPG/JPEG, BMP, or WebP: {}",
        path.display()
    )]
    InvalidInputExtension { image_index: usize, path: PathBuf },
    /// Sequence targets use the editable-project directory suffix.
    #[error("static-image sequence target must end in .{PROJECT_EXTENSION}: {}", path.display())]
    InvalidTargetExtension { path: PathBuf },
    /// The target's parent must already exist.
    #[error("static-image sequence target parent does not exist: {}", path.display())]
    ParentNotFound { path: PathBuf },
    /// The target's parent must be a directory.
    #[error("static-image sequence target parent is not a directory: {}", path.display())]
    ParentNotDirectory { path: PathBuf },
    /// Parent metadata could not be inspected.
    #[error("could not inspect static-image sequence target parent {}: {source}", path.display())]
    InspectParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Existing project paths are never replaced.
    #[error("static-image sequence target already exists: {}", path.display())]
    TargetExists { path: PathBuf },
    /// Target existence could not be inspected.
    #[error("could not inspect static-image sequence target {}: {source}", path.display())]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Failure that prevents a sequence-import job from starting.
#[derive(Debug, Error)]
pub(crate) enum ImportStaticSequenceJobStartError {
    /// This one-shot handle is already running or finished.
    #[error("this static-sequence import job has already started and is currently {state:?}")]
    AlreadyStarted { state: ImportStaticSequenceJobState },
    /// Lightweight synchronous request/path preflight failed.
    #[error(transparent)]
    InvalidRequest(#[from] StaticSequenceImportPathError),
    /// The named background thread could not be created.
    #[error("could not spawn static-sequence import worker: {0}")]
    Spawn(#[source] io::Error),
}

/// Terminal failure produced by the sequence-import worker.
#[derive(Debug, Error)]
pub(crate) enum ImportStaticSequenceJobError {
    /// A preflighted source could no longer be opened.
    #[error("could not open static-image sequence input {image_index} {}: {source}", path.display())]
    OpenInput {
        image_index: usize,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Content detection or bounded pixel decoding failed.
    #[error("could not decode static-image sequence input {image_index} {}: {source}", path.display())]
    Decode {
        image_index: usize,
        path: PathBuf,
        #[source]
        source: Box<StaticImageDecodeError>,
    },
    /// The decoder violated its one-frame static-image contract.
    #[error("decoded static-image sequence input {image_index} has {actual} frames")]
    DecodedFrameCount { image_index: usize, actual: usize },
    /// A decoded frame length cannot be represented by the cumulative counter.
    #[error("decoded static-image sequence input {image_index} byte length exceeds u64")]
    RgbaLengthOutOfRange { image_index: usize },
    /// Cumulative retained RGBA bytes overflowed.
    #[error("static-image sequence retained byte count overflowed at input {image_index}")]
    TotalRgbaBytesOverflow { image_index: usize },
    /// Decoding another source would exceed the overall retained RGBA bound.
    #[error(
        "static-image sequence needs {required_bytes} RGBA bytes at input {image_index}, above the configured limit of {limit_bytes}"
    )]
    TotalRgbaBytesLimitExceeded {
        image_index: usize,
        required_bytes: u64,
        limit_bytes: u64,
    },
    /// The decoded source list could not reserve its bounded frame count.
    #[error("could not reserve {requested} decoded static-image inputs")]
    DecodedListAllocationFailed { requested: usize },
    /// Whole-sequence timing, dimensions, or limits were rejected.
    #[error("could not assemble static-image sequence: {0}")]
    Assemble(#[source] Box<StaticImageSequenceError>),
    /// Another entry appeared at the target after preflight.
    #[error("static-image sequence target appeared while importing: {}", path.display())]
    TargetAppeared { path: PathBuf },
    /// Target existence could no longer be inspected.
    #[error("could not inspect static-image sequence target {}: {source}", path.display())]
    InspectTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Durable editable-project creation failed.
    #[error("could not persist static-image sequence: {0}")]
    Persist(#[source] Box<PersistStaticImageSequenceError>),
    /// The worker channel disconnected without a terminal result.
    #[error("static-sequence import worker exited without reporting a result")]
    WorkerExited,
}

impl From<StaticImageSequenceError> for ImportStaticSequenceJobError {
    fn from(value: StaticImageSequenceError) -> Self {
        Self::Assemble(Box::new(value))
    }
}

impl From<PersistStaticImageSequenceError> for ImportStaticSequenceJobError {
    fn from(value: PersistStaticImageSequenceError) -> Self {
        Self::Persist(Box::new(value))
    }
}

/// Non-blocking lifecycle event emitted after worker completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImportStaticSequenceJobEvent {
    /// A terminal result is ready through `result` or `take_result`.
    Finished,
}

enum WorkerMessage {
    Finished(Box<Result<ActiveProject, ImportStaticSequenceJobError>>),
}

/// UI-owned one-shot handle for ordered static-image sequence import.
///
/// Decode, assembly, and persistence currently have no shared cooperative
/// cancellation boundary. This job intentionally exposes no `cancel`; dropping
/// it only disconnects the UI without joining. A successful unsent result is
/// dropped on the worker and releases its [`ActiveProject`] lock.
#[derive(Default)]
pub(crate) struct ImportStaticSequenceJob {
    lifecycle: ImportStaticSequenceLifecycle,
    receiver: Option<Receiver<WorkerMessage>>,
    result: Option<Result<ActiveProject, ImportStaticSequenceJobError>>,
}

impl ImportStaticSequenceJob {
    /// Runs lightweight validation and starts a named background worker.
    ///
    /// # Errors
    ///
    /// Returns a typed request/path, duplicate-start, or thread-spawn error.
    /// Synchronous failure restores `Idle`, allowing the same handle to retry.
    pub(crate) fn start(
        &mut self,
        request: ImportStaticSequenceRequest,
    ) -> Result<(), ImportStaticSequenceJobStartError> {
        self.lifecycle
            .begin()
            .map_err(|state| ImportStaticSequenceJobStartError::AlreadyStarted { state })?;
        if let Err(error) = preflight_static_sequence_request(&request) {
            self.lifecycle.start_failed();
            return Err(error.into());
        }
        let (sender, receiver) = mpsc::channel();
        let spawn = thread::Builder::new()
            .name(IMPORT_STATIC_SEQUENCE_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = import_static_sequence(request);
                send_worker_result(&sender, result);
            });
        if let Err(source) = spawn {
            self.lifecycle.start_failed();
            return Err(ImportStaticSequenceJobStartError::Spawn(source));
        }
        self.receiver = Some(receiver);
        self.result = None;
        Ok(())
    }

    /// Returns the current one-shot lifecycle state.
    pub(crate) const fn state(&self) -> ImportStaticSequenceJobState {
        self.lifecycle.state
    }

    /// Sequence import is explicitly non-cooperative in this initial job.
    pub(crate) const fn cancellation_supported() -> bool {
        false
    }

    /// Drains at most one terminal worker message without blocking.
    pub(crate) fn drain(&mut self) -> Vec<ImportStaticSequenceJobEvent> {
        if self.lifecycle.state != ImportStaticSequenceJobState::Running {
            return Vec::new();
        }
        let message = match self.receiver.as_ref().map(Receiver::try_recv) {
            Some(Ok(message)) => message,
            Some(Err(TryRecvError::Empty)) => return Vec::new(),
            Some(Err(TryRecvError::Disconnected)) | None => {
                self.finish(Err(ImportStaticSequenceJobError::WorkerExited));
                return vec![ImportStaticSequenceJobEvent::Finished];
            }
        };
        match message {
            WorkerMessage::Finished(result) => self.finish(*result),
        }
        vec![ImportStaticSequenceJobEvent::Finished]
    }

    /// Borrows the terminal result while preserving a successful project lock.
    pub(crate) fn result(&self) -> Option<&Result<ActiveProject, ImportStaticSequenceJobError>> {
        self.result.as_ref()
    }

    /// Takes the terminal result for conversion into an editor workspace.
    pub(crate) fn take_result(
        &mut self,
    ) -> Option<Result<ActiveProject, ImportStaticSequenceJobError>> {
        self.result.take()
    }

    fn finish(&mut self, result: Result<ActiveProject, ImportStaticSequenceJobError>) {
        self.lifecycle.finish();
        self.receiver = None;
        self.result = Some(result);
    }
}

impl Drop for ImportStaticSequenceJob {
    fn drop(&mut self) {
        self.receiver = None;
    }
}

fn preflight_static_sequence_request(
    request: &ImportStaticSequenceRequest,
) -> Result<(), StaticSequenceImportPathError> {
    if request.inputs.len() < MIN_SEQUENCE_FRAMES {
        return Err(StaticSequenceImportPathError::TooFewInputs {
            minimum: MIN_SEQUENCE_FRAMES,
            actual: request.inputs.len(),
        });
    }
    if request.inputs.len() > request.limits.max_frames {
        return Err(StaticSequenceImportPathError::FrameLimitExceeded {
            actual: request.inputs.len(),
            limit: request.limits.max_frames,
        });
    }
    validate_static_sequence_metadata(request)?;
    for (image_index, input) in request.inputs.iter().enumerate() {
        preflight_static_input(input, image_index)?;
    }
    preflight_sequence_target(&request.target)
}

fn validate_static_sequence_metadata(
    request: &ImportStaticSequenceRequest,
) -> Result<(), StaticSequenceImportPathError> {
    if request.display_names.len() != request.inputs.len() {
        return Err(StaticSequenceImportPathError::DisplayNameCountMismatch {
            inputs: request.inputs.len(),
            labels: request.display_names.len(),
        });
    }
    if let Some(image_index) = request
        .display_names
        .iter()
        .position(|name| name.trim().is_empty())
    {
        return Err(StaticSequenceImportPathError::EmptyDisplayName { image_index });
    }
    if request.frame_ids.len() != request.inputs.len() {
        return Err(StaticSequenceImportPathError::FrameIdCountMismatch {
            inputs: request.inputs.len(),
            frame_ids: request.frame_ids.len(),
        });
    }
    if request.project_id.is_nil() {
        return Err(StaticSequenceImportPathError::NilProjectId);
    }
    if request.app_version.trim().is_empty() {
        return Err(StaticSequenceImportPathError::EmptyAppVersion);
    }
    if matches!(request.loop_behavior, LoopBehavior::Finite(0)) {
        return Err(StaticSequenceImportPathError::ZeroFiniteLoopCount);
    }
    let mut frame_ids = HashSet::new();
    frame_ids
        .try_reserve(request.frame_ids.len())
        .map_err(
            |_| StaticSequenceImportPathError::FrameIdSetAllocationFailed {
                requested: request.frame_ids.len(),
            },
        )?;
    for (image_index, frame_id) in request.frame_ids.iter().copied().enumerate() {
        if frame_id.is_nil() {
            return Err(StaticSequenceImportPathError::NilFrameId { image_index });
        }
        if !frame_ids.insert(frame_id) {
            return Err(StaticSequenceImportPathError::DuplicateFrameId {
                image_index,
                frame_id,
            });
        }
    }
    Ok(())
}

fn preflight_static_input(
    input: &Path,
    image_index: usize,
) -> Result<(), StaticSequenceImportPathError> {
    let metadata = fs::metadata(input).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            StaticSequenceImportPathError::InputNotFound {
                image_index,
                path: input.to_owned(),
            }
        } else {
            StaticSequenceImportPathError::InspectInput {
                image_index,
                path: input.to_owned(),
                source,
            }
        }
    })?;
    if !metadata.is_file() {
        return Err(StaticSequenceImportPathError::InputNotFile {
            image_index,
            path: input.to_owned(),
        });
    }
    if !has_supported_static_extension(input) {
        return Err(StaticSequenceImportPathError::InvalidInputExtension {
            image_index,
            path: input.to_owned(),
        });
    }
    Ok(())
}

fn preflight_sequence_target(target: &Path) -> Result<(), StaticSequenceImportPathError> {
    let valid_extension = target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(PROJECT_EXTENSION));
    if !valid_extension {
        return Err(StaticSequenceImportPathError::InvalidTargetExtension {
            path: target.to_owned(),
        });
    }
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let metadata = fs::metadata(parent).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            StaticSequenceImportPathError::ParentNotFound {
                path: parent.to_owned(),
            }
        } else {
            StaticSequenceImportPathError::InspectParent {
                path: parent.to_owned(),
                source,
            }
        }
    })?;
    if !metadata.is_dir() {
        return Err(StaticSequenceImportPathError::ParentNotDirectory {
            path: parent.to_owned(),
        });
    }
    match target.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(StaticSequenceImportPathError::TargetExists {
            path: target.to_owned(),
        }),
        Err(source) => Err(StaticSequenceImportPathError::InspectTarget {
            path: target.to_owned(),
            source,
        }),
    }
}

fn has_supported_static_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "bmp", "webp"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn import_static_sequence(
    request: ImportStaticSequenceRequest,
) -> Result<ActiveProject, ImportStaticSequenceJobError> {
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(request.inputs.len())
        .map_err(
            |_| ImportStaticSequenceJobError::DecodedListAllocationFailed {
                requested: request.inputs.len(),
            },
        )?;
    let decode_options = StaticImageDecodeOptions {
        limits: DecodeLimits {
            max_frames: 1,
            ..request.limits
        },
        frame_duration_us: match request.duration_policy {
            StaticImageSequenceDurationPolicy::PreserveDecoded => {
                StaticImageDecodeOptions::default().frame_duration_us
            }
            StaticImageSequenceDurationPolicy::Uniform(duration) => duration,
        },
    };
    let mut retained_rgba_bytes = 0_u64;
    for (image_index, input) in request.inputs.iter().enumerate() {
        let file = File::open(input).map_err(|source| ImportStaticSequenceJobError::OpenInput {
            image_index,
            path: input.clone(),
            source,
        })?;
        let image = decode_static_image_with_format(BufReader::new(file), &decode_options)
            .map_err(|source| ImportStaticSequenceJobError::Decode {
                image_index,
                path: input.clone(),
                source: Box::new(source),
            })?;
        let frames = image.animation().frames();
        let Some(frame) = frames.first() else {
            return Err(ImportStaticSequenceJobError::DecodedFrameCount {
                image_index,
                actual: 0,
            });
        };
        if frames.len() != 1 {
            return Err(ImportStaticSequenceJobError::DecodedFrameCount {
                image_index,
                actual: frames.len(),
            });
        }
        let frame_bytes = u64::try_from(frame.rgba().len())
            .map_err(|_| ImportStaticSequenceJobError::RgbaLengthOutOfRange { image_index })?;
        let required_bytes = retained_rgba_bytes
            .checked_add(frame_bytes)
            .ok_or(ImportStaticSequenceJobError::TotalRgbaBytesOverflow { image_index })?;
        if required_bytes > request.limits.max_total_rgba_bytes {
            return Err(ImportStaticSequenceJobError::TotalRgbaBytesLimitExceeded {
                image_index,
                required_bytes,
                limit_bytes: request.limits.max_total_rgba_bytes,
            });
        }
        retained_rgba_bytes = required_bytes;
        decoded.push(image);
    }

    let sequence = assemble_static_image_sequence(
        decoded,
        &StaticImageSequenceOptions {
            limits: request.limits,
            duration: request.duration_policy,
            loop_behavior: request.loop_behavior,
        },
    )?;
    ensure_target_still_absent(&request.target)?;
    persist_decoded_static_image_sequence(
        &request.target,
        sequence,
        StaticImageSequenceProjectOptions {
            project_id: request.project_id,
            frame_ids: request.frame_ids,
            app_version: request.app_version,
            created_at: request.created_at,
            display_names: request.display_names,
        },
    )
    .map_err(Into::into)
}

fn ensure_target_still_absent(target: &Path) -> Result<(), ImportStaticSequenceJobError> {
    match target.try_exists() {
        Ok(false) => Ok(()),
        Ok(true) => Err(ImportStaticSequenceJobError::TargetAppeared {
            path: target.to_owned(),
        }),
        Err(source) => Err(ImportStaticSequenceJobError::InspectTarget {
            path: target.to_owned(),
            source,
        }),
    }
}

fn send_worker_result(
    sender: &Sender<WorkerMessage>,
    result: Result<ActiveProject, ImportStaticSequenceJobError>,
) {
    // SendError retains the complete result, so dropping it here releases a
    // successful project's lock when the UI receiver has disappeared.
    let _ = sender.send(WorkerMessage::Finished(Box::new(result)));
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU64, time::Duration};

    use gif_from_screen_application::IMPORTED_STATIC_SEQUENCE_PRESET_NAME;
    use gif_from_screen_domain::{GifLoop, SourceProvenance};
    use gif_from_screen_media::DEFAULT_ZERO_DELAY_US;
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
    const BMP_COLOR: &[u8] = &[
        66, 77, 146, 0, 0, 0, 0, 0, 0, 0, 138, 0, 0, 0, 124, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 1, 0,
        24, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 255, 66, 71, 82, 115, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 179, 101, 42, 179, 101, 42, 0, 0,
    ];

    fn write_inputs(directory: &Path) -> Vec<PathBuf> {
        let first = directory.join("first.png");
        let second = directory.join("second.bmp");
        let third = directory.join("third.png");
        fs::write(&first, PNG_ALPHA).unwrap();
        fs::write(&second, BMP_COLOR).unwrap();
        fs::write(&third, PNG_ALPHA).unwrap();
        vec![first, second, third]
    }

    fn request(inputs: Vec<PathBuf>, target: PathBuf) -> ImportStaticSequenceRequest {
        let display_names = inputs
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        ImportStaticSequenceRequest {
            frame_ids: (0..inputs.len())
                .map(|index| FrameId::from_u128(u128::try_from(index + 10).unwrap()))
                .collect(),
            inputs,
            target,
            duration_policy: StaticImageSequenceDurationPolicy::Uniform(
                NonZeroU64::new(25_000).unwrap(),
            ),
            loop_behavior: LoopBehavior::Finite(3),
            limits: DecodeLimits {
                max_width: 16_384,
                max_height: 16_384,
                max_frames: 10_000,
                max_total_rgba_bytes: 512 * 1024 * 1024,
            },
            project_id: ProjectId::from_u128(1),
            app_version: "sequence-job-test".to_owned(),
            created_at: UnixTimeMs::new(42),
            display_names,
        }
    }

    fn wait_for_finished(job: &mut ImportStaticSequenceJob) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while job.state() != ImportStaticSequenceJobState::Finished {
            let _ = job.drain();
            assert!(
                std::time::Instant::now() < deadline,
                "static-sequence worker timed out"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn imports_order_mime_loop_and_content_deduplicated_frames() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        let target = directory.path().join("sequence.gfsproj");
        let mut job = ImportStaticSequenceJob::default();
        job.start(request(inputs, target.clone())).unwrap();
        wait_for_finished(&mut job);

        let project = job.take_result().unwrap().unwrap();
        let manifest = project.manifest();
        assert_eq!(manifest.timeline.frames.len(), 3);
        assert_eq!(manifest.assets.len(), 2);
        assert_eq!(manifest.timeline.frames[0].id, FrameId::from_u128(10));
        assert_eq!(manifest.timeline.frames[1].id, FrameId::from_u128(11));
        assert_eq!(manifest.timeline.frames[2].id, FrameId::from_u128(12));
        assert_eq!(
            manifest
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [25_000, 25_000, 25_000]
        );
        assert_eq!(
            manifest.timeline.frames[0].asset_id,
            manifest.timeline.frames[2].asset_id
        );
        assert_ne!(
            manifest.timeline.frames[0].asset_id,
            manifest.timeline.frames[1].asset_id
        );
        assert_eq!(
            manifest.source_provenance,
            [
                SourceProvenance::Imported {
                    display_name: "first.png".to_owned(),
                    media_type: "image/png".to_owned(),
                },
                SourceProvenance::Imported {
                    display_name: "second.bmp".to_owned(),
                    media_type: "image/bmp".to_owned(),
                },
                SourceProvenance::Imported {
                    display_name: "third.png".to_owned(),
                    media_type: "image/png".to_owned(),
                },
            ]
        );
        assert_eq!(
            manifest.export_presets[IMPORTED_STATIC_SEQUENCE_PRESET_NAME].repeat,
            GifLoop::Finite(3)
        );
        assert_eq!(project.layout().root, target);
    }

    #[test]
    fn preserve_duration_and_every_valid_loop_policy_reach_the_project() {
        for (loop_behavior, expected_loop) in [
            (LoopBehavior::Once, GifLoop::Finite(1)),
            (LoopBehavior::Infinite, GifLoop::Infinite),
            (LoopBehavior::Finite(7), GifLoop::Finite(7)),
        ] {
            let directory = tempdir().unwrap();
            let inputs = write_inputs(directory.path())[..2].to_vec();
            let target = directory.path().join("sequence.gfsproj");
            let mut request = request(inputs, target);
            request.duration_policy = StaticImageSequenceDurationPolicy::PreserveDecoded;
            request.loop_behavior = loop_behavior;
            let mut job = ImportStaticSequenceJob::default();
            job.start(request).unwrap();
            wait_for_finished(&mut job);

            let project = job.take_result().unwrap().unwrap();
            assert!(
                project
                    .manifest()
                    .timeline
                    .frames
                    .iter()
                    .all(|frame| frame.duration.get() == DEFAULT_ZERO_DELAY_US)
            );
            assert_eq!(
                project.manifest().export_presets[IMPORTED_STATIC_SEQUENCE_PRESET_NAME].repeat,
                expected_loop
            );
        }

        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path())[..2].to_vec();
        let target = directory.path().join("zero-loop.gfsproj");
        let mut request = request(inputs, target.clone());
        request.loop_behavior = LoopBehavior::Finite(0);
        let mut job = ImportStaticSequenceJob::default();
        assert!(matches!(
            job.start(request),
            Err(ImportStaticSequenceJobStartError::InvalidRequest(
                StaticSequenceImportPathError::ZeroFiniteLoopCount
            ))
        ));
        assert_eq!(job.state(), ImportStaticSequenceJobState::Idle);
        assert!(!target.exists());
    }

    #[test]
    fn malformed_middle_input_fails_before_project_creation() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        fs::write(&inputs[1], b"not an image").unwrap();
        let target = directory.path().join("bad-middle.gfsproj");
        let mut job = ImportStaticSequenceJob::default();
        job.start(request(inputs, target.clone())).unwrap();
        wait_for_finished(&mut job);

        assert!(matches!(
            job.take_result(),
            Some(Err(ImportStaticSequenceJobError::Decode {
                image_index: 1,
                ..
            }))
        ));
        assert!(!target.exists());
    }

    #[test]
    fn overall_rgba_limit_fails_before_project_creation() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        let target = directory.path().join("limited.gfsproj");
        let mut limited = request(inputs, target.clone());
        limited.limits.max_total_rgba_bytes = 15;
        let mut job = ImportStaticSequenceJob::default();
        job.start(limited).unwrap();
        wait_for_finished(&mut job);

        assert!(matches!(
            job.take_result(),
            Some(Err(
                ImportStaticSequenceJobError::TotalRgbaBytesLimitExceeded {
                    image_index: 1,
                    required_bytes: 16,
                    limit_bytes: 15,
                }
            ))
        ));
        assert!(!target.exists());
    }

    #[test]
    fn preflight_rejects_counts_inputs_labels_target_and_existing_project() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        let target = directory.path().join("sequence.gfsproj");

        let mut too_few = request(inputs[..1].to_vec(), target.clone());
        too_few.display_names.truncate(1);
        too_few.frame_ids.truncate(1);
        assert!(matches!(
            preflight_static_sequence_request(&too_few),
            Err(StaticSequenceImportPathError::TooFewInputs { actual: 1, .. })
        ));

        let mut too_many = request(inputs.clone(), target.clone());
        too_many.limits.max_frames = 2;
        assert!(matches!(
            preflight_static_sequence_request(&too_many),
            Err(StaticSequenceImportPathError::FrameLimitExceeded {
                actual: 3,
                limit: 2
            })
        ));

        let mut labels = request(inputs.clone(), target.clone());
        labels.display_names.pop();
        assert!(matches!(
            preflight_static_sequence_request(&labels),
            Err(StaticSequenceImportPathError::DisplayNameCountMismatch { .. })
        ));

        let missing = directory.path().join("missing.png");
        let mut missing_request = request(vec![inputs[0].clone(), missing], target.clone());
        missing_request.display_names = vec!["first".to_owned(), "missing".to_owned()];
        assert!(matches!(
            preflight_static_sequence_request(&missing_request),
            Err(StaticSequenceImportPathError::InputNotFound { image_index: 1, .. })
        ));

        let unsupported = directory.path().join("image.tiff");
        fs::write(&unsupported, PNG_ALPHA).unwrap();
        let unsupported_request = request(
            vec![inputs[0].clone(), unsupported],
            directory.path().join("unsupported.gfsproj"),
        );
        assert!(matches!(
            preflight_static_sequence_request(&unsupported_request),
            Err(StaticSequenceImportPathError::InvalidInputExtension { image_index: 1, .. })
        ));

        let mut invalid_target = request(inputs.clone(), directory.path().join("sequence.txt"));
        assert!(matches!(
            preflight_static_sequence_request(&invalid_target),
            Err(StaticSequenceImportPathError::InvalidTargetExtension { .. })
        ));
        invalid_target.target = target.clone();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), b"keep").unwrap();
        assert!(matches!(
            preflight_static_sequence_request(&invalid_target),
            Err(StaticSequenceImportPathError::TargetExists { .. })
        ));
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"keep");
    }

    #[test]
    fn preflight_requires_regular_inputs_and_an_existing_directory_parent() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        let target = directory.path().join("sequence.gfsproj");
        let folder = directory.path().join("folder.png");
        fs::create_dir(&folder).unwrap();
        let folder_request = request(vec![inputs[0].clone(), folder], target.clone());
        assert!(matches!(
            preflight_static_sequence_request(&folder_request),
            Err(StaticSequenceImportPathError::InputNotFile { image_index: 1, .. })
        ));

        let missing_parent = request(
            inputs.clone(),
            directory.path().join("missing/sequence.gfsproj"),
        );
        assert!(matches!(
            preflight_static_sequence_request(&missing_parent),
            Err(StaticSequenceImportPathError::ParentNotFound { .. })
        ));

        let parent_file = directory.path().join("parent-file");
        fs::write(&parent_file, b"not a directory").unwrap();
        let file_parent = request(inputs, parent_file.join("sequence.gfsproj"));
        assert!(matches!(
            preflight_static_sequence_request(&file_parent),
            Err(StaticSequenceImportPathError::ParentNotDirectory { .. })
        ));
    }

    #[test]
    fn preflight_rejects_invalid_identity_version_and_loop_metadata() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        let target = directory.path().join("sequence.gfsproj");

        let mut wrong_count = request(inputs.clone(), target.clone());
        wrong_count.frame_ids.pop();
        assert!(matches!(
            preflight_static_sequence_request(&wrong_count),
            Err(StaticSequenceImportPathError::FrameIdCountMismatch {
                inputs: 3,
                frame_ids: 2
            })
        ));

        let mut nil_project = request(inputs.clone(), target.clone());
        nil_project.project_id = ProjectId::NIL;
        assert!(matches!(
            preflight_static_sequence_request(&nil_project),
            Err(StaticSequenceImportPathError::NilProjectId)
        ));

        let mut nil_frame = request(inputs.clone(), target.clone());
        nil_frame.frame_ids[1] = FrameId::NIL;
        assert!(matches!(
            preflight_static_sequence_request(&nil_frame),
            Err(StaticSequenceImportPathError::NilFrameId { image_index: 1 })
        ));

        let mut duplicate = request(inputs.clone(), target.clone());
        duplicate.frame_ids[2] = duplicate.frame_ids[0];
        assert!(matches!(
            preflight_static_sequence_request(&duplicate),
            Err(StaticSequenceImportPathError::DuplicateFrameId { image_index: 2, .. })
        ));

        let mut empty_version = request(inputs.clone(), target.clone());
        empty_version.app_version = "  ".to_owned();
        assert!(matches!(
            preflight_static_sequence_request(&empty_version),
            Err(StaticSequenceImportPathError::EmptyAppVersion)
        ));

        let mut zero_loop = request(inputs, target);
        zero_loop.loop_behavior = LoopBehavior::Finite(0);
        assert!(matches!(
            preflight_static_sequence_request(&zero_loop),
            Err(StaticSequenceImportPathError::ZeroFiniteLoopCount)
        ));
    }

    #[test]
    fn synchronous_start_failure_restores_idle_and_allows_retry() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        let invalid = request(inputs.clone(), directory.path().join("sequence.txt"));
        let valid = request(inputs, directory.path().join("sequence.gfsproj"));
        let mut job = ImportStaticSequenceJob::default();

        assert!(matches!(
            job.start(invalid),
            Err(ImportStaticSequenceJobStartError::InvalidRequest(
                StaticSequenceImportPathError::InvalidTargetExtension { .. }
            ))
        ));
        assert_eq!(job.state(), ImportStaticSequenceJobState::Idle);
        job.start(valid).unwrap();
        wait_for_finished(&mut job);
        assert!(job.take_result().unwrap().is_ok());
    }

    #[test]
    fn target_appearance_after_preflight_is_typed_and_preserves_existing_data() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        let target = directory.path().join("race.gfsproj");
        let request = request(inputs, target.clone());
        preflight_static_sequence_request(&request).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), b"keep").unwrap();

        assert!(matches!(
            import_static_sequence(request),
            Err(ImportStaticSequenceJobError::TargetAppeared { path }) if path == target
        ));
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"keep");
    }

    #[test]
    fn receiver_drop_eventually_releases_successful_project_lock() {
        let directory = tempdir().unwrap();
        let inputs = write_inputs(directory.path());
        let target = directory.path().join("detached.gfsproj");
        let mut job = ImportStaticSequenceJob::default();
        job.start(request(inputs, target.clone())).unwrap();
        drop(job);

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if target.join("manifest.json").is_file() {
                match ActiveProject::open(&target, LockPolicy::FailIfPresent) {
                    Ok(opened) => {
                        assert_eq!(opened.project.manifest().timeline.frames.len(), 3);
                        break;
                    }
                    Err(ProjectError::AlreadyLocked { .. }) => {}
                    Err(error) => panic!("detached sequence could not reopen: {error}"),
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detached static-sequence job timed out"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn lifecycle_and_worker_disconnect_are_explicitly_non_cancellable() {
        let directory = tempdir().unwrap();
        let first = request(
            write_inputs(directory.path()),
            directory.path().join("first.gfsproj"),
        );
        let mut job = ImportStaticSequenceJob::default();
        job.start(first.clone()).unwrap();
        assert!(matches!(
            job.start(first),
            Err(ImportStaticSequenceJobStartError::AlreadyStarted {
                state: ImportStaticSequenceJobState::Running
            })
        ));
        assert!(!ImportStaticSequenceJob::cancellation_supported());
        wait_for_finished(&mut job);
        assert_eq!(job.state(), ImportStaticSequenceJobState::Finished);

        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let mut disconnected = ImportStaticSequenceJob::default();
        disconnected.lifecycle.begin().unwrap();
        disconnected.receiver = Some(receiver);
        assert_eq!(
            disconnected.drain(),
            [ImportStaticSequenceJobEvent::Finished]
        );
        assert!(matches!(
            disconnected.result(),
            Some(Err(ImportStaticSequenceJobError::WorkerExited))
        ));
    }
}
