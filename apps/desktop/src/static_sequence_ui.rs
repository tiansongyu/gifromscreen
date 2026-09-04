use std::path::{Path, PathBuf};

use eframe::egui;
use gif_from_screen_media::DEFAULT_ZERO_DELAY_US;

const MAX_UNIFORM_FRAME_DURATION_MS: u64 = 3_600_000;

/// User-selected timing mode for an imported static-image sequence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum StaticSequenceTimingChoice {
    #[default]
    Uniform,
    PreserveDefault,
}

/// User-selected playback behavior for an imported sequence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum StaticSequenceLoopChoice {
    Once,
    #[default]
    Infinite,
    Finite,
}

/// Editable state retained while the sequence-import page is open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StaticSequenceUiState {
    pub(crate) inputs: Vec<PathBuf>,
    pub(crate) pending_path: String,
    pub(crate) target: String,
    pub(crate) timing: StaticSequenceTimingChoice,
    pub(crate) uniform_duration_ms: u64,
    pub(crate) loop_choice: StaticSequenceLoopChoice,
    pub(crate) finite_repeats: u16,
}

impl Default for StaticSequenceUiState {
    fn default() -> Self {
        let target = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("image-sequence.gfsproj")
            .to_string_lossy()
            .into_owned();
        Self {
            inputs: Vec::new(),
            pending_path: String::new(),
            target,
            timing: StaticSequenceTimingChoice::Uniform,
            uniform_duration_ms: 100,
            loop_choice: StaticSequenceLoopChoice::Infinite,
            finite_repeats: 3,
        }
    }
}

impl StaticSequenceUiState {
    pub(crate) fn replace_inputs(&mut self, inputs: Vec<PathBuf>, target: &Path) {
        self.inputs = inputs;
        self.target = target.to_string_lossy().into_owned();
        self.pending_path.clear();
    }

    fn add_pending_path(&mut self) -> Result<(), String> {
        let path = self.pending_path.trim();
        if path.is_empty() {
            return Err("Enter an image path before adding it to the sequence.".to_owned());
        }
        self.inputs.push(PathBuf::from(path));
        self.pending_path.clear();
        Ok(())
    }

    fn apply_list_action(&mut self, action: StaticSequenceListAction) {
        match action {
            StaticSequenceListAction::MoveUp(index) if index > 0 && index < self.inputs.len() => {
                self.inputs.swap(index, index - 1);
            }
            StaticSequenceListAction::MoveDown(index) => {
                if let Some(next) = index
                    .checked_add(1)
                    .filter(|next| *next < self.inputs.len())
                {
                    self.inputs.swap(index, next);
                }
            }
            StaticSequenceListAction::Remove(index) if index < self.inputs.len() => {
                self.inputs.remove(index);
            }
            StaticSequenceListAction::MoveUp(_) | StaticSequenceListAction::Remove(_) => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StaticSequenceListAction {
    MoveUp(usize),
    MoveDown(usize),
    Remove(usize),
}

/// Action requested by the sequence-import page during one UI frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StaticSequenceUiAction {
    None,
    Start,
    Back,
    Notice(String),
}

/// Draws the ordered sequence form without starting background work itself.
pub(crate) fn show_static_sequence_ui(
    ui: &mut egui::Ui,
    state: &mut StaticSequenceUiState,
    running: bool,
) -> StaticSequenceUiAction {
    ui.heading("Import image sequence");
    ui.label("Add at least two PNG, JPEG, BMP, or WebP files in timeline order.");
    ui.weak(
        "You can also drop two or more image files anywhere in the app. Decode is limited to 10,000 frames, 16K edges, and 512 MiB RGBA, and cannot currently be cancelled.",
    );
    ui.add_space(8.0);

    let mut action = StaticSequenceUiAction::None;
    ui.add_enabled_ui(!running, |ui| {
        ui.horizontal(|ui| {
            ui.label("Image path");
            let response = ui.add(
                egui::TextEdit::singleline(&mut state.pending_path)
                    .desired_width(430.0)
                    .hint_text("/path/to/frame.png"),
            );
            let submit = ui.button("Add").clicked()
                || response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if submit && let Err(message) = state.add_pending_path() {
                action = StaticSequenceUiAction::Notice(message);
            }
        });
    });

    ui.add_space(6.0);
    ui.label(format!("Timeline order · {} image(s)", state.inputs.len()));
    let list_action = show_ordered_input_list(ui, state, running);
    if let Some(list_action) = list_action {
        state.apply_list_action(list_action);
    }

    ui.add_space(8.0);
    show_sequence_settings(ui, state, running);

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!running, egui::Button::new("Import sequence"))
            .clicked()
        {
            action = StaticSequenceUiAction::Start;
        }
        if ui
            .add_enabled(!running, egui::Button::new("Back"))
            .clicked()
        {
            action = StaticSequenceUiAction::Back;
        }
        if running {
            ui.spinner();
            ui.label(format!(
                "Decoding and creating {} frames… this operation is not cancellable.",
                state.inputs.len()
            ));
        }
    });
    action
}

fn show_sequence_settings(ui: &mut egui::Ui, state: &mut StaticSequenceUiState, running: bool) {
    ui.add_enabled_ui(!running, |ui| {
        ui.horizontal(|ui| {
            ui.label("Frame timing");
            ui.radio_value(
                &mut state.timing,
                StaticSequenceTimingChoice::Uniform,
                "Uniform",
            );
            ui.add_enabled(
                state.timing == StaticSequenceTimingChoice::Uniform,
                egui::DragValue::new(&mut state.uniform_duration_ms)
                    .range(1..=MAX_UNIFORM_FRAME_DURATION_MS)
                    .suffix(" ms"),
            );
            ui.radio_value(
                &mut state.timing,
                StaticSequenceTimingChoice::PreserveDefault,
                format!(
                    "Keep decoder default ({} ms)",
                    DEFAULT_ZERO_DELAY_US / 1_000
                ),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Loop");
            ui.radio_value(
                &mut state.loop_choice,
                StaticSequenceLoopChoice::Once,
                "Once",
            );
            ui.radio_value(
                &mut state.loop_choice,
                StaticSequenceLoopChoice::Finite,
                "Finite",
            );
            ui.add_enabled(
                state.loop_choice == StaticSequenceLoopChoice::Finite,
                egui::DragValue::new(&mut state.finite_repeats)
                    .range(1..=u16::MAX)
                    .suffix(" repeats"),
            );
            ui.radio_value(
                &mut state.loop_choice,
                StaticSequenceLoopChoice::Infinite,
                "Infinite",
            );
        });
        ui.horizontal(|ui| {
            ui.label("Project directory");
            ui.add(
                egui::TextEdit::singleline(&mut state.target)
                    .desired_width(430.0)
                    .hint_text("/path/to/sequence.gfsproj"),
            );
        });
    });
}

fn show_ordered_input_list(
    ui: &mut egui::Ui,
    state: &StaticSequenceUiState,
    running: bool,
) -> Option<StaticSequenceListAction> {
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("static-sequence-inputs")
        .max_height(180.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if state.inputs.is_empty() {
                ui.weak("No images selected yet.");
                return;
            }
            for (index, path) in state.inputs.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{}.", index + 1));
                    if ui
                        .add_enabled(!running && index > 0, egui::Button::new("↑"))
                        .on_hover_text("Move earlier")
                        .clicked()
                    {
                        action = Some(StaticSequenceListAction::MoveUp(index));
                    }
                    if ui
                        .add_enabled(
                            !running && index + 1 < state.inputs.len(),
                            egui::Button::new("↓"),
                        )
                        .on_hover_text("Move later")
                        .clicked()
                    {
                        action = Some(StaticSequenceListAction::MoveDown(index));
                    }
                    if ui
                        .add_enabled(!running, egui::Button::new("Remove"))
                        .clicked()
                    {
                        action = Some(StaticSequenceListAction::Remove(index));
                    }
                    ui.monospace(path.display().to_string());
                });
            }
        });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<PathBuf> {
        ["one.png", "two.jpg", "three.webp"]
            .into_iter()
            .map(PathBuf::from)
            .collect()
    }

    #[test]
    fn replace_add_move_and_remove_preserve_explicit_user_order() {
        let mut state = StaticSequenceUiState::default();
        state.replace_inputs(paths(), Path::new("ordered.gfsproj"));
        assert_eq!(state.inputs, paths());
        assert_eq!(state.target, "ordered.gfsproj");

        state.apply_list_action(StaticSequenceListAction::MoveUp(2));
        assert_eq!(state.inputs[1], PathBuf::from("three.webp"));
        state.apply_list_action(StaticSequenceListAction::MoveDown(0));
        assert_eq!(state.inputs[0], PathBuf::from("three.webp"));
        state.apply_list_action(StaticSequenceListAction::Remove(1));
        assert_eq!(
            state.inputs,
            [PathBuf::from("three.webp"), PathBuf::from("two.jpg")]
        );

        state.pending_path = " four.bmp ".to_owned();
        state.add_pending_path().unwrap();
        assert_eq!(state.inputs[2], PathBuf::from("four.bmp"));
        assert!(state.pending_path.is_empty());
    }

    #[test]
    fn invalid_list_actions_and_empty_add_are_non_destructive() {
        let mut state = StaticSequenceUiState {
            inputs: paths(),
            pending_path: "  ".to_owned(),
            ..StaticSequenceUiState::default()
        };
        let before = state.inputs.clone();
        assert!(state.add_pending_path().is_err());
        state.apply_list_action(StaticSequenceListAction::MoveUp(0));
        state.apply_list_action(StaticSequenceListAction::MoveDown(usize::MAX));
        state.apply_list_action(StaticSequenceListAction::Remove(usize::MAX));
        assert_eq!(state.inputs, before);
    }
}
