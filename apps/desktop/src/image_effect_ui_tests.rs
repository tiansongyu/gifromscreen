use super::*;
use gif_from_screen_domain::SignedEdgeWidths;

fn language(tag: &str) -> Localizer {
    Localizer::new(gif_from_screen_localization::find_language(tag).unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Style {
    Border(ImageBorderStyle),
    Shadow(ImageShadowStyle),
}

struct Form {
    context: egui::Context,
    style: Style,
    language: Localizer,
}

impl Form {
    fn new(style: Style, language: Localizer) -> Self {
        let context = egui::Context::default();
        crate::preferences::fonts::install(&context);
        Self {
            context,
            style,
            language,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        self.context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 540.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| match &mut self.style {
                    Style::Border(style) => show_border(ui, style, self.language),
                    Style::Shadow(style) => show_shadow(ui, style, self.language),
                });
            },
        )
    }

    fn position(&mut self, wanted: &str) -> egui::Pos2 {
        self.frame(Vec::new());
        let output = self.frame(Vec::new());
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == wanted
                {
                    let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                    shape.clip_rect.contains_rect(rect).then_some(rect.center())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing effect field {wanted:?}"))
    }

    fn edit(&mut self, wanted: &str, value: &str) {
        let position = self.position(wanted);
        for pressed in [true, false] {
            self.frame(vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        self.frame(vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
            },
            egui::Event::Text(value.into()),
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        self.frame(Vec::new());
    }
}

#[test]
fn language_and_zoom_never_normalize_fixed_point_border_values_or_colors() {
    for widths in [
        SignedEdgeWidths::default(),
        SignedEdgeWidths {
            top_milli: -500_000,
            right_milli: 50_000,
            bottom_milli: 1_001,
            left_milli: -1_999,
        },
    ] {
        let original = Style::Border(ImageBorderStyle {
            widths,
            color: Rgba {
                red: 13,
                green: 47,
                blue: 199,
                alpha: 21,
            },
            background: Rgba::TRANSPARENT,
        });
        let mut form = Form::new(original, language("en"));
        for tag in ["en", "zh", "en"] {
            form.language = language(tag);
            form.context.set_zoom_factor(1.25);
            form.frame(Vec::new());
            assert_eq!(form.style, original);
        }
    }
}

#[test]
fn language_never_changes_shadow_polar_hundredths_opacity_or_transparent_colors() {
    for original in [
        ImageShadowStyle::default(),
        ImageShadowStyle {
            blur_radius_hundredths: 25,
            depth_hundredths: 9_999,
            direction_hundredths: 35_999,
            opacity_basis_points: 1_234,
            color: Rgba {
                red: 9,
                green: 57,
                blue: 233,
                alpha: 81,
            },
            background: Rgba {
                red: 21,
                green: 22,
                blue: 23,
                alpha: 41,
            },
        },
    ] {
        let mut form = Form::new(Style::Shadow(original), language("en"));
        for tag in ["en", "zh", "en"] {
            form.language = language(tag);
            form.frame(Vec::new());
            assert_eq!(form.style, Style::Shadow(original));
        }
    }
}

#[test]
fn chinese_numeric_edits_keep_milli_pixels_hundredths_and_basis_points_distinct() {
    let localizer = language("zh");
    let mut border = Form::new(Style::Border(ImageBorderStyle::default()), localizer);
    border.edit(
        &format!("{} 1 px", localizer.text(Message::ImageBorderTop)),
        "-3",
    );
    let Style::Border(style) = border.style else {
        unreachable!()
    };
    assert_eq!(style.widths.top_milli, -3_000);
    assert_eq!(style.widths.right_milli, 1_000);
    let mut shadow = Form::new(Style::Shadow(ImageShadowStyle::default()), localizer);
    for (message, previous, value, suffix) in [
        (Message::ImageShadowBlur, "10.00", "0.25", " px"),
        (Message::ImageShadowDistance, "10.00", "12.34", " px"),
        (Message::ImageShadowDirection, "0.00", "359.99", "°"),
        (Message::ImageShadowOpacity, "60.00", "12.34", "%"),
    ] {
        shadow.edit(
            &format!("{} {previous}{suffix}", localizer.text(message)),
            value,
        );
    }
    let Style::Shadow(style) = shadow.style else {
        unreachable!()
    };
    assert_eq!(style.blur_radius_hundredths, 25);
    assert_eq!(style.depth_hundredths, 1_234);
    assert_eq!(style.direction_hundredths, 35_999);
    assert_eq!(style.opacity_basis_points, 1_234);
    assert_eq!(style.color, ImageShadowStyle::default().color);
}

#[test]
fn actual_border_field_hit_ids_do_not_depend_on_translated_prefixes() {
    let hit_ids = |localizer: Localizer| {
        let mut form = Form::new(Style::Border(ImageBorderStyle::default()), localizer);
        [
            Message::ImageBorderTop,
            Message::ImageBorderRight,
            Message::ImageBorderBottom,
            Message::ImageBorderLeft,
        ]
        .map(|message| {
            let position = form.position(&format!("{} 1 px", localizer.text(message)));
            form.frame(vec![egui::Event::PointerMoved(position)]);
            let mut ids: Vec<_> = form
                .context
                .interaction_snapshot(|state| state.hovered.iter().copied().collect());
            assert!(!ids.is_empty());
            ids.sort_by_key(egui::Id::value);
            ids
        })
    };
    assert_eq!(hit_ids(language("en")), hit_ids(language("zh")));
}
