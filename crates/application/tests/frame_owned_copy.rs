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
            stage: None,
            input_replay: None,
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
fn save_as_preserves_ordered_geometry_and_hidden_stage_artwork() {
    use gif_from_screen_domain::{FrameRenderStep, QuarterTurn};
    let directory = tempfile::tempdir().unwrap();
    let mut source = source(&directory.path().join("staged-source.gfsproj"));
    let mut commands = Vec::new();
    for frame in &source.manifest().timeline.frames {
        let mut replacement = frame.clone();
        replacement.render_steps = vec![
            FrameRenderStep::Composite { stage_id: 7 },
            FrameRenderStep::Resize {
                size: PhysicalSize::new(1, 2).unwrap(),
            },
            FrameRenderStep::Rotate {
                rotation: QuarterTurn::Clockwise90,
            },
        ];
        commands.push(EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(replacement),
        });
    }
    for track in &source.manifest().timeline.overlay_tracks {
        let mut track = track.clone();
        for cell in track.frame_cells.iter_mut().flatten() {
            cell.stage = Some(7);
        }
        commands.push(EditCommand::UpsertOverlayTrack { track });
    }
    let mut canvas = source.manifest().canvas.clone();
    canvas.size = PhysicalSize::new(2, 1).unwrap();
    commands.push(EditCommand::SetCanvas { canvas });
    source.commit(EditCommand::Compound { commands }).unwrap();
    let target = directory.path().join("staged-copy.gfsproj");
    save_project_copy(
        &ProjectCopySnapshot::from_active(&source),
        &SaveProjectCopyOptions {
            target: target.clone(),
            project_id: ProjectId::from_u128(23),
            created_at: UnixTimeMs::new(2),
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let copied = ActiveProject::open(&target, LockPolicy::FailIfPresent)
        .unwrap()
        .project;
    assert_eq!(copied.manifest().timeline, source.manifest().timeline);
    assert_eq!(copied.manifest().canvas, source.manifest().canvas);
    assert_eq!(
        export(&copied, &directory.path().join("copy.gif")),
        export(&source, &directory.path().join("source.gif"))
    );
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
    assert_eq!(
        copied.manifest().schema_version,
        gif_from_screen_domain::CURRENT_SCHEMA_VERSION
    );
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

#[test]
fn save_as_keeps_non_raster_input_pools_without_putting_them_into_the_gif() {
    use gif_from_screen_domain::{
        FrameInputReplay, FrameInputReplayPool, FrameInputReplayRef, INPUT_REPLAY_MEDIA_TYPE,
        TimeUs,
    };
    let directory = tempfile::tempdir().unwrap();
    let mut source = source(&directory.path().join("source.gfsproj"));
    let before_gif = export(&source, &directory.path().join("before.gif"));
    let bytes = serde_json::to_vec(&FrameInputReplayPool {
        version: 1,
        clock_id: None,
        started_at: TimeUs::ZERO,
        steps: Vec::new(),
    })
    .unwrap();
    let id = source.assets().put(&bytes).unwrap();
    let mut track = source.manifest().timeline.overlay_tracks[1].clone();
    track.frame_cells.as_mut().unwrap()[0].input_replay = Some(FrameInputReplay {
        runs: vec![FrameInputReplayRef {
            run_id: 1,
            asset_id: id,
            sample_at: TimeUs::ZERO,
            step_end: 0,
        }],
    });
    source
        .commit(EditCommand::Compound {
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
                EditCommand::UpsertOverlayTrack { track },
            ],
        })
        .unwrap();
    let before = source.manifest().clone();
    let target = directory.path().join("with-history.gfsproj");
    save_project_copy(
        &ProjectCopySnapshot::from_active(&source),
        &SaveProjectCopyOptions {
            target: target.clone(),
            project_id: ProjectId::from_u128(23),
            created_at: UnixTimeMs::new(3),
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let copied = ActiveProject::open(target, LockPolicy::FailIfPresent)
        .unwrap()
        .project;
    assert_eq!(
        std::fs::read(copied.assets().asset_path(id)).unwrap(),
        bytes
    );
    assert_eq!(copied.manifest().timeline, before.timeline);
    assert_eq!(source.manifest(), &before);
    assert_eq!(
        export(&source, &directory.path().join("source-with-pool.gif")),
        before_gif
    );
    assert_eq!(
        export(&copied, &directory.path().join("copied-with-pool.gif")),
        before_gif
    );
}
