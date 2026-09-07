//! Typed PM snapshots keep their container and shape across editable transfers.

use std::{fs, path::Path, sync::atomic::AtomicBool};

use gif_from_screen_application::{
    NoopProjectExportProgress, ProjectCopySnapshot, ProjectExportSnapshot, ProjectGifExportOptions,
    SaveProjectCopyOptions, export_project_snapshot_to_gif, save_project_copy,
};
use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, EditCommand, FrameId, FrameRenderStep, PhysicalSize,
    ProjectId, ProjectManifest, UnixTimeMs,
};
use gif_from_screen_gif::NeverCancel;
use gif_from_screen_media::{GifDecodeOptions, decode_gif};
use gif_from_screen_project::{AssetStore, LockPolicy};
use gif_from_screen_render::PremultipliedRgbaSurface;

use super::{
    EditorWorkspace, ProjectInsertionError, prepare_project_insertion,
    tests::{prepare, same_content, workspace},
};

fn size() -> PhysicalSize {
    PhysicalSize::new(2, 1).unwrap()
}

fn snapshot_bytes() -> Vec<u8> {
    PremultipliedRgbaSurface::new(size(), [0, 128, 0, 128].repeat(2))
        .unwrap()
        .encode(25)
        .unwrap()
}

fn attach_snapshot(
    workspace: &mut EditorWorkspace,
    bytes: &[u8],
    snapshot_size: PhysicalSize,
) -> AssetId {
    let id = workspace.active_project().assets().put(bytes).unwrap();
    let mut frame = workspace.manifest().timeline.frames[0].clone();
    if snapshot_size != size() {
        frame.transform.output_size = Some(snapshot_size);
    }
    frame.render_steps = vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::CinemagraphOverlay {
            snapshot_asset: id,
            snapshot_size,
        },
    ];
    workspace
        .execute(EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset {
                    asset: AssetDescriptor {
                        id,
                        byte_len: u64::try_from(bytes.len()).unwrap(),
                        kind: AssetKind::PremultipliedSnapshot {
                            size: snapshot_size,
                            format_version: 1,
                        },
                    },
                },
                EditCommand::ReplaceFrame {
                    frame_id: frame.id,
                    replacement: Box::new(frame),
                },
            ],
        })
        .unwrap();
    id
}

fn pixels(workspace: &EditorWorkspace, frame: FrameId) -> Vec<u8> {
    crate::editor_preview::render_frame_surface(workspace.active_project(), frame, 1024)
        .unwrap()
        .pixels()
        .to_vec()
}

fn assert_export(workspace: &EditorWorkspace, output: &Path, expected: &[Vec<u8>]) {
    export_project_snapshot_to_gif(
        &ProjectExportSnapshot::from_active(workspace.active_project()),
        output,
        &ProjectGifExportOptions::default(),
        &NeverCancel,
        &mut NoopProjectExportProgress,
    )
    .unwrap();
    let gif = decode_gif(
        fs::File::open(output).unwrap(),
        &GifDecodeOptions::default(),
    )
    .unwrap();
    assert_eq!((gif.width(), gif.height()), (2, 1));
    assert_eq!(gif.frames().len(), expected.len());
    for (frame, expected) in gif.frames().iter().zip(expected) {
        assert_eq!(frame.rgba(), expected);
        assert_eq!(frame.duration_us(), 100_000);
    }
}

fn assert_rejected_without_writes(
    destination: &EditorWorkspace,
    source: &ProjectManifest,
    source_assets: &AssetStore,
    expected: impl FnOnce(&ProjectInsertionError) -> bool,
) {
    let before = destination.manifest().clone();
    let journal = fs::read(&destination.active_project().layout().journal).unwrap();
    let mut assets = fs::read_dir(destination.active_project().assets().directory())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assets.sort();
    let error = prepare_project_insertion(
        destination.project_insertion_target(None).unwrap(),
        source,
        source_assets,
        &NeverCancel,
    )
    .unwrap_err();
    assert!(expected(&error), "{error}");
    assert_eq!(destination.manifest(), &before);
    assert_eq!(
        fs::read(&destination.active_project().layout().journal).unwrap(),
        journal
    );
    let mut after_assets = fs::read_dir(destination.active_project().assets().directory())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    after_assets.sort();
    assert_eq!(after_assets, assets);
}

#[test]
fn typed_pm_insertion_preview_gif_undo_and_journal_reopen_agree() {
    let directory = tempfile::tempdir().unwrap();
    let mut source = workspace(&directory.path().join("source"), 1, [255, 0, 0, 255]);
    let bytes = snapshot_bytes();
    let id = attach_snapshot(&mut source, &bytes, size());
    let source_before = source.manifest().clone();
    let root = directory.path().join("destination");
    let mut destination = workspace(&root, 1, [0, 0, 255, 255]);
    let before = destination.manifest().clone();
    let prepared = prepare(&destination, &source, None);
    let owner = prepared.inserted_frames[0];
    assert_eq!(destination.insert_prepared_project(prepared).unwrap(), 1);
    let expected = [127, 128, 0, 255].repeat(2);
    assert_eq!(pixels(&destination, owner), expected);
    assert_eq!(
        destination.active_project().assets().read(id).unwrap(),
        bytes
    );
    assert_eq!(
        destination.manifest().assets[&id],
        source_before.assets[&id]
    );
    assert_eq!(source.manifest(), &source_before);
    let after = destination.manifest().clone();
    destination.undo().unwrap();
    same_content(destination.manifest(), &before);
    destination.redo().unwrap();
    same_content(destination.manifest(), &after);
    drop(destination);
    let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 16).unwrap();
    assert!(reopened.journal_recovery().unwrap().is_clean());
    same_content(reopened.manifest(), &after);
    assert_eq!(pixels(&reopened, owner), expected);
    assert_export(
        &reopened,
        &directory.path().join("inserted.gif"),
        &[expected, [0, 0, 255, 255].repeat(2)],
    );
}

#[test]
fn copy_delete_paste_save_as_preserves_typed_pm_snapshot_and_raw_source() {
    let directory = tempfile::tempdir().unwrap();
    let mut project = workspace(&directory.path().join("source"), 2, [255, 0, 0, 255]);
    let bytes = snapshot_bytes();
    let id = attach_snapshot(&mut project, &bytes, size());
    let original = project.manifest().timeline.frames[0].clone();
    project.select_only(original.id).unwrap();
    assert_eq!(project.copy_selection().unwrap(), 1);
    project.delete_selection().unwrap();
    project.select_first().unwrap();
    let before = project.manifest().clone();
    assert_eq!(project.paste_after_current().unwrap(), 1);
    let copied = project.manifest().timeline.frames[1].clone();
    assert_ne!(copied.id, original.id);
    assert_eq!(copied.asset_id, original.asset_id);
    assert_eq!(copied.capture_metadata, original.capture_metadata);
    assert_eq!(copied.render_steps, original.render_steps);
    let expected = [127, 128, 0, 255].repeat(2);
    assert_eq!(pixels(&project, copied.id), expected);
    let after = project.manifest().clone();
    project.undo().unwrap();
    same_content(project.manifest(), &before);
    project.redo().unwrap();
    same_content(project.manifest(), &after);
    let saved = directory.path().join("saved.gfsproj");
    save_project_copy(
        &ProjectCopySnapshot::from_active(project.active_project()),
        &SaveProjectCopyOptions {
            target: saved.clone(),
            project_id: ProjectId::from_u128(123_456),
            created_at: UnixTimeMs::new(0),
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let reopened = EditorWorkspace::open(saved, LockPolicy::FailIfPresent, 16).unwrap();
    assert_eq!(reopened.active_project().assets().read(id).unwrap(), bytes);
    assert_eq!(pixels(&reopened, copied.id), expected);
    assert_export(
        &reopened,
        &directory.path().join("copied.gif"),
        &[[255, 0, 0, 255].repeat(2), expected],
    );
}

#[test]
fn matching_digest_does_not_accept_bad_pm_magic_version_shape_or_channels() {
    for corrupt_index in [0, 7, 9, 17] {
        let directory = tempfile::tempdir().unwrap();
        let mut source = workspace(&directory.path().join("source"), 1, [255, 0, 0, 255]);
        let mut bytes = snapshot_bytes();
        bytes[corrupt_index] = 255;
        let id = attach_snapshot(&mut source, &bytes, size());
        assert_eq!(AssetStore::id_for_bytes(&bytes), id);
        let destination = workspace(&directory.path().join("destination"), 1, [0, 0, 255, 255]);
        assert_rejected_without_writes(
            &destination,
            source.manifest(),
            source.active_project().assets(),
            |error| matches!(error, ProjectInsertionError::InvalidPremultipliedSnapshot { asset_id, .. } if *asset_id == id),
        );
    }
}

#[test]
fn typed_snapshot_cannot_use_same_area_different_shape_view_on_insertion() {
    let directory = tempfile::tempdir().unwrap();
    let mut source = workspace(&directory.path().join("source"), 1, [255, 0, 0, 255]);
    let column = PhysicalSize::new(1, 2).unwrap();
    let id = attach_snapshot(&mut source, &snapshot_bytes(), column);
    let mut destination = workspace(&directory.path().join("destination"), 1, [0, 0, 255, 255]);
    let mut frame = destination.manifest().timeline.frames[0].clone();
    frame.transform.output_size = Some(column);
    destination
        .execute(EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame),
        })
        .unwrap();
    assert_rejected_without_writes(
        &destination,
        source.manifest(),
        source.active_project().assets(),
        |error| matches!(error, ProjectInsertionError::InvalidPremultipliedSnapshot { asset_id, .. } if *asset_id == id),
    );
}

#[test]
fn identical_snapshot_digest_does_not_relax_destination_kind_or_shape_identity() {
    for kind in [
        AssetKind::ImportedSource {
            media_type: "application/octet-stream".to_owned(),
        },
        AssetKind::PremultipliedSnapshot {
            size: PhysicalSize::new(1, 2).unwrap(),
            format_version: 1,
        },
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut source = workspace(&directory.path().join("source"), 1, [255, 0, 0, 255]);
        let bytes = snapshot_bytes();
        let id = attach_snapshot(&mut source, &bytes, size());
        let mut destination = workspace(&directory.path().join("destination"), 1, [0, 0, 255, 255]);
        assert_eq!(
            destination.active_project().assets().put(&bytes).unwrap(),
            id
        );
        destination
            .execute(EditCommand::RegisterAsset {
                asset: AssetDescriptor {
                    id,
                    byte_len: 25,
                    kind,
                },
            })
            .unwrap();
        assert_rejected_without_writes(
            &destination,
            source.manifest(),
            source.active_project().assets(),
            |error| matches!(error, ProjectInsertionError::AssetCollision(asset) if *asset == id),
        );
    }
}

#[test]
fn same_digest_with_non_pm_source_descriptor_is_rejected_before_destination_writes() {
    let directory = tempfile::tempdir().unwrap();
    let mut source = workspace(&directory.path().join("source"), 1, [255, 0, 0, 255]);
    let id = attach_snapshot(&mut source, &snapshot_bytes(), size());
    let mut malformed = source.manifest().clone();
    malformed.assets.get_mut(&id).unwrap().kind = AssetKind::ImportedSource {
        media_type: "application/octet-stream".to_owned(),
    };
    let destination = workspace(&directory.path().join("destination"), 1, [0, 0, 255, 255]);
    assert_rejected_without_writes(
        &destination,
        &malformed,
        source.active_project().assets(),
        |error| matches!(error, ProjectInsertionError::Project(_)),
    );
}
