//! Shared, unit-explicit image effects for interactive editing and saved task chains.

use eframe::egui;
use gif_from_screen_domain::{ImageBorderStyle, ImageShadowStyle, Rgba};

pub(crate) fn show_border(ui: &mut egui::Ui, style: &mut ImageBorderStyle) {
    ui.label("Positive edges draw inside; negative edges expand the canvas on all frames.");
    ui.horizontal_wrapped(|ui| {
        for (label, edge) in [
            ("Top", &mut style.widths.top_milli),
            ("Right", &mut style.widths.right_milli),
            ("Bottom", &mut style.widths.bottom_milli),
            ("Left", &mut style.widths.left_milli),
        ] {
            border_edge(ui, label, edge);
        }
    });
    color_input(ui, "Border", &mut style.color);
    color_input(ui, "Background", &mut style.background);
    ui.weak("The reference border uses a white background. A transparent background preserves source transparency.");
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "finite GUI value is rounded and bounded before fixed-point conversion"
)]
fn border_edge(ui: &mut egui::Ui, label: &str, stored: &mut i32) {
    let mut value = f64::from(*stored) / 1000.0;
    let response = ui.add(
        egui::DragValue::new(&mut value)
            .prefix(format!("{label} "))
            .suffix(" px")
            .range(-500.0..=50.0)
            .speed(1.0)
            .max_decimals(0),
    );
    if response.changed() && value.is_finite() {
        *stored = (value.clamp(-500.0, 50.0).round() * 1000.0) as i32;
    }
}

pub(crate) fn show_shadow(ui: &mut egui::Ui, style: &mut ImageShadowStyle) {
    ui.label("Applies to all frames. The canvas expands to keep space for the shadow.");
    ui.horizontal_wrapped(|ui| {
        hundredths(ui, "Blur", " px", &mut style.blur_radius_hundredths, 100.0);
        hundredths(ui, "Distance", " px", &mut style.depth_hundredths, 100.0);
        hundredths(ui, "Direction", "°", &mut style.direction_hundredths, 360.0);
        hundredths(ui, "Opacity", "%", &mut style.opacity_basis_points, 100.0);
    });
    ui.weak("0° points right; 90° points up. Blur, distance, angle and opacity keep two decimal places.");
    ui.horizontal(|ui| {
        ui.label("Shadow color");
        let mut color = [style.color.red, style.color.green, style.color.blue];
        if ui.color_edit_button_srgb(&mut color).changed() {
            style.color = Rgba {
                red: color[0],
                green: color[1],
                blue: color[2],
                alpha: 255,
            };
        }
    });
    color_input(ui, "Background", &mut style.background);
    ui.weak(
        "Use an opaque background to keep soft shadow edges in a GIF; GIF transparency is binary.",
    );
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite GUI value is clamped into a positive u16 fixed-point range"
)]
fn hundredths(ui: &mut egui::Ui, label: &str, suffix: &str, stored: &mut u16, max: f64) {
    let mut value = f64::from(*stored) / 100.0;
    let response = ui.add(
        egui::DragValue::new(&mut value)
            .prefix(format!("{label} "))
            .suffix(suffix)
            .range(0.0..=max)
            .speed(0.5)
            .min_decimals(2)
            .max_decimals(2),
    );
    if response.changed() && value.is_finite() {
        *stored = (value.clamp(0.0, max) * 100.0).round() as u16;
    }
}

fn color_input(ui: &mut egui::Ui, label: &str, color: &mut Rgba) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut bytes = [color.red, color.green, color.blue, color.alpha];
        if ui
            .color_edit_button_srgba_unmultiplied(&mut bytes)
            .changed()
        {
            *color = Rgba {
                red: bytes[0],
                green: bytes[1],
                blue: bytes[2],
                alpha: bytes[3],
            };
        }
    });
}
