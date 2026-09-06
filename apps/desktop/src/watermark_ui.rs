use eframe::egui;
use gif_from_screen_domain::{BlendMode, PhysicalPoint, PhysicalPx, PhysicalSize};

use crate::{
    editor_workspace::{EditorWorkspace, OverlaySelectionAnchor, RasterOverlayEdit},
    watermark_decode_job::{DecodedWatermark, WatermarkDecodeJob, WatermarkDecodeJobState},
};

#[derive(Clone, Debug)]
pub(crate) struct WatermarkUiState {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) item_opacity: u8,
    pub(crate) track_opacity: u8,
    pub(crate) blend_mode: BlendMode,
    pub(crate) z_index: i32,
}

/// A decode result may only be committed to the authoring target that requested it.
pub(crate) struct PendingWatermark {
    target: OverlaySelectionAnchor,
    settings: WatermarkUiState,
}

impl PendingWatermark {
    pub(crate) fn start(
        settings: &WatermarkUiState,
        workspace: &EditorWorkspace,
        job: &mut WatermarkDecodeJob,
    ) -> Result<Self, String> {
        let target = workspace
            .overlay_selection_anchor()
            .map_err(|error| error.to_string())?;
        if settings.name.trim().is_empty() {
            return Err("Watermark name is required.".to_owned());
        }
        if settings.item_opacity == 0 || settings.track_opacity == 0 {
            return Err("Watermark opacity must be greater than zero.".to_owned());
        }
        job.start(settings.path.trim().into())
            .map_err(|error| error.to_string())?;
        Ok(Self {
            target,
            settings: settings.clone(),
        })
    }

    pub(crate) fn commit(
        self,
        workspace: &mut EditorWorkspace,
        decoded: &DecodedWatermark,
    ) -> Result<(), String> {
        if !self.target.matches(workspace) {
            return Err(
                "The project or selected frames changed while decoding. Select the intended frames and retry."
                    .to_owned(),
            );
        }
        let edit = build_raster_edit(&self.settings, decoded)?;
        workspace
            .add_raster_overlay_for_selection(edit, &decoded.rgba)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

impl Default for WatermarkUiState {
    fn default() -> Self {
        Self {
            path: String::new(),
            name: "Watermark".to_owned(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            item_opacity: 255,
            track_opacity: 255,
            blend_mode: BlendMode::Normal,
            z_index: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum WatermarkUiAction {
    #[default]
    None,
    Start,
}

pub(crate) fn show_watermark_ui(
    ui: &mut egui::Ui,
    state: &mut WatermarkUiState,
    job_state: WatermarkDecodeJobState,
    has_selection: bool,
) -> WatermarkUiAction {
    let running = job_state == WatermarkDecodeJobState::Running;
    let mut action = WatermarkUiAction::None;
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Raster watermark");
            ui.weak("PNG, JPEG, BMP, or WebP · decoded off the UI thread · maximum 4096²/64 MiB");
        });
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Image path");
                ui.add(
                    egui::TextEdit::singleline(&mut state.path)
                        .desired_width(360.0)
                        .hint_text("/path/to/logo.png"),
                );
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut state.name).desired_width(110.0));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("X/Y/W/H");
                ui.add(egui::DragValue::new(&mut state.x));
                ui.add(egui::DragValue::new(&mut state.y));
                ui.add(egui::DragValue::new(&mut state.width));
                ui.add(egui::DragValue::new(&mut state.height));
                ui.weak("W/H = 0 keeps source dimensions");
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Image opacity");
                ui.add(egui::DragValue::new(&mut state.item_opacity).range(1..=u8::MAX));
                ui.label("Track opacity");
                ui.add(egui::DragValue::new(&mut state.track_opacity).range(1..=u8::MAX));
                ui.label("Z");
                ui.add(egui::DragValue::new(&mut state.z_index));
                ui.label("Blend");
                egui::ComboBox::from_id_salt("watermark_blend")
                    .selected_text(blend_label(state.blend_mode))
                    .show_ui(ui, |ui| {
                        for blend in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
                            ui.selectable_value(&mut state.blend_mode, blend, blend_label(blend));
                        }
                    });
            });
        });
        if running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Decoding watermark…");
            });
        } else if ui
            .add_enabled(has_selection, egui::Button::new("Decode and add watermark"))
            .clicked()
        {
            action = WatermarkUiAction::Start;
        }
        if !has_selection {
            ui.weak("Select at least one frame before adding a watermark.");
        }
    });
    action
}

pub(crate) fn build_raster_edit(
    state: &WatermarkUiState,
    decoded: &DecodedWatermark,
) -> Result<RasterOverlayEdit, String> {
    if state.name.trim().is_empty() {
        return Err("Watermark name is required.".to_owned());
    }
    if state.item_opacity == 0 || state.track_opacity == 0 {
        return Err("Watermark image and track opacity must be greater than zero.".to_owned());
    }
    let width = if state.width == 0 {
        decoded.size.width.get()
    } else {
        state.width
    };
    let height = if state.height == 0 {
        decoded.size.height.get()
    } else {
        state.height
    };
    let display_size = PhysicalSize::new(width, height)
        .map_err(|error| format!("Invalid watermark display size: {error}"))?;
    Ok(RasterOverlayEdit {
        name: state.name.trim().to_owned(),
        source_size: decoded.size,
        position: PhysicalPoint {
            x: PhysicalPx::new(state.x),
            y: PhysicalPx::new(state.y),
        },
        display_size,
        item_opacity: state.item_opacity,
        track_opacity: state.track_opacity,
        blend_mode: state.blend_mode,
        z_index: state.z_index,
    })
}

const fn blend_label(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn decoded() -> DecodedWatermark {
        DecodedWatermark {
            source_path: PathBuf::from("logo.png"),
            size: PhysicalSize::new(20, 10).unwrap(),
            rgba: vec![0; 20 * 10 * 4],
        }
    }

    #[test]
    fn zero_display_dimensions_use_source_and_explicit_values_override_them() {
        let source = decoded();
        let natural = build_raster_edit(&WatermarkUiState::default(), &source).unwrap();
        assert_eq!(natural.display_size, source.size);

        let resized = build_raster_edit(
            &WatermarkUiState {
                x: 4,
                y: 5,
                width: 100,
                height: 50,
                blend_mode: BlendMode::Screen,
                ..WatermarkUiState::default()
            },
            &source,
        )
        .unwrap();
        assert_eq!(resized.position.x.get(), 4);
        assert_eq!(resized.position.y.get(), 5);
        assert_eq!(resized.display_size, PhysicalSize::new(100, 50).unwrap());
        assert_eq!(resized.blend_mode, BlendMode::Screen);
    }

    #[test]
    fn invisible_or_unnamed_watermarks_are_rejected_before_persistence() {
        let source = decoded();
        for state in [
            WatermarkUiState {
                name: String::new(),
                ..WatermarkUiState::default()
            },
            WatermarkUiState {
                item_opacity: 0,
                ..WatermarkUiState::default()
            },
            WatermarkUiState {
                track_opacity: 0,
                ..WatermarkUiState::default()
            },
        ] {
            assert!(build_raster_edit(&state, &source).is_err());
        }
    }
}
