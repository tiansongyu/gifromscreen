//! End-to-end stage order through workspace commands, previews and GIF export.

use super::{
    tests::{create_rendered_duplicate_workspace, frame_id},
    *,
};
use gif_from_screen_application::{
    NoopProjectExportProgress, ProjectExportSnapshot, ProjectGifExportOptions,
    export_project_snapshot_to_gif,
};
use gif_from_screen_domain::{FrameRenderStep, Rgba, ShapeKind};

fn image(workspace: &EditorWorkspace) -> RgbaSurface {
    render_frame_surface(workspace.active_project(), frame_id(1), 1024 * 1024).unwrap()
}

fn add_shape(workspace: &mut EditorWorkspace, bounds: PhysicalRect, color: Rgba, z_index: i32) {
    workspace
        .add_overlay_for_selection(
            "Stage artwork".to_owned(),
            OverlayContent::Shape {
                kind: ShapeKind::Rectangle,
                bounds,
                stroke_width: 0,
                stroke: color,
                fill: Some(color),
            },
            z_index,
            255,
            BlendMode::Normal,
        )
        .unwrap();
}

#[test]
fn complete_pixels_follow_operation_order_and_recover_for_gif_export() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = create_rendered_duplicate_workspace(&directory);
    workspace
        .execute(EditCommand::SetFrameDurations {
            changes: workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| gif_from_screen_domain::FrameDurationChange {
                    frame_id: frame.id,
                    duration: DurationUs::new(100_000).unwrap(),
                })
                .collect(),
        })
        .unwrap();
    workspace.select_only(frame_id(1)).unwrap();
    workspace
        .set_selection_output_size(PhysicalSize::new(8, 4).unwrap())
        .unwrap();
    add_shape(
        &mut workspace,
        PhysicalRect::new(2, 1, 3, 2).unwrap(),
        Rgba {
            red: 0,
            green: 255,
            blue: 0,
            alpha: 255,
        },
        100,
    );
    workspace.rotate_selection_clockwise().unwrap();
    let rotated = image(&workspace);
    assert_eq!(rotated.size(), PhysicalSize::new(4, 8).unwrap());
    assert_eq!(
        &rotated.pixels()[(2 * 4 + 1) * 4..(2 * 4 + 2) * 4],
        &[0, 255, 0, 255]
    );
    workspace
        .add_selection_effect(Effect::Darken {
            region: PhysicalRect::new(0, 0, 4, 8).unwrap(),
            amount_percent: 100,
        })
        .unwrap();
    let darkened = image(&workspace);
    for (pixel, before) in darkened
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rotated.pixels().as_chunks::<4>().0)
    {
        assert_eq!(&pixel[..3], &[0, 0, 0]);
        assert_eq!(pixel[3], before[3], "tone edits preserve alpha");
    }
    // Even a low-z new mark is authored after the earlier image effect.
    add_shape(
        &mut workspace,
        PhysicalRect::new(0, 0, 1, 1).unwrap(),
        Rgba {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        },
        -100,
    );
    let final_image = image(&workspace);
    assert_eq!(&final_image.pixels()[..4], &[255, 0, 0, 255]);
    let tracks = &workspace.manifest().timeline.overlay_tracks;
    assert_eq!(tracks[0].frame_cells.as_ref().unwrap()[0].stage, Some(2));
    assert_eq!(tracks[1].frame_cells.as_ref().unwrap()[0].stage, None);
    assert_eq!(workspace.manifest().schema_version, 3);
    workspace.clear_selection_effects().unwrap();
    assert_eq!(
        &image(&workspace).pixels()[(2 * 4 + 1) * 4..(2 * 4 + 2) * 4],
        &[0, 255, 0, 255]
    );
    workspace.undo().unwrap();
    assert_eq!(image(&workspace), final_image);
    workspace.checkpoint().unwrap();
    drop(workspace);
    let workspace = EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(image(&workspace), final_image);
    let output = directory.path().join("stage-order.gif");
    assert_export_size(&workspace, &output, (4, 8));
}

fn assert_export_size(workspace: &EditorWorkspace, output: &std::path::Path, size: (u16, u16)) {
    let snapshot = ProjectExportSnapshot::from_active(workspace.active_project());
    let report = export_project_snapshot_to_gif(
        &snapshot,
        output,
        &ProjectGifExportOptions::default(),
        &gif_from_screen_gif::NeverCancel,
        &mut NoopProjectExportProgress,
    )
    .unwrap();
    assert_eq!(
        report.selected_frames,
        workspace.manifest().timeline.frames.len() as u64
    );
    let decoded = gif_from_screen_media::decode_gif(
        std::fs::File::open(output).unwrap(),
        &gif_from_screen_media::GifDecodeOptions::default(),
    )
    .unwrap();
    assert_eq!((decoded.width(), decoded.height()), size);
}

#[test]
fn crops_use_current_coordinates_and_canvas_change_is_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = create_rendered_duplicate_workspace(&directory);
    workspace.select_only(frame_id(1)).unwrap();
    workspace
        .set_selection_output_size(PhysicalSize::new(8, 4).unwrap())
        .unwrap();
    workspace
        .set_selection_crop(PhysicalRect::new(4, 1, 3, 2).unwrap())
        .unwrap();
    assert_eq!(image(&workspace).size(), PhysicalSize::new(3, 2).unwrap());
    assert_eq!(workspace.manifest().canvas.size, image(&workspace).size());
    let before = workspace.manifest().clone();
    assert!(workspace.clear_selection_output_size().is_err());
    assert_eq!(workspace.manifest(), &before);
    workspace.undo().unwrap();
    assert_eq!(image(&workspace).size(), PhysicalSize::new(8, 4).unwrap());
    assert!(workspace.manifest().timeline.frames.iter().all(|frame| {
        !frame
            .render_steps
            .iter()
            .any(|step| matches!(step, FrameRenderStep::Crop { .. }))
    }));
}
