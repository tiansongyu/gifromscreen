//! Shared visual language for the desktop; capture geometry stays independent.

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle};

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) background: Color32,
    pub(crate) surface: Color32,
    pub(crate) elevated: Color32,
    pub(crate) border: Color32,
    pub(crate) text: Color32,
    pub(crate) muted: Color32,
    pub(crate) accent: Color32,
    pub(crate) accent_soft: Color32,
}

impl Palette {
    pub(crate) const fn for_theme(dark: bool) -> Self {
        if dark {
            Self {
                background: Color32::from_rgb(20, 23, 29),
                surface: Color32::from_rgb(28, 32, 40),
                elevated: Color32::from_rgb(37, 43, 54),
                border: Color32::from_rgb(53, 60, 73),
                text: Color32::from_rgb(235, 238, 245),
                muted: Color32::from_rgb(156, 166, 184),
                accent: Color32::from_rgb(120, 177, 255),
                accent_soft: Color32::from_rgb(30, 48, 74),
            }
        } else {
            Self {
                background: Color32::WHITE,
                surface: Color32::from_rgb(246, 246, 246),
                elevated: Color32::from_rgb(232, 232, 232),
                border: Color32::from_rgb(190, 190, 190),
                text: Color32::from_rgb(32, 32, 32),
                muted: Color32::from_rgb(96, 96, 96),
                accent: Color32::from_rgb(0, 120, 196),
                accent_soft: Color32::from_rgb(229, 242, 252),
            }
        }
    }

    pub(crate) fn of(ui: &egui::Ui) -> Self {
        Self::for_theme(ui.visuals().dark_mode)
    }
}

pub(crate) fn configure(context: &egui::Context) {
    context.set_theme(egui::Theme::Light);
    // Configure both themes so a system theme change keeps the same visual language.
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        context.style_mut_of(theme, |style| {
            let colors = Palette::for_theme(style.visuals.dark_mode);
            style.spacing.item_spacing = egui::vec2(6.0, 5.0);
            style.spacing.button_padding = egui::vec2(8.0, 3.0);
            style.spacing.interact_size.y = 28.0;
            // Reserve a gutter: a floating scrollbar steals the edge of cards.
            style.spacing.scroll = egui::style::ScrollStyle::solid();
            style.spacing.window_margin = egui::Margin::same(12);
            style.spacing.menu_margin = egui::Margin::same(6);
            for text_style in [TextStyle::Body, TextStyle::Button] {
                style
                    .text_styles
                    .insert(text_style, FontId::proportional(14.0));
            }
            style
                .text_styles
                .insert(TextStyle::Small, FontId::proportional(12.0));
            style
                .text_styles
                .insert(TextStyle::Heading, FontId::proportional(20.0));
            style.visuals.panel_fill = colors.background;
            style.visuals.window_fill = colors.surface;
            style.visuals.extreme_bg_color = colors.surface;
            style.visuals.faint_bg_color = colors.elevated;
            style.visuals.text_edit_bg_color = Some(colors.surface);
            style.visuals.override_text_color = Some(colors.text);
            style.visuals.weak_text_color = Some(colors.muted);
            style.visuals.window_stroke = Stroke::new(1.0_f32, colors.border);
            style.visuals.window_corner_radius = CornerRadius::same(2);
            style.visuals.selection.bg_fill = colors.accent_soft;
            style.visuals.selection.stroke = Stroke::new(1.0_f32, colors.accent);
            style.visuals.hyperlink_color = colors.accent;
            for widget in [
                &mut style.visuals.widgets.noninteractive,
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.open,
            ] {
                widget.corner_radius = CornerRadius::same(2);
                widget.fg_stroke = Stroke::new(1.0_f32, colors.text);
                widget.bg_stroke = Stroke::new(1.0_f32, colors.border);
            }
            style.visuals.widgets.noninteractive.bg_fill = colors.surface;
            style.visuals.widgets.inactive.bg_fill = colors.surface;
            style.visuals.widgets.inactive.weak_bg_fill = colors.surface;
            style.visuals.widgets.hovered.bg_fill = colors.elevated;
            style.visuals.widgets.hovered.weak_bg_fill = colors.elevated;
            style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, colors.accent);
            style.visuals.widgets.active.bg_fill = colors.accent_soft;
            style.visuals.widgets.active.weak_bg_fill = colors.accent_soft;
            style.visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, colors.accent);
            style.visuals.widgets.open.bg_fill = colors.accent_soft;
            style.visuals.widgets.open.weak_bg_fill = colors.accent_soft;
        });
    }
}

pub(crate) fn primary_button(ui: &egui::Ui, label: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(Color32::WHITE).strong())
        .fill(Color32::from_rgb(0, 120, 196))
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(0.0, ui.spacing().interact_size.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_theme_switch_retains_configured_spacing_and_distinct_surfaces() {
        let context = egui::Context::default();
        configure(&context);
        for theme in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            context.set_theme(theme);
            let style = context.style();
            assert_eq!(style.visuals.dark_mode, theme == egui::Theme::Dark);
            assert!(style.spacing.interact_size.y >= 28.0);
            assert_ne!(style.visuals.text_color(), style.visuals.panel_fill);
            assert_ne!(style.visuals.window_fill, style.visuals.panel_fill);
        }
    }
}
