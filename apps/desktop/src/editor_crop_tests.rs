use super::*;
use crate::editor_canvas::tests::workspace;

fn language(tag: &str) -> Localizer {
    Localizer::new(gif_from_screen_localization::find_language(tag).unwrap())
}

struct Controls {
    _directory: tempfile::TempDir,
    workspace: EditorWorkspace,
    draft: DirectCropDraft,
    context: egui::Context,
    language: Localizer,
    viewport: egui::Rect,
    enabled: bool,
}

impl Controls {
    fn new(width: f32, height: f32, font_scale: f32, language: Localizer) -> Self {
        let (directory, workspace) = workspace();
        let context = egui::Context::default();
        crate::preferences::fonts::install(&context);
        context.style_mut(|style| {
            style.animation_time = 0.0;
            for font in style.text_styles.values_mut() {
                font.size *= font_scale;
            }
        });
        Self {
            _directory: directory,
            workspace,
            draft: DirectCropDraft::default(),
            context,
            language,
            viewport: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height)),
            enabled: true,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> (egui::FullOutput, CropOutcome) {
        let mut outcome = CropOutcome::default();
        let frame = self.workspace.selection().current().unwrap();
        let size = self.workspace.manifest().canvas.size;
        let output = self.context.run(
            egui::RawInput {
                screen_rect: Some(self.viewport),
                events,
                focused: true,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    outcome = self.draft.show_controls(
                        ui,
                        &mut self.workspace,
                        frame,
                        [size.width.get(), size.height.get()],
                        self.enabled,
                        self.language,
                    );
                });
            },
        );
        (output, outcome)
    }

    fn text(&mut self, wanted: &str) -> egui::Rect {
        self.frame(Vec::new());
        let output = self.frame(Vec::new()).0;
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == wanted
                {
                    let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                    (shape.clip_rect.contains_rect(rect) && self.viewport.contains_rect(rect))
                        .then_some(rect)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing or clipped crop control: {wanted:?}"))
    }

    fn click(&mut self, wanted: &str) -> CropOutcome {
        let position = self.text(wanted).center();
        self.frame(pointer(position, true));
        self.frame(pointer(position, false)).1
    }

    fn replace_text(&mut self, previous: &str, next: &str) {
        let output = self.frame(Vec::new()).0;
        // TextEdit horizontally scrolls long values; click its actual visible
        // text intersection rather than requiring the entire value to fit.
        let position = output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == previous
                {
                    let rect = egui::Rect::from_min_size(text.pos, text.galley.size())
                        .intersect(shape.clip_rect)
                        .intersect(self.viewport);
                    rect.is_positive().then_some(rect.center())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing editable text {previous:?}"));
        self.frame(pointer(position, true));
        self.frame(pointer(position, false));
        self.frame(vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                // egui-winit marks Linux Ctrl as both physical Ctrl and the
                // platform command modifier used by TextEdit's Select All.
                modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            },
            egui::Event::Text(next.into()),
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            },
        ]);
    }
}

#[test]
fn chinese_fields_keep_original_spelling_and_apply_once_then_undo_redo_and_reopen() {
    let mut controls = Controls::new(460.0, 340.0, 1.0, language("zh"));
    let before = controls.workspace.manifest().clone();
    assert!(
        controls
            .click(controls.language.text(Message::CropStart))
            .started
    );
    for (previous, next) in [("8", "004"), ("6", " 03 "), ("0", "+02"), ("0", "01")] {
        controls.replace_text(previous, next);
    }
    let spelling = ["+02", "01", "004", " 03 "].map(str::to_owned);
    assert_eq!(controls.draft.session.as_ref().unwrap().fields, spelling);
    assert_eq!(controls.workspace.manifest(), &before);
    controls.language = language("en");
    controls.frame(Vec::new());
    assert_eq!(controls.draft.session.as_ref().unwrap().fields, spelling);
    controls.language = language("zh");
    assert!(
        controls
            .click(controls.language.text(Message::CropApplyAll))
            .applied
    );
    assert!(!controls.draft.active());
    assert!(!controls.frame(Vec::new()).1.applied);
    let after = controls.workspace.manifest().clone();
    assert_eq!(after.revision.get(), before.revision.get() + 1);
    assert_eq!(after.canvas.size, PhysicalSize::new(4, 3).unwrap());
    assert_eq!(
        controls.draft.notice.as_ref().unwrap().message_id(),
        Some(Message::CropApplied)
    );
    controls.text(language("zh").text(Message::CropApplied));
    controls.language = language("en");
    controls.text(language("en").text(Message::CropApplied));
    controls.workspace.undo().unwrap();
    assert_eq!(
        controls.workspace.manifest().timeline.frames,
        before.timeline.frames
    );
    controls.workspace.redo().unwrap();
    controls.workspace.checkpoint_and_compact().unwrap();
    let expected = controls.workspace.manifest().clone();
    let root = controls.workspace.project_root().to_path_buf();
    drop(controls.workspace);
    let opened = gif_from_screen_project::ActiveProject::open(
        &root,
        gif_from_screen_project::LockPolicy::FailIfPresent,
    )
    .unwrap();
    assert_eq!(opened.project.manifest(), &expected);
}

#[test]
fn chinese_numeric_input_rejects_localized_numbers_without_rewriting_text_or_project() {
    let mut controls = Controls::new(460.0, 340.0, 1.0, language("zh"));
    controls.click(controls.language.text(Message::CropStart));
    let before = controls.workspace.manifest().clone();
    let mut previous = "8";
    for invalid in ["三", "４", "-1", "1.5", "4294967296"] {
        controls.replace_text(previous, invalid);
        assert_eq!(controls.draft.session.as_ref().unwrap().fields[2], invalid);
        controls.text(controls.language.text(Message::CropUnsignedPixels));
        assert!(
            !controls
                .click(controls.language.text(Message::CropApplyAll))
                .applied
        );
        assert_eq!(controls.workspace.manifest(), &before);
        previous = invalid;
    }
    controls.replace_text(previous, "0");
    controls.text(controls.language.text(Message::CropEmptySize));
    assert!(
        !controls
            .click(controls.language.text(Message::CropApplyAll))
            .applied
    );
    assert_eq!(controls.workspace.manifest(), &before);
    controls.click(controls.language.text(Message::CropCancel));
    assert!(!controls.draft.active());
}

#[test]
fn unsigned_parser_and_typed_bounds_errors_keep_the_same_numeric_boundary() {
    let canvas = PhysicalSize::new(8, 6).unwrap();
    for (fields, message) in [
        (["-1", "0", "2", "2"], Message::CropUnsignedPixels),
        (["0", "0", "0", "2"], Message::CropEmptySize),
        (["7", "0", "2", "2"], Message::CropFitsImage),
        (
            ["4294967295", "0", "1", "1"],
            Message::CropCoordinateOverflow,
        ),
    ] {
        let fields = fields.map(str::to_owned);
        let error = parse_fields(&fields, canvas).unwrap_err();
        assert_eq!(error.message_id(), Some(message));
        assert_eq!(error.render(language("zh")), language("zh").text(message));
    }
    let spelling = [" 02 ", "+01", "004", "3"].map(str::to_owned);
    assert_eq!(
        parse_fields(&spelling, canvas).unwrap(),
        PhysicalRect::new(2, 1, 4, 3).unwrap()
    );
    assert_eq!(spelling, [" 02 ", "+01", "004", "3"].map(str::to_owned));
}

#[test]
fn chinese_large_font_layout_keeps_start_apply_cancel_and_disabled_states_accessible() {
    for scale in [1.0, 1.5, 2.0] {
        let mut controls = Controls::new(320.0, 420.0, scale, language("zh"));
        let before = controls.workspace.manifest().clone();
        controls.enabled = false;
        assert!(
            !controls
                .click(controls.language.text(Message::CropStart))
                .started
        );
        assert!(!controls.draft.active());
        controls.enabled = true;
        assert!(
            controls
                .click(controls.language.text(Message::CropStart))
                .started
        );
        controls.enabled = false;
        assert!(
            !controls
                .click(controls.language.text(Message::CropApplyAll))
                .applied
        );
        controls.click(controls.language.text(Message::CropCancel));
        assert!(!controls.draft.active());
        assert_eq!(controls.workspace.manifest(), &before);
    }
}

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
        let notice = draft.notice.as_ref().unwrap();
        assert_eq!(notice.message_id(), Some(Message::CropDraftDiscarded));
        for language in [language("en"), language("zh")] {
            assert_eq!(
                notice.render(language),
                language.text(Message::CropDraftDiscarded)
            );
        }
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

#[test]
fn layout_change_consumes_pre_threshold_release_then_accepts_fresh_click_and_drag() {
    let (_directory, workspace) = workspace();
    let before = workspace.manifest().clone();
    let mut draft = DirectCropDraft::default();
    draft
        .begin(&workspace, workspace.selection().current().unwrap(), [8, 6])
        .unwrap();
    let confirmed = PhysicalRect::new(2, 1, 4, 3).unwrap();
    let typed = ["02", "01", "4", "3"].map(str::to_owned);
    let session = draft.session.as_mut().unwrap();
    session.rect = confirmed;
    session.fields.clone_from(&typed);
    let context = egui::Context::default();
    let image = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(160.0, 120.0));
    gesture_frame(&context, &mut draft, image, Vec::new(), true);
    let point = egui::pos2(110.0, 90.0);
    gesture_frame(&context, &mut draft, image, pointer(point, true), true);
    assert!(draft.session.as_ref().unwrap().gesture.is_none());
    draft.cancel_layout_gesture();
    let moved = image.translate(egui::vec2(10.0, 10.0));
    gesture_frame(&context, &mut draft, moved, pointer(point, false), true);
    let session = draft.session.as_ref().unwrap();
    assert_eq!(session.rect, confirmed);
    assert_eq!(session.fields, typed);
    assert!(!session.suppress_primary_until_release);
    let click = moved.min + egui::vec2(50.0, 50.0);
    gesture_frame(&context, &mut draft, moved, pointer(click, true), true);
    gesture_frame(&context, &mut draft, moved, pointer(click, false), true);
    assert_eq!(
        draft.session.as_ref().unwrap().rect,
        PhysicalRect::new(2, 2, 1, 1).unwrap()
    );
    let start = moved.min + egui::vec2(20.0, 20.0);
    let end = moved.min + egui::vec2(80.0, 80.0);
    gesture_frame(&context, &mut draft, moved, pointer(start, true), true);
    gesture_frame(
        &context,
        &mut draft,
        moved,
        vec![egui::Event::PointerMoved(end)],
        true,
    );
    gesture_frame(&context, &mut draft, moved, pointer(end, false), true);
    assert_eq!(
        draft.session.as_ref().unwrap().rect,
        PhysicalRect::new(1, 1, 3, 3).unwrap()
    );
    assert_eq!(workspace.manifest(), &before);
}

#[test]
fn layout_change_rolls_back_active_crop_and_preserves_confirmed_numeric_spelling() {
    let (_directory, workspace) = workspace();
    let before = workspace.manifest().clone();
    let mut draft = DirectCropDraft::default();
    draft
        .begin(&workspace, workspace.selection().current().unwrap(), [8, 6])
        .unwrap();
    let confirmed = PhysicalRect::new(2, 2, 2, 2).unwrap();
    let typed = ["002", "02", "2", "2"].map(str::to_owned);
    let session = draft.session.as_mut().unwrap();
    session.rect = confirmed;
    session.fields.clone_from(&typed);
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
        vec![egui::Event::PointerMoved(egui::pos2(140.0, 110.0))],
        true,
    );
    assert!(draft.session.as_ref().unwrap().gesture.is_some());
    assert_ne!(draft.session.as_ref().unwrap().rect, confirmed);
    draft.cancel_layout_gesture();
    assert_eq!(draft.session.as_ref().unwrap().rect, confirmed);
    assert_eq!(draft.session.as_ref().unwrap().fields, typed);
    let moved = image.translate(egui::vec2(-10.0, 10.0));
    gesture_frame(
        &context,
        &mut draft,
        moved,
        vec![egui::Event::PointerMoved(egui::pos2(100.0, 70.0))],
        true,
    );
    assert!(
        draft
            .session
            .as_ref()
            .unwrap()
            .suppress_primary_until_release
    );
    gesture_frame(
        &context,
        &mut draft,
        moved,
        pointer(egui::pos2(100.0, 70.0), false),
        true,
    );
    let session = draft.session.as_ref().unwrap();
    assert_eq!(session.rect, confirmed);
    assert_eq!(session.fields, typed);
    assert!(session.gesture.is_none() && !session.suppress_primary_until_release);
    assert_eq!(workspace.manifest(), &before);
}

#[test]
fn layout_change_with_no_pointer_keeps_ready_crop_and_clears_its_temporary_gate() {
    let (_directory, workspace) = workspace();
    let mut draft = DirectCropDraft::default();
    draft
        .begin(&workspace, workspace.selection().current().unwrap(), [8, 6])
        .unwrap();
    let typed = ["0001", "2", "3", "2"].map(str::to_owned);
    draft.session.as_mut().unwrap().fields.clone_from(&typed);
    let confirmed = PhysicalRect::new(1, 2, 3, 2).unwrap();
    draft.session.as_mut().unwrap().rect = confirmed;
    draft.cancel_layout_gesture();
    let context = egui::Context::default();
    let image = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(160.0, 120.0));
    gesture_frame(&context, &mut draft, image, Vec::new(), true);
    let session = draft.session.as_ref().unwrap();
    assert!(!session.suppress_primary_until_release);
    assert_eq!(session.rect, confirmed);
    assert_eq!(session.fields, typed);
    assert!(draft.active());
}
