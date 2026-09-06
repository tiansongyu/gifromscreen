//! Ripple overlay spans through order-preserving frame edits.
//!
//! Removing frames compresses time; changing their duration scales the covered
//! interval. Insertions before an item shift it, and insertions inside it extend
//! it. Reordering is deliberately not a time edit: tracks remain time-anchored.

use std::collections::BTreeMap;

use crate::{
    DomainError, DurationUs, FrameClip, FrameId, OverlayTrack, TimeUs, TimelineSpan,
    ValidationIssue,
};

struct FrameInterval {
    old_start: u64,
    old_end: u64,
    new_start: u64,
    new_duration: u64,
}

pub(crate) struct FrameTimingMap {
    intervals: Vec<FrameInterval>,
}

impl FrameTimingMap {
    pub(crate) fn new(
        before: &[(FrameId, DurationUs)],
        after: &[FrameClip],
    ) -> Result<Self, DomainError> {
        let overflow =
            || DomainError::InvalidManifest(vec![ValidationIssue::TimelineDurationOverflow]);
        let mut starts = BTreeMap::new();
        let mut new_start = 0_u64;
        for frame in after {
            starts.insert(frame.id, (new_start, frame.duration.get()));
            new_start = new_start
                .checked_add(frame.duration.get())
                .ok_or_else(overflow)?;
        }

        let mut intervals = Vec::with_capacity(before.len());
        let mut old_start = 0_u64;
        let mut preceding_end = 0_u64;
        for (frame_id, duration) in before {
            let old_end = old_start.checked_add(duration.get()).ok_or_else(overflow)?;
            // Deleted frames collapse at the preceding retained frame's end.
            let (new_start, new_duration) =
                starts.get(frame_id).copied().unwrap_or((preceding_end, 0));
            intervals.push(FrameInterval {
                old_start,
                old_end,
                new_start,
                new_duration,
            });
            preceding_end = new_start + new_duration;
            old_start = old_end;
        }
        Ok(Self { intervals })
    }

    pub(crate) fn retime(&self, tracks: &mut [OverlayTrack]) {
        for track in tracks {
            track.items.retain_mut(|item| {
                let Some(old_end) = item.span.end() else {
                    // Compound edits can have temporarily invalid spans. Leave
                    // those for the final manifest validation, never panic.
                    return true;
                };
                let Some(start) = self.map_time(item.span.start.get(), false) else {
                    return true;
                };
                let Some(end) = self.map_time(old_end.get(), true) else {
                    return true;
                };
                let Some(duration) = end.checked_sub(start).and_then(DurationUs::new) else {
                    return false;
                };
                item.span = TimelineSpan {
                    start: TimeUs::new(start),
                    duration,
                };
                true
            });
        }
    }

    fn map_time(&self, time: u64, end_boundary: bool) -> Option<u64> {
        let index = self.intervals.partition_point(|interval| {
            if end_boundary {
                interval.old_end < time
            } else {
                interval.old_end <= time
            }
        });
        let interval = self.intervals.get(index)?;
        let offset = time.checked_sub(interval.old_start)?;
        let scaled = u128::from(offset) * u128::from(interval.new_duration);
        let old_duration = u128::from(interval.old_end - interval.old_start);
        // Round outward so a short overlay on a retained frame does not vanish.
        let scaled = if end_boundary {
            scaled.div_ceil(old_duration)
        } else {
            scaled / old_duration
        };
        interval.new_start.checked_add(u64::try_from(scaled).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        BlendMode, EditCommand, FrameDurationChange, OverlayContent, OverlayId, OverlayItem,
        PhysicalPoint, PhysicalSize, ProjectManifest, TrackId,
        model::test_fixtures::{asset, frame, manifest},
    };

    use super::*;

    fn project(durations: &[u64], spans: &[(u64, u64)]) -> ProjectManifest {
        let mut project = manifest();
        let asset = asset(1);
        project.assets.insert(asset.id, asset.clone());
        project.timeline.frames = durations
            .iter()
            .enumerate()
            .map(|(index, duration)| {
                let mut frame = frame(u8::try_from(index + 1).unwrap(), asset.id);
                frame.duration = DurationUs::new(*duration).unwrap();
                frame
            })
            .collect();
        project.timeline.overlay_tracks.push(OverlayTrack {
            annotation: None,
            id: TrackId::from_u128(1),
            name: "Watermarks".to_owned(),
            visible: true,
            opacity: 213,
            blend_mode: BlendMode::Normal,
            items: spans
                .iter()
                .enumerate()
                .map(|(index, (start, end))| OverlayItem {
                    id: OverlayId::from_u128(index as u128 + 1),
                    span: TimelineSpan {
                        start: TimeUs::new(*start),
                        duration: DurationUs::new(end - start).unwrap(),
                    },
                    z_index: i32::try_from(index).unwrap(),
                    content: OverlayContent::Raster {
                        asset_id: asset.id,
                        position: PhysicalPoint::default(),
                        size: PhysicalSize::new(2, 2).unwrap(),
                        opacity: 177,
                    },
                })
                .collect(),
        });
        project.validate().unwrap();
        project
    }

    #[test]
    fn same_duration_frame_effect_edit_does_not_copy_overlay_timing_into_inverse() {
        let mut project = project(&[10, 20], &[(2, 17)]);
        let before = project.clone();
        let mut replacement = project.timeline.frames[0].clone();
        replacement.effects.push(crate::Effect::Shadow {
            offset_x: 1,
            offset_y: 1,
            blur_radius: 0,
            color: crate::Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
        });
        let applied = project
            .apply_command(&EditCommand::ReplaceFrame {
                frame_id: replacement.id,
                replacement: Box::new(replacement),
            })
            .unwrap();
        assert!(matches!(applied.inverse, EditCommand::ReplaceFrame { .. }));
        assert_eq!(
            project.timeline.overlay_tracks,
            before.timeline.overlay_tracks
        );
        project.apply_command(&applied.inverse).unwrap();
        project.revision = before.revision;
        assert_eq!(project, before);
    }

    fn spans(project: &ProjectManifest) -> Vec<(u64, u64, u128)> {
        project.timeline.overlay_tracks[0]
            .items
            .iter()
            .map(|item| {
                (
                    item.span.start.get(),
                    item.span.end().unwrap().get(),
                    // IDs remain in their original order even when items disappear.
                    u128::from_be_bytes(*item.id.as_bytes()),
                )
            })
            .collect()
    }

    fn remove(ids: &[u128]) -> EditCommand {
        EditCommand::RemoveFrames {
            frame_ids: ids.iter().copied().map(FrameId::from_u128).collect(),
        }
    }

    #[test]
    fn deleting_sparse_frames_compresses_spans_and_removes_only_deleted_content() {
        let mut project = project(
            &[100, 100, 100, 100, 100],
            &[(0, 500), (100, 200), (150, 450), (400, 500)],
        );
        let original = project.timeline.clone();
        let inverse = project.apply_command(&remove(&[2, 4])).unwrap().inverse;
        assert_eq!(
            spans(&project),
            vec![(0, 300, 1), (100, 250, 3), (200, 300, 4)]
        );
        // Removing an overlay does not discard its immutable asset.
        assert_eq!(project.assets.len(), 1);
        project.apply_command(&inverse).unwrap();
        assert_eq!(project.timeline, original);
    }

    #[test]
    fn deleting_all_frames_keeps_track_settings_and_exact_undo() {
        let mut project = project(&[100, 100], &[(0, 200), (125, 150)]);
        let original = project.timeline.clone();
        let inverse = project.apply_command(&remove(&[1, 2])).unwrap().inverse;
        assert!(project.timeline.frames.is_empty());
        assert!(project.timeline.overlay_tracks[0].items.is_empty());
        assert_eq!(project.timeline.overlay_tracks[0].opacity, 213);
        project.apply_command(&inverse).unwrap();
        assert_eq!(project.timeline, original);
    }

    #[test]
    fn duration_edits_follow_frame_boundaries_and_round_short_spans_outward() {
        let mut project = project(&[100, 100, 100], &[(100, 200), (200, 300), (101, 102)]);
        let original = project.timeline.clone();
        let inverse = project
            .apply_command(&EditCommand::SetFrameDurations {
                changes: vec![FrameDurationChange {
                    frame_id: FrameId::from_u128(2),
                    duration: DurationUs::new(1).unwrap(),
                }],
            })
            .unwrap()
            .inverse;
        assert_eq!(
            spans(&project),
            vec![(100, 101, 1), (101, 201, 2), (100, 101, 3)]
        );
        project.apply_command(&inverse).unwrap();
        assert_eq!(project.timeline, original);
    }

    #[test]
    fn serialized_undo_redo_remains_exact_and_bounded_after_rounding() {
        let mut project = project(&[7, 7], &[(1, 6), (7, 13)]);
        let original = project.timeline.clone();
        let mut next = project
            .apply_command(&EditCommand::SetFrameDurations {
                changes: vec![FrameDurationChange {
                    frame_id: FrameId::from_u128(1),
                    duration: DurationUs::new(3).unwrap(),
                }],
            })
            .unwrap()
            .inverse;
        let edited = project.timeline.clone();
        let first_size = serde_json::to_string(&next).unwrap().len();
        for iteration in 0..100 {
            let json = serde_json::to_string(&next).unwrap();
            assert!(json.len() <= first_size + 10, "undo history grew: {json}");
            let decoded = serde_json::from_str(&json).unwrap();
            next = project.apply_command(&decoded).unwrap().inverse;
            assert_eq!(
                project.timeline,
                if iteration % 2 == 0 {
                    &original
                } else {
                    &edited
                }
                .clone()
            );
        }
    }

    #[test]
    fn inserting_at_overlay_boundaries_excludes_new_frames_but_ripples_later_spans() {
        let mut project = project(&[100, 100], &[(0, 100), (100, 200), (0, 200)]);
        let original = project.timeline.clone();
        let mut inserted = project.timeline.frames[0].clone();
        inserted.id = FrameId::from_u128(3);
        inserted.duration = DurationUs::new(25).unwrap();
        let inverse = project
            .apply_command(&EditCommand::InsertFrames {
                index: 1,
                frames: vec![inserted],
            })
            .unwrap()
            .inverse;
        assert_eq!(
            spans(&project),
            vec![(0, 100, 1), (125, 225, 2), (0, 225, 3)]
        );
        project.apply_command(&inverse).unwrap();
        assert_eq!(project.timeline, original);
    }

    #[test]
    fn appending_frames_does_not_extend_existing_overlays() {
        let mut project = project(&[100], &[(0, 100)]);
        let expected = project.timeline.overlay_tracks.clone();
        let mut inserted = project.timeline.frames[0].clone();
        inserted.id = FrameId::from_u128(2);
        let inverse = project
            .apply_command(&EditCommand::InsertFrames {
                index: 1,
                frames: vec![inserted],
            })
            .unwrap()
            .inverse;
        assert_eq!(project.timeline.overlay_tracks, expected);
        assert!(matches!(inverse, EditCommand::RemoveFrames { .. }));
        project.apply_command(&inverse).unwrap();
        assert_eq!(project.timeline.overlay_tracks, expected);
    }

    #[test]
    fn overflowing_duration_edit_is_atomic_even_with_overlays() {
        let mut project = project(&[100, 100], &[(0, 200)]);
        let original = project.clone();
        let error = project
            .apply_command(&EditCommand::SetFrameDurations {
                changes: vec![FrameDurationChange {
                    frame_id: FrameId::from_u128(1),
                    duration: DurationUs::new(u64::MAX).unwrap(),
                }],
            })
            .unwrap_err();
        assert_eq!(
            error,
            DomainError::InvalidManifest(vec![ValidationIssue::TimelineDurationOverflow])
        );
        assert_eq!(project, original);
    }

    #[test]
    fn restore_rejects_arbitrary_nested_commands_without_mutation() {
        let mut project = project(&[100], &[(0, 100)]);
        let original = project.clone();
        let error = project
            .apply_command(&EditCommand::RestoreFrameEdit {
                edit: Box::new(EditCommand::Compound {
                    commands: vec![remove(&[1])],
                }),
                overlay_tracks: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(error, DomainError::InvalidFrameEditRestore);
        assert_eq!(project, original);
    }
}
