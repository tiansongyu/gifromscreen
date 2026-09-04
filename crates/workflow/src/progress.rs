use std::time::Duration;

use gif_from_screen_gif::EncodeProgress;

/// Current phase of a record-to-GIF workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WorkflowPhase {
    /// The native capture session is being created.
    StartingCapture,
    /// Frames are being collected and normalized.
    Capturing,
    /// The native capture session is being stopped.
    StoppingCapture,
    /// Collected frames are being encoded into a temporary GIF.
    Encoding,
    /// The synchronized temporary file is being atomically committed.
    Committing,
    /// The target path contains the completed GIF.
    Complete,
}

/// Monotonic best-effort progress for collection and encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowProgress {
    /// Current top-level phase.
    pub phase: WorkflowPhase,
    /// Number of native frames retained by the collection phase.
    pub frames_captured: u64,
    /// Presentation duration currently represented by captured timestamps.
    pub capture_duration: Duration,
    /// Detailed encoder progress while `phase` is [`WorkflowPhase::Encoding`].
    pub encode: Option<EncodeProgress>,
}

impl WorkflowProgress {
    pub(crate) const fn capture(
        phase: WorkflowPhase,
        frames_captured: u64,
        capture_duration: Duration,
    ) -> Self {
        Self {
            phase,
            frames_captured,
            capture_duration,
            encode: None,
        }
    }

    pub(crate) const fn encoding(
        frames_captured: u64,
        capture_duration: Duration,
        encode: EncodeProgress,
    ) -> Self {
        Self {
            phase: WorkflowPhase::Encoding,
            frames_captured,
            capture_duration,
            encode: Some(encode),
        }
    }
}

/// Receives workflow progress on the thread executing the workflow.
pub trait WorkflowProgressSink {
    /// Handles a new progress snapshot.
    fn report(&mut self, progress: WorkflowProgress);
}

impl<F> WorkflowProgressSink for F
where
    F: FnMut(WorkflowProgress),
{
    fn report(&mut self, progress: WorkflowProgress) {
        self(progress);
    }
}

/// Progress sink for callers that do not need updates.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopWorkflowProgress;

impl WorkflowProgressSink for NoopWorkflowProgress {
    fn report(&mut self, _progress: WorkflowProgress) {}
}
