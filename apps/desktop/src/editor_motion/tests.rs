use super::*;
use gif_from_screen_application::{
    IncrementalRecordingProject, IncrementalRecordingProjectOptions,
};
use gif_from_screen_domain::{
    BlendMode, OverlayContent, PhysicalPoint, PhysicalPx, ProjectId, ProjectManifest, Rgba,
    ShapeKind, UnixTimeMs,
};
use gif_from_screen_gif::RgbaFrame;
use gif_from_screen_project::LockPolicy;

fn workspace(root: &std::path::Path) -> EditorWorkspace {
    let mut writer = IncrementalRecordingProject::create(
        root,
        PhysicalSize::new(2, 1).unwrap(),
        IncrementalRecordingProjectOptions {
            project_id: ProjectId::from_u128(100),
            app_version: "motion-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            source_label: None,
        },
    )
    .unwrap();
    for (index, pixels) in [
        vec![255, 0, 0, 0, 0, 255, 0, 255],
        vec![0, 0, 255, 255, 255, 255, 0, 255],
        vec![255, 255, 255, 255, 0, 0, 255, 255],
    ]
    .into_iter()
    .enumerate()
    {
        writer
            .append_frame(
                FrameId::from_u128(index as u128 + 1),
                &RgbaFrame::new(2, 1, pixels, 10_000).unwrap(),
            )
            .unwrap();
    }
    let mut workspace = EditorWorkspace::from_active(writer.finish().unwrap(), 32).unwrap();
    workspace.select_first().unwrap();
    workspace
}

fn rect() -> PhysicalRect {
    PhysicalRect {
        origin: PhysicalPoint {
            x: PhysicalPx::new(1),
            y: PhysicalPx::ZERO,
        },
        size: PhysicalSize::new(1, 1).unwrap(),
    }
}

fn pixels(workspace: &EditorWorkspace, id: u128) -> Vec<u8> {
    render(
        workspace.active_project(),
        FrameId::from_u128(id),
        workspace.manifest().canvas.size,
        &AtomicBool::new(false),
    )
    .unwrap()
    .into_pixels()
}

fn equal_except_revision(actual: &ProjectManifest, expected: &ProjectManifest) {
    let mut expected = expected.clone();
    expected.revision = actual.revision;
    assert_eq!(*actual, expected);
}

fn add_overlay(workspace: &mut EditorWorkspace) {
    workspace.select_all();
    workspace
        .add_overlay_for_selection(
            "Before baking".to_owned(),
            OverlayContent::Shape {
                kind: ShapeKind::Rectangle,
                bounds: rect(),
                stroke_width: 0,
                stroke: Rgba::TRANSPARENT,
                fill: Some(Rgba {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 128,
                }),
            },
            4,
            255,
            BlendMode::Normal,
        )
        .unwrap();
}

#[test]
fn cinemagraph_freezes_transparent_pixels_and_does_not_touch_selection_gaps() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    add_overlay(&mut workspace);
    workspace.select_only(FrameId::from_u128(3)).unwrap();
    workspace.toggle_selection(FrameId::from_u128(1)).unwrap();
    let before = workspace.manifest().clone();
    let frozen = pixels(&workspace, 1);
    let gap = pixels(&workspace, 2);
    let animated = pixels(&workspace, 3);
    let anchor = workspace.project_edit_anchor();
    workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::Cinemagraph {
                region: rect(),
                invert: false,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(pixels(&workspace, 1), frozen);
    assert_eq!(pixels(&workspace, 2), gap);
    assert_eq!(
        pixels(&workspace, 3),
        [frozen[..4].to_vec(), animated[4..].to_vec()].concat()
    );
    assert_eq!(
        pixels(&workspace, 3)[3],
        0,
        "transparent frozen pixels must erase moving pixels"
    );
    let overlay = &workspace.manifest().timeline.overlay_tracks[0].items[0];
    assert_eq!(overlay.span.start.get(), 10_000);
    assert_eq!(overlay.span.duration.get(), 10_000);
    assert!(workspace.undo().unwrap());
    equal_except_revision(workspace.manifest(), &before);
    assert!(workspace.redo().unwrap());
    let saved = pixels(&workspace, 3);
    let root = workspace.project_root().to_owned();
    drop(workspace);
    let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(pixels(&reopened, 3), saved);
}

#[test]
fn inverted_cinemagraph_only_freezes_inside_the_rectangle() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    workspace.select_all();
    let frozen = pixels(&workspace, 1);
    let animated = pixels(&workspace, 3);
    let anchor = workspace.project_edit_anchor();
    workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::Cinemagraph {
                region: rect(),
                invert: true,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(
        pixels(&workspace, 3),
        [animated[..4].to_vec(), frozen[4..].to_vec()].concat()
    );
}

#[test]
fn smooth_loop_appends_exact_duration_and_first_endpoint_without_overlay_double_composition() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    add_overlay(&mut workspace);
    let before = workspace.manifest().clone();
    let first = pixels(&workspace, 1);
    let anchor = workspace.project_edit_anchor();
    let count = workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::SmoothLoop {
                frames: 3,
                duration_us: 33_334,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(count, 3);
    assert_eq!(
        &workspace.manifest().timeline.frames[..3],
        &before.timeline.frames
    );
    assert_eq!(
        workspace.manifest().timeline.overlay_tracks,
        before.timeline.overlay_tracks
    );
    let appended = &workspace.manifest().timeline.frames[3..];
    assert_eq!(
        appended
            .iter()
            .map(|frame| frame.duration.get())
            .collect::<Vec<_>>(),
        [11_111, 11_111, 11_112]
    );
    let last_id = appended[2].id;
    let last = render(
        workspace.active_project(),
        last_id,
        PhysicalSize::new(2, 1).unwrap(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(last.pixels(), first);
    let after = workspace.manifest().clone();
    assert!(workspace.undo().unwrap());
    equal_except_revision(workspace.manifest(), &before);
    assert!(workspace.redo().unwrap());
    equal_except_revision(workspace.manifest(), &after);
    let root = workspace.project_root().to_owned();
    drop(workspace);
    let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(reopened.manifest().timeline.frames.len(), 6);
    assert_eq!(
        render(
            reopened.active_project(),
            last_id,
            PhysicalSize::new(2, 1).unwrap(),
            &AtomicBool::new(false)
        )
        .unwrap()
        .pixels(),
        first
    );
}

#[test]
fn motion_rejects_stale_selection_invalid_rectangles_canvas_mismatch_and_limits() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    let stale = workspace.project_edit_anchor();
    workspace.select_last().unwrap();
    let before = workspace.manifest().clone();
    assert!(
        workspace
            .apply_motion_edit(
                &stale,
                MotionOperation::SmoothLoop {
                    frames: 3,
                    duration_us: 30_000
                },
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    equal_except_revision(workspace.manifest(), &before);
    for operation in [
        MotionOperation::SmoothLoop {
            frames: 121,
            duration_us: 30_000,
        },
        MotionOperation::SmoothLoop {
            frames: 3,
            duration_us: 2,
        },
        MotionOperation::Cinemagraph {
            region: PhysicalRect {
                origin: PhysicalPoint::default(),
                size: PhysicalSize::new(3, 1).unwrap(),
            },
            invert: false,
        },
        MotionOperation::Cinemagraph {
            region: PhysicalRect {
                origin: PhysicalPoint::default(),
                size: PhysicalSize {
                    width: PhysicalPx::ZERO,
                    height: PhysicalPx::new(1),
                },
            },
            invert: false,
        },
    ] {
        let anchor = workspace.project_edit_anchor();
        assert!(
            workspace
                .apply_motion_edit(&anchor, operation, &AtomicBool::new(false), |_| {})
                .is_err()
        );
        equal_except_revision(workspace.manifest(), &before);
    }
    assert!(validate_budget(PhysicalSize::new(4096, 4096).unwrap(), 5).is_err());
    workspace
        .set_selection_output_size(PhysicalSize::new(1, 1).unwrap())
        .unwrap();
    let mismatch = workspace.manifest().clone();
    let anchor = workspace.project_edit_anchor();
    assert!(
        workspace
            .apply_motion_edit(
                &anchor,
                MotionOperation::SmoothLoop {
                    frames: 3,
                    duration_us: 30_000
                },
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    equal_except_revision(workspace.manifest(), &mismatch);
}

#[test]
fn cancelling_after_pixel_preparation_preserves_timeline_and_undo_history() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    let before = workspace.manifest().clone();
    let anchor = workspace.project_edit_anchor();
    let cancel = AtomicBool::new(false);
    let error = workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::SmoothLoop {
                frames: 8,
                duration_us: 400_000,
            },
            &cancel,
            |update| {
                if update.completed == 1 {
                    cancel.store(true, Ordering::Release);
                }
            },
        )
        .unwrap_err();
    assert!(error.contains("cancelled"));
    equal_except_revision(workspace.manifest(), &before);
    assert!(!workspace.can_undo());
}

#[test]
fn removing_selected_overlay_intervals_retains_every_gap_exactly() {
    let span = |start, duration| TimelineSpan {
        start: TimeUs::new(start),
        duration: DurationUs::new(duration).unwrap(),
    };
    assert_eq!(
        subtract_spans(span(0, 100), &[span(10, 20), span(50, 10), span(80, 20)]).unwrap(),
        [span(0, 10), span(30, 20), span(60, 20)]
    );
}

#[test]
fn smooth_loop_duration_overflow_is_rejected_before_storing_any_new_pixels() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    workspace.select_last().unwrap();
    workspace
        .override_selection_duration(DurationUs::new(u64::MAX - 20_000).unwrap())
        .unwrap();
    let before = workspace.manifest().clone();
    let before_files = std::fs::read_dir(workspace.active_project().assets().directory())
        .unwrap()
        .count();
    let anchor = workspace.project_edit_anchor();
    let error = workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::SmoothLoop {
                frames: 3,
                duration_us: 30_000,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
    assert!(error.contains("overflow"));
    equal_except_revision(workspace.manifest(), &before);
    assert_eq!(
        std::fs::read_dir(workspace.active_project().assets().directory())
            .unwrap()
            .count(),
        before_files
    );
}
