//! Real egui layout/input regressions; no native display or host configuration.

use eframe::egui;
use gif_from_screen_localization::{Localizer, Message, find_language};

use crate::editor_shell_layout::{self, InspectorLayout};

fn language(tag: &str) -> Localizer {
    Localizer::new(find_language(tag).unwrap())
}

fn context(zoom: f32) -> egui::Context {
    let context = egui::Context::default();
    crate::preferences::fonts::install(&context);
    context.set_zoom_factor(zoom);
    context.style_mut(|style| style.animation_time = 0.0);
    context
}

fn input(context: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::RawInput {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            size / context.zoom_factor(),
        )),
        events,
        focused: true,
        ..Default::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .native_pixels_per_point = Some(1.0);
    input
}

fn pointer(point: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(point),
        egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn app_fixture() -> (tempfile::TempDir, crate::GifFromScreenApp) {
    use gif_from_screen_application::{
        BlankAnimationProjectOptions, create_blank_animation_project,
    };
    use gif_from_screen_domain::{DurationUs, FrameId, PhysicalSize, ProjectId, Rgba, UnixTimeMs};
    let directory = tempfile::tempdir().unwrap();
    let project = create_blank_animation_project(
        directory.path().join("layout.gfsproj"),
        BlankAnimationProjectOptions {
            project_id: ProjectId::from_u128(7001),
            frame_id: FrameId::from_u128(7002),
            app_version: "layout-test".into(),
            created_at: UnixTimeMs::new(0),
            canvas: PhysicalSize::new(160, 96).unwrap(),
            background: Rgba {
                red: 20,
                green: 50,
                blue: 90,
                alpha: 255,
            },
            frame_duration: DurationUs::new(100_000).unwrap(),
            frame_limit_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let mut workspace = crate::editor_workspace::EditorWorkspace::from_active(project, 20).unwrap();
    workspace.select_first().unwrap();
    let mut app = crate::GifFromScreenApp::default();
    app.view = crate::AppView::Editor;
    app.editor_workspace = Some(workspace);
    (directory, app)
}

fn app_frame(
    context: &egui::Context,
    app: &mut crate::GifFromScreenApp,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut input = input(context, size, events);
    eframe::App::raw_input_hook(app, context, &mut input);
    context.run(input, |context| {
        app.show_main_view(context);
        app.language_settings.show(context);
    })
}

fn click_app(
    context: &egui::Context,
    app: &mut crate::GifFromScreenApp,
    size: egui::Vec2,
    point: egui::Pos2,
) -> egui::FullOutput {
    app_frame(context, app, size, pointer(point, true));
    app_frame(context, app, size, pointer(point, false))
}

fn visible_text(output: &egui::FullOutput, value: &str) -> Option<egui::Rect> {
    output.shapes.iter().find_map(|shape| {
        if let egui::Shape::Text(text) = &shape.shape
            && text.galley.text() == value
        {
            let rect = text.visual_bounding_rect();
            (rect.is_positive() && shape.clip_rect.contains_rect(rect)).then_some(rect)
        } else {
            None
        }
    })
}

fn scroll_to_text(
    context: &egui::Context,
    app: &mut crate::GifFromScreenApp,
    size: egui::Vec2,
    value: &str,
) -> egui::Rect {
    for _ in 0..40 {
        let output = app_frame(context, app, size, vec![]);
        if let Some(rect) = visible_text(&output, value) {
            return rect;
        }
        let point = egui::pos2(
            size.x / context.zoom_factor() - 12.0,
            size.y / context.zoom_factor() - 30.0,
        );
        app_frame(
            context,
            app,
            size,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -100.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    panic!("could not reach {value} using actual wheel input");
}

fn preview_rect(output: &egui::FullOutput) -> egui::Rect {
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id != egui::TextureId::default() => {
                Some(mesh.calc_bounds().intersect(shape.clip_rect))
            }
            egui::Shape::Rect(rect) if rect.brush.is_some() => {
                Some(rect.rect.intersect(shape.clip_rect))
            }
            _ => None,
        })
        .filter(egui::Rect::is_positive)
        .max_by(|left, right| left.area().total_cmp(&right.area()))
        .expect("actual preview image")
}

#[test]
fn real_editor_preserves_wide_columns_and_font_focus_across_stacked_resize_and_language() {
    let (_directory, mut app) = app_fixture();
    let context = context(1.0);
    let wide = egui::vec2(1280.0, 900.0);
    for _ in 0..3 {
        app_frame(&context, &mut app, wide, vec![]);
    }
    let output = app_frame(&context, &mut app, wide, vec![]);
    let image = preview_rect(&output);
    assert!(
        text_rect(&output, language("en").text(Message::EditorExpression)).right() < image.left()
    );
    let tab = text_rect(&output, language("en").text(Message::EditorOverlaysTab));
    click_app(&context, &mut app, wide, tab.center());
    let font = scroll_to_text(&context, &mut app, wide, "sans-serif");
    click_app(&context, &mut app, wide, font.center());
    let focus = context.memory(egui::Memory::focused).unwrap();
    let before = app.editor_workspace.as_ref().unwrap().manifest().clone();
    for (width, zoom, tag) in [(680.0, 1.5, "zh"), (1280.0, 1.0, "en"), (680.0, 1.0, "zh")] {
        context.set_zoom_factor(zoom);
        app.language_settings = crate::preferences::LanguageSettings::with_language(
            gif_from_screen_localization::LanguagePreference::explicit(tag).unwrap(),
        );
        app.sync_ui_language(language(tag));
        let size = egui::vec2(width, 900.0);
        for _ in 0..3 {
            app_frame(&context, &mut app, size, vec![]);
        }
        scroll_to_text(&context, &mut app, size, "sans-serif");
        assert_eq!(
            context.memory(egui::Memory::focused),
            Some(focus),
            "{width}/{zoom}/{tag}"
        );
        assert_eq!(app.editor_workspace.as_ref().unwrap().manifest(), &before);
    }
    let size = egui::vec2(680.0, 900.0);
    app_frame(
        &context,
        &mut app,
        size,
        vec![
            egui::Event::Key {
                key: egui::Key::End,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Text("{kept}".into()),
        ],
    );
    scroll_to_text(&context, &mut app, size, "sans-serif{kept}");
    assert_eq!(app.editor_workspace.as_ref().unwrap().manifest(), &before);
}

#[test]
fn stacked_editor_action_is_reachable_by_page_wheel_and_dispatches_real_text_validation() {
    let (_directory, mut app) = app_fixture();
    let context = context(1.5);
    let size = egui::vec2(680.0, 760.0);
    for _ in 0..3 {
        app_frame(&context, &mut app, size, vec![]);
    }
    let output = app_frame(&context, &mut app, size, vec![]);
    let tab = text_rect(&output, language("en").text(Message::EditorOverlaysTab));
    click_app(&context, &mut app, size, tab.center());
    let button = scroll_to_text(
        &context,
        &mut app,
        size,
        language("en").text(Message::TextAddSelected),
    );
    let before = app.editor_workspace.as_ref().unwrap().manifest().clone();
    click_app(&context, &mut app, size, button.center());
    assert_eq!(
        app.notice
            .as_ref()
            .and_then(crate::ui_notice::Notice::message_id),
        Some(Message::TextEmpty)
    );
    assert!(!app.text_overlay.is_running());
    assert_eq!(app.editor_workspace.as_ref().unwrap().manifest(), &before);
}

#[test]
fn actual_header_navigation_and_language_button_keep_background_job_gates() {
    let (directory, mut app) = app_fixture();
    let context = context(1.5);
    let size = egui::vec2(680.0, 760.0);
    let image = directory.path().join("invalid.png");
    std::fs::write(&image, b"not a PNG").unwrap();
    app.watermark_job.start(image).unwrap();
    let before = app.editor_workspace.as_ref().unwrap().manifest().clone();
    for _ in 0..3 {
        app_frame(&context, &mut app, size, vec![]);
    }
    let output = app_frame(&context, &mut app, size, vec![]);
    let back = text_rect(&output, language("en").text(Message::BackToHome));
    click_app(&context, &mut app, size, back.center());
    assert_eq!(app.view, crate::AppView::Editor);
    assert_eq!(app.editor_workspace.as_ref().unwrap().manifest(), &before);
    let output = app_frame(&context, &mut app, size, vec![]);
    let locale = text_rect(&output, language("en").text(Message::LanguageSettingsTitle));
    click_app(&context, &mut app, size, locale.center());
    app_frame(&context, &mut app, size, vec![]);
    let output = app_frame(&context, &mut app, size, vec![]);
    text_rect(&output, language("en").text(Message::LanguageChoice));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.watermark_job.state() == crate::WatermarkDecodeJobState::Running {
        app.watermark_job.drain();
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(app.watermark_job.take_result().unwrap().is_err());
    assert_eq!(app.editor_workspace.as_ref().unwrap().manifest(), &before);
}

#[test]
fn enabled_header_back_really_leaves_editor_and_pauses_playback() {
    let (_directory, mut app) = app_fixture();
    let context = context(1.0);
    let size = egui::vec2(1280.0, 900.0);
    for _ in 0..3 {
        app_frame(&context, &mut app, size, vec![]);
    }
    let output = app_frame(&context, &mut app, size, vec![]);
    let play = text_rect(&output, language("en").text(Message::EditorPlay));
    click_app(&context, &mut app, size, play.center());
    assert!(app.editor_ui_state.playback.is_some());
    let output = app_frame(&context, &mut app, size, vec![]);
    let back = text_rect(&output, language("en").text(Message::BackToHome));
    click_app(&context, &mut app, size, back.center());
    assert_eq!(app.view, crate::AppView::Landing);
    assert!(app.editor_ui_state.playback.is_none());
}

#[test]
fn switching_editor_layout_rolls_back_held_drawing_and_preserves_confirmed_draft() {
    let (_directory, mut app) = app_fixture();
    let context = context(1.0);
    let wide = egui::vec2(1280.0, 900.0);
    app.editor_ui_state
        .drawing_overlay
        .begin_for_selection(app.editor_workspace.as_ref().unwrap())
        .unwrap();
    for _ in 0..3 {
        app_frame(&context, &mut app, wide, vec![]);
    }
    let output = app_frame(&context, &mut app, wide, vec![]);
    let start = preview_rect(&output).center();
    app_frame(&context, &mut app, wide, pointer(start, true));
    app_frame(
        &context,
        &mut app,
        wide,
        vec![egui::Event::PointerMoved(start + egui::vec2(20.0, 10.0))],
    );
    assert!(!app.editor_ui_state.drawing_overlay.points.is_empty());
    let narrow = egui::vec2(680.0, 900.0);
    let output = app_frame(&context, &mut app, narrow, vec![]);
    assert!(app.editor_ui_state.drawing_overlay.points.is_empty());
    let point = preview_rect(&output).center();
    app_frame(&context, &mut app, narrow, pointer(point, false));
    assert_ne!(
        app.editor_ui_state.drawing_overlay.phase,
        crate::DrawingDraftPhase::Ready
    );
    for _ in 0..3 {
        app_frame(&context, &mut app, narrow, vec![]);
    }
    let output = app_frame(&context, &mut app, narrow, vec![]);
    let point = preview_rect(&output).center();
    app_frame(&context, &mut app, narrow, pointer(point, true));
    app_frame(
        &context,
        &mut app,
        narrow,
        pointer(point + egui::vec2(15.0, 10.0), false),
    );
    app_frame(&context, &mut app, narrow, vec![]);
    assert_eq!(
        app.editor_ui_state.drawing_overlay.phase,
        crate::DrawingDraftPhase::Ready
    );
    let confirmed = app.editor_ui_state.drawing_overlay.points.clone();
    app_frame(&context, &mut app, wide, vec![]);
    assert_eq!(app.editor_ui_state.drawing_overlay.points, confirmed);
}

fn text_rect(output: &egui::FullOutput, value: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape
                && text.galley.text() == value
            {
                // Wrapped horizontal labels can include leading empty space for
                // preceding buttons. Inspect painted glyph bounds, not that indent.
                let rect = text.visual_bounding_rect();
                assert!(
                    shape.clip_rect.contains_rect(rect),
                    "clipped {value}: {rect:?} / {:?}",
                    shape.clip_rect
                );
                Some(rect)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("missing visible label {value}"))
}

fn header_frame(
    context: &egui::Context,
    size: egui::Vec2,
    tag: &str,
    enabled: bool,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, [bool; 2]) {
    let mut actions = [false; 2];
    let output = context.run(input(context, size, events), |context| {
        egui::TopBottomPanel::top("app_header").show(context, |ui| {
            actions[0] = editor_shell_layout::header(ui, Some(enabled), language(tag), |ui| {
                actions[1] = ui
                    .button(language(tag).text(Message::LanguageSettingsTitle))
                    .clicked();
            });
        });
    });
    (output, actions)
}

#[test]
fn header_keeps_brand_back_and_language_visible_and_clickable_at_narrow_zoom() {
    for (width, zoom, font_scale) in [
        (680.0, 1.0, 1.0),
        (680.0, 1.5, 1.0),
        (1280.0, 1.0, 1.0),
        (480.0, 1.5, 1.0),
        (640.0, 1.0, 2.0),
        (1280.0, 1.0, 2.0),
    ] {
        let context = context(zoom);
        context.style_mut(|style| {
            for font in style.text_styles.values_mut() {
                font.size *= font_scale;
            }
        });
        let size = egui::vec2(width, 760.0);
        for tag in ["en", "zh", "en"] {
            for _ in 0..3 {
                header_frame(&context, size, tag, true, vec![]);
            }
            let (output, _) = header_frame(&context, size, tag, true, vec![]);
            let back = text_rect(&output, language(tag).text(Message::BackToHome));
            let brand = text_rect(&output, crate::APP_NAME);
            let locale = text_rect(&output, language(tag).text(Message::LanguageSettingsTitle));
            assert!(
                !brand.intersects(back) && !brand.intersects(locale) && !back.intersects(locale),
                "{width}/{zoom}/{tag}: back={back:?}, brand={brand:?}, language={locale:?}"
            );
            assert!(locale.right() <= width / zoom && brand.right() <= width / zoom);
            for (point, expected) in [
                (back.center(), [true, false]),
                (locale.center(), [false, true]),
            ] {
                header_frame(&context, size, tag, true, pointer(point, true));
                assert_eq!(
                    header_frame(&context, size, tag, true, pointer(point, false)).1,
                    expected
                );
            }
            header_frame(&context, size, tag, false, pointer(back.center(), true));
            assert_eq!(
                header_frame(&context, size, tag, false, pointer(back.center(), false)).1,
                [false; 2]
            );
        }
    }
}

#[test]
fn original_nested_scroll_reproduces_64_point_collapse_and_inline_shell_expands() {
    let context = context(1.5);
    let mut before = None;
    let mut after = None;
    for _ in 0..3 {
        let _ = context.run(
            input(&context, egui::vec2(680.0, 760.0), vec![]),
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    crate::show_editor_scroll_area(ui, |ui| {
                        // The real ScrollArea passes finite height to its content.
                        // This exceeds it, just as chrome + stacked preview does.
                        ui.allocate_space(egui::vec2(1.0, 800.0));
                        let remaining = ui.available_rect_before_wrap();
                        let old = egui::ScrollArea::vertical()
                            .id_salt("old-inspector")
                            .max_height(380.0)
                            .show(ui, |ui| {
                                ui.allocate_space(egui::vec2(1.0, 600.0));
                            });
                        before = Some((old.inner_rect.height(), remaining));
                        let top = ui.cursor().min.y;
                        let body_height =
                            editor_shell_layout::inspector(ui, InspectorLayout::Stacked, |ui| {
                                ui.allocate_space(egui::vec2(1.0, 600.0));
                                ui.min_size().y
                            });
                        after = Some((ui.cursor().min.y - top, body_height));
                    });
                });
            },
        );
    }
    let (old_height, remaining) = before.unwrap();
    assert_eq!(
        old_height.to_bits(),
        64.0_f32.to_bits(),
        "remaining={remaining:?}, old_height={old_height}"
    );
    let (visible, body_height) = after.unwrap();
    assert!(
        visible >= 600.0 && visible >= body_height,
        "inline visible={visible}, body_height={body_height}"
    );
}
