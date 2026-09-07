//! First-frame reference and selected targets through the real ink/PM pipeline.

use super::*;
use crate::cinemagraph_draft::CinemagraphDraft;
use gif_from_screen_render::{InkAttributes, InkPoint, InkSample, InkTip};

fn draw_rect(workspace: &EditorWorkspace, x: f64) -> crate::cinemagraph_draft::CinemagraphRequest {
    let mut draft = CinemagraphDraft::default();
    draft.begin(workspace).unwrap();
    draft.pen = InkAttributes {
        width: 16.0,
        height: 16.0,
        tip: InkTip::Rectangle,
        ..InkAttributes::default()
    };
    let sample = InkSample {
        position: InkPoint { x, y: 8.0 },
        pressure: 0.5,
    };
    draft.pointer_down(sample).unwrap();
    draft.pointer_up(sample).unwrap();
    draft.request(workspace).unwrap()
}

fn image(workspace: &EditorWorkspace, frame: u128) -> RgbaSurface {
    render(
        workspace.active_project(),
        FrameId::from_u128(frame),
        workspace.manifest().canvas.size,
        &AtomicBool::new(false),
    )
    .unwrap()
}

#[test]
fn real_authoring_uses_first_frame_not_current_and_keeps_unselected_frames() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("ink.gfsproj");
    let mut workspace = workspace(&root);
    workspace
        .set_selection_output_size(PhysicalSize::new(32, 16).unwrap())
        .unwrap();
    workspace.select_only(FrameId::from_u128(2)).unwrap();
    let before = workspace.manifest().clone();
    let first = image(&workspace, 1);
    let last = image(&workspace, 3);
    let request = draw_rect(&workspace, 8.0);
    assert_eq!(request.reference_frame, FrameId::from_u128(1));
    let result = workspace
        .apply_motion_edit(
            &request.anchor.clone(),
            MotionOperation::Cinemagraph(Box::new(request)),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(result, MotionOutcome::Edited(1));
    let output = image(&workspace, 2);
    for row in output.pixels().as_chunks::<128>().0 {
        assert!(
            row[..16 * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [0, 0, 255, 255])
        );
        assert!(
            row[16 * 4..]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [0, 255, 0, 255]),
            "outside ink uses the first frame's green, not the current frame's yellow"
        );
    }
    assert_eq!(image(&workspace, 1), first);
    assert_eq!(image(&workspace, 3), last);
    assert_eq!(
        workspace.manifest().timeline.frames[0],
        before.timeline.frames[0]
    );
    assert_eq!(
        workspace.manifest().timeline.frames[2],
        before.timeline.frames[2]
    );
    let asset_count = before.assets.len();
    assert_eq!(workspace.manifest().assets.len(), asset_count + 1);
    let after = workspace.manifest().clone();
    workspace.undo().unwrap();
    equal_except_revision(workspace.manifest(), &before);
    workspace.redo().unwrap();
    equal_except_revision(workspace.manifest(), &after);
    drop(workspace);
    let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(image(&reopened, 2), output);
    let gif = directory.path().join("ink.gif");
    gif_from_screen_application::export_project_snapshot_to_gif(
        &gif_from_screen_application::ProjectExportSnapshot::from_active(reopened.active_project()),
        &gif,
        &gif_from_screen_application::ProjectGifExportOptions::default(),
        &gif_from_screen_gif::NeverCancel,
        &mut gif_from_screen_application::NoopProjectExportProgress,
    )
    .unwrap();
    let decoded = gif_from_screen_media::decode_gif(
        std::fs::File::open(gif).unwrap(),
        &gif_from_screen_media::GifDecodeOptions::default(),
    )
    .unwrap();
    assert_eq!((decoded.width(), decoded.height()), (32, 16));
    assert_eq!(decoded.frames()[1].rgba(), output.pixels());
}

#[test]
fn transparent_first_frame_reference_does_not_erase_selected_pixels() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("transparent.gfsproj"));
    workspace
        .set_selection_output_size(PhysicalSize::new(32, 16).unwrap())
        .unwrap();
    workspace.select_only(FrameId::from_u128(3)).unwrap();
    let before = image(&workspace, 3);
    let request = draw_rect(&workspace, 24.0);
    workspace
        .apply_motion_edit(
            &request.anchor.clone(),
            MotionOperation::Cinemagraph(Box::new(request)),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(
        image(&workspace, 3),
        before,
        "outside ink, transparent source-over is not RGBA overwrite"
    );
}

#[test]
fn stale_cancelled_and_invalid_ink_do_not_publish_a_snapshot_or_edit_the_project() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("cancel.gfsproj"));
    workspace
        .set_selection_output_size(PhysicalSize::new(32, 16).unwrap())
        .unwrap();
    let request = draw_rect(&workspace, 8.0);
    let before = workspace.manifest().clone();
    let count = std::fs::read_dir(workspace.active_project().assets().directory())
        .unwrap()
        .count();
    let stop = AtomicBool::new(false);
    assert!(
        workspace
            .apply_motion_edit(
                &request.anchor,
                MotionOperation::Cinemagraph(Box::new(request.clone())),
                &stop,
                |_| stop.store(true, Ordering::Release)
            )
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
    let mut invalid = request.clone();
    invalid.strokes[0].samples[0].pressure = f32::NAN;
    assert!(
        workspace
            .apply_motion_edit(
                &invalid.anchor.clone(),
                MotionOperation::Cinemagraph(Box::new(invalid)),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
    workspace.select_only(FrameId::from_u128(2)).unwrap();
    assert!(
        workspace
            .apply_motion_edit(
                &request.anchor,
                MotionOperation::Cinemagraph(Box::new(request.clone())),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
    assert_eq!(
        std::fs::read_dir(workspace.active_project().assets().directory())
            .unwrap()
            .count(),
        count
    );
}
