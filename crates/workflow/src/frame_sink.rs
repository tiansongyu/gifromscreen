use std::error::Error;

use gif_from_screen_capture::{
    CaptureTimestamp, CapturedFrame, CursorImage, CursorMetadata, InputEvent,
};
use gif_from_screen_gif::RgbaFrame;

/// Native metadata retained alongside a normalized frame without retaining native frame pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingMetadata {
    /// Original session timestamp with pause time excluded.
    pub captured_at: CaptureTimestamp,
    /// Signed physical capture rectangle origin for retarget-aware annotations.
    pub capture_origin: Option<gif_from_screen_capture::PhysicalPosition>,
    /// Editable pointer position and hotspot.
    pub cursor: Option<CursorMetadata>,
    /// Immutable separate cursor pixels.
    pub cursor_image: Option<CursorImage>,
    /// Prevents annotating the cursor a second time when it is already embedded.
    pub cursor_embedded: bool,
    /// Input events observed since the preceding sampled frame.
    pub input_events: Vec<InputEvent>,
    /// Explicit event loss due to the native bounded queue.
    pub dropped_input_events: u32,
}

impl RecordingMetadata {
    pub(crate) fn from_frame(frame: &CapturedFrame) -> Self {
        Self {
            captured_at: frame.captured_at(),
            capture_origin: frame.capture_origin(),
            cursor: frame.cursor().cloned(),
            cursor_image: frame.cursor_image().cloned(),
            cursor_embedded: frame.cursor_embedded(),
            input_events: frame.input_events().to_vec(),
            dropped_input_events: frame.dropped_input_events(),
        }
    }

    pub(crate) fn requires_retention(&self, previous: Option<&Self>) -> bool {
        !self.input_events.is_empty()
            || self.dropped_input_events != 0
            || previous.is_some_and(|previous| {
                previous.cursor != self.cursor
                    || previous.cursor_image != self.cursor_image
                    || previous.capture_origin != self.capture_origin
            })
    }
}

/// Boxed application persistence failure returned by a recording frame sink.
pub type RecordingFrameSinkError = Box<dyn Error + Send + Sync + 'static>;

/// Durable observer for frames retained by the capture workflow.
///
/// A newly retained frame is appended immediately with its fixed playback
/// duration or a safe measured-playback provisional tail. Only measured timing
/// is corrected when a later sample or final stop boundary is known. Fixed
/// playback never requires a duration update. Calls are strictly ordered and
/// never overlap; native metadata always keeps its original capture clock.
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

    /// Appends pixels and metadata atomically when the sink supports native metadata.
    ///
    /// # Errors
    /// Returns a persistence error if the frame or its immutable metadata assets cannot be stored.
    fn append_provisional_frame_with_metadata(
        &mut self,
        frame_index: u64,
        frame: &RgbaFrame,
        _metadata: &RecordingMetadata,
    ) -> Result<(), RecordingFrameSinkError> {
        self.append_provisional_frame(frame_index, frame)
    }

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
