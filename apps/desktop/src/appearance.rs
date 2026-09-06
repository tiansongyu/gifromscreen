//! Shared desktop appearance; recorder geometry stays independent of visual styling.

use eframe::egui::{self, Color32, CornerRadius, FontId, TextStyle};

pub(crate) fn configure(context: &egui::Context) {
    // Retain the OS-selected light/dark theme and its accessible default text colors.
    context.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 28.0;
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(23.0));
        style.visuals.window_corner_radius = CornerRadius::same(10);
        for widget in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = CornerRadius::same(6);
        }
        style.visuals.selection.bg_fill = if style.visuals.dark_mode {
            Color32::from_rgb(34, 78, 132)
        } else {
            Color32::from_rgb(198, 224, 253)
        };
        style.visuals.hyperlink_color = if style.visuals.dark_mode {
            Color32::from_rgb(112, 181, 255)
        } else {
            Color32::from_rgb(0, 92, 192)
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styling_preserves_theme_and_text_contrast_defaults() {
        for dark in [false, true] {
            let context = egui::Context::default();
            context.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            let text = context.style().visuals.text_color();
            configure(&context);
            let style = context.style();
            assert_eq!(style.visuals.dark_mode, dark);
            assert_eq!(style.visuals.text_color(), text);
            assert!(style.spacing.interact_size.y >= 28.0);
        }
    }
}
