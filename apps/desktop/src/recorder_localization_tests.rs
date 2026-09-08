//! Localized recorder rendering must not become a source of capture geometry.

use std::time::Duration;

use eframe::egui;
use gif_from_screen_capture::{PhysicalRect, PhysicalSize};
use gif_from_screen_localization::{Localizer, Message, find_language};

use crate::{
    GifFromScreenApp, RecorderOverlayAction, RecorderStage, RecordingCadenceChoice,
    RecordingCursor, RecordingIntervalUnit, RecordingSettings, RegionPicker, WaylandCropController,
    WaylandFrozenPreview, WorkflowPhase, WorkflowProgress, draw_wayland_crop_controller,
};

fn localizer(tag: &str) -> Localizer {
    Localizer::new(find_language(tag).unwrap())
}

#[test]
fn chinese_catalog_templates_have_real_egui_glyphs_in_both_ui_families() {
    let chinese = localizer("zh");
    let characters = gif_from_screen_localization::ALL_MESSAGES
        .iter()
        .flat_map(|message| chinese.text(*message).chars())
        .filter(|character| !character.is_control())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(!characters.is_empty());
    let context = context(1.0, 1.0);
    let _ = context.run(egui::RawInput::default(), |context| {
        context.fonts(|fonts| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                let font = egui::FontId::new(18.0, family);
                for &character in &characters {
                    assert!(
                        fonts.has_glyph(&font, character),
                        "missing Chinese UI glyph {character:?}"
                    );
                }
            }
        });
    });
}

fn context(zoom: f32, font_scale: f32) -> egui::Context {
    let context = egui::Context::default();
    crate::preferences::fonts::install(&context);
    context.set_zoom_factor(zoom);
    context.style_mut(|style| {
        for font in style.text_styles.values_mut() {
            font.size *= font_scale;
        }
    });
    context
}

fn input(size: egui::Vec2, events: Vec<egui::Event>) -> egui::RawInput {
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        events,
        ..egui::RawInput::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .native_pixels_per_point = Some(1.0);
    input
}

fn controller(context: &egui::Context) -> WaylandCropController {
    WaylandCropController {
        texture: context.load_texture(
            "localized-recorder-fixture",
            egui::ColorImage::filled([64, 48], egui::Color32::RED),
            egui::TextureOptions::NEAREST,
        ),
        source_size: PhysicalSize::new(1280, 960).unwrap(),
        region: PhysicalRect::new(173, 257, 321, 219).unwrap(),
        drag_start: None,
        drag_current: None,
        drag_initial_region: None,
    }
}

fn controller_frame(
    context: &egui::Context,
    controller: &mut WaylandCropController,
    stage: RecorderStage,
    language: Localizer,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, RecorderOverlayAction) {
    let mut action = RecorderOverlayAction::None;
    let output = context.run(input(size, events), |context| {
        let frame = draw_wayland_crop_controller(
            context,
            stage,
            Some(WorkflowProgress {
                phase: WorkflowPhase::Capturing,
                frames_captured: 7,
                capture_duration: Duration::from_secs(31),
                playback_duration: Duration::from_millis(700),
                encode: None,
            }),
            controller,
            true,
            None,
            false,
            language,
        );
        assert!(
            frame.region.is_none(),
            "a label/toolbar must not create a crop"
        );
        action = frame.action;
    });
    (output, action)
}

fn text_rectangle(output: &egui::FullOutput, label: &str) -> (egui::Rect, egui::Rect) {
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
        .unwrap_or_else(|| panic!("missing localized recorder label {label}"))
}

#[test]
fn preparation_language_switch_preserves_complete_capture_settings_and_control_ids() {
    let context = context(1.25, 1.0);
    let mut settings = RecordingSettings {
        output: "/synthetic/语言-output.gif".to_owned(),
        duration_ms: 8712,
        cadence: RecordingCadenceChoice::Periodic,
        fps: 23,
        interval_count: 2,
        interval_unit: RecordingIntervalUnit::Minutes,
        manual_frame_duration_ms: 257,
        countdown_seconds: 7,
        changes_only: true,
        input_events: true,
        cursor: RecordingCursor::Editable,
        region_enabled: true,
        region_x: 173,
        region_y: 257,
        region_width: 321,
        region_height: 219,
        ..RecordingSettings::default()
    };
    let expected = format!("{settings:?}");
    let mut identities = None;
    for tag in ["en", "zh", "en", "zh"] {
        let _ = context.run(input(egui::vec2(480.0, 360.0), Vec::new()), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let (open, cancel) = GifFromScreenApp::show_wayland_preparation_actions(
                    ui,
                    true,
                    false,
                    localizer(tag),
                );
                assert!(!open.clicked() && !cancel.clicked());
                let current = (open.id, cancel.id);
                assert_eq!(*identities.get_or_insert(current), current);
                assert!(!GifFromScreenApp::show_wayland_preparation_coordinates(
                    ui,
                    &mut settings,
                    localizer(tag),
                ));
            });
        });
        assert_eq!(format!("{settings:?}"), expected);
    }
}

#[test]
fn live_controller_language_switch_keeps_source_geometry_and_ui_scale() {
    for zoom in [1.0, 1.25, 2.0] {
        let context = context(zoom, 1.0);
        let size = egui::vec2(480.0, 360.0) / zoom;
        let _ = context.run(input(size, Vec::new()), |_| {});
        let expected_ppp = context.pixels_per_point();
        let mut controller = controller(&context);
        let original_region = controller.region;
        let original_source = controller.source_size;
        for tag in ["en", "zh", "en"] {
            for stage in [
                RecorderStage::Ready,
                RecorderStage::Countdown(3),
                RecorderStage::Recording,
                RecorderStage::Paused,
                RecorderStage::Finalizing,
            ] {
                let (_, action) = controller_frame(
                    &context,
                    &mut controller,
                    stage,
                    localizer(tag),
                    size,
                    Vec::new(),
                );
                assert_eq!(action, RecorderOverlayAction::None);
                assert_eq!(controller.region, original_region);
                assert_eq!(controller.source_size, original_source);
                assert_eq!(context.zoom_factor().to_bits(), zoom.to_bits());
                assert_eq!(context.pixels_per_point().to_bits(), expected_ppp.to_bits());
            }
        }
    }
}

#[test]
fn actual_catalog_change_cancels_screen_space_gestures_without_touching_committed_regions() {
    let context = context(1.25, 1.0);
    let mut live = controller(&context);
    let original_region = live.region;
    let source_size = live.source_size;
    let start = egui::pos2(33.0, 51.0);
    let current = egui::pos2(92.0, 117.0);
    live.drag_start = Some(start);
    live.drag_current = Some(current);
    live.drag_initial_region = Some(original_region);
    let mut app = GifFromScreenApp::default();
    app.region_picker = Some(RegionPicker {
        texture: live.texture.clone(),
        source_width: source_size.width(),
        source_height: source_size.height(),
        drag_start: Some(start),
        drag_current: Some(current),
        selection: Some(original_region),
    });
    app.wayland_frozen_preview = Some(WaylandFrozenPreview {
        texture: live.texture.clone(),
        source_size,
        selection: original_region,
        drag_start: Some(start),
        drag_current: Some(current),
        drag_initial_region: Some(original_region),
    });
    app.wayland_crop_controller = Some(live);
    app.settings.region_enabled = true;
    app.settings.region_x = original_region.origin().x;
    app.settings.region_y = original_region.origin().y;
    app.settings.region_width = original_region.size().width();
    app.settings.region_height = original_region.size().height();
    let before = format!("{:?}", app.settings);
    for tag in ["en", "ar"] {
        app.sync_recorder_language(localizer(tag));
        assert_eq!(app.region_picker.as_ref().unwrap().drag_start, Some(start));
        assert_eq!(
            app.wayland_frozen_preview.as_ref().unwrap().drag_current,
            Some(current)
        );
        assert_eq!(
            app.wayland_crop_controller
                .as_ref()
                .unwrap()
                .drag_initial_region,
            Some(original_region)
        );
    }
    app.sync_recorder_language(localizer("zh"));
    let picker = app.region_picker.as_ref().unwrap();
    assert!(picker.drag_start.is_none() && picker.drag_current.is_none());
    assert_eq!(picker.selection, Some(original_region));
    let frozen = app.wayland_frozen_preview.as_ref().unwrap();
    assert!(
        frozen.drag_start.is_none()
            && frozen.drag_current.is_none()
            && frozen.drag_initial_region.is_none()
    );
    assert_eq!(frozen.selection, original_region);
    assert_eq!(frozen.source_size, source_size);
    let live = app.wayland_crop_controller.as_ref().unwrap();
    assert!(
        live.drag_start.is_none()
            && live.drag_current.is_none()
            && live.drag_initial_region.is_none()
    );
    assert_eq!(live.region, original_region);
    assert_eq!(live.source_size, source_size);
    assert_eq!(format!("{:?}", app.settings), before);
    app.wayland_crop_controller.as_mut().unwrap().drag_start = Some(start);
    app.sync_recorder_language(localizer("zh"));
    assert_eq!(
        app.wayland_crop_controller.as_ref().unwrap().drag_start,
        Some(start)
    );
    app.sync_recorder_language(localizer("en"));
    assert!(
        app.wayland_crop_controller
            .as_ref()
            .unwrap()
            .drag_start
            .is_none()
    );
    assert_eq!(
        app.wayland_crop_controller.as_ref().unwrap().region,
        original_region
    );
    assert_eq!(format!("{:?}", app.settings), before);
}

fn check_control(
    size: egui::Vec2,
    zoom: f32,
    font_scale: f32,
    stage: RecorderStage,
    key: Message,
    expected: RecorderOverlayAction,
) {
    let context = context(zoom, font_scale);
    let logical_size = size / zoom;
    let mut controller = controller(&context);
    let original = controller.region;
    let chinese = localizer("zh");
    let label = chinese.text(key);
    assert_ne!(label, localizer("en").text(key));
    for _ in 0..2 {
        controller_frame(
            &context,
            &mut controller,
            stage,
            chinese,
            logical_size,
            Vec::new(),
        );
    }
    let (output, _) = controller_frame(
        &context,
        &mut controller,
        stage,
        chinese,
        logical_size,
        Vec::new(),
    );
    let (text, clip) = text_rectangle(&output, label);
    assert!(
        egui::Rect::from_min_size(egui::Pos2::ZERO, logical_size).contains_rect(text),
        "{label} off viewport {size:?}/zoom{zoom}"
    );
    assert!(
        clip.contains_rect(text),
        "{label} clipped: {text:?} by {clip:?}"
    );
    let position = text.center();
    let mut action = RecorderOverlayAction::None;
    for pressed in [true, false] {
        action = controller_frame(
            &context,
            &mut controller,
            stage,
            chinese,
            logical_size,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .1;
    }
    assert_eq!(
        action, expected,
        "{label} did not activate at {size:?}/zoom{zoom}"
    );
    assert_eq!(
        controller.region, original,
        "hidden measurement or action changed source pixels"
    );
}

#[test]
fn chinese_compact_wayland_primary_actions_are_visible_and_clickable() {
    for (size, zoom, font_scale) in [
        (egui::vec2(320.0, 240.0), 1.0, 1.0),
        (egui::vec2(320.0, 240.0), 1.0, 2.0),
        (egui::vec2(320.0, 240.0), 1.25, 1.0),
        (egui::vec2(320.0, 240.0), 2.0, 1.0),
        (egui::vec2(480.0, 360.0), 1.0, 2.0),
    ] {
        for (stage, key, action) in [
            (
                RecorderStage::Ready,
                Message::RecorderStart,
                RecorderOverlayAction::Start,
            ),
            (
                RecorderStage::Ready,
                Message::CancelButton,
                RecorderOverlayAction::Close,
            ),
            (
                RecorderStage::Countdown(3),
                Message::RecorderCancelCountdown,
                RecorderOverlayAction::CancelCountdown,
            ),
            (
                RecorderStage::Recording,
                Message::RecorderTakeSnapshot,
                RecorderOverlayAction::Snapshot,
            ),
            (
                RecorderStage::Recording,
                Message::RecorderPause,
                RecorderOverlayAction::Pause,
            ),
            (
                RecorderStage::Recording,
                Message::RecorderStopShort,
                RecorderOverlayAction::Stop,
            ),
            (
                RecorderStage::Recording,
                Message::RecorderDiscardShort,
                RecorderOverlayAction::Discard,
            ),
            (
                RecorderStage::Paused,
                Message::RecorderResume,
                RecorderOverlayAction::Resume,
            ),
            (
                RecorderStage::Paused,
                Message::RecorderStopShort,
                RecorderOverlayAction::Stop,
            ),
            (
                RecorderStage::Paused,
                Message::RecorderDiscardShort,
                RecorderOverlayAction::Discard,
            ),
            (
                RecorderStage::Finalizing,
                Message::CancelButton,
                RecorderOverlayAction::Discard,
            ),
        ] {
            check_control(size, zoom, font_scale, stage, key, action);
        }
    }
}
