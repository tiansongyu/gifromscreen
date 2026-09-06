#![forbid(unsafe_code)]

//! Application-level state machines and use-case orchestration.

mod blank_project;
mod board;
mod project_copy;
pub use project_copy::{
    ProjectCopyProgress, ProjectCopyReport, ProjectCopySnapshot, SaveProjectCopyOptions,
    save_project_copy,
};
mod camera_capture;
mod import_project;
mod live_rgba_recording;
mod presentation_plan;
mod project_export;
mod recording_project;
mod rgba_project;
mod static_image_project;
mod static_image_sequence_project;
mod video_import;

pub use board::{BoardBrush, BoardCanvas, BoardPoint};
pub use live_rgba_recording::{
    LiveFrameSubmission, LiveRecordingOptions, LiveRecordingOutcome, LiveRecordingProgress,
    LiveRgbaRecorder,
};

pub use video_import::{
    VideoImportError, VideoImportLimits, VideoImportOptions, VideoImportProgress,
    import_video_project,
};

pub use camera_capture::{
    CameraCaptureOptions, CameraCaptureProgress, CameraControl, CameraDevice, CameraPhase,
    CameraPreviewFrame, enumerate_camera_devices, run_camera_capture,
};

pub use blank_project::{
    BLANK_ANIMATION_PRESET_NAME, BlankAnimationProjectOptions, CreateBlankAnimationError,
    DEFAULT_BLANK_FRAME_LIMIT_BYTES, create_blank_animation_project,
};
pub use import_project::{
    DecodedAnimationProjectOptions, IMPORTED_GIF_PRESET_NAME, PersistDecodedAnimationError,
    persist_decoded_animation,
};

pub use presentation_plan::{
    PresentationPlan, PresentationSample, PresentationTransitionStep, transition_step_duration,
    transition_step_progress,
};
pub use project_export::{
    CustomGifPalette, CustomGifPaletteError, NoopProjectExportProgress, ProjectExportPhase,
    ProjectExportProgress, ProjectExportProgressSink, ProjectExportSnapshot, ProjectFrameSelection,
    ProjectGifExportError, ProjectGifExportOptions, ProjectGifExportReport,
    export_project_snapshot_to_gif,
};
pub use recording_project::{
    INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES, IncrementalRecordingProject,
    IncrementalRecordingProjectError, IncrementalRecordingProjectOptions,
    IncrementalRecordingSummary, PersistRecordingError, RecordingProjectOptions,
    persist_collected_recording,
};
pub use static_image_project::{
    IMPORTED_STATIC_IMAGE_PRESET_NAME, PersistStaticImageError, StaticImageProjectOptions,
    persist_decoded_static_image,
};
pub use static_image_sequence_project::{
    IMPORTED_STATIC_SEQUENCE_PRESET_NAME, PersistStaticImageSequenceError,
    StaticImageSequenceProjectOptions, persist_decoded_static_image_sequence,
};

use thiserror::Error;

/// The lifecycle of a recording session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecorderState {
    /// No recording is active.
    Idle,
    /// The application is requesting operating-system permission.
    Permission,
    /// The user or system is choosing a capture source.
    SourceSelection,
    /// The source is selected and the countdown is running.
    Countdown,
    /// Frames are being captured.
    Recording,
    /// Capture is temporarily paused.
    Paused,
    /// Queued frames are being drained and persisted.
    Finalizing,
    /// The completed recording is ready in the editor.
    EditorReady,
    /// The session was cancelled without a recording.
    Cancelled,
    /// The session failed, retaining a user-facing explanation.
    Failed(String),
}

/// An intent that can change [`RecorderState`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderCommand {
    /// Begin permission negotiation.
    Begin,
    /// Permission was granted.
    PermissionGranted,
    /// A source was selected.
    SourceSelected,
    /// Start delivering frames after the countdown.
    CountdownElapsed,
    /// Pause capture.
    Pause,
    /// Resume capture.
    Resume,
    /// Stop and persist queued frames.
    Stop,
    /// Persistence completed successfully.
    Finalized,
    /// Cancel the session.
    Cancel,
}

/// An invalid recorder transition.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("command {command:?} is invalid while recorder is {state:?}")]
pub struct TransitionError {
    /// State in which the command was attempted.
    pub state: RecorderState,
    /// Rejected command.
    pub command: RecorderCommand,
}

impl RecorderState {
    /// Applies a command and returns the next state.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] when the command is invalid for the current state.
    pub fn transition(self, command: RecorderCommand) -> Result<Self, TransitionError> {
        use RecorderCommand as Command;
        use RecorderState as State;

        let current = self.clone();
        let next = match (self, command) {
            (State::Idle, Command::Begin) => State::Permission,
            (State::Permission, Command::PermissionGranted) => State::SourceSelection,
            (State::SourceSelection, Command::SourceSelected) => State::Countdown,
            (State::Countdown, Command::CountdownElapsed) | (State::Paused, Command::Resume) => {
                State::Recording
            }
            (State::Recording, Command::Pause) => State::Paused,
            (State::Recording | State::Paused, Command::Stop) => State::Finalizing,
            (State::Finalizing, Command::Finalized) => State::EditorReady,
            (
                State::Permission
                | State::SourceSelection
                | State::Countdown
                | State::Recording
                | State::Paused,
                Command::Cancel,
            ) => State::Cancelled,
            (state, command) => {
                return Err(TransitionError { state, command });
            }
        };

        debug_assert_ne!(current, next);
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::{RecorderCommand as Command, RecorderState as State};

    #[test]
    fn successful_recording_has_an_explicit_finalization_step() {
        let mut state = State::Idle;
        for command in [
            Command::Begin,
            Command::PermissionGranted,
            Command::SourceSelected,
            Command::CountdownElapsed,
            Command::Pause,
            Command::Resume,
            Command::Stop,
            Command::Finalized,
        ] {
            state = state.transition(command).expect("valid transition");
        }
        assert_eq!(state, State::EditorReady);
    }

    #[test]
    fn cannot_finalize_before_writer_has_drained() {
        let error = State::Recording
            .transition(Command::Finalized)
            .expect_err("invalid transition must fail");
        assert_eq!(error.state, State::Recording);
    }

    #[test]
    fn paused_recording_can_be_cancelled() {
        assert_eq!(
            State::Paused.transition(Command::Cancel),
            Ok(State::Cancelled)
        );
    }

    #[test]
    fn active_recording_can_be_discarded() {
        assert_eq!(
            State::Recording.transition(Command::Cancel),
            Ok(State::Cancelled)
        );
    }
}
