use super::*;
use gif_from_screen_capture::{CaptureSession, CaptureSessionState, FramePoll};
use gif_from_screen_workflow::{RecordingControl, RecordingFrameSink, RecordingFrameSinkError};

fn controller(context: &egui::Context) -> WaylandCropController {
    WaylandCropController {
        texture: context.load_texture(
            "wayland-controller-test",
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::NEAREST,
        ),
        source_size: gif_from_screen_capture::PhysicalSize::new(640, 480).unwrap(),
        region: PhysicalRect::new(10, 20, 100, 80).unwrap(),
        drag_start: None,
        drag_current: None,
        drag_initial_region: None,
    }
}

fn geometry_input(size: egui::Vec2, maximized: Option<bool>) -> egui::RawInput {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        ..egui::RawInput::default()
    };
    input
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .maximized = maximized;
    input
}

fn geometry_frame(
    context: &egui::Context,
    app: &mut GifFromScreenApp,
    size: egui::Vec2,
    maximized: Option<bool>,
) -> Vec<egui::ViewportCommand> {
    context
        .run(geometry_input(size, maximized), |context| {
            app.restore_main_window_if_requested(context);
        })
        .viewport_output[&egui::ViewportId::ROOT]
        .commands
        .clone()
}

#[test]
fn compact_geometry_is_bounded_and_waits_for_the_unmaximize_configuration() {
    use super::wayland_controller_layout::{GeometryTransition, compact_size};
    for (available, expected) in [
        (egui::vec2(1280.0, 720.0), egui::vec2(720.0, 480.0)),
        (egui::vec2(640.0, 360.0), egui::vec2(640.0, 360.0)),
        (egui::vec2(280.0, 200.0), egui::vec2(280.0, 200.0)),
    ] {
        assert_eq!(compact_size(available), expected);
    }
    let now = Instant::now();
    let original = egui::vec2(1280.0, 720.0);
    let compact = egui::vec2(720.0, 480.0);
    let mut transition = GeometryTransition::compact(original, now);
    let first = transition.advance(original, Some(true), 1.0, now);
    assert_eq!(
        first.commands,
        [
            egui::ViewportCommand::MinInnerSize(egui::vec2(320.0, 240.0)),
            egui::ViewportCommand::Maximized(false)
        ]
    );
    assert!(!first.finished);
    assert!(
        transition
            .advance(original, Some(true), 1.0, now + Duration::from_millis(10))
            .commands
            .is_empty()
    );
    let resize = transition.advance(original, Some(false), 1.0, now + Duration::from_millis(20));
    assert_eq!(resize.commands, [egui::ViewportCommand::InnerSize(compact)]);
    assert!(!resize.finished);
    let acknowledged =
        transition.advance(compact, Some(false), 1.0, now + Duration::from_millis(30));
    assert!(acknowledged.finished && !acknowledged.timed_out);
    assert!(acknowledged.commands.is_empty());
}

#[test]
fn pending_geometry_uses_stable_native_logical_units_across_ui_zoom_changes() {
    use super::wayland_controller_layout::GeometryTransition;
    let now = Instant::now();
    let original = egui::vec2(1040.0, 760.0);
    let compact = egui::vec2(720.0, 480.0);
    let mut entry = GeometryTransition::compact(original, now);
    let first = entry.advance(original, Some(true), 2.0, now);
    assert!(
        first
            .commands
            .contains(&egui::ViewportCommand::MinInnerSize(egui::vec2(
                160.0, 120.0
            )))
    );
    let resized = entry.advance(original, Some(false), 1.5, now + Duration::from_millis(10));
    assert_eq!(
        resized.commands,
        [egui::ViewportCommand::InnerSize(compact / 1.5)]
    );
    assert!(
        entry
            .advance(compact, Some(false), 0.75, now + Duration::from_millis(20))
            .finished
    );
    let mut restore = GeometryTransition::restore(original, Some(false), now);
    assert!(
        restore
            .advance(compact, Some(false), 2.0, now)
            .commands
            .contains(&egui::ViewportCommand::InnerSize(original / 2.0))
    );
    assert!(
        restore
            .advance(original, Some(false), 1.25, now + Duration::from_millis(10))
            .finished
    );
}

#[test]
fn restoring_a_zoomed_snapshot_does_not_multiply_in_monitor_dpi() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    app.main_window_snapshot = Some(MainWindowSnapshot {
        position: None,
        size: egui::vec2(520.0, 380.0),
        maximized: Some(false),
        restore: MainWindowRestore::Wayland {
            pending: None,
            restoring: false,
            zoom_factor: 2.0,
        },
    });
    app.restore_main_window = true;
    let mut input = geometry_input(egui::vec2(720.0, 480.0), Some(false));
    input
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .native_pixels_per_point = Some(3.0);
    let output = context.run(input, |context| {
        assert!((context.zoom_factor() - 1.0).abs() < f32::EPSILON);
        app.restore_main_window_if_requested(context);
    });
    assert!(
        output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .contains(&egui::ViewportCommand::InnerSize(egui::vec2(1040.0, 760.0)))
    );
}

#[test]
fn compact_timeout_is_visible_nonblocking_and_does_not_repeat_window_requests() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    let size = egui::vec2(1280.0, 720.0);
    app.wayland_crop_controller = Some(controller(&context));
    app.notice = Some("Existing recording status.".to_owned());
    app.main_window_snapshot = Some(MainWindowSnapshot {
        position: None,
        size,
        maximized: Some(true),
        restore: MainWindowRestore::Wayland {
            pending: Some(wayland_controller_layout::GeometryTransition::compact(
                size,
                Instant::now().checked_sub(Duration::from_secs(3)).unwrap(),
            )),
            restoring: false,
            zoom_factor: 1.0,
        },
    });
    let _ = geometry_frame(&context, &mut app, size, Some(true));
    let notice = app.notice.as_deref().unwrap();
    assert!(notice.contains("Existing recording status."));
    assert!(notice.contains("did not confirm") && notice.contains("manually"));
    assert!(app.wayland_crop_controller.is_some());
    assert!(matches!(
        app.main_window_snapshot.unwrap().restore,
        MainWindowRestore::Wayland { pending: None, .. }
    ));
    assert!(geometry_frame(&context, &mut app, size, Some(true)).is_empty());
}

#[test]
fn restoration_timeout_releases_the_pending_request_without_blocking_the_editor() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    let original = egui::vec2(1040.0, 760.0);
    let compact = egui::vec2(720.0, 480.0);
    app.view = AppView::Editor;
    app.restore_main_window = true;
    app.main_window_snapshot = Some(MainWindowSnapshot {
        position: None,
        size: original,
        maximized: Some(true),
        restore: MainWindowRestore::Wayland {
            pending: Some(wayland_controller_layout::GeometryTransition::restore(
                original,
                Some(true),
                Instant::now().checked_sub(Duration::from_secs(3)).unwrap(),
            )),
            restoring: true,
            zoom_factor: 1.0,
        },
    });
    let commands = geometry_frame(&context, &mut app, compact, Some(false));
    assert!(commands.contains(&egui::ViewportCommand::Maximized(true)));
    assert_eq!(app.view, AppView::Editor);
    assert!(!app.restore_main_window && app.main_window_snapshot.is_none());
    assert!(app.notice.as_deref().unwrap().contains("did not confirm"));
    assert!(geometry_frame(&context, &mut app, compact, Some(false)).is_empty());
}

#[test]
fn exiting_at_each_compact_phase_restores_original_geometry_without_late_shrink() {
    for acknowledged_frames in 0..=2 {
        for maximized in [Some(true), Some(false), None] {
            check_exit_during_compact(acknowledged_frames, maximized);
        }
    }
}

fn check_exit_during_compact(acknowledged_frames: u8, maximized: Option<bool>) {
    let original = egui::vec2(1040.0, 760.0);
    let compact = egui::vec2(720.0, 480.0);
    let (context, mut app) = preparation_fixture(1.0);
    let _ = context.run(geometry_input(original, maximized), |context| {
        app.enter_wayland_crop_controller(context).unwrap();
    });
    if acknowledged_frames >= 1 {
        let _ = geometry_frame(&context, &mut app, original, Some(false));
    }
    if acknowledged_frames >= 2 {
        let _ = geometry_frame(&context, &mut app, compact, Some(false));
    }
    app.close_wayland_crop_controller();
    let first = geometry_frame(&context, &mut app, compact, Some(false));
    assert!(first.contains(&egui::ViewportCommand::InnerSize(original)));
    assert!(!first.iter().any(
        |command| matches!(command, egui::ViewportCommand::InnerSize(size) if *size == compact)
    ));
    // Repeat an exit while restoration is pending, followed by a late compact
    // configure. It must not restart the old transition or reset its deadline.
    app.close_wayland_crop_controller();
    assert!(geometry_frame(&context, &mut app, compact, Some(false)).is_empty());
    assert!(app.restore_main_window);
    let acknowledged = geometry_frame(&context, &mut app, original, Some(false));
    if maximized == Some(true) {
        assert!(acknowledged.contains(&egui::ViewportCommand::Maximized(true)));
        assert!(app.restore_main_window);
        let _ = geometry_frame(&context, &mut app, original, Some(true));
    }
    assert!(!app.restore_main_window);
    assert!(app.main_window_snapshot.is_none());
    assert!(geometry_frame(&context, &mut app, original, maximized).is_empty());
    assert!(first.iter().all(|command| !matches!(
        command,
        egui::ViewportCommand::Visible(_) | egui::ViewportCommand::OuterPosition(_)
    )));
}

#[test]
fn completed_compact_transition_does_not_override_manual_window_resizing() {
    let original = egui::vec2(1040.0, 760.0);
    let compact = egui::vec2(720.0, 480.0);
    let (context, mut app) = preparation_fixture(1.0);
    let _ = context.run(geometry_input(original, Some(false)), |context| {
        app.enter_wayland_crop_controller(context).unwrap();
    });
    let _ = geometry_frame(&context, &mut app, compact, Some(false));
    assert!(geometry_frame(&context, &mut app, egui::vec2(530.0, 350.0), Some(false)).is_empty());
}

#[test]
fn a_new_controller_during_restoration_keeps_the_original_launcher_snapshot() {
    let original = egui::vec2(1040.0, 760.0);
    let compact = egui::vec2(720.0, 480.0);
    let (context, mut app) = preparation_fixture(1.0);
    let _ = context.run(geometry_input(original, Some(true)), |context| {
        app.enter_wayland_crop_controller(context).unwrap();
    });
    app.close_wayland_crop_controller();
    let _ = geometry_frame(&context, &mut app, compact, Some(false));
    let (_, mut next) = preparation_fixture(1.0);
    app.wayland_frozen_preview = next.wayland_frozen_preview.take();
    let _ = context.run(geometry_input(compact, Some(false)), |context| {
        app.enter_wayland_crop_controller(context).unwrap();
    });
    let snapshot = app.main_window_snapshot.unwrap();
    assert_eq!(snapshot.size, original);
    assert_eq!(snapshot.maximized, Some(true));
    assert!(!app.restore_main_window);
    assert!(matches!(
        snapshot.restore,
        MainWindowRestore::Wayland {
            restoring: false,
            ..
        }
    ));
}

#[test]
fn x11_shell_restoration_keeps_its_existing_position_and_decoration_commands() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    let original = egui::vec2(960.0, 640.0);
    let position = egui::pos2(17.0, 23.0);
    app.main_window_snapshot = Some(MainWindowSnapshot {
        position: Some(position),
        size: original,
        maximized: Some(true),
        restore: MainWindowRestore::X11Geometry,
    });
    app.restore_main_window = true;
    assert_eq!(
        geometry_frame(&context, &mut app, egui::vec2(720.0, 480.0), Some(false)),
        [
            egui::ViewportCommand::Title(APP_NAME.to_owned()),
            egui::ViewportCommand::Decorations(true),
            egui::ViewportCommand::MinInnerSize(egui::vec2(680.0, 440.0)),
            egui::ViewportCommand::InnerSize(original),
            egui::ViewportCommand::Maximized(true),
            egui::ViewportCommand::OuterPosition(position),
            egui::ViewportCommand::Focus,
        ]
    );
    assert!(!app.restore_main_window && app.main_window_snapshot.is_none());
}

fn toolbar_frame(
    context: &egui::Context,
    controller: &mut WaylandCropController,
    stage: RecorderStage,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, RecorderOverlayAction) {
    let mut action = RecorderOverlayAction::None;
    let mut input = geometry_input(size, Some(false));
    input.events = events;
    let output = context.run(input, |context| {
        action = draw_wayland_crop_controller(
            context,
            stage,
            Some(WorkflowProgress {
                phase: WorkflowPhase::Capturing,
                frames_captured: 7,
                capture_duration: Duration::from_secs(30),
                playback_duration: Duration::from_millis(700),
                encode: None,
            }),
            controller,
            true,
            None,
            false,
        )
        .action;
    });
    (output, action)
}

#[test]
fn compact_controller_buttons_remain_visible_and_clickable_with_wrapped_large_fonts() {
    for size in [
        egui::vec2(720.0, 480.0),
        egui::vec2(480.0, 360.0),
        egui::vec2(320.0, 240.0),
    ] {
        for font_scale in [1.0, 2.0] {
            for (stage, label, action) in [
                (RecorderStage::Ready, "Start", RecorderOverlayAction::Start),
                (RecorderStage::Ready, "Cancel", RecorderOverlayAction::Close),
                (
                    RecorderStage::Countdown(3),
                    "Cancel countdown",
                    RecorderOverlayAction::CancelCountdown,
                ),
                (
                    RecorderStage::Recording,
                    "Take snapshot",
                    RecorderOverlayAction::Snapshot,
                ),
                (
                    RecorderStage::Recording,
                    "Pause",
                    RecorderOverlayAction::Pause,
                ),
                (
                    RecorderStage::Recording,
                    "Stop",
                    RecorderOverlayAction::Stop,
                ),
                (
                    RecorderStage::Recording,
                    "Discard",
                    RecorderOverlayAction::Discard,
                ),
                (
                    RecorderStage::Paused,
                    "Resume",
                    RecorderOverlayAction::Resume,
                ),
                (RecorderStage::Paused, "Stop", RecorderOverlayAction::Stop),
                (
                    RecorderStage::Paused,
                    "Discard",
                    RecorderOverlayAction::Discard,
                ),
                (
                    RecorderStage::Finalizing,
                    "Cancel",
                    RecorderOverlayAction::Discard,
                ),
            ] {
                check_compact_toolbar_hit(size, font_scale, stage, label, action);
            }
        }
    }
}

fn check_compact_toolbar_hit(
    size: egui::Vec2,
    font_scale: f32,
    stage: RecorderStage,
    label: &str,
    expected: RecorderOverlayAction,
) {
    check_compact_toolbar_hit_with_zoom(size, font_scale, 1.0, stage, label, expected);
}

#[test]
fn compact_toolbar_hit_targets_fit_the_same_native_budget_after_ui_zoom() {
    for size in [egui::vec2(720.0, 480.0), egui::vec2(320.0, 240.0)] {
        for zoom in [1.25, 2.0] {
            for (label, action) in [
                ("Take snapshot", RecorderOverlayAction::Snapshot),
                ("Pause", RecorderOverlayAction::Pause),
                ("Stop", RecorderOverlayAction::Stop),
                ("Discard", RecorderOverlayAction::Discard),
            ] {
                check_compact_toolbar_hit_with_zoom(
                    size,
                    1.0,
                    zoom,
                    RecorderStage::Recording,
                    label,
                    action,
                );
            }
        }
    }
}

fn check_compact_toolbar_hit_with_zoom(
    native_size: egui::Vec2,
    font_scale: f32,
    zoom: f32,
    stage: RecorderStage,
    label: &str,
    expected: RecorderOverlayAction,
) {
    let context = egui::Context::default();
    context.set_zoom_factor(zoom);
    let size = native_size / zoom;
    let _ = context.run(geometry_input(size, Some(false)), |_| {});
    context.style_mut(|style| {
        for font in style.text_styles.values_mut() {
            font.size *= font_scale;
        }
    });
    let mut controller = controller(&context);
    let original_region = controller.region;
    let _ = toolbar_frame(&context, &mut controller, stage, size, Vec::new());
    let (output, _) = toolbar_frame(&context, &mut controller, stage, size, Vec::new());
    let (text, clip) = preparation_text_rect(&output, label);
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    assert!(
        viewport.contains_rect(text) && clip.contains_rect(text),
        "{label} clipped at {size:?}, font {font_scale}: text {text:?}, clip {clip:?}"
    );
    let pos = text.center();
    let mut clicked_action = RecorderOverlayAction::None;
    for pressed in [true, false] {
        clicked_action = toolbar_frame(
            &context,
            &mut controller,
            stage,
            size,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .1;
    }
    assert_eq!(
        clicked_action, expected,
        "{label} must receive its click at {size:?}, font {font_scale}"
    );
    assert_eq!(
        controller.region, original_region,
        "the invisible sizing pass must not move the capture area"
    );
}

fn preparation_fixture(font_scale: f32) -> (egui::Context, GifFromScreenApp) {
    let context = egui::Context::default();
    context.style_mut(|style| {
        for font in style.text_styles.values_mut() {
            font.size *= font_scale;
        }
    });
    let mut app = GifFromScreenApp::default();
    app.view = AppView::ScreenRecorder;
    app.settings.region_x = 41;
    app.settings.region_y = 53;
    app.settings.region_width = 211;
    app.settings.region_height = 173;
    app.wayland_frozen_preview = Some(WaylandFrozenPreview {
        texture: context.load_texture(
            "preparation-layout",
            egui::ColorImage::filled([692, 509], egui::Color32::BLUE),
            egui::TextureOptions::NEAREST,
        ),
        source_size: gif_from_screen_capture::PhysicalSize::new(692, 509).unwrap(),
        selection: PhysicalRect::new(10, 20, 100, 80).unwrap(),
        drag_start: None,
        drag_current: None,
        drag_initial_region: None,
    });
    (context, app)
}

fn preparation_frame(
    context: &egui::Context,
    app: &mut GifFromScreenApp,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..egui::RawInput::default()
        },
        |context| {
            app.show_app_header(context);
            egui::CentralPanel::default().show(context, |ui| {
                let scroll_id = ui.make_persistent_id(egui::Id::new("wayland-preparation-content"));
                context.data_mut(|data| {
                    data.insert_temp(egui::Id::new("preparation-test-scroll-id"), scroll_id);
                });
                app.show_wayland_preparation(ui);
            });
        },
    )
}

fn preparation_text_rect(output: &egui::FullOutput, label: &str) -> (egui::Rect, egui::Rect) {
    fn find(shape: &egui::Shape, label: &str) -> Option<egui::Rect> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|clipped| find(&clipped.shape, label).map(|rect| (rect, clipped.clip_rect)))
        .unwrap_or_else(|| panic!("missing preparation control {label}"))
}

fn preparation_image_rect(output: &egui::FullOutput, texture: egui::TextureId) -> egui::Rect {
    fn find(shape: &egui::Shape, texture: egui::TextureId) -> Option<egui::Rect> {
        match shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh.calc_bounds()),
            egui::Shape::Rect(rect)
                if rect
                    .brush
                    .as_ref()
                    .is_some_and(|brush| brush.fill_texture_id == texture) =>
            {
                Some(rect.rect)
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, texture)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|clipped| find(&clipped.shape, texture))
        .expect("preview image mesh")
}

#[test]
fn prepared_page_controls_have_visible_hit_targets_in_small_and_large_font_viewports() {
    for size in [egui::vec2(640.0, 480.0), egui::vec2(1280.0, 720.0)] {
        for font_scale in [1.0, 2.0] {
            for label in [
                "Open source-local recorder controller",
                "Cancel preparation",
                "Apply exact region",
                "41",
                "53",
                "211",
                "173",
            ] {
                check_preparation_control_hit(size, font_scale, label);
            }
        }
    }
}

fn check_preparation_control_hit(size: egui::Vec2, font_scale: f32, label: &str) {
    let (context, mut app) = preparation_fixture(font_scale);
    preparation_frame(&context, &mut app, size, Vec::new());
    let output = preparation_frame(&context, &mut app, size, Vec::new());
    let (text, clip) = preparation_text_rect(&output, label);
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
    assert!(
        viewport.contains_rect(text),
        "{label}: {text:?} outside {size:?}, font {font_scale}"
    );
    assert!(
        clip.contains_rect(text),
        "{label}: control is clipped, font {font_scale}"
    );
    let pos = text.center();
    for pressed in [true, false] {
        preparation_frame(
            &context,
            &mut app,
            size,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    let clicked = context
        .interaction_snapshot(|snapshot| snapshot.clicked)
        .expect("visible control must receive the pointer click");
    let response = context.read_response(clicked).unwrap();
    assert!(
        response.clicked() && response.rect.contains(pos),
        "{label} must have a real clickable target"
    );
    assert!(
        viewport.contains_rect(response.interact_rect),
        "{label} hit target must fit viewport"
    );
    if label == "Apply exact region" {
        assert_eq!(
            app.wayland_frozen_preview.as_ref().unwrap().selection,
            PhysicalRect::new(41, 53, 211, 173).unwrap()
        );
    } else if label == "Open source-local recorder controller" {
        assert_eq!(
            app.notice.as_deref(),
            Some("The Wayland source is not ready yet."),
            "the synthetic preview has no portal session, but the real Open handler must be reached"
        );
    }
}

#[test]
fn prepared_page_preview_fits_available_space_without_changing_aspect() {
    for size in [egui::vec2(640.0, 480.0), egui::vec2(1280.0, 720.0)] {
        for font_scale in [1.0, 2.0] {
            let (context, mut app) = preparation_fixture(font_scale);
            let texture = app.wayland_frozen_preview.as_ref().unwrap().texture.id();
            preparation_frame(&context, &mut app, size, Vec::new());
            let before = preparation_frame(&context, &mut app, size, Vec::new());
            let initial = preparation_image_rect(&before, texture);
            assert!((initial.aspect_ratio() - 692.0 / 509.0).abs() < 0.001);
            assert!(initial.is_positive());
            assert!(
                egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(initial),
                "preview should fit rather than require scrolling at {size:?}, font {font_scale}"
            );
        }
    }
}

#[test]
fn prepared_page_overflow_scrolls_without_hiding_top_actions() {
    for size in [egui::vec2(640.0, 480.0), egui::vec2(1280.0, 720.0)] {
        for font_scale in [1.0, 2.0] {
            let (context, mut app) = preparation_fixture(font_scale);
            app.notice = Some("Diagnostic detail must remain accessible.\n".repeat(40));
            preparation_frame(&context, &mut app, size, Vec::new());
            let before = preparation_frame(&context, &mut app, size, Vec::new());
            let scroll_id = context
                .data(|data| data.get_temp::<egui::Id>(egui::Id::new("preparation-test-scroll-id")))
                .unwrap();
            assert!(
                egui::scroll_area::State::load(&context, scroll_id)
                    .unwrap()
                    .offset
                    .y
                    .abs()
                    < f32::EPSILON
            );
            let after = preparation_frame(
                &context,
                &mut app,
                size,
                vec![
                    egui::Event::PointerMoved(egui::pos2(size.x / 2.0, size.y - 30.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -1000.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert!(
                egui::scroll_area::State::load(&context, scroll_id)
                    .unwrap()
                    .offset
                    .y
                    > 0.0,
                "overflowing body must scroll at {size:?}, font {font_scale}"
            );
            for label in [
                "Open source-local recorder controller",
                "Cancel preparation",
            ] {
                assert_eq!(
                    preparation_text_rect(&before, label).0,
                    preparation_text_rect(&after, label).0,
                    "{label} must stay above the scrolling body"
                );
            }
        }
    }
}

#[test]
fn controller_explains_source_specific_visibility_limits_without_font_dependent_arrows() {
    fn labels(shape: &egui::Shape, output: &mut String) {
        match shape {
            egui::Shape::Text(text) => output.push_str(text.galley.text()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| labels(shape, output)),
            _ => {}
        }
    }
    for monitor_source in [false, true] {
        let context = egui::Context::default();
        let mut controller = controller(&context);
        let output = context.run(egui::RawInput::default(), |context| {
            draw_wayland_crop_controller(
                context,
                RecorderStage::Ready,
                None,
                &mut controller,
                false,
                None,
                monitor_source,
            );
        });
        let mut text = String::new();
        for clipped in &output.shapes {
            labels(&clipped.shape, &mut text);
        }
        assert_eq!(
            text.contains("Monitor capture includes this controller"),
            monitor_source
        );
        assert_eq!(
            text.contains("Keep the selected window visible"),
            !monitor_source
        );
        assert!(
            !text.contains(['←', '→', '↑', '↓']),
            "nudge arrows must be painted, not missing-font glyphs"
        );
    }
}

#[test]
fn wayland_handoff_accepts_local_extent_when_desktop_window_positions_are_private() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    let prepared = controller(&context);
    let expected_region = prepared.region;
    app.wayland_frozen_preview = Some(WaylandFrozenPreview {
        texture: prepared.texture,
        source_size: prepared.source_size,
        selection: prepared.region,
        drag_start: None,
        drag_current: None,
        drag_initial_region: None,
    });
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(960.0, 640.0),
        )),
        ..egui::RawInput::default()
    };
    let output =
        context.run(input, |context| {
            assert!(context.input(|input| input.viewport().inner_rect.is_none()
                && input.viewport().outer_rect.is_none()));
            app.enter_wayland_crop_controller(context).unwrap();
        });
    assert_eq!(
        app.wayland_crop_controller.as_ref().unwrap().region,
        expected_region
    );
    assert!(app.wayland_frozen_preview.is_none());
    let snapshot = app.main_window_snapshot.as_ref().unwrap();
    assert_eq!(snapshot.size, egui::vec2(960.0, 640.0));
    assert!(snapshot.position.is_none());
    assert!(matches!(
        snapshot.restore,
        MainWindowRestore::Wayland {
            restoring: false,
            ..
        }
    ));
    let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
    assert!(commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(720.0, 480.0))));
    assert!(
        output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .all(|command| !matches!(
                command,
                egui::ViewportCommand::Visible(_) | egui::ViewportCommand::OuterPosition(_)
            ))
    );
}

fn recording_job() -> (
    RecordingJob,
    RecordingControl,
    std::sync::mpsc::Sender<JobMessage>,
) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (controller, control) = RecordingController::channel();
    (
        RecordingJob {
            receiver,
            controller,
            cancellation: CancellationFlag::default(),
            paused: false,
            pause_requested: None,
            terminal_requested: false,
            retarget: None,
            snapshot_requests: std::collections::VecDeque::new(),
        },
        control,
        sender,
    )
}

#[test]
fn failed_or_discarded_recording_clears_live_progress_and_ignores_late_messages() {
    for completion in [
        RecordingCompletion::Failed {
            error: "window content dimensions changed".to_owned(),
            recovery_path: Some(PathBuf::from("/synthetic-qa/retained.gfsproj")),
        },
        RecordingCompletion::Discarded {
            cleanup_error: None,
        },
    ] {
        let mut app = GifFromScreenApp::default();
        let (job, _control, sender) = recording_job();
        app.job = Some(job);
        let progress = WorkflowProgress {
            phase: WorkflowPhase::Capturing,
            frames_captured: 209,
            capture_duration: Duration::from_secs(21),
            playback_duration: Duration::from_secs(21),
            encode: None,
        };
        sender.send(JobMessage::Progress(progress)).unwrap();
        sender.send(JobMessage::Finished(completion)).unwrap();
        sender.send(JobMessage::Progress(progress)).unwrap();
        sender.send(JobMessage::Persisting).unwrap();
        app.receive_job_messages();
        assert!(app.job.is_none());
        assert!(app.progress.is_none());
        assert!(app.restore_main_window);
        let notice = app.notice.as_deref().unwrap();
        assert!(notice.contains("Recoverable autosave retained") || notice.contains("discarded"));
        assert!(!notice.contains("Finalizing"));
    }
}

#[test]
fn wayland_controller_renders_only_the_root_surface_and_close_restores_the_shell() {
    // The renderer callback is thread-local. Isolate it so no other GUI test inherits it.
    std::thread::spawn(|| {
        egui::Context::set_immediate_viewport_renderer(|_, _| {
            panic!("Wayland controller must not create an immediate child surface")
        });
        let context = egui::Context::default();
        context.set_embed_viewports(false);
        let mut app = GifFromScreenApp::default();
        app.view = AppView::ScreenRecorder;
        app.wayland_crop_controller = Some(controller(&context));
        let original_region = app.wayland_crop_controller.as_ref().unwrap().region;
        app.main_window_snapshot = Some(MainWindowSnapshot {
            position: None,
            size: egui::vec2(1040.0, 760.0),
            maximized: Some(true),
            restore: MainWindowRestore::Wayland {
                pending: None,
                restoring: false,
                zoom_factor: 1.0,
            },
        });
        let output = context.run(egui::RawInput::default(), |context| {
            app.show_wayland_crop_controller(context);
        });
        assert_eq!(output.viewport_output.len(), 1);
        assert_eq!(
            app.wayland_crop_controller.as_ref().unwrap().region,
            original_region
        );
        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let output = context.run(input, |context| app.show_wayland_crop_controller(context));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        );
        assert!(app.wayland_crop_controller.is_none());
        assert!(app.restore_main_window);
        let output = context.run(egui::RawInput::default(), |context| {
            app.restore_main_window_if_requested(context);
        });
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands.contains(&egui::ViewportCommand::Title(APP_NAME.to_owned())));
        assert!(!commands.iter().any(|command| matches!(
            command,
            egui::ViewportCommand::Visible(_) | egui::ViewportCommand::OuterPosition(_)
        )));
        assert!(commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(1040.0, 760.0))));
        let input = geometry_input(egui::vec2(1040.0, 760.0), Some(false));
        let output = context.run(input, |context| {
            app.restore_main_window_if_requested(context);
        });
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Maximized(true))
        );
        let _ = context.run(
            geometry_input(egui::vec2(1040.0, 760.0), Some(true)),
            |context| app.restore_main_window_if_requested(context),
        );
        assert!(!app.restore_main_window);
    })
    .join()
    .unwrap();
}

#[test]
fn active_controller_close_queues_stop_while_explicit_discard_remains_destructive() {
    for wayland in [true, false] {
        for (action, expected) in [
            (RecorderOverlayAction::Close, CaptureSessionState::Stopped),
            (
                RecorderOverlayAction::Discard,
                CaptureSessionState::Discarded,
            ),
        ] {
            let mut app = GifFromScreenApp::default();
            let (job, mut control, _sender) = recording_job();
            app.job = Some(job);
            let context = egui::Context::default();
            context.set_embed_viewports(false);
            let output = context.run(egui::RawInput::default(), |context| {
                if wayland {
                    app.handle_wayland_controller_action(context, action);
                } else {
                    context.show_viewport_deferred(
                        recorder_viewport_id(),
                        egui::ViewportBuilder::default(),
                        |_, _| {},
                    );
                    app.handle_recorder_overlay_action(context, action);
                }
            });
            if !wayland && action == RecorderOverlayAction::Close {
                assert!(
                    output.viewport_output[&recorder_viewport_id()]
                        .commands
                        .contains(&egui::ViewportCommand::CancelClose)
                );
            }
            assert_eq!(
                app.job.as_ref().unwrap().cancellation.is_cancelled(),
                action == RecorderOverlayAction::Discard
            );
            assert!(app.job.as_ref().unwrap().terminal_requested);
            let mut session = NoFramesSession::new();
            drain_control(&app, &mut session, &mut control);
            assert_eq!(
                session.events.first().copied(),
                Some(if expected == CaptureSessionState::Stopped {
                    "stop"
                } else {
                    "discard"
                })
            );
        }
    }
}

#[test]
fn controller_countdown_cancel_pause_ack_resume_and_stop_keep_existing_state_machine() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    app.wayland_crop_controller = Some(controller(&context));
    app.recording_countdown.start(Instant::now(), 3);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::CancelCountdown);
    assert!(!app.recording_countdown.is_active());
    assert!(app.wayland_crop_controller.is_some());
    let (job, mut control, _sender) = recording_job();
    app.job = Some(job);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::Pause);
    app.job
        .as_mut()
        .unwrap()
        .acknowledge_phase(WorkflowPhase::Paused);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::Resume);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::Stop);
    let mut session = NoFramesSession::new();
    drain_control(&app, &mut session, &mut control);
    // The no-frame fixture is discarded during empty-recording cleanup, after Stop.
    assert_eq!(&session.events[..3], ["pause", "resume", "stop"]);
    assert!(!app.job.as_ref().unwrap().cancellation.is_cancelled());
}

#[test]
fn closing_an_active_controller_keeps_the_completed_project_and_returns_to_editor() {
    use gif_from_screen_application::{
        BlankAnimationProjectOptions, create_blank_animation_project,
    };
    use gif_from_screen_domain::{DurationUs, FrameId, ProjectId, Rgba, UnixTimeMs};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("close-saves.gfsproj");
    let project = create_blank_animation_project(
        &root,
        BlankAnimationProjectOptions {
            project_id: ProjectId::from_u128(1),
            frame_id: FrameId::from_u128(1),
            app_version: "controller-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            canvas: gif_from_screen_domain::PhysicalSize::new(1, 1).unwrap(),
            background: Rgba::TRANSPARENT,
            frame_duration: DurationUs::new(100_000).unwrap(),
            frame_limit_bytes: 4,
        },
    )
    .unwrap();
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    app.view = AppView::ScreenRecorder;
    app.wayland_crop_controller = Some(controller(&context));
    let (job, _control, sender) = recording_job();
    app.job = Some(job);
    app.handle_wayland_controller_action(&context, RecorderOverlayAction::Close);
    assert!(!app.job.as_ref().unwrap().cancellation.is_cancelled());
    sender
        .send(JobMessage::Finished(RecordingCompletion::Completed(
            Box::new(project),
        )))
        .unwrap();
    app.receive_job_messages();
    assert_eq!(app.view, AppView::Editor);
    assert!(app.wayland_crop_controller.is_none());
    assert!(app.restore_main_window);
    let workspace = app.editor_workspace.as_ref().unwrap();
    assert_eq!(workspace.project_root(), root);
    assert_eq!(workspace.manifest().timeline.frames.len(), 1);
    assert!(root.join("manifest.json").is_file());
}

fn drain_control(
    app: &GifFromScreenApp,
    session: &mut NoFramesSession,
    control: &mut RecordingControl,
) {
    let _ = gif_from_screen_workflow::collect_prestarted_controlled_to_sink(
        session,
        &collection_options(&app.settings).unwrap(),
        control,
        &mut NoFramesSink,
        &gif_from_screen_gif::NeverCancel,
        &mut gif_from_screen_workflow::NoopWorkflowProgress,
    );
}

struct NoFramesSession {
    request: CaptureRequest,
    state: CaptureSessionState,
    events: Vec<&'static str>,
}
impl NoFramesSession {
    fn new() -> Self {
        Self {
            request: CaptureRequest::new(
                CaptureTarget::Monitor(CaptureSourceId::new("wayland-test").unwrap()),
                CaptureCadence::fixed_fps(10).unwrap(),
            ),
            state: CaptureSessionState::Recording,
            events: Vec::new(),
        }
    }
}
impl CaptureSession for NoFramesSession {
    fn state(&self) -> CaptureSessionState {
        self.state
    }
    fn request(&self) -> &CaptureRequest {
        &self.request
    }
    fn update_target(
        &mut self,
        target: CaptureTarget,
    ) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.request.target = target;
        Ok(())
    }
    fn pause(&mut self) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.state = CaptureSessionState::Paused;
        self.events.push("pause");
        Ok(())
    }
    fn resume(&mut self) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.state = CaptureSessionState::Recording;
        self.events.push("resume");
        Ok(())
    }
    fn stop(&mut self) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.state = CaptureSessionState::Stopped;
        self.events.push("stop");
        Ok(())
    }
    fn discard(&mut self) -> Result<(), gif_from_screen_capture::CaptureError> {
        self.state = CaptureSessionState::Discarded;
        self.events.push("discard");
        Ok(())
    }
    fn poll_frame(
        &mut self,
        _: Duration,
    ) -> Result<FramePoll, gif_from_screen_capture::CaptureError> {
        panic!("terminal control must be applied before polling");
    }
}

struct NoFramesSink;
impl RecordingFrameSink for NoFramesSink {
    fn append_provisional_frame(
        &mut self,
        _: u64,
        _: &gif_from_screen_gif::RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        panic!("no frame expected");
    }
    fn update_frame_duration(&mut self, _: u64, _: u64) -> Result<(), RecordingFrameSinkError> {
        panic!("no frame expected");
    }
}
