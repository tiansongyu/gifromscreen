//! Real journal/asset-store coverage for frozen annotation transfer.

use super::tests::{annotate, prepare, same_content, workspace};
use super::*;
use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, AssetKind, BlendMode, FrameAuthoringSpan, FrameLocalSpan,
    FrameOverlayCell, FrameOverlayMark, OverlayContent, OverlayId, OverlayTrack, PhysicalPoint,
    PhysicalSize, Rgba, TextRaster,
};
use gif_from_screen_gif::NeverCancel;

fn add_owned(
    workspace: &mut EditorWorkspace,
    color: [u8; 4],
    visible: bool,
    opacity: u8,
) -> (TrackId, AssetId) {
    let size = PhysicalSize::new(1, 1).unwrap();
    let asset_id = workspace.active_project().assets().put(&color).unwrap();
    let track_id = TrackId::from_u128(Uuid::new_v4().as_u128());
    let scopes = vec![
        FrameAuthoringSpan {
            run_id: 1,
            span: FrameLocalSpan::new(0, 1, DurationUs::new(3).unwrap()).unwrap(),
        },
        FrameAuthoringSpan {
            run_id: 2,
            span: FrameLocalSpan::new(2, 3, DurationUs::new(3).unwrap()).unwrap(),
        },
    ];
    let track = OverlayTrack {
        id: track_id,
        frame_cells: Some(vec![
            FrameOverlayCell {
                stage: None,
                input_replay: None,
                frame_id: FrameId::from_u128(2),
                scopes: scopes.clone(),
                marks: vec![FrameOverlayMark {
                    id: OverlayId::from_u128(Uuid::new_v4().as_u128()),
                    z_index: 2,
                    content: OverlayContent::KeyStroke {
                        text: "Held Ctrl+C".to_owned(),
                        position: PhysicalPoint::default(),
                        raster: Some(TextRaster { asset_id, size }),
                    },
                }],
            },
            FrameOverlayCell {
                stage: None,
                input_replay: None,
                frame_id: FrameId::from_u128(3),
                scopes,
                marks: Vec::new(),
            },
        ]),
        annotation: Some(AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            ..AnnotationRequest::default()
        }),
        annotation_scope: None,
        name: format!("Frozen {track_id}"),
        visible,
        opacity,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
    };
    let mut commands = Vec::new();
    if !workspace.manifest().assets.contains_key(&asset_id) {
        commands.push(EditCommand::RegisterAsset {
            asset: AssetDescriptor {
                id: asset_id,
                byte_len: 4,
                kind: AssetKind::OverlayImage {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        });
    }
    commands.push(EditCommand::UpsertOverlayTrack { track });
    workspace
        .execute(EditCommand::Compound { commands })
        .unwrap();
    (track_id, asset_id)
}

fn pixels(workspace: &EditorWorkspace, frame: FrameId) -> Vec<u8> {
    crate::editor_preview::render_frame_surface(workspace.active_project(), frame, 1024 * 1024)
        .unwrap()
        .pixels()
        .to_vec()
}

fn assert_imported_track(
    source: &OverlayTrack,
    imported: &OverlayTrack,
    owners: &BTreeMap<FrameId, FrameId>,
) {
    assert_ne!(source.id, imported.id);
    let mut expected = source.clone();
    expected.id = imported.id;
    if let Some(cells) = &mut expected.frame_cells {
        for (cell, actual) in cells.iter_mut().zip(imported.frame_cells.as_ref().unwrap()) {
            cell.frame_id = owners[&cell.frame_id];
            for (mark, new_mark) in cell.marks.iter_mut().zip(&actual.marks) {
                assert_ne!(mark.id, new_mark.id);
                mark.id = new_mark.id;
            }
        }
        assert_eq!(&expected, imported);
    } else {
        // The existing legacy insertion contract shifts its absolute spans.
        assert_eq!(source.name, imported.name);
        for (original, copied) in source.items.iter().zip(&imported.items) {
            assert_ne!(original.id, copied.id);
            assert_eq!(original.content, copied.content);
            assert_eq!(copied.span.start.get(), original.span.start.get() + 100_000);
        }
    }
}

#[test]
fn whole_project_insertion_preserves_frozen_cells_hidden_assets_and_mixed_track_order() {
    let source_root = tempfile::tempdir().unwrap();
    let dest_root = tempfile::tempdir().unwrap();
    let mut source = workspace(source_root.path(), 3, [255, 0, 0, 255]);
    let mut dest = workspace(dest_root.path(), 3, [255, 255, 255, 255]);
    annotate(
        &mut source,
        Rgba {
            red: 0,
            green: 0,
            blue: 255,
            alpha: 255,
        },
    );
    let (_, visible_asset) = add_owned(&mut source, [0, 255, 0, 255], true, 255);
    let (_, hidden_asset) = add_owned(&mut source, [17, 89, 123, 255], false, 255);
    let (_, transparent_asset) = add_owned(&mut source, [101, 83, 23, 255], true, 0);
    add_owned(&mut dest, [255, 255, 0, 255], true, 255);
    assert!(
        source.manifest().timeline.frames[1]
            .capture_metadata
            .key_strokes
            .is_empty()
    );
    let expected_pixels: Vec<_> = source
        .manifest()
        .timeline
        .frames
        .iter()
        .map(|frame| pixels(&source, frame.id))
        .collect();
    assert_eq!(&expected_pixels[1][..4], &[0, 255, 0, 255]);
    let source_before = source.manifest().clone();
    let before = dest.manifest().clone();
    let prepared = prepare(&dest, &source, Some(FrameId::from_u128(1)));
    let inserted = prepared.inserted_frames.clone();
    dest.insert_prepared_project(prepared).unwrap();
    let owners: BTreeMap<_, _> = source_before
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .zip(inserted.iter().copied())
        .collect();
    assert_eq!(
        dest.manifest().timeline.overlay_tracks[0],
        before.timeline.overlay_tracks[0]
    );
    let imported_tracks =
        &dest.manifest().timeline.overlay_tracks[before.timeline.overlay_tracks.len()..];
    for (original, imported) in source_before
        .timeline
        .overlay_tracks
        .iter()
        .zip(imported_tracks)
    {
        assert_imported_track(original, imported, &owners);
    }
    for (index, id) in inserted.iter().copied().enumerate() {
        assert_eq!(pixels(&dest, id), expected_pixels[index]);
        let frame = dest
            .manifest()
            .timeline
            .frames
            .iter()
            .find(|frame| frame.id == id)
            .unwrap();
        assert_eq!(
            frame.capture_metadata,
            source_before.timeline.frames[index].capture_metadata
        );
    }
    for id in [visible_asset, hidden_asset, transparent_asset] {
        assert_eq!(
            dest.active_project().assets().read(id).unwrap(),
            source.active_project().assets().read(id).unwrap()
        );
    }
    same_content(source.manifest(), &source_before);
    let after = dest.manifest().clone();
    dest.undo().unwrap();
    same_content(dest.manifest(), &before);
    dest.redo().unwrap();
    same_content(dest.manifest(), &after);
    drop(dest);
    let reopened = EditorWorkspace::open(dest_root.path(), LockPolicy::FailIfPresent, 16).unwrap();
    same_content(reopened.manifest(), &after);
    assert!(reopened.journal_recovery().unwrap().is_clean());
    for (index, id) in inserted.into_iter().enumerate() {
        assert_eq!(pixels(&reopened, id), expected_pixels[index]);
    }
}

#[test]
fn copying_only_a_held_frame_survives_source_deletion_undo_and_journal_reopen() {
    let root = tempfile::tempdir().unwrap();
    let mut current = workspace(root.path(), 3, [255, 0, 0, 255]);
    let (source_track, _) = add_owned(&mut current, [0, 255, 0, 255], true, 255);
    let (_, hidden_asset) = add_owned(&mut current, [13, 87, 111, 255], false, 255);
    let source_cells = current.manifest().timeline.overlay_tracks[0]
        .frame_cells
        .clone()
        .unwrap();
    let expected = pixels(&current, FrameId::from_u128(2));
    current.select_only(FrameId::from_u128(2)).unwrap();
    assert_eq!(current.copy_selection().unwrap(), 1);
    current.delete_selection().unwrap();
    assert!(
        current
            .manifest()
            .timeline
            .frames
            .iter()
            .all(|frame| frame.id != FrameId::from_u128(2))
    );
    current.select_only(FrameId::from_u128(1)).unwrap();
    let before = current.manifest().clone();
    assert_eq!(current.paste_after_current().unwrap(), 1);
    let copied_frame = current.manifest().timeline.frames[1].id;
    assert_ne!(copied_frame, FrameId::from_u128(2));
    assert_eq!(pixels(&current, copied_frame), expected);
    let copied_track =
        &current.manifest().timeline.overlay_tracks[before.timeline.overlay_tracks.len()];
    assert_ne!(copied_track.id, source_track);
    let copied = &copied_track.frame_cells.as_ref().unwrap()[0];
    assert_eq!(copied.frame_id, copied_frame);
    assert_eq!(copied.scopes, source_cells[0].scopes);
    assert_eq!(copied.marks[0].content, source_cells[0].marks[0].content);
    assert!(current.manifest().references_asset(hidden_asset));
    let after = current.manifest().clone();
    current.undo().unwrap();
    same_content(current.manifest(), &before);
    current.redo().unwrap();
    same_content(current.manifest(), &after);
    drop(current);
    let reopened = EditorWorkspace::open(root.path(), LockPolicy::FailIfPresent, 16).unwrap();
    same_content(reopened.manifest(), &after);
    assert!(reopened.journal_recovery().unwrap().is_clean());
    assert_eq!(pixels(&reopened, copied_frame), expected);
    assert_eq!(
        reopened
            .active_project()
            .assets()
            .read(hidden_asset)
            .unwrap(),
        [13, 87, 111, 255]
    );
}

#[test]
fn hidden_frame_owned_marks_count_toward_project_insertion_limit_before_asset_writes() {
    let source_root = tempfile::tempdir().unwrap();
    let dest_root = tempfile::tempdir().unwrap();
    let mut source = workspace(source_root.path(), 3, [255, 0, 0, 255]);
    let dest = workspace(dest_root.path(), 3, [255, 255, 255, 255]);
    add_owned(&mut source, [0, 255, 0, 255], false, 0);
    let mut oversized = source.manifest().clone();
    let cell = &mut oversized.timeline.overlay_tracks[0]
        .frame_cells
        .as_mut()
        .unwrap()[0];
    let sample = cell.marks[0].clone();
    cell.marks = (0..=MAX_OVERLAY_ITEMS)
        .map(|index| FrameOverlayMark {
            id: OverlayId::from_u128(u128::try_from(index).unwrap() + 1),
            ..sample.clone()
        })
        .collect();
    oversized.validate().unwrap();
    let before = dest.manifest().clone();
    let assets_before = fs::read_dir(dest.active_project().assets().directory())
        .unwrap()
        .count();
    let result = prepare_project_insertion(
        dest.project_insertion_target(None).unwrap(),
        &oversized,
        source.active_project().assets(),
        &NeverCancel,
    );
    assert!(matches!(result, Err(ProjectInsertionError::OverlayLimit)));
    assert_eq!(dest.manifest(), &before);
    assert_eq!(
        fs::read_dir(dest.active_project().assets().directory())
            .unwrap()
            .count(),
        assets_before
    );
}
