use std::error::Error;

use gif_from_screen_gif::RgbaFrame;

/// Boxed application persistence failure returned by a recording frame sink.
pub type RecordingFrameSinkError = Box<dyn Error + Send + Sync + 'static>;

/// Durable observer for frames retained by the capture workflow.
///
/// A newly retained frame is appended immediately with the configured safe
/// provisional tail duration. Once a later retained timestamp or the final
/// stop boundary is known, the workflow replaces that frame's duration. Calls
/// are strictly ordered and never overlap.
pub trait RecordingFrameSink {
    /// Durably appends one newly retained frame in zero-based timeline order.
    ///
    /// # Errors
    ///
    /// Returns an application persistence error when pixels or the journal
    /// entry cannot be stored durably.
    fn append_provisional_frame(
        &mut self,
        frame_index: u64,
        frame: &RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError>;

    /// Durably replaces the duration of an already appended frame.
    ///
    /// # Errors
    ///
    /// Returns an application persistence error when the referenced frame is
    /// unknown or the duration update cannot be journaled durably.
    fn update_frame_duration(
        &mut self,
        frame_index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError>;
}

/// Sink operation that failed during incremental capture persistence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RecordingFrameSinkOperation {
    /// Append a new frame with its provisional tail duration.
    AppendProvisionalFrame,
    /// Replace a previous provisional duration with its known duration.
    UpdateFrameDuration,
}
