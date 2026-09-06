//! Input pools own non-rendered assets, including on hidden or empty annotation cells.

use crate::model::test_fixtures::{asset, frame, manifest};
use crate::*;

fn project() -> (ProjectManifest, AssetId) {
    let mut project = manifest();
    let base = asset(1);
    project.assets.insert(base.id, base.clone());
    project.timeline.frames.push(frame(1, base.id));
    let pool_id = asset(2).id;
    project.assets.insert(
        pool_id,
        AssetDescriptor {
            id: pool_id,
            byte_len: 128,
            kind: AssetKind::ImportedSource {
                media_type: INPUT_REPLAY_MEDIA_TYPE.to_owned(),
            },
        },
    );
    let mut cell = FrameOverlayCell::whole(FrameId::from_u128(1), 1, Vec::new());
    cell.input_replay = Some(FrameInputReplay {
        runs: vec![FrameInputReplayRef {
            run_id: 1,
            asset_id: pool_id,
            sample_at: TimeUs::ZERO,
            step_end: 0,
        }],
    });
    project.timeline.overlay_tracks.push(OverlayTrack {
        id: TrackId::from_u128(1),
        frame_cells: Some(vec![cell]),
        annotation: Some(AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            ..AnnotationRequest::default()
        }),
        annotation_scope: None,
        name: "Empty hidden owner".to_owned(),
        visible: false,
        opacity: 0,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
    });
    (project, pool_id)
}

#[test]
fn input_pool_references_are_preserved_and_protected_even_without_visible_marks() {
    let (mut project, pool) = project();
    project.validate().unwrap();
    let before = project.clone();
    let track = &project.timeline.overlay_tracks[0];
    assert_eq!(track.mark_count(), 0);
    assert_eq!(track.referenced_assets().collect::<Vec<_>>(), [pool]);
    assert!(project.references_asset(pool));
    assert!(
        project
            .apply_command(&EditCommand::UnregisterAsset { asset_id: pool })
            .is_err()
    );
    assert_eq!(project, before);
    let json = serde_json::to_vec(&project).unwrap();
    assert_eq!(
        serde_json::from_slice::<ProjectManifest>(&json).unwrap(),
        before
    );
}

#[test]
fn missing_wrong_kind_or_oversized_input_pool_descriptors_are_rejected() {
    let (project, pool) = project();
    for case in 0..4 {
        let mut invalid = project.clone();
        match case {
            0 => {
                invalid.assets.remove(&pool);
            }
            1 => {
                invalid.assets.get_mut(&pool).unwrap().kind = AssetKind::ImportedSource {
                    media_type: "application/json".to_owned(),
                };
            }
            2 => {
                invalid.assets.get_mut(&pool).unwrap().byte_len = MAX_INPUT_REPLAY_POOL_BYTES + 1;
            }
            _ => {
                invalid.assets.get_mut(&pool).unwrap().byte_len = 0;
            }
        }
        assert!(invalid.validate().is_err(), "case {case}");
    }
}
