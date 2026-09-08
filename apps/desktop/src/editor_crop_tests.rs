use super::*;
use crate::editor_canvas::tests::workspace;

#[test]
fn reverse_drag_uses_rendered_pixels_not_preview_texture_pixels() {
    let image = egui::Rect::from_min_size(egui::pos2(50.0, 20.0), egui::vec2(320.0, 210.0));
    let start = map_point(image, egui::pos2(150.0, 120.0), [640, 420]).unwrap();
    let end = map_point(image, egui::pos2(55.0, 25.0), [640, 420]).unwrap();
    assert_eq!(
        drag_rect(start, end, PhysicalSize::new(640, 420).unwrap()),
        PhysicalRect::new(10, 10, 190, 190).unwrap()
    );
}

#[test]
fn explicit_crop_applies_to_all_frames_and_survives_undo_redo_and_reopen() {
    let (directory, mut workspace) = workspace();
    let mut draft = DirectCropDraft::default();
    let frame = workspace.selection().current().unwrap();
    let before = workspace.manifest().clone();
    draft.begin(&workspace, frame, [8, 6]).unwrap();
    draft.session.as_mut().unwrap().fields = ["2", "1", "4", "3"].map(str::to_owned);
    assert_eq!(workspace.manifest(), &before);
    draft.apply(&mut workspace).unwrap();
    assert!(!draft.active());
    assert_eq!(
        workspace.manifest().canvas.size,
        PhysicalSize::new(4, 3).unwrap()
    );
    let expected: Vec<u8> = (1_u8..4)
        .flat_map(|y| (2_u8..6).flat_map(move |x| [x * 20, y * 30, 100, 255]))
        .collect();
    for frame in &workspace.manifest().timeline.frames {
        let surface =
            crate::editor_preview::render_frame_surface(workspace.active_project(), frame.id, 4096)
                .unwrap();
        assert_eq!(surface.pixels(), expected);
        assert_eq!(frame.asset_id, before.timeline.frames[0].asset_id);
    }
    workspace.undo().unwrap();
    assert_eq!(workspace.manifest().canvas, before.canvas);
    assert_eq!(workspace.manifest().timeline.frames, before.timeline.frames);
    workspace.redo().unwrap();
    workspace.checkpoint_and_compact().unwrap();
    let after = workspace.manifest().clone();
    drop(workspace);
    let reopened = gif_from_screen_project::ActiveProject::open(
        directory.path(),
        gif_from_screen_project::LockPolicy::FailIfPresent,
    )
    .unwrap();
    assert_eq!(reopened.project.manifest(), &after);
    assert_eq!(
        crate::editor_preview::render_frame_surface(&reopened.project, frame, 4096)
            .unwrap()
            .pixels(),
        expected
    );
}

#[test]
fn changing_selection_revision_or_project_rejects_stale_crop() {
    for change in 0..3 {
        let (_directory, mut workspace) = workspace();
        let mut draft = DirectCropDraft::default();
        draft
            .begin(&workspace, workspace.selection().current().unwrap(), [8, 6])
            .unwrap();
        let _replacement;
        match change {
            0 => {
                workspace.select_only(FrameId::from_u128(2)).unwrap();
            }
            1 => {
                workspace
                    .override_selection_duration(
                        gif_from_screen_domain::DurationUs::new(200_000).unwrap(),
                    )
                    .unwrap();
            }
            _ => {
                let (directory, replacement) = self::workspace();
                _replacement = directory;
                workspace = replacement;
            }
        }
        let before = workspace.manifest().clone();
        assert!(draft.apply(&mut workspace).is_err());
        assert_eq!(workspace.manifest(), &before);
        assert!(!draft.active());
    }
}

#[test]
fn invalid_numeric_fields_and_cancel_never_modify_the_project() {
    let (_directory, mut workspace) = workspace();
    let before = workspace.manifest().clone();
    let mut draft = DirectCropDraft::default();
    draft
        .begin(&workspace, workspace.selection().current().unwrap(), [8, 6])
        .unwrap();
    for fields in [
        ["-1", "0", "2", "2"],
        ["0", "0", "0", "2"],
        ["7", "0", "2", "2"],
        ["0", "5", "2", "2"],
        ["4294967296", "0", "1", "1"],
    ] {
        draft.session.as_mut().unwrap().fields = fields.map(str::to_owned);
        assert!(draft.apply(&mut workspace).is_err());
        assert_eq!(workspace.manifest(), &before);
    }
    draft.cancel();
    assert!(!draft.active());
    assert_eq!(workspace.manifest(), &before);
}

fn pointer(pos: egui::Pos2, down: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: down,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn gesture_frame(
    context: &egui::Context,
    draft: &mut DirectCropDraft,
    rect: egui::Rect,
    events: Vec<egui::Event>,
    focused: bool,
) {
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(240.0, 180.0),
            )),
            events,
            focused,
            ..Default::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let response = ui.interact(
                    rect,
                    egui::Id::new("crop-gesture-test"),
                    egui::Sense::click_and_drag(),
                );
                draft.interact(ui, &response, [8, 6], true);
            });
        },
    );
}

#[test]
fn release_before_leaving_window_keeps_the_completed_crop() {
    let (_directory, workspace) = workspace();
    let mut draft = DirectCropDraft::default();
    draft
        .begin(&workspace, workspace.selection().current().unwrap(), [8, 6])
        .unwrap();
    let context = egui::Context::default();
    let image = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(160.0, 120.0));
    gesture_frame(&context, &mut draft, image, Vec::new(), true);
    gesture_frame(
        &context,
        &mut draft,
        image,
        pointer(egui::pos2(60.0, 50.0), true),
        true,
    );
    gesture_frame(
        &context,
        &mut draft,
        image,
        vec![egui::Event::PointerMoved(egui::pos2(100.0, 70.0))],
        true,
    );
    let mut release = pointer(egui::pos2(120.0, 90.0), false);
    release.push(egui::Event::PointerMoved(egui::pos2(230.0, 170.0)));
    release.push(egui::Event::PointerGone);
    gesture_frame(&context, &mut draft, image, release, true);
    let session = draft.session.as_ref().unwrap();
    assert!(session.gesture.is_none());
    assert_eq!(session.rect, PhysicalRect::new(1, 1, 3, 2).unwrap());
}

#[test]
fn crop_pointer_mapping_commits_only_on_apply_and_lost_mapping_restores_previous_draft() {
    let (_directory, workspace) = workspace();
    let mut draft = DirectCropDraft::default();
    draft
        .begin(&workspace, workspace.selection().current().unwrap(), [8, 6])
        .unwrap();
    let before = workspace.manifest().clone();
    let context = egui::Context::default();
    let image = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(160.0, 120.0));
    gesture_frame(&context, &mut draft, image, Vec::new(), true);
    gesture_frame(
        &context,
        &mut draft,
        image,
        pointer(egui::pos2(60.0, 50.0), true),
        true,
    );
    gesture_frame(
        &context,
        &mut draft,
        image,
        vec![egui::Event::PointerMoved(egui::pos2(120.0, 90.0))],
        true,
    );
    assert_eq!(
        draft.session.as_ref().unwrap().rect,
        PhysicalRect::new(1, 1, 3, 2).unwrap()
    );
    assert_eq!(workspace.manifest(), &before);
    gesture_frame(
        &context,
        &mut draft,
        image.translate(egui::vec2(-10.0, 0.0)),
        Vec::new(),
        true,
    );
    assert!(draft.session.as_ref().unwrap().gesture.is_none());
    assert_eq!(
        draft.session.as_ref().unwrap().rect,
        PhysicalRect::new(0, 0, 8, 6).unwrap()
    );
    assert_eq!(workspace.manifest(), &before);
}
