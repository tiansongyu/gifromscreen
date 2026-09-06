//! Expanded-image effects through real workspace persistence and render consumers.

use std::{
    hash::{Hash, Hasher},
    sync::atomic::AtomicBool,
};

use super::{
    tests::{create_rendered_duplicate_workspace, frame_id},
    *,
};
use gif_from_screen_application::{
    NoopProjectExportProgress, ProjectCopySnapshot, ProjectExportSnapshot, ProjectGifExportOptions,
    SaveProjectCopyOptions, export_project_snapshot_to_gif, save_project_copy,
};
use gif_from_screen_domain::{
    CaptureClockContext, CaptureClockId, FrameDurationChange, FrameRenderStep, ImageBorderStyle,
    ImageShadowStyle, Rgba, ShapeKind, SignedEdgeWidths, UnixTimeMs,
};

const RED: Rgba = color(255, 0, 0);
const GREEN: Rgba = color(0, 255, 0);
const BLUE: Rgba = color(0, 0, 255);
const MAGENTA: Rgba = color(255, 0, 255);

const fn color(red: u8, green: u8, blue: u8) -> Rgba {
    Rgba {
        red,
        green,
        blue,
        alpha: 255,
    }
}

fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}

fn image(workspace: &EditorWorkspace, id: FrameId) -> RgbaSurface {
    render_frame_surface(workspace.active_project(), id, 1024 * 1024).unwrap()
}

fn pixel(surface: &RgbaSurface, x: usize, y: usize) -> &[u8] {
    let offset = (y * surface.size().width.get() as usize + x) * 4;
    &surface.pixels()[offset..offset + 4]
}

fn pixel_hash(bytes: &[u8]) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}

fn add_shape(workspace: &mut EditorWorkspace, name: &str, x: u32, y: u32, fill: Rgba) {
    workspace
        .add_overlay_for_selection(
            name.to_owned(),
            OverlayContent::Shape {
                kind: ShapeKind::Rectangle,
                bounds: PhysicalRect::new(x, y, 1, 1).unwrap(),
                stroke_width: 0,
                stroke: fill,
                fill: Some(fill),
            },
            // Low z cannot put a newly authored mark back under an earlier stage.
            -100,
            255,
            BlendMode::Normal,
        )
        .unwrap();
}

fn shadow() -> ImageShadowStyle {
    ImageShadowStyle {
        blur_radius_hundredths: 0,
        depth_hundredths: 100,
        direction_hundredths: 0,
        opacity_basis_points: 10_000,
        ..ImageShadowStyle::default()
    }
}

fn workspace(directory: &tempfile::TempDir) -> EditorWorkspace {
    let mut workspace = create_rendered_duplicate_workspace(directory);
    workspace
        .execute(EditCommand::SetFrameDurations {
            changes: workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| FrameDurationChange {
                    frame_id: frame.id,
                    duration: DurationUs::new(100_000).unwrap(),
                })
                .collect(),
        })
        .unwrap();
    let mut first = workspace.manifest().timeline.frames[0].clone();
    first.capture_metadata.captured_at = Some(TimeUs::new(123_456));
    first.capture_metadata.dropped_frames_before = 7;
    first.capture_clock = Some(CaptureClockContext {
        id: Some(CaptureClockId::from_u128(77)),
        sampled_at: TimeUs::new(123_456),
    });
    workspace
        .execute(EditCommand::ReplaceFrame {
            frame_id: first.id,
            replacement: Box::new(first),
        })
        .unwrap();
    workspace.select_only(frame_id(1)).unwrap();
    workspace.set_selection_output_size(size(6, 4)).unwrap();
    workspace
}

fn add_complete_chain(workspace: &mut EditorWorkspace) {
    add_shape(workspace, "A before image effects", 4, 1, GREEN);
    workspace
        .add_image_effect(ComposedImageEffect::Shadow(shadow()))
        .unwrap();
    assert_eq!(workspace.manifest().canvas.size, size(7, 4));
    let shadowed = image(workspace, frame_id(1));
    assert_eq!(pixel(&shadowed, 4, 1), &[0, 255, 0, 255]);
    assert_eq!(
        pixel(&shadowed, 5, 1),
        // Reference fixed-point alpha = floor(255^3 / 65536) = 253;
        // black over white therefore leaves two intensity levels.
        &[2, 2, 2, 255],
        "A participates in the shadow"
    );
    assert_eq!(
        pixel(&shadowed, 5, 0),
        &[255, 255, 255, 255],
        "transparent source holes see the explicit white background"
    );
    workspace
        .add_image_effect(ComposedImageEffect::Border(ImageBorderStyle {
            widths: SignedEdgeWidths {
                top_milli: -1_000,
                right_milli: 1_000,
                bottom_milli: 0,
                left_milli: -2_000,
            },
            color: BLUE,
            ..ImageBorderStyle::default()
        }))
        .unwrap();
    add_shape(workspace, "B after image effects", 0, 3, MAGENTA);
}

fn assert_chain_pixels(surface: &RgbaSurface) {
    assert_eq!(surface.size(), size(9, 5));
    assert_eq!(
        pixel(surface, 2, 2),
        &[RED.red, RED.green, RED.blue, RED.alpha],
        "original source translates by the exterior border origin"
    );
    assert_eq!(pixel(surface, 6, 2), &[0, 255, 0, 255]);
    assert_eq!(pixel(surface, 7, 2), &[2, 2, 2, 255]);
    assert_eq!(pixel(surface, 7, 1), &[255, 255, 255, 255]);
    assert_eq!(pixel(surface, 0, 3), &[255, 0, 255, 255]);
    assert_eq!(
        pixel(surface, 1, 3),
        &[0, 0, 255, 255],
        "B is at the tail and casts no earlier shadow"
    );
}

fn export(workspace: &EditorWorkspace, name: &str) -> gif_from_screen_media::DecodedAnimation {
    let output = workspace.active_project().layout().root.join(name);
    export_project_snapshot_to_gif(
        &ProjectExportSnapshot::from_active(workspace.active_project()),
        &output,
        &ProjectGifExportOptions::default(),
        &gif_from_screen_gif::NeverCancel,
        &mut NoopProjectExportProgress,
    )
    .unwrap();
    gif_from_screen_media::decode_gif(
        std::fs::File::open(output).unwrap(),
        &gif_from_screen_media::GifDecodeOptions::default(),
    )
    .unwrap()
}

#[test]
fn ordered_image_effects_expand_all_frames_and_survive_undo_reopen_and_gif_export() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory);
    let raw = workspace
        .manifest()
        .timeline
        .frames
        .iter()
        .map(|frame| (frame.capture_metadata.clone(), frame.capture_clock))
        .collect::<Vec<_>>();
    add_complete_chain(&mut workspace);
    assert_eq!(
        workspace.selection.selected(),
        &BTreeSet::from([frame_id(1)])
    );
    assert_eq!(workspace.manifest().schema_version, 4);
    for frame in &workspace.manifest().timeline.frames {
        assert_eq!(image(&workspace, frame.id).size(), size(9, 5));
        assert_eq!(
            frame
                .render_steps
                .iter()
                .filter(|step| matches!(
                    step,
                    FrameRenderStep::ImageShadow { .. } | FrameRenderStep::ImageBorder { .. }
                ))
                .count(),
            2
        );
    }
    let tracks = &workspace.manifest().timeline.overlay_tracks;
    assert!(tracks[0].frame_cells.as_ref().unwrap()[0].stage.is_some());
    assert_eq!(tracks[1].frame_cells.as_ref().unwrap()[0].stage, None);
    let expected = image(&workspace, frame_id(1));
    assert_chain_pixels(&expected);
    assert!(workspace.undo().unwrap());
    assert_eq!(
        pixel(&image(&workspace, frame_id(1)), 0, 3),
        &[0, 0, 255, 255]
    );
    assert!(workspace.undo().unwrap());
    assert_eq!(workspace.manifest().canvas.size, size(7, 4));
    for frame in &workspace.manifest().timeline.frames {
        assert_eq!(image(&workspace, frame.id).size(), size(7, 4));
        assert!(
            !frame
                .render_steps
                .iter()
                .any(|step| matches!(step, FrameRenderStep::ImageBorder { .. }))
        );
    }
    assert!(workspace.redo().unwrap());
    assert_eq!(workspace.manifest().canvas.size, size(9, 5));
    assert!(workspace.redo().unwrap());
    assert_eq!(image(&workspace, frame_id(1)), expected);
    assert_eq!(
        workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .map(|frame| (frame.capture_metadata.clone(), frame.capture_clock))
            .collect::<Vec<_>>(),
        raw
    );
    let revision = workspace.manifest().revision;
    // Deliberately do not checkpoint: reopen must recover the schema4 steps
    // and owner-stage anchors from the actual project journal.
    drop(workspace);
    let workspace = EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(workspace.manifest().revision, revision);
    assert_chain_pixels(&image(&workspace, frame_id(1)));
    let decoded = export(&workspace, "image-effects.gif");
    assert_eq!((decoded.width(), decoded.height()), (9, 5));
    assert_eq!(
        decoded
            .frames()
            .iter()
            .map(gif_from_screen_media::DecodedFrame::duration_us)
            .sum::<u64>(),
        400_000
    );
    assert_eq!(decoded.frames()[0].rgba(), expected.pixels());
    assert_eq!(
        pixel_hash(decoded.frames()[0].rgba()),
        pixel_hash(expected.pixels())
    );
}

#[test]
fn inward_border_is_selected_only_and_shadow_replacement_is_atomic_and_undoable() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory);
    let others = workspace.manifest().timeline.frames[1..].to_vec();
    let other_image = image(&workspace, frame_id(2));
    workspace
        .add_image_effect(ComposedImageEffect::Border(ImageBorderStyle {
            color: BLUE,
            ..ImageBorderStyle::default()
        }))
        .unwrap();
    assert_eq!(workspace.manifest().canvas.size, size(6, 4));
    assert_eq!(workspace.manifest().timeline.frames[1..], others);
    assert_eq!(image(&workspace, frame_id(2)), other_image);
    assert_eq!(
        pixel(&image(&workspace, frame_id(1)), 0, 0),
        &[0, 0, 255, 255]
    );
    assert!(workspace.undo().unwrap());
    add_shape(&mut workspace, "A", 4, 1, GREEN);
    workspace
        .add_image_effect(ComposedImageEffect::Shadow(shadow()))
        .unwrap();
    let before = workspace.manifest().clone();
    let before_image = image(&workspace, frame_id(1));
    workspace
        .replace_image_effect(
            0,
            ComposedImageEffect::Shadow(ImageShadowStyle {
                opacity_basis_points: 0,
                ..shadow()
            }),
        )
        .unwrap();
    assert_eq!(workspace.manifest().canvas.size, size(7, 4));
    assert_eq!(
        pixel(&image(&workspace, frame_id(1)), 5, 1),
        &[255, 255, 255, 255]
    );
    assert!(workspace.manifest().timeline.frames.iter().all(|frame| frame.render_steps.iter().any(|step| matches!(step, FrameRenderStep::ImageShadow { style } if style.opacity_basis_points == 0))));
    assert!(workspace.undo().unwrap());
    assert_eq!(workspace.manifest().timeline, before.timeline);
    assert_eq!(image(&workspace, frame_id(1)), before_image);
    let before = workspace.manifest().clone();
    assert!(
        workspace
            .replace_image_effect(
                0,
                ComposedImageEffect::Shadow(ImageShadowStyle {
                    depth_hundredths: 10_001,
                    ..shadow()
                })
            )
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
}

#[test]
fn copied_and_yoyo_frame_owners_keep_image_steps_and_save_as_keeps_pixels_and_assets() {
    let directory = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory);
    add_complete_chain(&mut workspace);
    let original = workspace.manifest().timeline.frames[0].clone();
    let expected = image(&workspace, original.id);
    assert_eq!(workspace.copy_selection().unwrap(), 1);
    assert_eq!(workspace.paste_after_current().unwrap(), 1);
    let pasted = workspace.manifest().timeline.frames[1].clone();
    assert_ne!(pasted.id, original.id);
    assert_eq!(pasted.render_steps, original.render_steps);
    assert_eq!(pasted.capture_metadata, original.capture_metadata);
    assert_eq!(pasted.capture_clock, original.capture_clock);
    assert_eq!(image(&workspace, pasted.id), expected);
    workspace.select_only(original.id).unwrap();
    workspace.toggle_selection(pasted.id).unwrap();
    workspace.yoyo(YoyoScope::Selection, true).unwrap();
    assert_eq!(workspace.manifest().timeline.frames.len(), 7);
    for frame in &workspace.manifest().timeline.frames[..4] {
        assert_eq!(frame.render_steps, original.render_steps);
        assert_eq!(frame.capture_metadata, original.capture_metadata);
        assert_eq!(frame.capture_clock, original.capture_clock);
        assert_chain_pixels(&image(&workspace, frame.id));
    }
    assert!(workspace.undo().unwrap());
    assert_eq!(workspace.manifest().timeline.frames.len(), 5);
    assert!(workspace.redo().unwrap());
    let target = destination.path().join("image-effects-copy.gfsproj");
    save_project_copy(
        &ProjectCopySnapshot::from_active(workspace.active_project()),
        &SaveProjectCopyOptions {
            target: target.clone(),
            project_id: ProjectId::from_u128(901),
            created_at: UnixTimeMs::new(2),
        },
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let copy = EditorWorkspace::open(&target, LockPolicy::FailIfPresent, 32).unwrap();
    assert_ne!(copy.manifest().project_id, workspace.manifest().project_id);
    assert_eq!(copy.manifest().schema_version, 4);
    assert_eq!(copy.manifest().assets, workspace.manifest().assets);
    assert_eq!(
        copy.manifest().timeline.overlay_tracks,
        workspace.manifest().timeline.overlay_tracks
    );
    for (copied, source) in copy
        .manifest()
        .timeline
        .frames
        .iter()
        .zip(&workspace.manifest().timeline.frames)
    {
        assert_eq!(copied.render_steps, source.render_steps);
        assert_eq!(copied.capture_metadata, source.capture_metadata);
        assert_eq!(image(&copy, copied.id), image(&workspace, source.id));
    }
    let source_export = export(&workspace, "source.gif");
    let copy_export = export(&copy, "copy.gif");
    assert_eq!(source_export, copy_export);
    assert_eq!((copy_export.width(), copy_export.height()), (9, 5));
    assert_eq!(
        pixel_hash(copy_export.frames()[0].rgba()),
        pixel_hash(expected.pixels())
    );
}
