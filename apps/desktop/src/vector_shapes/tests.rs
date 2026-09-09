use std::{
    fs,
    time::{Duration, Instant},
};

use eframe::egui;
use gif_from_screen_application::{BlankAnimationProjectOptions, create_blank_animation_project};
use gif_from_screen_domain::{
    CompositePrecision, DurationUs, EditCommand, FrameClip, FrameId, FrameRenderStep, PhysicalSize,
    ProjectId, Rgba, UnixTimeMs, VectorShapeBounds, VectorShapeKind,
};
use gif_from_screen_localization::{Localizer, find_language};
use gif_from_screen_project::LockPolicy;

use super::{
    VectorShapes,
    draft::{Handle, ShapeTool},
};
use crate::editor_workspace::EditorWorkspace;

struct Scene {
    directory: tempfile::TempDir,
    workspace: EditorWorkspace,
    tool: VectorShapes,
    context: egui::Context,
    rect: egui::Rect,
    field: String,
}

impl Scene {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut project = create_blank_animation_project(
            directory.path().join("vector.gfsproj"),
            BlankAnimationProjectOptions {
                project_id: ProjectId::from_u128(601),
                frame_id: FrameId::from_u128(1),
                app_version: "vector-test".into(),
                created_at: UnixTimeMs::new(0),
                canvas: PhysicalSize::new(200, 160).unwrap(),
                background: Rgba::TRANSPARENT,
                frame_duration: DurationUs::new(50_000).unwrap(),
                frame_limit_bytes: 200 * 160 * 4,
            },
        )
        .unwrap();
        let first = project.manifest().timeline.frames[0].clone();
        project
            .commit(EditCommand::InsertFrames {
                index: 1,
                frames: (2..=3)
                    .map(|id| FrameClip {
                        id: FrameId::from_u128(id),
                        ..first.clone()
                    })
                    .collect(),
            })
            .unwrap();
        let mut workspace = EditorWorkspace::from_active(project, 16).unwrap();
        workspace.select_only(FrameId::from_u128(1)).unwrap();
        let mut tool = VectorShapes::default();
        tool.begin(&workspace, FrameId::from_u128(1), [200, 160])
            .unwrap();
        let context = egui::Context::default();
        crate::preferences::fonts::install(&context);
        let mut scene = Self {
            directory,
            workspace,
            tool,
            context,
            rect: egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(200.0, 160.0)),
            field: "keep".into(),
        };
        scene.frame(vec![], true);
        scene.frame(vec![], true);
        scene
    }
    fn frame(&mut self, events: Vec<egui::Event>, focused: bool) {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(520.0, 400.0),
            )),
            events,
            focused,
            ..Default::default()
        };
        self.tool.filter_raw_input(&self.context, &mut input, true);
        let _ = self.context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.put(
                    egui::Rect::from_min_size(egui::pos2(280.0, 45.0), egui::vec2(150.0, 30.0)),
                    egui::TextEdit::singleline(&mut self.field).id_salt("unrelated-field"),
                );
                let response = ui.interact(
                    self.rect,
                    egui::Id::new("vector-image"),
                    egui::Sense::click_and_drag(),
                );
                self.tool
                    .show_preview(ui, &response, [200, 160], [200, 160], true, english());
            });
        });
    }
    fn drag(&mut self, start: egui::Pos2, end: egui::Pos2) {
        self.frame(
            vec![
                egui::Event::PointerMoved(start),
                button(start, true, egui::Modifiers::NONE),
                egui::Event::PointerMoved(end),
                button(end, false, egui::Modifiers::NONE),
            ],
            true,
        );
        self.frame(vec![], true); // automatic FIFO tail, not a user-imposed pause
    }
    fn ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.tool.preview.current(self.tool.draft.generation) {
            self.frame(vec![], true);
            assert!(
                self.tool.preview.failure.is_none(),
                "{:?}",
                self.tool.preview.failure
            );
            assert!(Instant::now() < deadline, "preview did not settle");
            std::thread::yield_now();
        }
    }
}

fn english() -> Localizer {
    Localizer::new(find_language("en").unwrap())
}
fn button(pos: egui::Pos2, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    }
}
fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn fast_native_batch_creates_bounded_inset_geometry_and_preview_without_project_writes() {
    let mut scene = Scene::new();
    let before = scene.workspace.manifest().clone();
    let journal = fs::read(scene.workspace.project_root().join("journal.ndjson")).unwrap();
    scene.drag(egui::pos2(60.0, 70.0), egui::pos2(150.0, 130.0));
    assert_eq!(scene.tool.objects().len(), 1);
    assert_eq!(
        scene.tool.objects()[0].shape.bounds,
        VectorShapeBounds {
            x_hundredths: 2060,
            y_hundredths: 3060,
            width_hundredths: 8880,
            height_hundredths: 5880
        }
    );
    scene.ready();
    let request = scene.tool.draft.request(&scene.workspace).unwrap();
    scene
        .tool
        .validate_apply(&scene.workspace, &request)
        .unwrap();
    assert_eq!(scene.workspace.manifest(), &before);
    assert_eq!(
        fs::read(scene.workspace.project_root().join("journal.ndjson")).unwrap(),
        journal
    );
}

#[test]
fn mapping_and_focus_changes_rollback_only_unfinished_objects_and_require_a_new_press() {
    let mut scene = Scene::new();
    scene.drag(egui::pos2(60.0, 60.0), egui::pos2(120.0, 100.0));
    let confirmed = scene.tool.objects().to_vec();
    let at = egui::pos2(150.0, 120.0);
    scene.frame(vec![button(at, true, egui::Modifiers::NONE)], true);
    assert_eq!(scene.tool.objects().len(), 2);
    scene.rect = scene.rect.translate(egui::vec2(7.0, 2.0));
    scene.frame(
        vec![
            egui::Event::PointerMoved(egui::pos2(185.0, 160.0)),
            button(at, false, egui::Modifiers::NONE),
        ],
        true,
    );
    assert_eq!(scene.tool.objects(), confirmed);
    scene.frame(vec![], true);
    scene.drag(egui::pos2(155.0, 123.0), egui::pos2(190.0, 165.0));
    assert_eq!(scene.tool.objects().len(), 2);
    scene.frame(vec![button(at, true, egui::Modifiers::NONE)], true);
    scene.frame(vec![egui::Event::WindowFocused(false)], false);
    assert_eq!(scene.tool.objects().len(), 2);
}

#[test]
fn another_text_field_owns_its_batched_delete_and_canvas_delete_never_edits_frames() {
    let mut scene = Scene::new();
    scene.drag(egui::pos2(60.0, 60.0), egui::pos2(120.0, 100.0));
    let at = egui::pos2(330.0, 60.0);
    scene.frame(
        vec![
            button(at, true, egui::Modifiers::NONE),
            button(at, false, egui::Modifiers::NONE),
            key(
                egui::Key::A,
                egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            ),
            key(egui::Key::Delete, egui::Modifiers::NONE),
        ],
        true,
    );
    scene.frame(vec![], true);
    assert!(scene.field.is_empty());
    assert_eq!(scene.tool.objects().len(), 1);
    scene.tool.draft.tool = ShapeTool::Select;
    scene.drag(egui::pos2(90.0, 80.0), egui::pos2(90.0, 80.0));
    scene.frame(vec![key(egui::Key::Delete, egui::Modifiers::NONE)], true);
    assert!(scene.tool.objects().is_empty());
    assert_eq!(scene.workspace.manifest().timeline.frames.len(), 3);
}

#[test]
fn stale_anchor_keeps_geometry_and_rejects_both_stale_and_forged_application() {
    let mut scene = Scene::new();
    scene.drag(egui::pos2(60.0, 60.0), egui::pos2(120.0, 110.0));
    scene.ready();
    let request = scene.tool.draft.request(&scene.workspace).unwrap();
    let geometry = scene.tool.objects().to_vec();
    scene.workspace.select_only(FrameId::from_u128(2)).unwrap();
    scene.tool.reconcile(&scene.workspace);
    assert!(scene.tool.is_stale());
    assert_eq!(scene.tool.objects(), geometry);
    assert!(scene.workspace.apply_vector_shapes(&request).is_err());
    assert!(
        scene
            .tool
            .begin(&scene.workspace, FrameId::from_u128(2), [200, 160])
            .is_err()
    );
    scene
        .tool
        .restart(&scene.workspace, FrameId::from_u128(2), [200, 160])
        .unwrap();
    assert!(scene.tool.objects().is_empty());
}

#[test]
fn two_shapes_are_one_undoable_group_with_selection_gaps_and_isolated_stages_after_reopen() {
    let mut scene = Scene::new();
    scene
        .workspace
        .toggle_selection(FrameId::from_u128(3))
        .unwrap();
    scene
        .tool
        .restart(&scene.workspace, FrameId::from_u128(3), [200, 160])
        .unwrap();
    scene.frame(vec![], true);
    scene.frame(vec![], true);
    scene.drag(egui::pos2(60.0, 60.0), egui::pos2(120.0, 110.0));
    scene.tool.draft.kind = VectorShapeKind::Triangle;
    scene.drag(egui::pos2(135.0, 100.0), egui::pos2(210.0, 170.0));
    let frames = scene.workspace.manifest().timeline.frames.clone();
    let request = scene.tool.draft.request(&scene.workspace).unwrap();
    let id = scene.workspace.apply_vector_shapes(&request).unwrap();
    let manifest = scene.workspace.manifest().clone();
    let cells = manifest.timeline.overlay_tracks[0]
        .frame_cells
        .as_ref()
        .unwrap();
    assert_eq!(
        cells.iter().map(|c| c.frame_id).collect::<Vec<_>>(),
        [FrameId::from_u128(1), FrameId::from_u128(3)]
    );
    assert_eq!(
        cells.iter().map(|c| c.scopes[0].run_id).collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(
        cells
            .iter()
            .all(|c| c.marks.len() == 2 && c.marks[0].z_index == 0 && c.marks[1].z_index == 1)
    );
    assert_eq!(manifest.timeline.frames[1], frames[1]);
    for index in [0, 2] {
        assert!(matches!(
            manifest.timeline.frames[index].render_steps.last(),
            Some(FrameRenderStep::Composite {
                precision: CompositePrecision::VectorCanvasPbgra8PngV1,
                ..
            })
        ));
    }
    scene.workspace.undo().unwrap();
    assert!(
        scene
            .workspace
            .manifest()
            .timeline
            .overlay_tracks
            .is_empty()
    );
    assert_eq!(scene.workspace.manifest().timeline.frames, frames);
    scene.workspace.redo().unwrap();
    let manifest = scene.workspace.manifest().clone();
    let path = scene.workspace.project_root().to_owned();
    scene.tool.shutdown();
    drop(scene.workspace);
    let reopened = EditorWorkspace::open(&path, LockPolicy::FailIfPresent, 16).unwrap();
    assert_eq!(reopened.manifest(), &manifest);
    assert_eq!(reopened.manifest().timeline.overlay_tracks[0].id, id);
    assert!(scene.directory.path().exists());
}

#[test]
fn tiny_insert_is_discarded_and_all_eight_handles_and_rotation_preserve_valid_data() {
    let mut scene = Scene::new();
    scene.drag(egui::pos2(60.0, 60.0), egui::pos2(63.0, 63.0));
    assert!(scene.tool.objects().is_empty());
    for handle in [
        Handle::TopLeft,
        Handle::Top,
        Handle::TopRight,
        Handle::Right,
        Handle::BottomRight,
        Handle::Bottom,
        Handle::BottomLeft,
        Handle::Left,
    ] {
        scene
            .tool
            .restart(&scene.workspace, FrameId::from_u128(1), [200, 160])
            .unwrap();
        scene.tool.draft.insert([3000, 3000]).unwrap();
        scene.tool.draft.update([9000, 9000]).unwrap();
        scene.tool.draft.finish();
        let before = scene.tool.objects().to_vec();
        scene.tool.draft.begin_handle([5000, 5000], handle).unwrap();
        scene.tool.draft.update([5500, 5500]).unwrap();
        assert_ne!(scene.tool.objects(), before);
        for object in scene.tool.objects() {
            object.shape.validate().unwrap();
        }
        scene.tool.draft.cancel_gesture();
        assert_eq!(scene.tool.objects(), before);
        scene.tool.draft.rotate_by(-9000).unwrap();
        assert_eq!(scene.tool.objects()[0].shape.rotation_hundredths, 27000);
        scene.tool.draft.set_rotation(0).unwrap();
    }
}

#[test]
fn rotated_handle_resize_uses_local_axes_and_preserves_the_opposite_edge() {
    let mut scene = Scene::new();
    scene.tool.draft.insert([5000, 5000]).unwrap();
    scene.tool.draft.update([11000, 9000]).unwrap();
    scene.tool.draft.finish();
    scene.tool.draft.set_rotation(9000).unwrap();
    let before = scene.tool.objects()[0];
    let positions = super::geometry::handles(before);
    assert!((positions[3].1.x - positions[7].1.x).abs() < 0.001);
    assert!(positions[3].1.y > positions[7].1.y);
    scene
        .tool
        .draft
        .begin_handle([8000, 10000], Handle::Right)
        .unwrap();
    scene.tool.draft.update([8000, 11000]).unwrap();
    let after = scene.tool.objects()[0];
    assert_eq!(
        after.shape.bounds.width_hundredths,
        before.shape.bounds.width_hundredths + 1000
    );
    assert_eq!(
        after.shape.bounds.height_hundredths,
        before.shape.bounds.height_hundredths
    );
    let anchored = super::geometry::handles(after)[7].1;
    assert!((anchored.x - positions[7].1.x).abs() < 0.01);
    assert!((anchored.y - positions[7].1.y).abs() < 0.01);
}

#[test]
fn modifier_wheel_rotates_only_focused_canvas_without_native_zoom_or_scroll() {
    let mut scene = Scene::new();
    scene.drag(egui::pos2(80.0, 80.0), egui::pos2(160.0, 140.0));
    let wheel = |modifiers| egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 20.0),
        modifiers,
    };
    let zoom = scene.context.zoom_factor();
    for (modifiers, expected) in [
        (egui::Modifiers::CTRL, 100),
        (egui::Modifiers::ALT, 9100),
        (egui::Modifiers::SHIFT, 11100),
    ] {
        scene.frame(vec![wheel(modifiers)], true);
        assert_eq!(scene.tool.objects()[0].shape.rotation_hundredths, expected);
        assert_eq!(scene.context.zoom_factor().to_bits(), zoom.to_bits());
        scene.context.input(|input| {
            assert_eq!(input.zoom_delta().to_bits(), 1.0_f32.to_bits());
            assert_eq!(input.raw_scroll_delta, egui::Vec2::ZERO);
        });
    }
    let at = egui::pos2(330.0, 60.0);
    scene.frame(
        vec![
            egui::Event::PointerMoved(at),
            button(at, true, egui::Modifiers::NONE),
            button(at, false, egui::Modifiers::NONE),
            wheel(egui::Modifiers::SHIFT),
        ],
        true,
    );
    scene.frame(vec![], true);
    assert_eq!(scene.tool.objects()[0].shape.rotation_hundredths, 11100);
    scene
        .context
        .input(|input| assert_ne!(input.raw_scroll_delta, egui::Vec2::ZERO));
}

#[test]
fn invalid_group_requests_are_atomic_and_do_not_write_the_journal() {
    let mut scene = Scene::new();
    scene.drag(egui::pos2(60.0, 60.0), egui::pos2(120.0, 110.0));
    let request = scene.tool.draft.request(&scene.workspace).unwrap();
    let manifest = scene.workspace.manifest().clone();
    let journal = scene.workspace.project_root().join("journal.ndjson");
    let original = fs::read(&journal).unwrap();
    for case in 0..5 {
        let mut invalid = request.clone();
        match case {
            0 => invalid.shapes.clear(),
            1 => invalid.shapes = vec![invalid.shapes[0]; 257],
            2 => invalid.shapes[0].bounds.width_hundredths = 0,
            3 => invalid.canvas_size = PhysicalSize::new(201, 160).unwrap(),
            _ => invalid.reference_frame = FrameId::from_u128(2),
        }
        assert!(scene.workspace.apply_vector_shapes(&invalid).is_err());
        assert_eq!(scene.workspace.manifest(), &manifest);
        assert_eq!(fs::read(&journal).unwrap(), original);
    }
}
