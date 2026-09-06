//! Raw recorded input is separate from its applicability to the current frame pixels.

use serde::{Deserialize, Serialize};

use crate::{AnnotationMode, FrameClip};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameCaptureBindingChange {
    pub frame_id: crate::FrameId,
    pub binding: CaptureBinding,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CaptureBindingSummary {
    pub original: usize,
    pub legacy_unknown: usize,
    pub archived_after_composite: usize,
    pub not_recorded: usize,
    /// Includes input-empty frames: these still interrupt carried annotations.
    pub selected_legacy_unknown: usize,
    pub selected_archived_after_composite: usize,
    pub selected_not_recorded: usize,
    /// Original/legacy frames without a confirmed shared capture-clock identity.
    pub selected_missing_clock: usize,
}

/// Input that can be used for recorded annotations, excluding empty image imports.
pub fn has_recorded_input(frame: &FrameClip) -> bool {
    let data = &frame.capture_metadata;
    !data.key_strokes.is_empty()
        || !data.mouse_events.is_empty()
        || !data.pressed_mouse_buttons.is_empty()
        || data.cursor_visible && !data.cursor_embedded && data.cursor_position.is_some()
}

impl FrameClip {
    pub fn has_recorded_annotation_input(&self) -> bool {
        has_recorded_input(self)
    }
}

pub fn capture_binding_summary<'a>(
    frames: impl IntoIterator<Item = &'a FrameClip>,
) -> CaptureBindingSummary {
    let mut summary = CaptureBindingSummary::default();
    for frame in frames {
        if matches!(
            frame.capture_binding,
            CaptureBinding::Original | CaptureBinding::LegacyUnknown
        ) && frame.capture_clock.and_then(|clock| clock.id).is_none()
        {
            summary.selected_missing_clock += 1;
        }
        match frame.capture_binding {
            CaptureBinding::LegacyUnknown => summary.selected_legacy_unknown += 1,
            CaptureBinding::ArchivedAfterComposite => {
                summary.selected_archived_after_composite += 1
            }
            CaptureBinding::Original => {}
            CaptureBinding::NotRecorded => summary.selected_not_recorded += 1,
        }
        if !has_recorded_input(frame) {
            continue;
        }
        match frame.capture_binding {
            CaptureBinding::Original => summary.original += 1,
            CaptureBinding::LegacyUnknown => summary.legacy_unknown += 1,
            CaptureBinding::ArchivedAfterComposite => summary.archived_after_composite += 1,
            CaptureBinding::NotRecorded => summary.not_recorded += 1,
        }
    }
    summary
}

/// Even an input-empty derived frame is a barrier to carrying labels from its neighbor.
pub fn recorded_annotation_barrier(frame: &FrameClip, mode: &AnnotationMode) -> bool {
    frame.capture_binding != CaptureBinding::Original
        && matches!(
            mode,
            AnnotationMode::RecordedKeys
                | AnnotationMode::RecordedClicks
                | AnnotationMode::RecordedCursor
        )
}

/// Whether original capture coordinates still describe this frame before its current transform.
/// This does not modify the recorded events or the capture-time `cursor_embedded` fact.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureBinding {
    /// Older manifests did not record destructive-pixel edits, so replay cannot be inferred.
    #[default]
    LegacyUnknown,
    /// Original input-to-pixel association, recorded here or explicitly confirmed by the user.
    Original,
    /// Mixed-source pixel edits retain raw input as archival data, not replay coordinates.
    ArchivedAfterComposite,
    /// Imported, generated, camera or board pixels have no screen-input coordinate source.
    NotRecorded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureReplayBlock {
    LegacyUnknown,
    ArchivedAfterComposite,
    NotRecorded,
}

impl CaptureReplayBlock {
    pub const fn message(self) -> &'static str {
        match self {
            Self::LegacyUnknown => {
                "Original input is preserved, but this older frame has no verified input-to-pixel association. Confirm original input association only after checking the untouched source pixels, or use manual annotations."
            }
            Self::ArchivedAfterComposite => {
                "Original input is preserved as archival data after a mixed-source pixel edit. Add recorded annotations before compositing, undo the composite, or use manual annotations."
            }
            Self::NotRecorded => {
                "This frame was imported or generated without a screen-input coordinate source. Use manual annotations; it cannot be confirmed as original screen input."
            }
        }
    }
}

/// One shared guard for manual authoring and automatic editing tasks.
/// Frames with no relevant input remain ordinary no-event cases, rather than false warnings.
pub fn recorded_annotation_block(
    frame: &FrameClip,
    mode: &AnnotationMode,
) -> Option<CaptureReplayBlock> {
    let metadata = &frame.capture_metadata;
    let relevant = match mode {
        AnnotationMode::RecordedKeys => !metadata.key_strokes.is_empty(),
        AnnotationMode::RecordedClicks => {
            !metadata.mouse_events.is_empty() || !metadata.pressed_mouse_buttons.is_empty()
        }
        AnnotationMode::RecordedCursor => {
            metadata.cursor_visible
                && !metadata.cursor_embedded
                && metadata.cursor_position.is_some()
        }
        _ => false,
    };
    if !relevant {
        return None;
    }
    match frame.capture_binding {
        CaptureBinding::Original => None,
        CaptureBinding::LegacyUnknown => Some(CaptureReplayBlock::LegacyUnknown),
        CaptureBinding::ArchivedAfterComposite => Some(CaptureReplayBlock::ArchivedAfterComposite),
        CaptureBinding::NotRecorded => Some(CaptureReplayBlock::NotRecorded),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        KeyStroke, PhysicalPoint, TimeUs,
        model::test_fixtures::{asset, frame},
    };

    #[test]
    fn non_recorded_frames_are_explicit_barriers_without_false_legacy_warnings() {
        let mut clip = frame(1, asset(1).id);
        clip.capture_binding = CaptureBinding::NotRecorded;
        assert!(recorded_annotation_barrier(
            &clip,
            &AnnotationMode::RecordedKeys
        ));
        assert_eq!(
            recorded_annotation_block(&clip, &AnnotationMode::RecordedKeys),
            None
        );
        let summary = capture_binding_summary([&clip]);
        assert_eq!(summary.selected_not_recorded, 1);
        assert_eq!(summary.legacy_unknown + summary.archived_after_composite, 0);
        assert!(!recorded_annotation_barrier(
            &clip,
            &AnnotationMode::ManualKeys {
                text: "Manual".to_owned()
            }
        ));
    }

    #[test]
    fn old_frames_remain_unknown_without_changing_raw_metadata() {
        let mut original = frame(1, asset(1).id);
        original.capture_metadata.key_strokes.push(KeyStroke {
            physical_key: "C".to_owned(),
            display_text: Some("Ctrl+C".to_owned()),
            pressed: true,
            at: TimeUs::ZERO,
            repeat: false,
            modifiers: 2,
        });
        let mut value = serde_json::to_value(&original).unwrap();
        value.as_object_mut().unwrap().remove("capture_binding");
        let loaded: FrameClip = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.capture_binding, CaptureBinding::LegacyUnknown);
        assert_eq!(loaded.capture_metadata, original.capture_metadata);
        assert_eq!(
            recorded_annotation_block(&loaded, &AnnotationMode::RecordedKeys),
            Some(CaptureReplayBlock::LegacyUnknown)
        );
        assert_eq!(
            recorded_annotation_block(&original, &AnnotationMode::RecordedKeys),
            None
        );
    }

    #[test]
    fn empty_derived_frames_are_carry_barriers_but_not_confirmation_candidates() {
        let mut clip = frame(1, asset(1).id);
        clip.capture_binding = CaptureBinding::ArchivedAfterComposite;
        assert!(recorded_annotation_barrier(
            &clip,
            &AnnotationMode::RecordedKeys
        ));
        assert!(!clip.has_recorded_annotation_input());
        assert_eq!(
            capture_binding_summary([&clip]),
            CaptureBindingSummary {
                selected_archived_after_composite: 1,
                ..CaptureBindingSummary::default()
            }
        );
        assert!(!recorded_annotation_barrier(
            &clip,
            &AnnotationMode::BuiltinCursor
        ));
    }

    #[test]
    fn archived_input_blocks_only_relevant_recorded_modes() {
        let mut clip = frame(1, asset(1).id);
        clip.capture_binding = CaptureBinding::ArchivedAfterComposite;
        assert_eq!(
            recorded_annotation_block(&clip, &AnnotationMode::RecordedKeys),
            None
        );
        clip.capture_metadata.cursor_visible = true;
        clip.capture_metadata.cursor_position = Some(PhysicalPoint::default());
        assert_eq!(
            recorded_annotation_block(&clip, &AnnotationMode::RecordedCursor),
            Some(CaptureReplayBlock::ArchivedAfterComposite)
        );
        assert_eq!(
            recorded_annotation_block(&clip, &AnnotationMode::BuiltinCursor),
            None
        );
        assert_eq!(
            recorded_annotation_block(&clip, &AnnotationMode::Progress(Default::default())),
            None
        );
        clip.capture_metadata.cursor_embedded = true;
        assert_eq!(
            recorded_annotation_block(&clip, &AnnotationMode::RecordedCursor),
            None
        );
    }
}
