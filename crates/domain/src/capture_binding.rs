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
    /// Current output follows a freeze; earlier source-bound paint stages survive.
    pub selected_mixed_image_stage: usize,
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
        if mixed_before_stage(frame, None) {
            summary.selected_mixed_image_stage += 1;
        }
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
    recorded_annotation_barrier_at_stage(frame, mode, None)
}

/// A source binding can be used before a mixed-image step, but not after it.
/// Unknown target stages conservatively interrupt held input as well. Manual
/// annotations and progress do not consume original screen coordinates.
pub fn recorded_annotation_barrier_at_stage(
    frame: &FrameClip,
    mode: &AnnotationMode,
    stage: Option<u32>,
) -> bool {
    matches!(
        mode,
        AnnotationMode::RecordedKeys
            | AnnotationMode::RecordedClicks
            | AnnotationMode::RecordedCursor
    ) && (frame.capture_binding != CaptureBinding::Original || mixed_before_stage(frame, stage))
}

fn mixed_before_stage(frame: &FrameClip, stage: Option<u32>) -> bool {
    if stage == Some(0) || frame.render_steps.len() > crate::MAX_FRAME_RENDER_STEPS {
        return true;
    }
    let mut mixed = false;
    for step in &frame.render_steps {
        match step {
            crate::FrameRenderStep::Composite { stage_id, .. } if Some(*stage_id) == stage => {
                return mixed;
            }
            crate::FrameRenderStep::FreezeRegion { .. }
            | crate::FrameRenderStep::CinemagraphOverlay { .. } => mixed = true,
            _ => {}
        }
    }
    stage.is_some() || mixed
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
    MixedImageStage,
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
            Self::MixedImageStage => {
                "Original input is preserved, but this paint stage follows frozen mixed-source pixels or is unknown. Edit recorded annotations at their earlier stage, undo the freeze, or use manual annotations."
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
    recorded_annotation_block_at_stage(frame, mode, None)
}

/// Stage-aware counterpart of [`recorded_annotation_block`]. Source-level
/// archival/unknown bindings remain blocked even before a frozen-image step.
/// Input-empty frames still produce no warning; use the barrier for held input.
pub fn recorded_annotation_block_at_stage(
    frame: &FrameClip,
    mode: &AnnotationMode,
    stage: Option<u32>,
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
        CaptureBinding::Original if mixed_before_stage(frame, stage) => {
            Some(CaptureReplayBlock::MixedImageStage)
        }
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

    fn staged_input() -> FrameClip {
        let mut clip = frame(1, asset(1).id);
        clip.capture_metadata.key_strokes.push(KeyStroke {
            physical_key: "C".into(),
            display_text: Some("C".into()),
            pressed: true,
            at: TimeUs::ZERO,
            repeat: false,
            modifiers: 0,
        });
        clip.capture_metadata
            .pressed_mouse_buttons
            .push(crate::MouseButton::Left);
        clip.capture_metadata.cursor_visible = true;
        clip.capture_metadata.cursor_position = Some(PhysicalPoint::default());
        clip.render_steps = vec![
            crate::FrameRenderStep::composite(1),
            crate::FrameRenderStep::composite(2),
            crate::FrameRenderStep::FreezeRegion {
                baseline_asset: asset(2).id,
                baseline_size: crate::PhysicalSize::new(320, 200).unwrap(),
                region: crate::PhysicalRect::new(1, 1, 10, 10).unwrap(),
                invert: false,
            },
            crate::FrameRenderStep::composite(3),
        ];
        clip
    }

    #[test]
    fn original_input_remains_replayable_before_freeze_but_not_after_or_at_unknown_stages() {
        let clip = staged_input();
        let before = clip.clone();
        for mode in [
            AnnotationMode::RecordedKeys,
            AnnotationMode::RecordedClicks,
            AnnotationMode::RecordedCursor,
        ] {
            for stage in [Some(1), Some(2)] {
                assert!(!recorded_annotation_barrier_at_stage(&clip, &mode, stage));
                assert_eq!(
                    recorded_annotation_block_at_stage(&clip, &mode, stage),
                    None
                );
            }
            for stage in [None, Some(3), Some(0), Some(999)] {
                assert!(recorded_annotation_barrier_at_stage(&clip, &mode, stage));
                assert_eq!(
                    recorded_annotation_block_at_stage(&clip, &mode, stage),
                    Some(CaptureReplayBlock::MixedImageStage)
                );
            }
            assert_eq!(
                recorded_annotation_block(&clip, &mode),
                recorded_annotation_block_at_stage(&clip, &mode, None)
            );
            assert_eq!(
                recorded_annotation_barrier(&clip, &mode),
                recorded_annotation_barrier_at_stage(&clip, &mode, None)
            );
        }
        for mode in [
            AnnotationMode::BuiltinCursor,
            AnnotationMode::ManualKeys {
                text: "manual".into(),
            },
            AnnotationMode::Progress(Default::default()),
        ] {
            assert!(!recorded_annotation_barrier_at_stage(
                &clip,
                &mode,
                Some(999)
            ));
            assert_eq!(recorded_annotation_block_at_stage(&clip, &mode, None), None);
        }
        assert_eq!(clip, before);
    }

    #[test]
    fn empty_and_embedded_input_remain_no_event_cases_while_freeze_stops_neighbor_holds() {
        let mut clip = staged_input();
        clip.capture_metadata = crate::CaptureMetadata::default();
        for mode in [
            AnnotationMode::RecordedKeys,
            AnnotationMode::RecordedClicks,
            AnnotationMode::RecordedCursor,
        ] {
            assert!(recorded_annotation_barrier(&clip, &mode));
            assert_eq!(recorded_annotation_block(&clip, &mode), None);
            assert!(!recorded_annotation_barrier_at_stage(&clip, &mode, Some(2)));
        }
        clip.capture_metadata.cursor_visible = true;
        clip.capture_metadata.cursor_position = Some(PhysicalPoint::default());
        clip.capture_metadata.cursor_embedded = true;
        assert_eq!(
            recorded_annotation_block(&clip, &AnnotationMode::RecordedCursor),
            None
        );
        clip.render_steps
            .retain(|step| !matches!(step, crate::FrameRenderStep::FreezeRegion { .. }));
        assert!(!recorded_annotation_barrier(
            &clip,
            &AnnotationMode::RecordedKeys
        ));
        assert!(recorded_annotation_barrier_at_stage(
            &clip,
            &AnnotationMode::RecordedKeys,
            Some(999)
        ));
    }

    #[test]
    fn source_binding_blocks_cannot_be_recovered_by_choosing_a_pre_freeze_stage() {
        let mut clip = staged_input();
        for (binding, reason) in [
            (
                CaptureBinding::LegacyUnknown,
                CaptureReplayBlock::LegacyUnknown,
            ),
            (
                CaptureBinding::ArchivedAfterComposite,
                CaptureReplayBlock::ArchivedAfterComposite,
            ),
            (CaptureBinding::NotRecorded, CaptureReplayBlock::NotRecorded),
        ] {
            clip.capture_binding = binding;
            for stage in [Some(1), Some(3), None, Some(999)] {
                assert!(recorded_annotation_barrier_at_stage(
                    &clip,
                    &AnnotationMode::RecordedKeys,
                    stage
                ));
                assert_eq!(
                    recorded_annotation_block_at_stage(&clip, &AnnotationMode::RecordedKeys, stage),
                    Some(reason)
                );
            }
        }
        clip.capture_binding = CaptureBinding::ArchivedAfterComposite;
        clip.render_steps.clear();
        assert_eq!(
            recorded_annotation_block(&clip, &AnnotationMode::RecordedKeys),
            Some(CaptureReplayBlock::ArchivedAfterComposite)
        );
    }
}
