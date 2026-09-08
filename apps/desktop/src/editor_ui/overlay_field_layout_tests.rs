use eframe::egui;
use gif_from_screen_domain::Rgba;
use gif_from_screen_localization::{Localizer, Message};

use super::{Harness, Panel, language, pointer, text_rect, texts};
use crate::editor_ui::{DrawingDraftPhase, EditorUiAction, EditorUiOperation, EditorUiResult};

fn values(harness: &mut Harness) {
    let shape = &mut harness.state.shape_overlay;
    shape.x = 101;
    shape.y = 102;
    shape.width = 103;
    shape.height = 104;
    shape.stroke_width = 7;
    shape.z_index = 6;
    shape.track_opacity = 177;
    shape.stroke = color([21, 42, 63, 84]);
    shape.fill = color([105, 126, 147, 168]);
    let drawing = &mut harness.state.drawing_overlay;
    drawing.width = 9;
    drawing.z_index = 8;
    drawing.track_opacity = 199;
    drawing.color = color([31, 52, 73, 94]);
}

const fn color([red, green, blue, alpha]: [u8; 4]) -> Rgba {
    Rgba {
        red,
        green,
        blue,
        alpha,
    }
}

fn field(
    harness: &mut Harness,
    panel: Panel,
    localizer: Localizer,
    number: &str,
) -> egui::Response {
    let output = harness.settled(panel, localizer);
    let rect = text_rect(&output, number);
    harness.frame(
        panel,
        localizer,
        vec![egui::Event::PointerMoved(rect.center())],
    );
    let ids = harness
        .context
        .interaction_snapshot(|state| state.contains_pointer.clone());
    ids.into_iter()
        .filter_map(|id| harness.context.read_response(id))
        .find(|response| response.sense.senses_drag() && response.rect.contains_rect(rect))
        .unwrap_or_else(|| panic!("no real numeric widget for {number}"))
}

fn assert_pair(
    harness: &mut Harness,
    panel: Panel,
    localizer: Localizer,
    label: &str,
    number: &str,
) -> egui::Id {
    let response = field(harness, panel, localizer, number);
    let output = harness.settled(panel, localizer);
    let label = text_rect(&output, label);
    assert!(
        label.right() <= response.rect.left(),
        "field precedes its label"
    );
    assert!(
        label.y_range().intersects(response.rect.y_range()),
        "label detached from numeric control"
    );
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, harness.size);
    assert!(
        viewport.contains_rect(response.rect),
        "numeric field extends offscreen: {:?}",
        response.rect
    );
    response.id
}

fn assert_rgba_group(
    harness: &mut Harness,
    panel: Panel,
    localizer: Localizer,
    label: &str,
    numbers: [&str; 4],
) {
    let fields = numbers.map(|number| field(harness, panel, localizer, number).rect);
    let output = harness.settled(panel, localizer);
    let header = text_rect(&output, label);
    assert!(
        fields
            .iter()
            .all(|rect| (rect.center().y - fields[0].center().y).abs() < 0.5)
    );
    for (channel, rect) in ["R", "G", "B", "A"].into_iter().zip(fields) {
        assert!(
            texts(&output)
                .iter()
                .any(|(text, channel_rect, clip)| *text == channel
                    && channel_rect.top() >= header.bottom()
                    && channel_rect.bottom() <= rect.top()
                    && (channel_rect.left() - rect.left()).abs() < 4.0
                    && clip.contains_rect(*channel_rect)),
            "channel {channel} no longer identifies its value"
        );
    }
    assert!(
        output.shapes.iter().any(|clipped| {
            let egui::Shape::Rect(group) = &clipped.shape else {
                return false;
            };
            group.rect.contains_rect(header)
                && fields.iter().all(|field| group.rect.contains_rect(*field))
                && clipped.clip_rect.contains_rect(group.rect)
        }),
        "RGBA title and channel values need one fully visible group"
    );
}

#[test]
fn shape_fields_and_rgba_stay_grouped_at_half_window_and_narrow_large_fonts() {
    for (width, font_size) in [(500.0, 14.0), (320.0, 14.0), (320.0, 22.0)] {
        let mut harness = Harness::new();
        harness.size = egui::vec2(width, 1_100.0);
        harness.context.style_mut(|style| {
            for font in style.text_styles.values_mut() {
                font.size = font_size;
            }
        });
        values(&mut harness);
        let original = harness.workspace.manifest().clone();
        let mut previous_ids = None;
        for tag in ["en", "zh", "en"] {
            let localizer = language(tag);
            let ids = [
                ("Z", "6"),
                (localizer.text(Message::EditorTrackOpacity), "177"),
                ("X", "101"),
                ("Y", "102"),
                ("W", "103"),
                ("H", "104"),
                (localizer.text(Message::EditorStrokeWidth), "7"),
            ]
            .map(|(label, number)| {
                assert_pair(&mut harness, Panel::Shape, localizer, label, number)
            });
            if let Some(previous) = previous_ids {
                assert_eq!(ids, previous);
            }
            previous_ids = Some(ids);
            assert_rgba_group(
                &mut harness,
                Panel::Shape,
                localizer,
                localizer.text(Message::EditorStrokeRgba),
                ["21", "42", "63", "84"],
            );
            assert_rgba_group(
                &mut harness,
                Panel::Shape,
                localizer,
                localizer.text(Message::EditorFillRgba),
                ["105", "126", "147", "168"],
            );
            let output = harness.settled(Panel::Shape, localizer);
            text_rect(&output, localizer.text(Message::EditorAddShapeOverlay));
            assert_eq!(harness.workspace.manifest(), &original);
        }
    }
}

#[test]
fn drawing_fields_preserve_ready_points_and_keep_language_independent_hit_ids() {
    let mut harness = Harness::new();
    harness.size = egui::vec2(320.0, 800.0);
    harness.context.style_mut(|style| {
        for font in style.text_styles.values_mut() {
            font.size = 22.0;
        }
    });
    values(&mut harness);
    harness.ready();
    let original = format!("{:?}", harness.state.drawing_overlay);
    let mut previous_ids = None;
    for tag in ["en", "zh", "en"] {
        let localizer = language(tag);
        let ids = [
            (localizer.text(Message::RecorderWidth), "9"),
            ("Z", "8"),
            (localizer.text(Message::EditorTrackOpacity), "199"),
        ]
        .map(|(label, number)| assert_pair(&mut harness, Panel::Drawing, localizer, label, number));
        if let Some(previous) = previous_ids {
            assert_eq!(ids, previous);
        }
        previous_ids = Some(ids);
        assert_rgba_group(
            &mut harness,
            Panel::Drawing,
            localizer,
            "RGBA",
            ["31", "52", "73", "94"],
        );
        assert_eq!(format!("{:?}", harness.state.drawing_overlay), original);
    }
}

fn edit(harness: &mut Harness, panel: Panel, number: &str, replacement: &str) {
    let localizer = language("zh");
    let position = field(harness, panel, localizer, number).rect.center();
    for pressed in [true, false] {
        harness.frame(panel, localizer, pointer(position, pressed));
    }
    harness.frame(
        panel,
        localizer,
        vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            },
            egui::Event::Text(replacement.into()),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    harness.frame(panel, localizer, Vec::new());
}

#[test]
fn chinese_numeric_edits_target_only_the_labelled_property_and_do_not_write_the_project() {
    let mut harness = Harness::new();
    harness.size = egui::vec2(320.0, 1_100.0);
    values(&mut harness);
    let original = harness.workspace.manifest().clone();
    edit(&mut harness, Panel::Shape, "177", "201");
    assert_eq!(harness.state.shape_overlay.track_opacity, 201);
    edit(&mut harness, Panel::Shape, "21", "37");
    assert_eq!(harness.state.shape_overlay.stroke, color([37, 42, 63, 84]));
    edit(&mut harness, Panel::Shape, "168", "209");
    assert_eq!(
        harness.state.shape_overlay.fill,
        color([105, 126, 147, 209])
    );
    harness.ready();
    let points = harness.state.drawing_overlay.points.clone();
    edit(&mut harness, Panel::Drawing, "199", "203");
    assert_eq!(harness.state.drawing_overlay.track_opacity, 203);
    edit(&mut harness, Panel::Drawing, "52", "71");
    assert_eq!(harness.state.drawing_overlay.color, color([31, 71, 73, 94]));
    assert_eq!(harness.state.drawing_overlay.points, points);
    assert_eq!(
        harness.state.drawing_overlay.phase,
        DrawingDraftPhase::Ready
    );
    assert_eq!(harness.workspace.manifest(), &original);
}

fn scrolled(
    harness: &mut Harness,
    panel: Panel,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Vec<EditorUiResult>) {
    let mut results = Vec::new();
    let output = harness.context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, harness.size)),
            events,
            ..egui::RawInput::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("field-layout-host-scroll")
                    .show(ui, |ui| match panel {
                        Panel::Shape => super::show_shape_overlay_toolbar(
                            ui,
                            &mut harness.workspace,
                            &mut harness.state,
                            std::time::Instant::now(),
                            &mut results,
                            language("zh"),
                        ),
                        Panel::Drawing => super::show_drawing_overlay_controls(
                            ui,
                            &mut harness.workspace,
                            &mut harness.state,
                            std::time::Instant::now(),
                            &mut results,
                            language("zh"),
                        ),
                        Panel::Layers | Panel::Project => unreachable!(),
                    });
            });
        },
    );
    (output, results)
}

#[test]
fn host_scroll_keeps_apply_and_commit_reachable_in_small_large_font_viewports() {
    for (panel, button, operation) in [
        (
            Panel::Shape,
            Message::EditorAddShapeOverlay,
            EditorUiOperation::AddShapeOverlay,
        ),
        (
            Panel::Drawing,
            Message::EditorCommitDrawing,
            EditorUiOperation::AddDrawingOverlay,
        ),
    ] {
        let mut harness = Harness::new();
        harness.size = egui::vec2(320.0, 240.0);
        harness.context.style_mut(|style| {
            for font in style.text_styles.values_mut() {
                font.size = 22.0;
            }
        });
        harness.ready();
        for _ in 0..3 {
            scrolled(&mut harness, panel, Vec::new());
        }
        scrolled(
            &mut harness,
            panel,
            vec![
                egui::Event::PointerMoved(egui::pos2(160.0, 120.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -2_000.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        for _ in 0..30 {
            scrolled(&mut harness, panel, Vec::new());
        }
        let output = scrolled(&mut harness, panel, Vec::new()).0;
        let position = text_rect(&output, language("zh").text(button)).center();
        let mut actions = Vec::new();
        for pressed in [true, false] {
            actions.extend(scrolled(&mut harness, panel, pointer(position, pressed)).1);
        }
        assert_eq!(actions, [Ok(EditorUiAction::Project(operation))]);
        assert_eq!(
            harness.workspace.manifest().timeline.overlay_tracks.len(),
            1
        );
    }
}
