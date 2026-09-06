//! Save As preserves frozen frame marks, including held labels on event-empty frames.

use std::{path::Path, sync::atomic::AtomicBool};

use gif_from_screen_application::{
    IncrementalRecordingProject, IncrementalRecordingProjectOptions, NoopProjectExportProgress,
    ProjectCopySnapshot, ProjectExportSnapshot, ProjectGifExportOptions, SaveProjectCopyOptions,
    export_project_snapshot_to_gif, save_project_copy,
};
use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, BlendMode, EditCommand, FrameAuthoringSpan, FrameId,
    FrameLocalSpan, FrameOverlayCell, FrameOverlayMark, OverlayContent, OverlayId, OverlayTrack,
    PhysicalPoint, PhysicalSize, ProjectId, RasterEncoding, TextRaster, TrackId, UnixTimeMs,
};
use gif_from_screen_gif::{NeverCancel, RgbaFrame};
use gif_from_screen_project::{ActiveProject, LockPolicy};

fn source(root: &Path) -> ActiveProject {
    let mut writer = IncrementalRecordingProject::create(
        root,
        PhysicalSize::new(2, 2).unwrap(),
        IncrementalRecordingProjectOptions {
            project_id: ProjectId::from_u128(21),
            app_version: "frame-owned-copy-test".to_owned(),
            created_at: UnixTimeMs::new(1),
            source_label: None,
        },
    )
    .unwrap();
    for id in 1..=3 {
        writer
            .append_frame(
                FrameId::from_u128(id),
                &RgbaFrame::new(2, 2, vec![255; 16], 100_000).unwrap(),
            )
            .unwrap();
    }
    let mut project = writer.finish().unwrap();
    let red = register_raster(&mut project, [255, 0, 0, 255]);
    let green = register_raster(&mut project, [0, 255, 0, 255]);
    project
        .commit(EditCommand::Compound {
            commands: vec![
                EditCommand::UpsertOverlayTrack {
                    track: owned_track(1, 2, red, true),
                },
                EditCommand::UpsertOverlayTrack {
                    track: owned_track(2, 1, green, false),
                },
            ],
        })
        .unwrap();
    project
}

fn register_raster(project: &mut ActiveProject, color: [u8; 4]) -> AssetId {
    let bytes = color.repeat(4);
    let id = project.assets().put(&bytes).unwrap();
    project
        .commit(EditCommand::RegisterAsset {
            asset: AssetDescriptor {
                id,
                byte_len: 16,
                kind: AssetKind::OverlayImage {
                    size: PhysicalSize::new(2, 2).unwrap(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        })
        .unwrap();
    id
}

fn owned_track(id: u128, owner: u128, asset: AssetId, visible: bool) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(id),
        frame_cells: Some(vec![FrameOverlayCell {
            frame_id: FrameId::from_u128(owner),
            scopes: vec![FrameAuthoringSpan {
                run_id: 1,
                span: FrameLocalSpan::WHOLE,
            }],
            marks: vec![FrameOverlayMark {
                id: OverlayId::from_u128(id),
                z_index: 0,
                content: OverlayContent::KeyStroke {
                    text: "Previously authored held label".to_owned(),
                    position: PhysicalPoint::default(),
                    raster: Some(TextRaster {
                        asset_id: asset,
                        size: PhysicalSize::new(2, 2).unwrap(),
                    }),
                },
            }],
        }]),
        annotation: None,
        annotation_scope: None,
        name: format!("Frozen group {id}"),
        visible,
        opacity: if visible { 255 } else { 0 },
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
    }
}

fn export(project: &ActiveProject, path: &Path) -> Vec<u8> {
    let report = export_project_snapshot_to_gif(
        &ProjectExportSnapshot::from_active(project),
        path,
        &ProjectGifExportOptions::default(),
        &NeverCancel,
        &mut NoopProjectExportProgress,
    )
    .unwrap();
    assert_eq!(report.selected_frames, 3);
    std::fs::read(path).unwrap()
}

#[test]
fn save_as_preserves_frozen_labels_hidden_assets_and_source_independence() {
    let directory = tempfile::tempdir().unwrap();
    let mut source = source(&directory.path().join("source.gfsproj"));
    assert!(
        source.manifest().timeline.frames[1]
            .capture_metadata
            .key_strokes
            .is_empty()
    );
    let before = source.manifest().clone();
    let target = directory.path().join("copy.gfsproj");
    save_project_copy(
        &ProjectCopySnapshot::from_active(&source),
        &SaveProjectCopyOptions {
            target: target.clone(),
            project_id: ProjectId::from_u128(22),
            created_at: UnixTimeMs::new(2),
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(source.manifest(), &before);
    let opened = ActiveProject::open(&target, LockPolicy::FailIfPresent).unwrap();
    assert!(opened.asset_issues.is_empty());
    let copied = opened.project;
    assert_eq!(copied.manifest().schema_version, 2);
    assert_ne!(copied.manifest().project_id, source.manifest().project_id);
    assert_eq!(copied.manifest().timeline, before.timeline);
    for asset in copied
        .manifest()
        .timeline
        .overlay_tracks
        .iter()
        .flat_map(OverlayTrack::referenced_assets)
    {
        assert!(
            copied.assets().asset_path(asset).is_file(),
            "hidden marks must be copied too"
        );
    }
    let expected = export(&source, &directory.path().join("source.gif"));
    assert_eq!(
        export(&copied, &directory.path().join("copy.gif")),
        expected
    );
    source
        .commit(EditCommand::RemoveOverlayTrack {
            track_id: TrackId::from_u128(1),
        })
        .unwrap();
    assert_eq!(copied.manifest().timeline, before.timeline);
    assert_eq!(
        export(&copied, &directory.path().join("still-frozen.gif")),
        expected
    );
    assert_ne!(
        export(&source, &directory.path().join("edited-source.gif")),
        expected
    );
    drop(copied);
    let reopened = ActiveProject::open(target, LockPolicy::FailIfPresent).unwrap();
    assert_eq!(reopened.project.manifest().timeline, before.timeline);
}
