//! Integrated non-destructive freeze ownership, resources and replay gates.

use super::*;
use gif_from_screen_application::{
    NoopProjectExportProgress, ProjectCopySnapshot, ProjectExportSnapshot, ProjectGifExportOptions,
    SaveProjectCopyOptions, export_project_snapshot_to_gif, save_project_copy,
};
use gif_from_screen_domain::{FrameRenderStep, QuarterTurn};

fn baseline(workspace: &EditorWorkspace, frame: FrameId) -> AssetId {
    workspace
        .manifest()
        .timeline
        .frames
        .iter()
        .find(|clip| clip.id == frame)
        .unwrap()
        .render_steps
        .iter()
        .find_map(|step| match step {
            FrameRenderStep::FreezeRegion { baseline_asset, .. } => Some(*baseline_asset),
            _ => None,
        })
        .unwrap()
}

fn export(workspace: &EditorWorkspace, path: &std::path::Path) -> Result<(), String> {
    export_project_snapshot_to_gif(
        &ProjectExportSnapshot::from_active(workspace.active_project()),
        path,
        &ProjectGifExportOptions::default(),
        &gif_from_screen_gif::NeverCancel,
        &mut NoopProjectExportProgress,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

#[test]
fn earlier_recorded_groups_can_be_reedited_but_new_groups_after_freeze_are_blocked() {
    for mode in [
        AnnotationMode::RecordedCursor,
        AnnotationMode::RecordedClicks,
        AnnotationMode::RecordedKeys,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("input.gfsproj"));
        add_raw_input(&mut workspace);
        if matches!(mode, AnnotationMode::RecordedKeys) {
            workspace
                .set_selection_output_size(PhysicalSize::new(64, 24).unwrap())
                .unwrap();
        }
        let request = AnnotationRequest {
            mode: mode.clone(),
            size: workspace.manifest().canvas.size,
            font_size_px: 8,
            click_radius: 1,
            ..AnnotationRequest::default()
        };
        workspace
            .apply_annotation_edit(
                &workspace.project_edit_anchor(),
                &request,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        let original = workspace.manifest().timeline.overlay_tracks[0].clone();
        let raw = workspace.manifest().timeline.frames[0]
            .capture_metadata
            .clone();
        bake_whole_selected(&mut workspace);
        let frozen = workspace.manifest().clone();
        let mut updated = request.clone();
        updated.opacity = 137;
        workspace
            .apply_annotation_group(
                &workspace.project_edit_anchor(),
                &updated,
                Some(original.id),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        assert_eq!(
            workspace.manifest().timeline.frames[0].capture_metadata,
            raw
        );
        assert_eq!(
            workspace.manifest().timeline.frames[0].capture_binding,
            CaptureBinding::Original
        );
        assert_eq!(
            workspace.manifest().timeline.overlay_tracks[0]
                .frame_cells
                .as_ref()
                .unwrap()[0]
                .stage,
            original.frame_cells.as_ref().unwrap()[0].stage
        );
        workspace.undo().unwrap();
        equal_except_revision(workspace.manifest(), &frozen);
        let before = workspace.manifest().clone();
        assert!(
            workspace
                .apply_annotation_edit(
                    &workspace.project_edit_anchor(),
                    &request,
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
        );
        assert_eq!(workspace.manifest(), &before);
        assert!(gif_from_screen_domain::recorded_annotation_barrier(
            &workspace.manifest().timeline.frames[0],
            &mode
        ));
    }
}

#[test]
fn baseline_survives_copy_source_deletion_save_as_reopen_and_gif_export() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("source.gfsproj"));
    add_owned_overlay(&mut workspace);
    bake_whole_selected(&mut workspace);
    let reference = baseline(&workspace, FrameId::from_u128(1));
    workspace.select_only(FrameId::from_u128(1)).unwrap();
    let expected = pixels(&workspace, 1);
    workspace.copy_selection().unwrap();
    workspace.delete_selection().unwrap();
    let before_paste: BTreeSet<_> = workspace
        .manifest()
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    workspace.paste_after_current().unwrap();
    let copied = workspace
        .manifest()
        .timeline
        .frames
        .iter()
        .find(|frame| !before_paste.contains(&frame.id))
        .unwrap()
        .id;
    assert_ne!(copied, FrameId::from_u128(1));
    assert_eq!(baseline(&workspace, copied), reference);
    let render_copied = |workspace: &EditorWorkspace| {
        render(
            workspace.active_project(),
            copied,
            workspace.manifest().canvas.size,
            &AtomicBool::new(false),
        )
        .unwrap()
    };
    assert_eq!(render_copied(&workspace).pixels(), expected);
    let before = workspace.manifest().clone();
    assert!(
        workspace
            .execute(EditCommand::UnregisterAsset {
                asset_id: reference
            })
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
    let target = directory.path().join("copy.gfsproj");
    save_project_copy(
        &ProjectCopySnapshot::from_active(workspace.active_project()),
        &SaveProjectCopyOptions {
            target: target.clone(),
            project_id: ProjectId::from_u128(9123),
            created_at: UnixTimeMs::new(9),
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let copy = EditorWorkspace::open(&target, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(render_copied(&copy).pixels(), expected);
    let original_gif = directory.path().join("original.gif");
    let copied_gif = directory.path().join("copied.gif");
    export(&workspace, &original_gif).unwrap();
    export(&copy, &copied_gif).unwrap();
    assert_eq!(
        std::fs::read(original_gif).unwrap(),
        std::fs::read(copied_gif).unwrap()
    );
}

#[test]
fn same_raw_asset_can_be_a_rotated_frozen_view_without_changing_its_canonical_shape() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("alias.gfsproj"));
    let bytes = [20, 40, 60, 255, 20, 40, 60, 255];
    let id = workspace.active_project().assets().put(&bytes).unwrap();
    let mut frame = workspace.manifest().timeline.frames[0].clone();
    frame.asset_id = id;
    let descriptor = AssetDescriptor {
        id,
        byte_len: 8,
        kind: AssetKind::Frame {
            size: PhysicalSize::new(2, 1).unwrap(),
            encoding: RasterEncoding::Rgba8,
        },
    };
    workspace
        .execute(EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset {
                    asset: descriptor.clone(),
                },
                EditCommand::ReplaceFrame {
                    frame_id: frame.id,
                    replacement: Box::new(frame),
                },
            ],
        })
        .unwrap();
    workspace.rotate_selection_clockwise().unwrap();
    workspace
        .apply_motion_edit(
            &workspace.project_edit_anchor(),
            MotionOperation::RectangularFreeze {
                region: PhysicalRect::new(0, 0, 1, 2).unwrap(),
                invert: true,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(baseline(&workspace, FrameId::from_u128(1)), id);
    assert_eq!(workspace.manifest().assets[&id], descriptor);
    let image = render(
        workspace.active_project(),
        FrameId::from_u128(1),
        PhysicalSize::new(1, 2).unwrap(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(image.pixels(), bytes);
    // Changing dimensions before the frozen view must fail as one atomic edit.
    let before = workspace.manifest().clone();
    let mut invalid = before.timeline.frames[0].clone();
    let rotation = invalid
        .render_steps
        .iter_mut()
        .find(|step| matches!(step, FrameRenderStep::Rotate { .. }))
        .unwrap();
    *rotation = FrameRenderStep::Rotate {
        rotation: QuarterTurn::Zero,
    };
    assert!(
        workspace
            .execute(EditCommand::ReplaceFrame {
                frame_id: invalid.id,
                replacement: Box::new(invalid)
            })
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
    workspace.rotate_selection_clockwise().unwrap();
    assert_eq!(
        workspace.manifest().canvas.size,
        PhysicalSize::new(2, 1).unwrap()
    );
    export(&workspace, &directory.path().join("alias.gif")).unwrap();
}

#[test]
fn preview_and_export_validate_baseline_bytes_and_do_not_publish_partial_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("integrity.gfsproj"));
    add_owned_overlay(&mut workspace);
    bake_whole_selected(&mut workspace);
    let frame_id = FrameId::from_u128(1);
    let reference = baseline(&workspace, frame_id);
    assert_ne!(reference, workspace.manifest().timeline.frames[0].asset_id);
    let plan = PreviewRenderPlan::new(
        workspace.active_project(),
        &workspace.manifest().timeline.frames[0],
        TimeUs::ZERO,
    )
    .unwrap();
    assert!(
        plan.render(15, &gif_from_screen_render::NeverCancel)
            .is_err()
    );
    plan.render(16, &gif_from_screen_render::NeverCancel)
        .unwrap();
    let path = workspace.active_project().assets().asset_path(reference);
    let bytes = std::fs::read(&path).unwrap();
    let mut damaged = bytes.clone();
    damaged[0] ^= 1;
    std::fs::write(&path, damaged).unwrap();
    assert!(
        plan.render(1024, &gif_from_screen_render::NeverCancel)
            .is_err()
    );
    let output = directory.path().join("corrupt.gif");
    assert!(export(&workspace, &output).is_err());
    assert!(!output.exists());
    std::fs::write(&path, &bytes).unwrap();
    plan.render(1024, &gif_from_screen_render::NeverCancel)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(
        plan.render(1024, &gif_from_screen_render::NeverCancel)
            .is_err()
    );
    assert!(export(&workspace, &output).is_err());
    assert!(!output.exists());
}

#[test]
fn frozen_transition_endpoints_export_through_local_and_global_palette_paths() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("transitions.gfsproj"));
    add_owned_overlay(&mut workspace);
    bake_whole_selected(&mut workspace);
    workspace
        .execute(EditCommand::SetTransitions {
            transitions: vec![gif_from_screen_domain::Transition {
                id: gif_from_screen_domain::TransitionId::from_u128(99),
                from_frame: FrameId::from_u128(1),
                to_frame: FrameId::from_u128(2),
                duration: DurationUs::new(20_000).unwrap(),
                steps: 2,
                kind: TransitionKind::FadeToNext,
            }],
        })
        .unwrap();
    for (name, palette_mode) in [
        ("local", gif_from_screen_gif::PaletteMode::LocalPerFrame),
        ("global", gif_from_screen_gif::PaletteMode::Global),
    ] {
        let path = directory.path().join(format!("{name}.gif"));
        let mut options = ProjectGifExportOptions::default();
        options.encoding.palette_mode = palette_mode;
        export_project_snapshot_to_gif(
            &ProjectExportSnapshot::from_active(workspace.active_project()),
            &path,
            &options,
            &gif_from_screen_gif::NeverCancel,
            &mut NoopProjectExportProgress,
        )
        .unwrap();
        let decoded = gif_from_screen_media::decode_gif(
            std::fs::File::open(path).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 1));
        assert_eq!(
            decoded
                .frames()
                .iter()
                .map(gif_from_screen_media::DecodedFrame::duration_us)
                .sum::<u64>(),
            50_000
        );
    }
}
