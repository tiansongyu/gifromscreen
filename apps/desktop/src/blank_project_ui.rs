use std::path::PathBuf;

use eframe::egui;

const MAX_GIF_EDGE: u32 = u16::MAX as u32;
const MAX_INITIAL_FRAME_DURATION_MS: u64 = 3_600_000;
const DEFAULT_TARGET_ATTEMPTS: usize = 10_000;

/// Background choice for a newly created animation canvas.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BlankBackgroundChoice {
    #[default]
    Transparent,
    Solid,
}

/// User-editable values retained by the blank-animation page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BlankProjectUiState {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) background_choice: BlankBackgroundChoice,
    pub(crate) red: u8,
    pub(crate) green: u8,
    pub(crate) blue: u8,
    pub(crate) alpha: u8,
    pub(crate) frame_duration_ms: u64,
    pub(crate) target: String,
}

impl Default for BlankProjectUiState {
    fn default() -> Self {
        let directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            width: 640,
            height: 480,
            background_choice: BlankBackgroundChoice::Transparent,
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
            frame_duration_ms: 100,
            target: available_blank_target(&directory)
                .to_string_lossy()
                .into_owned(),
        }
    }
}

/// Action requested by the blank-animation page during one UI frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BlankProjectUiAction {
    #[default]
    None,
    Start,
    Back,
}

/// Draws blank-animation controls without performing filesystem work.
pub(crate) fn show_blank_project_ui(
    ui: &mut egui::Ui,
    state: &mut BlankProjectUiState,
    running: bool,
) -> BlankProjectUiAction {
    ui.heading("New blank animation");
    ui.label("Create a one-frame editable project, then add or duplicate frames in the editor.");
    ui.weak(
        "Canvas edges are limited to 65,535 pixels and the initial RGBA frame to 512 MiB. Existing project paths are never overwritten.",
    );
    ui.add_space(12.0);

    ui.add_enabled_ui(!running, |ui| {
        show_canvas_controls(ui, state);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Initial frame duration");
            ui.add(
                egui::DragValue::new(&mut state.frame_duration_ms)
                    .range(1..=MAX_INITIAL_FRAME_DURATION_MS)
                    .suffix(" ms"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Project directory");
            ui.add(
                egui::TextEdit::singleline(&mut state.target)
                    .desired_width(430.0)
                    .hint_text("/path/to/blank-animation.gfsproj"),
            );
        });
    });

    ui.add_space(12.0);
    let mut action = BlankProjectUiAction::None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!running, egui::Button::new("Create project"))
            .clicked()
        {
            action = BlankProjectUiAction::Start;
        }
        if ui
            .add_enabled(!running, egui::Button::new("Back"))
            .clicked()
        {
            action = BlankProjectUiAction::Back;
        }
        if running {
            ui.spinner();
            ui.label("Allocating and creating the project… this operation is not cancellable.");
        }
    });
    action
}

fn show_canvas_controls(ui: &mut egui::Ui, state: &mut BlankProjectUiState) {
    egui::Grid::new("blank-project-canvas")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Canvas width");
            ui.add(egui::DragValue::new(&mut state.width).range(1..=MAX_GIF_EDGE));
            ui.end_row();
            ui.label("Canvas height");
            ui.add(egui::DragValue::new(&mut state.height).range(1..=MAX_GIF_EDGE));
            ui.end_row();
            ui.label("Background");
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut state.background_choice,
                    BlankBackgroundChoice::Transparent,
                    "Transparent",
                );
                ui.radio_value(
                    &mut state.background_choice,
                    BlankBackgroundChoice::Solid,
                    "Solid RGBA",
                );
            });
            ui.end_row();
            ui.label("Solid color");
            ui.add_enabled_ui(
                state.background_choice == BlankBackgroundChoice::Solid,
                |ui| {
                    ui.label("R");
                    ui.add(egui::DragValue::new(&mut state.red));
                    ui.label("G");
                    ui.add(egui::DragValue::new(&mut state.green));
                    ui.label("B");
                    ui.add(egui::DragValue::new(&mut state.blue));
                    ui.label("A");
                    ui.add(egui::DragValue::new(&mut state.alpha).range(1..=u8::MAX));
                },
            );
            ui.end_row();
        });
}

fn available_blank_target(directory: &std::path::Path) -> PathBuf {
    for suffix in 1..=DEFAULT_TARGET_ATTEMPTS {
        let filename = if suffix == 1 {
            "blank-animation.gfsproj".to_owned()
        } else {
            format!("blank-animation-{suffix}.gfsproj")
        };
        let candidate = directory.join(filename);
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join("blank-animation-new.gfsproj")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn defaults_are_safe_positive_and_gif_compatible() {
        let state = BlankProjectUiState::default();
        assert!((1..=MAX_GIF_EDGE).contains(&state.width));
        assert!((1..=MAX_GIF_EDGE).contains(&state.height));
        assert!(state.frame_duration_ms > 0);
        assert_eq!(state.background_choice, BlankBackgroundChoice::Transparent);
        assert!(state.target.ends_with(".gfsproj"));
    }

    #[test]
    fn default_target_skips_existing_project_names_without_overwriting() {
        let directory = tempdir().unwrap();
        std::fs::create_dir(directory.path().join("blank-animation.gfsproj")).unwrap();
        std::fs::write(
            directory.path().join("blank-animation-2.gfsproj"),
            b"occupied",
        )
        .unwrap();

        assert_eq!(
            available_blank_target(directory.path()),
            directory.path().join("blank-animation-3.gfsproj")
        );
    }
}
