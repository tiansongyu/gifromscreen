//! Capture-clock identity and sampling time remain independent of playback edits and raw input.

use crate::{CaptureBinding, CaptureClockId, FrameClip, FrameId, TimeUs};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CaptureClockContext {
    /// Only an explicit nonempty identity permits held input to cross frame boundaries.
    pub id: Option<CaptureClockId>,
    /// Sampling instant in the source clock, never in a destination editing timeline.
    pub sampled_at: TimeUs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameCaptureClockChange {
    pub frame_id: FrameId,
    pub clock: Option<CaptureClockContext>,
}

impl FrameClip {
    /// Shared by complete-manifest validation and incremental append validation.
    pub fn has_valid_capture_clock(&self) -> bool {
        self.capture_clock.is_none_or(|clock| {
            !clock.id.is_some_and(CaptureClockId::is_nil)
                && self
                    .capture_metadata
                    .captured_at
                    .is_none_or(|raw| raw == clock.sampled_at)
        })
    }
    pub fn capture_sample_time(&self) -> Option<TimeUs> {
        self.capture_clock
            .map(|clock| clock.sampled_at)
            .or(self.capture_metadata.captured_at)
    }

    /// Freeze the legacy source-time interpretation before moving/copying pixels.
    /// This never asserts a shared clock identity or changes the raw event timestamps.
    pub fn freeze_capture_clock(&mut self, source_frame_start: TimeUs) {
        if self.capture_clock.is_none() && self.capture_binding != CaptureBinding::NotRecorded {
            self.capture_clock = Some(CaptureClockContext {
                id: None,
                sampled_at: self
                    .capture_metadata
                    .captured_at
                    .unwrap_or(source_frame_start),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::test_fixtures::{asset, frame};

    #[test]
    fn freezing_preserves_known_identity_and_raw_data_without_guessing_an_unknown_one() {
        let mut frame = frame(1, asset(1).id);
        let raw = frame.capture_metadata.clone();
        frame.freeze_capture_clock(TimeUs::new(123));
        assert_eq!(
            frame.capture_clock,
            Some(CaptureClockContext {
                id: None,
                sampled_at: TimeUs::new(123)
            })
        );
        frame.freeze_capture_clock(TimeUs::new(999));
        assert_eq!(frame.capture_sample_time(), Some(TimeUs::new(123)));
        assert_eq!(frame.capture_metadata, raw);
        frame.capture_clock.as_mut().unwrap().id = Some(CaptureClockId::from_u128(8));
        frame.freeze_capture_clock(TimeUs::new(0));
        assert_eq!(
            frame.capture_clock.unwrap().id,
            Some(CaptureClockId::from_u128(8))
        );
    }
}
