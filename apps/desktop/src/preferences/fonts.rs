//! Embedded, deterministic CJK fallback for application UI text only.
//!
//! The unchanged upstream font and OFL provenance live in `assets/fonts`.
//! This single SC face supplies character coverage, not locale-dependent Han
//! substitutions or complex-script shaping. Project text rendering is separate.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

const FONT_NAME: &str = "Noto Sans CJK SC Regular 2.004";
static FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/NotoSansCJKsc-Regular.otf");

/// Install once during UI setup, before the first frame.
///
/// Font parsing/rasterization remains lazy in egui. These embedded bytes need no
/// filesystem lookup, network request, system locale, or DPI/zoom adjustment.
/// Keep every default Latin/emoji font in its existing order and append CJK last.
pub(crate) fn install(context: &egui::Context) {
    context.set_fonts(definitions());
}

fn definitions() -> FontDefinitions {
    let mut definitions = FontDefinitions::default();
    definitions.font_data.insert(
        FONT_NAME.to_owned(),
        FontData::from_static(FONT_BYTES).into(),
    );
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        definitions
            .families
            .entry(family)
            .or_default()
            .push(FONT_NAME.to_owned());
    }
    definitions
}

#[cfg(test)]
#[path = "fonts_tests.rs"]
mod tests;
