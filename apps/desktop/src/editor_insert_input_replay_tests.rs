//! Input history is verified as bounded JSON, not decoded as a raster or silently omitted.

use super::{
    tests::{prepare, workspace},
    *,
};
use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, AssetKind, BlendMode, CaptureClockId, FrameInputReplay,
    FrameInputReplayPool, FrameInputReplayRef, FrameOverlayCell, INPUT_REPLAY_MEDIA_TYPE,
    InputReplayStep, KeyStroke, OverlayTrack,
};
use gif_from_screen_gif::NeverCancel;

fn with_pool(root: &Path, bad_prefix: bool) -> (EditorWorkspace, AssetId, Vec<u8>) {
    let mut source = workspace(root, 3, [255, 0, 0, 255]);
    let pool = FrameInputReplayPool {
        version: 1,
        clock_id: Some(CaptureClockId::from_u128(77)),
        started_at: TimeUs::ZERO,
        steps: vec![InputReplayStep {
            sample_at: TimeUs::ZERO,
            capture_origin: None,
            keys: vec![KeyStroke {
                physical_key: "KeyC".to_owned(),
                display_text: Some("Ctrl+C".to_owned()),
                pressed: true,
                at: TimeUs::ZERO,
                repeat: false,
                modifiers: 2,
            }],
            mouse_events: Vec::new(),
        }],
    };
    let bytes = serde_json::to_vec(&pool).unwrap();
    let id = source.active_project().assets().put(&bytes).unwrap();
    let mut cell = FrameOverlayCell::whole(FrameId::from_u128(2), 1, Vec::new());
    cell.input_replay = Some(FrameInputReplay {
        runs: vec![FrameInputReplayRef {
            run_id: 1,
            asset_id: id,
            sample_at: TimeUs::new(100_000),
            step_end: u32::from(!bad_prefix),
        }],
    });
    source
        .execute(EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset {
                    asset: AssetDescriptor {
                        id,
                        byte_len: bytes.len() as u64,
                        kind: AssetKind::ImportedSource {
                            media_type: INPUT_REPLAY_MEDIA_TYPE.to_owned(),
                        },
                    },
                },
                EditCommand::UpsertOverlayTrack {
                    track: OverlayTrack {
                        id: TrackId::from_u128(7),
                        frame_cells: Some(vec![cell]),
                        annotation: Some(AnnotationRequest {
                            mode: AnnotationMode::RecordedKeys,
                            ..AnnotationRequest::default()
                        }),
                        annotation_scope: None,
                        name: "Hidden input history".to_owned(),
                        visible: false,
                        opacity: 0,
                        blend_mode: BlendMode::Normal,
                        items: Vec::new(),
                    },
                },
            ],
        })
        .unwrap();
    (source, id, bytes)
}

#[test]
fn project_insertion_copies_hidden_input_pools_and_preserves_refs_through_undo_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let (source, id, bytes) = with_pool(&directory.path().join("source"), false);
    let destination = directory.path().join("destination");
    let mut target = workspace(&destination, 2, [0, 0, 255, 255]);
    let before = target.manifest().clone();
    let original = source.manifest().clone();
    let prepared = prepare(&target, &source, None);
    let owner = prepared.inserted_frames[1];
    target.insert_prepared_project(prepared).unwrap();
    let after = target.manifest().clone();
    assert_eq!(
        fs::read(target.active_project().assets().asset_path(id)).unwrap(),
        bytes
    );
    let cell = &after.timeline.overlay_tracks[0]
        .frame_cells
        .as_ref()
        .unwrap()[0];
    assert_eq!(cell.frame_id, owner);
    assert_eq!(
        cell.input_replay,
        original.timeline.overlay_tracks[0]
            .frame_cells
            .as_ref()
            .unwrap()[0]
            .input_replay
    );
    assert!(target.manifest().references_asset(id));
    target.undo().unwrap();
    super::tests::same_content(target.manifest(), &before);
    target.redo().unwrap();
    assert_eq!(target.manifest().timeline, after.timeline);
    assert_eq!(source.manifest(), &original);
    drop(target);
    let reopened = EditorWorkspace::open(destination, LockPolicy::FailIfPresent, 16).unwrap();
    assert_eq!(reopened.manifest().timeline, after.timeline);
    assert_eq!(
        fs::read(reopened.active_project().assets().asset_path(id)).unwrap(),
        bytes
    );
}

#[test]
fn invalid_pool_prefix_fails_before_any_destination_blob_or_journal_write() {
    let directory = tempfile::tempdir().unwrap();
    let (source, _, _) = with_pool(&directory.path().join("source"), true);
    let target = workspace(&directory.path().join("destination"), 2, [0, 0, 255, 255]);
    let before = target.manifest().clone();
    let assets = fs::read_dir(target.active_project().assets().directory())
        .unwrap()
        .count();
    let journal = fs::read(target.project_root().join("journal.ndjson")).unwrap();
    assert!(matches!(
        prepare_project_insertion(
            target.project_insertion_target(None).unwrap(),
            source.manifest(),
            source.active_project().assets(),
            &NeverCancel
        ),
        Err(ProjectInsertionError::InvalidInputReplay { .. })
    ));
    assert_eq!(target.manifest(), &before);
    assert_eq!(
        fs::read_dir(target.active_project().assets().directory())
            .unwrap()
            .count(),
        assets
    );
    assert_eq!(
        fs::read(target.project_root().join("journal.ndjson")).unwrap(),
        journal
    );
}

#[test]
fn same_bytes_with_an_unrelated_binary_descriptor_are_not_accepted_as_input_history() {
    let directory = tempfile::tempdir().unwrap();
    let (source, id, bytes) = with_pool(&directory.path().join("source"), false);
    let mut target = workspace(&directory.path().join("destination"), 2, [0, 0, 255, 255]);
    target.active_project().assets().put(&bytes).unwrap();
    target
        .execute(EditCommand::RegisterAsset {
            asset: AssetDescriptor {
                id,
                byte_len: bytes.len() as u64,
                kind: AssetKind::ImportedSource {
                    media_type: "application/json".to_owned(),
                },
            },
        })
        .unwrap();
    let before = target.manifest().clone();
    assert!(
        matches!(prepare_project_insertion(target.project_insertion_target(None).unwrap(), source.manifest(), source.active_project().assets(), &NeverCancel), Err(ProjectInsertionError::AssetCollision(asset)) if asset == id)
    );
    assert_eq!(target.manifest(), &before);
}
