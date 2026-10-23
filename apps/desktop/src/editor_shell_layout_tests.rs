//! Real egui layout/input regressions; no native display or host configuration.

use eframe::egui;
use gif_from_screen_localization::{Localizer, Message, find_language};

use crate::editor_shell_layout::{self, InspectorLayout};

fn language(tag: &str) -> Localizer {
    Localizer::new(find_language(tag).unwrap())
}

fn context(zoom: f32) -> egui::Context {
    let context = egui::Context::default();
    crate::appearance::configure(&context);
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one real-layout matrix compares ready, first-load, fallback and long-error geometry"
)]
fn pending_and_failed_previews_keep_the_stacked_inspector_in_place() {
    for (width, font_scale, tag, zoom) in [
        (320.0, 1.0, "en", crate::editor_canvas::PreviewZoom::Fit),
        (440.0, 1.5, "zh", crate::editor_canvas::PreviewZoom::Fit),
        (320.0, 1.0, "en", crate::editor_canvas::PreviewZoom::Double),
    ] {
        let (_directory, mut app) = app_fixture();
        let workspace = app.editor_workspace.as_mut().unwrap();
        let frame_id = workspace.selection().current().unwrap();
        let mut positions = Vec::new();
        for phase in ["ready", "first-load", "cached-pending", "error"] {
            let context = context(1.0);
            context.style_mut(|style| {
                for font in style.text_styles.values_mut() {
                    font.size *= font_scale;
                }
            });
            let mut cache = crate::EditorPreviewCache::new();
            let mut state = crate::EditorUiState::default();
            state.canvas.zoom = zoom;
            let mut motion = crate::MotionTools::default();
            let mut vector = crate::vector_shapes::VectorShapes::default();
            let preview = if zoom == crate::editor_canvas::PreviewZoom::Fit {
                cache.preview(
                    workspace.active_project(),
                    frame_id,
                    &context,
                    crate::EDITOR_PREVIEW_MAX_SIZE,
                )
            } else {
                cache.native_presentation_preview(
                    workspace.active_project(),
                    frame_id,
                    None,
                    &context,
                )
            }
            .unwrap();
            // Bind the cache to the exact production request without starting
            // a worker, so this layout regression is deterministic.
            cache
                .request_presentation_preview(
                    workspace.active_project(),
                    frame_id,
                    None,
                    &context,
                    crate::EDITOR_PREVIEW_MAX_SIZE,
                    zoom != crate::editor_canvas::PreviewZoom::Fit,
                    false,
                )
                .unwrap();
            if phase == "first-load" {
                cache = crate::EditorPreviewCache::new();
            }
            let target = crate::EditorPreviewTarget {
                frame_id,
                transition: None,
                cinemagraph: false,
                input_enabled: true,
            };
            let long_error = "A deliberately long asset path and read failure. ".repeat(80);
            let mut inspector_y = 0.0;
            let _ = context.run(
                input(&context, egui::vec2(width, 640.0), Vec::new()),
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        editor_shell_layout::work_area(ui, InspectorLayout::Stacked, |columns| {
                            let ui = &mut columns[0];
                            state.canvas.show_zoom(ui, language(tag));
                            crate::show_editor_preview_status(
                                ui,
                                phase != "ready" && phase != "error",
                                language(tag),
                            );
                            if phase == "ready" {
                                crate::draw_editor_preview(
                                    ui,
                                    workspace,
                                    &mut state,
                                    (&mut motion, &mut vector),
                                    &preview,
                                    target,
                                    language(tag),
                                );
                            } else {
                                crate::show_unavailable_editor_preview(
                                    ui,
                                    workspace,
                                    &mut cache,
                                    &mut state,
                                    (&mut motion, &mut vector),
                                    (target, (phase == "error").then_some(long_error.as_str())),
                                    language(tag),
                                );
                            }
                            ui.separator();
                            inspector_y = ui.cursor().top();
                            ui.label("Inspector sentinel");
                        });
                    });
                },
            );
            positions.push((phase, inspector_y));
        }
        let ready = positions[0].1;
        for (phase, y) in positions {
            assert!(
                (y - ready).abs() <= 1.0,
                "{tag} {width}px {zoom:?} {phase}: inspector moved from {ready} to {y}"
            );
        }
    }
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
fn frame_workspace_grows_with_the_window_and_keeps_its_timeline_visible() {
    for (size, zoom, tag) in [
        (egui::vec2(960.0, 680.0), 1.0, "en"),
        (egui::vec2(1366.0, 900.0), 1.0, "zh"),
        (egui::vec2(1920.0, 1080.0), 1.0, "en"),
        (egui::vec2(1280.0, 900.0), 1.5, "zh"),
    ] {
        let (_directory, mut app) = app_fixture();
        let context = context(zoom);
        app.language_settings = crate::preferences::LanguageSettings::with_language(
            gif_from_screen_localization::LanguagePreference::explicit(tag).unwrap(),
        );
        app.sync_ui_language(language(tag));
        // Prime the real asset synchronously; the layout under test still goes
        // through the production presentation request and complete app shell.
        let workspace = app.editor_workspace.as_ref().unwrap();
        app.editor_preview_cache
            .preview(
                workspace.active_project(),
                workspace.selection().current().unwrap(),
                &context,
                crate::EDITOR_PREVIEW_MAX_SIZE,
            )
            .unwrap();
        for _ in 0..5 {
            app_frame(&context, &mut app, size, vec![]);
        }
        let output = app_frame(&context, &mut app, size, vec![]);
        let image = preview_rect(&output);
        let footer =
            visible_text(&output, language(tag).text(Message::EditorJump)).unwrap_or_else(|| {
                panic!("{size:?}/{zoom}/{tag}: timeline navigation must be visible")
            });
        let screen = size / zoom;
        assert!(
            image.height() > screen.y * if zoom > 1.0 { 0.27 } else { 0.38 },
            "{size:?}/{zoom}: {image:?}"
        );
        assert!(footer.top() > image.bottom());
        assert!(
            screen.y - footer.bottom() < 100.0,
            "unused bottom space: {footer:?}"
        );
        assert!(
            text_rect(
                &output,
                language(tag).text(Message::EditorAdvancedSelection)
            )
            .bottom()
                < image.top()
        );
    }
}

#[test]
fn page_scrolling_does_not_enlarge_the_editor_canvas_and_push_controls_away() {
    let context = context(1.0);
    let mut heights = Vec::new();
    for offset in [0.0, 80.0, 240.0] {
        let _ = context.run(
            input(&context, egui::vec2(1000.0, 800.0), vec![]),
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    editor_shell_layout::set_page_scroll(context, offset);
                    let mut rect = ui.available_rect_before_wrap();
                    rect.min.y -= offset;
                    let child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    heights.push(editor_shell_layout::available_work_height(&child));
                });
            },
        );
    }
    assert!(
        heights
            .iter()
            .all(|height| (*height - heights[0]).abs() < 0.5)
    );
}

#[test]
fn real_editor_uses_full_width_for_frames_and_preserves_tool_focus_across_resize_and_language() {
    let (_directory, mut app) = app_fixture();
    let context = context(1.0);
    let wide = egui::vec2(1280.0, 900.0);
    for _ in 0..3 {
        app_frame(&context, &mut app, wide, vec![]);
    }
    let output = app_frame(&context, &mut app, wide, vec![]);
    let image = preview_rect(&output);
    assert!(
        text_rect(
            &output,
            language("en").text(Message::EditorAdvancedSelection)
        )
        .bottom()
            < image.top()
    );
    assert!(
        image.height() > 360.0,
        "image still has a fixed height cap: {image:?}"
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
    let clear = text_rect(&output, language("en").text(Message::EditorClearSelection));
    click_app(&context, &mut app, size, clear.center());
    assert_eq!(app.editor_workspace.as_ref().unwrap().selection().len(), 1);
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
fn redesigned_home_actions_fit_narrow_zoom_and_full_cards_dispatch_the_correct_route() {
    for (width, height, zoom) in [
        (1280.0, 900.0, 1.0),
        (680.0, 900.0, 1.5),
        (480.0, 900.0, 1.5),
        (680.0, 440.0, 1.5),
        (600.0, 360.0, 1.0),
        (600.0, 360.0, 1.5),
        (600.0, 260.0, 1.0),
        (600.0, 260.0, 1.5),
    ] {
        for tag in ["en", "zh"] {
            let context = context(zoom);
            crate::appearance::configure(&context);
            context
                .style_mut(|style| style.scroll_animation = egui::style::ScrollAnimation::none());
            let mut app = crate::GifFromScreenApp::default();
            app.language_settings = crate::preferences::LanguageSettings::with_language(
                gif_from_screen_localization::LanguagePreference::explicit(tag).unwrap(),
            );
            let size = egui::vec2(width, height);
            for (message, route) in [
                (Message::HomeRecorderTile, crate::AppView::ScreenRecorder),
                (Message::HomeCameraTile, crate::AppView::CameraRecorder),
                (Message::HomeBoardTile, crate::AppView::BoardRecorder),
                (Message::HomeEditorTile, crate::AppView::Editor),
            ] {
                app.view = crate::AppView::Landing;
                for _ in 0..3 {
                    app_frame(&context, &mut app, size, vec![]);
                }
                scroll_to_text(&context, &mut app, size, language(tag).text(message));
                // Wheel input is smoothed separately from scroll-to animations.
                // Let that motion finish before choosing a physical click point.
                for _ in 0..30 {
                    app_frame(&context, &mut app, size, vec![]);
                }
                let output = app_frame(&context, &mut app, size, vec![]);
                let title = visible_text(&output, language(tag).text(message)).unwrap();
                assert!(
                    title.left() >= 0.0 && title.right() <= width / zoom,
                    "{width}/{zoom}/{tag}: {message:?} {title:?}"
                );
                // Click the empty corner of a large action card, beyond its title.
                let card = output
                    .shapes
                    .iter()
                    .filter_map(|shape| {
                        if let egui::Shape::Rect(rect) = &shape.shape {
                            (rect.rect.height() >= 60.0
                                && rect.rect.height() < 300.0
                                && rect.rect.contains_rect(title)
                                && rect.rect.width() < width / zoom * 0.4
                                && shape.clip_rect.contains_rect(rect.rect))
                            .then_some(rect.rect)
                        } else {
                            None
                        }
                    })
                    .min_by(|a, b| a.area().total_cmp(&b.area()));
                let point = card.map_or(title.center(), |rect| {
                    rect.right_bottom() - egui::vec2(9.0, 9.0)
                });
                let clicked = click_app(&context, &mut app, size, point);
                assert_eq!(
                    app.view,
                    route,
                    "{width}/{zoom}/{tag}: {message:?}, title={title:?}, card={card:?}, point={point:?}, after={:?}",
                    visible_text(&clicked, language(tag).text(message))
                );
            }
        }
    }
}

#[test]
fn header_export_reveals_configuration_without_exporting_or_mutating_the_project() {
    let (_directory, mut app) = app_fixture();
    let context = context(1.5);
    crate::appearance::configure(&context);
    let size = egui::vec2(680.0, 900.0);
    let before = app.editor_workspace.as_ref().unwrap().manifest().clone();
    for _ in 0..3 {
        app_frame(&context, &mut app, size, vec![]);
    }
    let output = app_frame(&context, &mut app, size, vec![]);
    let export = text_rect(&output, language("en").text(Message::ExportTitle));
    click_app(&context, &mut app, size, export.center());
    for _ in 0..12 {
        app_frame(&context, &mut app, size, vec![]);
    }
    let output = app_frame(&context, &mut app, size, vec![]);
    assert!(visible_text(&output, language("en").text(Message::ExportOutputLabel)).is_some());
    assert_eq!(app.export_job.state(), crate::ExportJobState::Idle);
    assert_eq!(app.editor_workspace.as_ref().unwrap().manifest(), &before);
    assert!(app.export_scroll_target.is_none());
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

/// Inspect actual glyph rows, not widget rectangles (which include whitespace).
pub(crate) fn text_layout_issues(output: &egui::FullOutput) -> Vec<String> {
    let mut rows = Vec::new();
    let mut issues = Vec::new();
    for (index, shape) in output.shapes.iter().enumerate() {
        if let egui::Shape::Text(text) = &shape.shape {
            for row in &text.galley.rows {
                let rect = row
                    .visuals
                    .mesh_bounds
                    .translate(text.pos.to_vec2() + row.pos.to_vec2());
                let visible = rect.intersect(shape.clip_rect);
                if !visible.is_positive() {
                    continue;
                }
                if rect.left() < shape.clip_rect.left() - 1.0
                    || rect.right() > shape.clip_rect.right() + 1.0
                {
                    // Single-line text fields and timeline cells intentionally scroll/clip.
                    if text.galley.job.wrap.max_width.is_finite() {
                        issues.push(format!(
                            "clipped {:?} at {rect:?} by {:?}",
                            text.galley.text(),
                            shape.clip_rect
                        ));
                    }
                }
                rows.push((index, text.galley.text(), visible));
            }
        }
    }
    for (i, (id, text, rect)) in rows.iter().enumerate() {
        for (other_id, other, other_rect) in &rows[i + 1..] {
            let overlap = rect.intersect(*other_rect);
            if id != other_id && overlap.width() > 1.0 && overlap.height() > 1.0 {
                issues.push(format!(
                    "overlap {text:?} {rect:?} with {other:?} {other_rect:?}"
                ));
            }
        }
    }
    issues
}

fn audit_scroll(
    ctx: &egui::Context,
    app: &mut crate::GifFromScreenApp,
    size: egui::Vec2,
    position: egui::Pos2,
) -> Vec<String> {
    let mut issues = Vec::new();
    for delta in std::iter::repeat_n(-250.0, 10).chain([100_000.0]) {
        app_frame(
            ctx,
            app,
            size,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, delta),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        // Let scroll smoothing finish before judging geometry.
        for _ in 0..12 {
            app_frame(ctx, app, size, vec![]);
        }
        issues.extend(text_layout_issues(&app_frame(ctx, app, size, vec![])));
    }
    issues.sort();
    issues.dedup();
    issues
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one matrix exercises the actual page routes and editor tabs with identical rendering checks"
)]
fn page_text_layout_audit() {
    let mut issues = Vec::new();
    for (width, font_scale, tag) in [
        (960.0, 1.0, "en"),
        (960.0, 1.0, "zh"),
        (440.0, 1.0, "en"),
        (440.0, 1.5, "zh"),
        (960.0, 1.5, "en"),
        (320.0, 2.0, "zh"),
    ] {
        for view in [
            crate::AppView::OpenProject,
            crate::AppView::ImportGif,
            crate::AppView::ImportImage,
            crate::AppView::ImportImageSequence,
            crate::AppView::ImportVideo,
            crate::AppView::NewBlankAnimation,
            crate::AppView::CameraRecorder,
            crate::AppView::BoardRecorder,
            crate::AppView::Automation,
            crate::AppView::ScreenRecorder,
            crate::AppView::Editor,
        ] {
            let (_dir, mut app) = app_fixture();
            let ctx = context(1.0);
            crate::appearance::configure(&ctx);
            ctx.style_mut(|style| {
                for font in style.text_styles.values_mut() {
                    font.size *= font_scale;
                }
            });
            app.view = view;
            app.language_settings = crate::preferences::LanguageSettings::with_language(
                gif_from_screen_localization::LanguagePreference::explicit(tag).unwrap(),
            );
            let size = egui::vec2(width, 1000.0);
            for _ in 0..4 {
                app_frame(&ctx, &mut app, size, vec![]);
            }
            if view == crate::AppView::ScreenRecorder {
                let output = app_frame(&ctx, &mut app, size, vec![]);
                let pos = visible_text(&output, language(tag).text(Message::UiAdvancedSettings))
                    .unwrap()
                    .center();
                click_app(&ctx, &mut app, size, pos);
                for _ in 0..4 {
                    app_frame(&ctx, &mut app, size, vec![]);
                }
            }
            let output = app_frame(&ctx, &mut app, size, vec![]);
            for issue in text_layout_issues(&output) {
                issues.push(format!("{view:?}/{width}/{font_scale}/{tag}: {issue}"));
            }
            for issue in audit_scroll(&ctx, &mut app, size, egui::pos2(width - 30.0, 970.0)) {
                issues.push(format!("{view:?}/{width}/{font_scale}/{tag}: {issue}"));
            }
            if view == crate::AppView::Editor {
                for tab in [
                    Message::EditorTimingTab,
                    Message::EditorTransformTab,
                    Message::EditorEffectsTab,
                    Message::EditorOverlaysTab,
                    Message::EditorProjectTab,
                ] {
                    let output = app_frame(&ctx, &mut app, size, vec![]);
                    let pos = visible_text(&output, language(tag).text(tab))
                        .unwrap()
                        .center();
                    click_app(&ctx, &mut app, size, pos);
                    for _ in 0..3 {
                        app_frame(&ctx, &mut app, size, vec![]);
                    }
                    let output = app_frame(&ctx, &mut app, size, vec![]);
                    for issue in text_layout_issues(&output) {
                        issues.push(format!("{tab:?}/{width}/{font_scale}/{tag}: {issue}"));
                    }
                    let point = if width >= 900.0 {
                        egui::pos2(width - 60.0, 300.0)
                    } else {
                        egui::pos2(width - 30.0, 970.0)
                    };
                    for issue in audit_scroll(&ctx, &mut app, size, point) {
                        issues.push(format!("{tab:?}/{width}/{font_scale}/{tag}: {issue}"));
                    }
                    if tab == Message::EditorOverlaysTab {
                        for tool in [
                            crate::editor_ui::OverlayTool::Image,
                            crate::editor_ui::OverlayTool::Shape,
                            crate::editor_ui::OverlayTool::Drawing,
                        ] {
                            app.editor_ui_state.overlay_tool = tool;
                            for _ in 0..4 {
                                app_frame(&ctx, &mut app, size, vec![]);
                            }
                            let output = app_frame(&ctx, &mut app, size, vec![]);
                            for issue in text_layout_issues(&output) {
                                issues
                                    .push(format!("{tool:?}/{width}/{font_scale}/{tag}: {issue}"));
                            }
                            for issue in audit_scroll(&ctx, &mut app, size, point) {
                                issues
                                    .push(format!("{tool:?}/{width}/{font_scale}/{tag}: {issue}"));
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(issues.is_empty(), "{}", issues.join("\n"));
}

#[test]
fn export_form_text_layout_audit() {
    for (width, scale, tag) in [(960.0, 1.0, "en"), (440.0, 1.5, "en"), (320.0, 2.0, "zh")] {
        let ctx = context(1.0);
        crate::appearance::configure(&ctx);
        ctx.style_mut(|style| {
            for font in style.text_styles.values_mut() {
                font.size *= scale;
            }
        });
        let mut settings = crate::EditorExportSettings {
            quantizer: crate::ExportQuantizerChoice::Custom,
            loop_choice: crate::ExportLoopChoice::Finite,
            ..Default::default()
        };
        let mut path = "/tmp/long-export-path-with-中文-animation.gif".to_owned();
        let mut output = None;
        for _ in 0..4 {
            output = Some(
                ctx.run(input(&ctx, egui::vec2(width, 4000.0), vec![]), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        crate::show_export_configuration(
                            ui,
                            &mut path,
                            &mut settings,
                            100,
                            language(tag),
                        );
                    });
                }),
            );
        }
        let issues = text_layout_issues(&output.unwrap());
        assert!(
            issues.is_empty(),
            "{width}/{scale}/{tag}: {}",
            issues.join("\n")
        );
    }
}
