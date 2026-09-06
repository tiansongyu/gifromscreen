use super::*;
use crate::{
    AnnotationRequest, BlendMode, DomainError, EditCommand, FrameDurationChange, OverlayTrack,
    PhysicalPoint, PhysicalSize, ProjectManifest, TrackId,
    model::test_fixtures::{asset, frame, manifest},
};

fn project() -> ProjectManifest {
    let mut project = manifest();
    project.assets.insert(asset(1).id, asset(1));
    project.assets.insert(asset(2).id, asset(2));
    project.timeline.frames = [10, 23, 51]
        .into_iter()
        .enumerate()
        .map(|(index, duration)| {
            let mut clip = frame(u8::try_from(index + 1).unwrap(), asset(1).id);
            clip.duration = DurationUs::new(duration).unwrap();
            clip
        })
        .collect();
    project
}

fn track(project: &ProjectManifest) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(1),
        frame_cells: Some(
            project
                .timeline
                .frames
                .iter()
                .enumerate()
                .map(|(index, frame)| FrameOverlayCell {
                    input_replay: None,
                    frame_id: frame.id,
                    scopes: vec![FrameAuthoringSpan {
                        run_id: u32::try_from(index + 1).unwrap(),
                        span: FrameLocalSpan::WHOLE,
                    }],
                    marks: vec![FrameOverlayMark {
                        id: OverlayId::from_u128(index as u128 + 1),
                        z_index: i32::try_from(index).unwrap(),
                        content: OverlayContent::Raster {
                            asset_id: asset(2).id,
                            position: PhysicalPoint::default(),
                            size: PhysicalSize::new(2, 2).unwrap(),
                            opacity: 173,
                        },
                    }],
                })
                .collect(),
        ),
        annotation: Some(AnnotationRequest::default()),
        annotation_scope: None,
        name: "Frame-owned test".to_owned(),
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
    }
}

#[test]
fn legacy_tracks_keep_their_wire_shape_and_new_payload_requires_schema_two() {
    let mut project = project();
    let mut legacy = track(&project);
    legacy.frame_cells = None;
    let legacy_wire = serde_json::to_value(&legacy).unwrap();
    assert!(legacy_wire.get("frame_cells").is_none());
    assert_eq!(
        serde_json::from_value::<OverlayTrack>(legacy_wire.clone()).unwrap(),
        legacy
    );
    project.schema_version = 1;
    project.timeline.overlay_tracks.push(legacy);
    project.validate().unwrap();
    let raw = serde_json::to_string(&project).unwrap();
    let decoded: ProjectManifest = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        decoded.schema_version, 1,
        "reading must not change typed journal bytes"
    );
    assert_eq!(serde_json::to_string(&decoded).unwrap(), raw);
    project.timeline.overlay_tracks[0].frame_cells = Some(Vec::new());
    assert!(project.validate().is_err());
    project.schema_version = 2;
    project.validate().unwrap();
    for version in [0, 3, u32::MAX] {
        project.schema_version = version;
        assert!(project.validate().is_err());
    }
}

#[test]
fn required_schema_inspects_nested_payloads_and_visual_undo_never_downgrades_format() {
    let mut project = project();
    project.schema_version = 1;
    let original = project.clone();
    let owned = track(&project);
    let command = EditCommand::Compound {
        commands: vec![EditCommand::UpsertOverlayTrack { track: owned }],
    };
    assert_eq!(command.required_schema_version(), 2);
    let applied = project.apply_command(&command).unwrap();
    assert_eq!(project.schema_version, 2);
    let redo = project.apply_command(&applied.inverse).unwrap();
    assert_eq!(project.schema_version, 2);
    assert_eq!(project.timeline, original.timeline);
    assert_eq!(redo.inverse.required_schema_version(), 2);
    project.apply_command(&redo.inverse).unwrap();
    let insert_then_remove = EditCommand::Compound {
        commands: vec![
            EditCommand::UpsertOverlayTrack {
                track: track(&project),
            },
            EditCommand::RemoveOverlayTrack {
                track_id: TrackId::from_u128(1),
            },
        ],
    };
    assert_eq!(
        insert_then_remove.required_schema_version(),
        2,
        "even transient payload must be understood by a journal reader"
    );
    let restore = EditCommand::RestoreFrameEdit {
        edit: Box::new(EditCommand::RemoveFrames {
            frame_ids: vec![FrameId::from_u128(1)],
        }),
        overlay_tracks: vec![track(&project)],
    };
    assert_eq!(restore.required_schema_version(), 2);
}

#[test]
fn invalid_new_payload_rolls_back_both_the_candidate_and_its_format_upgrade() {
    let mut project = project();
    project.schema_version = 1;
    let before = project.clone();
    let mut invalid = track(&project);
    invalid.frame_cells.as_mut().unwrap()[0].frame_id = FrameId::from_u128(999);
    assert!(
        project
            .apply_command(&EditCommand::UpsertOverlayTrack { track: invalid })
            .is_err()
    );
    assert_eq!(project, before);
}

#[test]
fn reordering_and_retiming_preserve_owned_marks_and_exact_authoring_fractions() {
    let mut project = project();
    let mut owned = track(&project);
    owned.frame_cells.as_mut().unwrap()[0].scopes[0].span =
        FrameLocalSpan::new(2, 7, DurationUs::new(10).unwrap()).unwrap();
    project.timeline.overlay_tracks.push(owned.clone());
    project.validate().unwrap();
    let reverse = project
        .apply_command(&EditCommand::ReorderFrames {
            order: project
                .timeline
                .frames
                .iter()
                .rev()
                .map(|frame| frame.id)
                .collect(),
        })
        .unwrap();
    assert_eq!(project.timeline.overlay_tracks, [owned.clone()]);
    let retime = project
        .apply_command(&EditCommand::SetFrameDurations {
            changes: vec![
                FrameDurationChange {
                    frame_id: FrameId::from_u128(1),
                    duration: DurationUs::new(3).unwrap(),
                },
                FrameDurationChange {
                    frame_id: FrameId::from_u128(2),
                    duration: DurationUs::new(97).unwrap(),
                },
            ],
        })
        .unwrap();
    assert_eq!(project.timeline.overlay_tracks, [owned.clone()]);
    project.apply_command(&retime.inverse).unwrap();
    project.apply_command(&reverse.inverse).unwrap();
    assert_eq!(project.timeline.overlay_tracks, [owned]);
}

#[test]
fn deletion_prunes_only_its_owner_and_undo_restores_run_gaps_while_insertions_inherit_nothing() {
    let mut project = project();
    project.timeline.overlay_tracks.push(track(&project));
    let before = project.clone();
    let deleted = project
        .apply_command(&EditCommand::RemoveFrames {
            frame_ids: vec![FrameId::from_u128(2)],
        })
        .unwrap();
    let cells = project.timeline.overlay_tracks[0]
        .frame_cells
        .as_ref()
        .unwrap();
    assert_eq!(
        cells.iter().map(|cell| cell.frame_id).collect::<Vec<_>>(),
        [FrameId::from_u128(1), FrameId::from_u128(3)]
    );
    assert_eq!(
        cells
            .iter()
            .map(|cell| cell.scopes[0].run_id)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert!(matches!(
        deleted.inverse,
        EditCommand::RestoreFrameEdit { .. }
    ));
    project.apply_command(&deleted.inverse).unwrap();
    assert_eq!(project.timeline, before.timeline);
    project
        .apply_command(&EditCommand::InsertFrames {
            index: 1,
            frames: vec![frame(99, asset(1).id)],
        })
        .unwrap();
    assert_eq!(
        project.timeline.overlay_tracks,
        before.timeline.overlay_tracks
    );
}

#[test]
fn hidden_marks_still_own_assets_and_duplicate_or_missing_mark_identity_is_rejected() {
    let mut project = project();
    let mut owned = track(&project);
    owned.visible = false;
    owned.opacity = 0;
    project.timeline.overlay_tracks.push(owned);
    project.validate().unwrap();
    assert!(project.references_asset(asset(2).id));
    assert_eq!(project.timeline.overlay_tracks[0].mark_count(), 3);
    assert_eq!(
        project.timeline.overlay_tracks[0]
            .referenced_assets()
            .count(),
        3
    );
    assert!(
        project
            .apply_command(&EditCommand::UnregisterAsset {
                asset_id: asset(2).id
            })
            .is_err()
    );
    let mut invalid = project.clone();
    invalid.timeline.overlay_tracks[0]
        .frame_cells
        .as_mut()
        .unwrap()[1]
        .marks[0]
        .id = OverlayId::from_u128(1);
    assert!(matches!(
        invalid.validate(),
        Err(DomainError::InvalidManifest(_))
    ));
    let mut missing = project.clone();
    missing.assets.remove(&asset(2).id);
    assert!(missing.validate().is_err());
}

#[test]
fn ambiguous_owners_dual_representations_and_invalid_authoring_runs_are_rejected() {
    let project = project();
    for case in 0..5 {
        let mut owned = track(&project);
        let cells = owned.frame_cells.as_mut().unwrap();
        match case {
            0 => cells.push(cells[0].clone()),
            1 => cells[0].scopes[0].run_id = 0,
            2 => {
                let scope = cells[0].scopes[0];
                cells[0].scopes.push(scope);
            }
            3 => cells[0].scopes.clear(),
            _ => owned.annotation_scope = Some(Vec::new()),
        }
        let mut invalid = project.clone();
        invalid.timeline.overlay_tracks.push(owned);
        assert!(invalid.validate().is_err(), "case {case}");
    }
}

#[test]
fn frame_owned_progress_requires_frozen_values_instead_of_an_invented_timeline_span() {
    let mut project = project();
    let mut owned = track(&project);
    owned.frame_cells.as_mut().unwrap()[0].marks[0].content = OverlayContent::Progress {
        bounds: crate::PhysicalRect::new(0, 0, 2, 2).unwrap(),
        foreground: crate::Rgba::TRANSPARENT,
        background: crate::Rgba::TRANSPARENT,
        show_frame_number: false,
        style: None,
    };
    project.timeline.overlay_tracks.push(owned);
    assert!(project.validate().is_err());
}
