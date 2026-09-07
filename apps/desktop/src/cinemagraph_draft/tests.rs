use super::*;
use gif_from_screen_application::{BlankAnimationProjectOptions, create_blank_animation_project};
use gif_from_screen_domain::{DurationUs, ProjectId, Rgba, UnixTimeMs};

fn workspace(root: &std::path::Path) -> EditorWorkspace {
    let project = create_blank_animation_project(
        root,
        BlankAnimationProjectOptions {
            project_id: ProjectId::from_u128(81),
            frame_id: FrameId::from_u128(1),
            app_version: "cinemagraph-draft-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            canvas: PhysicalSize::new(100, 100).unwrap(),
            background: Rgba {
                red: 20,
                green: 30,
                blue: 40,
                alpha: 128,
            },
            frame_duration: DurationUs::new(100_000).unwrap(),
            frame_limit_bytes: 64 * 1024,
        },
    )
    .unwrap();
    let mut workspace = EditorWorkspace::from_active(project, 32).unwrap();
    workspace.select_first().unwrap();
    workspace
}

fn sample(x: f64, y: f64) -> InkSample {
    InkSample {
        position: InkPoint { x, y },
        pressure: 0.5,
    }
}

fn draft(workspace: &EditorWorkspace) -> CinemagraphDraft {
    let mut draft = CinemagraphDraft::default();
    draft.begin(workspace).unwrap();
    draft.pen = InkAttributes {
        width: 2.0,
        height: 2.0,
        tip: InkTip::Rectangle,
        ..InkAttributes::default()
    };
    draft.eraser = InkAttributes {
        width: 4.0,
        height: 4.0,
        tip: InkTip::Rectangle,
        ignore_pressure: true,
        ..InkAttributes::default()
    };
    draft
}

fn line(draft: &mut CinemagraphDraft, start: InkSample, end: InkSample) {
    draft.tool = CinemagraphTool::Pen;
    draft.pointer_down(start).unwrap();
    draft.pointer_up(end).unwrap();
}

fn assert_close(a: f64, b: f64) {
    assert!((a - b).abs() < 1.0e-7, "{a} != {b}");
}

#[test]
fn dot_and_multiple_strokes_keep_independent_attributes_pressure_and_ids() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let before = workspace.manifest().clone();
    let mut draft = draft(&workspace);
    let initial_generation = draft.generation();
    let dot = InkSample {
        pressure: 0.25,
        ..sample(20.125, 30.75)
    };
    line(&mut draft, dot, dot);
    let first_id = draft.strokes()[0].id;
    draft.pen.width = 100.0;
    draft.pen.height = 1.0;
    draft.pen.tip = InkTip::Ellipse;
    draft.pen.fit_to_curve = true;
    line(&mut draft, sample(40.0, 50.0), sample(60.0, 70.0));
    assert_eq!(draft.strokes().len(), 2);
    assert_eq!(draft.strokes()[0].stroke.samples, [dot]);
    assert_close(draft.strokes()[0].stroke.attributes.width, 2.0);
    assert_close(draft.strokes()[1].stroke.attributes.width, 100.0);
    assert!(draft.strokes()[1].stroke.attributes.fit_to_curve);
    assert!(draft.strokes()[1].id > first_id);
    assert!(draft.generation() > initial_generation);
    assert_eq!(draft.tool, CinemagraphTool::Pen);
    assert_eq!(workspace.manifest(), &before);
}

#[test]
fn first_frame_reference_and_selected_target_remain_separate() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    workspace.copy_selection().unwrap();
    workspace.paste_after_current().unwrap();
    let selected = workspace.select_last().unwrap();
    assert_ne!(selected, FrameId::from_u128(1));
    let mut draft = draft(&workspace);
    line(&mut draft, sample(10.0, 10.0), sample(20.0, 20.0));
    let request = draft.request(&workspace).unwrap();
    assert_eq!(request.reference_frame, FrameId::from_u128(1));
    assert_eq!(request.reference_size, PhysicalSize::new(100, 100).unwrap());
    assert_eq!(workspace.selection().current(), Some(selected));
    assert!(request.anchor.matches(&workspace));
    assert_eq!(request.strokes.len(), 1);
}

#[test]
fn stale_selection_rolls_back_gesture_but_preserves_completed_draft_and_requires_restart() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(&mut draft, sample(5.0, 10.0), sample(30.0, 10.0));
    let completed = draft.strokes().to_vec();
    draft.pointer_down(sample(40.0, 40.0)).unwrap();
    workspace.clear_selection();
    assert!(draft.request(&workspace).is_err());
    draft.reconcile(&workspace);
    assert!(draft.is_active());
    assert!(draft.is_stale());
    assert!(draft.stale_reason().unwrap().contains("selection changed"));
    assert!(!draft.gesture_active());
    assert_eq!(draft.strokes(), completed);
    assert!(draft.pointer_down(sample(10.0, 10.0)).is_err());
    workspace.select_first().unwrap();
    draft.reconcile(&workspace);
    assert!(draft.is_stale());
    draft.begin(&workspace).unwrap();
    assert!(!draft.is_stale());
    assert!(draft.strokes().is_empty());
}

#[test]
fn changed_revision_or_same_id_different_root_cannot_rebind_draft() {
    let directory = tempfile::tempdir().unwrap();
    let mut first = workspace(&directory.path().join("first"));
    let second = workspace(&directory.path().join("second"));
    let mut draft = draft(&first);
    line(&mut draft, sample(10.0, 10.0), sample(20.0, 20.0));
    assert!(draft.request(&second).is_err());
    first
        .override_selection_duration(DurationUs::new(200_000).unwrap())
        .unwrap();
    assert!(draft.request(&first).is_err());
    draft.reconcile(&first);
    assert!(draft.is_stale());
    assert_eq!(draft.strokes().len(), 1);
    first.undo().unwrap();
    assert!(draft.request(&first).is_err());
}

#[test]
fn continuous_point_eraser_splits_between_distant_samples_and_does_not_repeat_erode() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(&mut draft, sample(10.0, 50.0), sample(90.0, 50.0));
    let id = draft.strokes()[0].id;
    draft.select_all().unwrap();
    draft.tool = CinemagraphTool::PointEraser;
    draft.pointer_down(sample(50.0, 10.0)).unwrap();
    draft.pointer_move(sample(50.0, 90.0)).unwrap();
    let split = draft.strokes().to_vec();
    assert_eq!(split.len(), 2);
    assert_eq!(split[0].id, id);
    assert_ne!(split[1].id, id);
    assert_eq!(
        draft.selected_ids(),
        &BTreeSet::from([split[0].id, split[1].id])
    );
    assert_close(split[0].stroke.samples.last().unwrap().position.x, 47.0);
    assert_close(split[1].stroke.samples[0].position.x, 53.0);
    draft.pointer_up(sample(50.0, 90.0)).unwrap();
    assert_eq!(draft.strokes(), split);
    assert_eq!(draft.strokes()[0].stroke.samples[0], sample(10.0, 50.0));
    assert_eq!(
        *draft.strokes()[1].stroke.samples.last().unwrap(),
        sample(90.0, 50.0)
    );
}

#[test]
fn pressure_is_interpolated_at_split_and_tip_attributes_are_retained() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(
        &mut draft,
        InkSample {
            pressure: 0.25,
            ..sample(10.0, 50.0)
        },
        InkSample {
            pressure: 0.75,
            ..sample(90.0, 50.0)
        },
    );
    let attributes = draft.strokes()[0].stroke.attributes;
    draft.tool = CinemagraphTool::PointEraser;
    draft.pointer_down(sample(50.0, 50.0)).unwrap();
    draft.pointer_up(sample(50.0, 50.0)).unwrap();
    assert_eq!(draft.strokes().len(), 2);
    for stroke in draft.strokes() {
        assert_eq!(stroke.stroke.attributes, attributes);
        for cut in &stroke.stroke.samples {
            let expected = 0.25 + (cut.position.x - 10.0) / 80.0 * 0.5;
            assert!((f64::from(cut.pressure) - expected).abs() < 1.0e-7);
        }
    }
}

#[test]
fn stroke_eraser_uses_swept_area_and_escape_restores_deleted_strokes() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(&mut draft, sample(10.0, 50.0), sample(90.0, 50.0));
    line(&mut draft, sample(10.0, 5.0), sample(20.0, 5.0));
    let original = draft.strokes().to_vec();
    draft.tool = CinemagraphTool::StrokeEraser;
    draft.pointer_down(sample(50.0, 20.0)).unwrap();
    draft.pointer_move(sample(50.0, 80.0)).unwrap();
    assert_eq!(draft.strokes().len(), 1);
    assert_eq!(draft.strokes()[0], original[1]);
    let generation = draft.generation();
    draft.cancel_gesture();
    assert_eq!(draft.strokes(), original);
    assert!(draft.generation() > generation);
}

#[test]
fn ellipse_point_erase_hits_tip_edges_without_centerline_sample_proximity() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    draft.pen.width = 20.0;
    draft.pen.height = 10.0;
    draft.pen.tip = InkTip::Ellipse;
    line(&mut draft, sample(10.0, 50.0), sample(90.0, 50.0));
    draft.tool = CinemagraphTool::PointEraser;
    draft.eraser.tip = InkTip::Ellipse;
    draft.pointer_down(sample(50.0, 55.0)).unwrap();
    draft.pointer_up(sample(50.0, 55.0)).unwrap();
    assert_eq!(draft.strokes().len(), 2);
    assert!(draft.strokes()[0].stroke.samples.last().unwrap().position.x < 50.0);
    assert!(draft.strokes()[1].stroke.samples[0].position.x > 50.0);
}

#[test]
fn click_select_hits_geometry_not_its_bounding_box_and_marquee_selects_multiple() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(&mut draft, sample(10.0, 10.0), sample(90.0, 90.0));
    line(&mut draft, sample(50.0, 10.0), sample(50.0, 90.0));
    let last = draft.strokes()[1].id;
    let generation = draft.generation();
    draft.tool = CinemagraphTool::Select;
    draft.pointer_down(sample(10.0, 90.0)).unwrap();
    draft.pointer_up(sample(10.0, 90.0)).unwrap();
    assert!(draft.selected_ids().is_empty());
    draft.pointer_down(sample(50.0, 50.0)).unwrap();
    draft.pointer_up(sample(50.0, 50.0)).unwrap();
    assert_eq!(draft.selected_ids(), &BTreeSet::from([last]));
    draft.pointer_down(sample(40.0, 40.0)).unwrap();
    draft.pointer_up(sample(60.0, 60.0)).unwrap();
    assert_eq!(draft.selected_ids().len(), 2);
    assert_eq!(draft.generation(), generation);
}

#[test]
fn selection_transforms_use_original_points_without_scaling_tips_or_pressure() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(
        &mut draft,
        InkSample {
            pressure: 0.2,
            ..sample(10.0, 20.0)
        },
        InkSample {
            pressure: 0.8,
            ..sample(30.0, 40.0)
        },
    );
    let original = draft.strokes().to_vec();
    draft.select_all().unwrap();
    draft.begin_selection_transform().unwrap();
    draft
        .update_selection_transform(InkPoint { x: 2.0, y: 0.5 }, InkPoint { x: 3.0, y: -2.0 })
        .unwrap();
    let moved = draft.strokes().to_vec();
    draft
        .update_selection_transform(InkPoint { x: 2.0, y: 0.5 }, InkPoint { x: 3.0, y: -2.0 })
        .unwrap();
    assert_eq!(draft.strokes(), moved);
    assert_eq!(moved[0].stroke.attributes, original[0].stroke.attributes);
    assert_close(
        f64::from(moved[0].stroke.samples[0].pressure),
        f64::from(original[0].stroke.samples[0].pressure),
    );
    assert_eq!(
        moved[0].stroke.samples[0].position,
        InkPoint { x: 23.0, y: 8.0 }
    );
    assert_eq!(
        moved[0].stroke.samples[1].position,
        InkPoint { x: 63.0, y: 18.0 }
    );
    assert!(draft.request(&workspace).is_err());
    draft.cancel_gesture();
    assert_eq!(draft.strokes(), original);
    draft.begin_selection_transform().unwrap();
    draft
        .update_selection_transform(InkPoint { x: 1.0, y: 1.0 }, InkPoint { x: 1.0, y: 2.0 })
        .unwrap();
    draft.finish_selection_transform().unwrap();
    assert!(draft.request(&workspace).is_ok());
    assert_close(draft.selection_bounds().unwrap().unwrap().min.x, 10.45);
}

#[test]
fn invalid_transform_and_nonfinite_drag_roll_back_without_partial_edits() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(&mut draft, sample(10.0, 20.0), sample(30.0, 40.0));
    let original = draft.strokes().to_vec();
    draft.select_all().unwrap();
    draft.begin_selection_transform().unwrap();
    assert!(
        draft
            .update_selection_transform(InkPoint { x: 0.0, y: 1.0 }, InkPoint::default())
            .is_err()
    );
    assert_eq!(draft.strokes(), original);
    draft.pointer_down(sample(20.0, 30.0)).unwrap();
    assert!(draft.pointer_move(sample(f64::NAN, 20.0)).is_err());
    assert!(!draft.gesture_active());
    assert_eq!(draft.strokes(), original);
    assert!(draft.pointer_down(sample(-1.0, 20.0)).is_err());
    assert!(
        draft
            .pointer_down(InkSample {
                pressure: 2.0,
                ..sample(20.0, 20.0)
            })
            .is_err()
    );
}

#[test]
fn delete_clear_and_cancel_do_not_reuse_stroke_ids_or_change_project_history() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let before = workspace.manifest().clone();
    let mut draft = draft(&workspace);
    draft.pointer_down(sample(10.0, 10.0)).unwrap();
    let cancelled_id = draft.strokes()[0].id;
    draft.cancel_gesture();
    line(&mut draft, sample(20.0, 20.0), sample(30.0, 30.0));
    let completed_id = draft.strokes()[0].id;
    assert!(completed_id > cancelled_id);
    draft.select_all().unwrap();
    draft.delete_selected().unwrap();
    assert!(draft.strokes().is_empty());
    assert!(draft.request(&workspace).is_err());
    line(&mut draft, sample(20.0, 20.0), sample(30.0, 30.0));
    assert!(draft.strokes()[0].id > completed_id);
    draft.clear().unwrap();
    draft.close();
    assert!(!draft.is_active());
    assert_eq!(workspace.manifest(), &before);
}

#[test]
fn event_and_stroke_budgets_fail_explicitly_and_restore_whole_gesture() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    draft.pointer_down(sample(10.0, 10.0)).unwrap();
    let mut rejected = false;
    for index in 0..MAX_GESTURE_EVENTS {
        let point = sample(
            10.0 + f64::from(u32::try_from(index).unwrap()) / 100.0,
            10.0,
        );
        if draft.pointer_move(point).is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected);
    assert!(draft.strokes().is_empty());
    assert!(!draft.gesture_active());
    for _ in 0..MAX_CINEMAGRAPH_STROKES {
        line(&mut draft, sample(10.0, 10.0), sample(10.0, 10.0));
    }
    assert!(
        draft
            .pointer_down(sample(20.0, 20.0))
            .unwrap_err()
            .contains("256")
    );
    assert_eq!(draft.strokes().len(), MAX_CINEMAGRAPH_STROKES);
}

#[test]
fn curve_fitted_stroke_is_only_materialized_when_an_erase_actually_hits() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    draft.pen.fit_to_curve = true;
    draft.pointer_down(sample(10.0, 50.0)).unwrap();
    draft.pointer_move(sample(30.0, 50.0)).unwrap();
    draft.pointer_move(sample(60.0, 50.0)).unwrap();
    draft.pointer_up(sample(90.0, 50.0)).unwrap();
    let original = draft.strokes().to_vec();
    draft.tool = CinemagraphTool::PointEraser;
    draft.pointer_down(sample(50.0, 10.0)).unwrap();
    draft.pointer_up(sample(50.0, 10.0)).unwrap();
    assert_eq!(draft.strokes(), original);
    draft.pointer_down(sample(50.0, 50.0)).unwrap();
    draft.pointer_up(sample(50.0, 50.0)).unwrap();
    assert_eq!(draft.strokes().len(), 2);
    assert!(
        draft
            .strokes()
            .iter()
            .all(|stroke| !stroke.stroke.attributes.fit_to_curve)
    );
}

#[test]
fn geometry_change_invalidates_reference_and_restart_uses_final_physical_size() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(&mut draft, sample(10.0, 10.0), sample(20.0, 20.0));
    workspace
        .set_selection_output_size(PhysicalSize::new(50, 40).unwrap())
        .unwrap();
    draft.reconcile(&workspace);
    assert!(draft.is_stale());
    assert_eq!(draft.strokes().len(), 1);
    draft.begin(&workspace).unwrap();
    assert_eq!(
        draft.reference_size(),
        Some(PhysicalSize::new(50, 40).unwrap())
    );
    assert!(draft.pointer_down(sample(60.0, 20.0)).is_err());
}

#[test]
fn target_limit_is_checked_before_cloning_an_anchor_or_clearing_old_ink() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    let mut draft = draft(&workspace);
    line(&mut draft, sample(10.0, 10.0), sample(20.0, 20.0));
    let original = draft.strokes().to_vec();
    let template = workspace.manifest().timeline.frames[0].clone();
    let frames = (0..MAX_CINEMAGRAPH_TARGETS)
        .map(|index| {
            let mut frame = template.clone();
            frame.id = FrameId::from_u128(u128::try_from(index).unwrap() + 2);
            frame
        })
        .collect();
    workspace
        .execute(gif_from_screen_domain::EditCommand::InsertFrames { index: 1, frames })
        .unwrap();
    workspace.select_all();
    assert!(draft.begin(&workspace).unwrap_err().contains("1,000"));
    assert_eq!(draft.strokes(), original);
    workspace.clear_selection();
    assert!(draft.begin(&workspace).is_err());
    assert_eq!(draft.strokes(), original);
}
