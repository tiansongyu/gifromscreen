//! Presets restore encoder choices only, never a path or overwrite permission.

use eframe::egui;
use gif_from_screen_domain::{
    GifExportPreset, GifLoop, GifPaletteStrategy, GifPresetOptions, validate_export_preset,
};

use crate::{
    EditorExportSettings, ExportLoopChoice, ExportPaletteChoice, ProjectFrameSelection,
    build_project_export_options, editor_workspace::EditorWorkspace,
};

impl EditorExportSettings {
    fn export_preset(&self) -> Result<GifExportPreset, String> {
        if self.custom_palette_text.len() > gif_from_screen_domain::MAX_CUSTOM_PALETTE_TEXT_BYTES {
            return Err("Preset custom palette text must not exceed 4096 bytes.".to_owned());
        }
        build_project_export_options(self, ProjectFrameSelection::All)?;
        let preset = GifExportPreset {
            colors: self.max_colors,
            palette: match self.palette {
                ExportPaletteChoice::Local => GifPaletteStrategy::PerFrame,
                ExportPaletteChoice::Global => GifPaletteStrategy::Global,
            },
            repeat: match self.loop_choice {
                ExportLoopChoice::Infinite => GifLoop::Infinite,
                ExportLoopChoice::Finite => GifLoop::Finite(self.finite_loop_count),
            },
            alpha_threshold: self.alpha_threshold,
            options: Some(GifPresetOptions {
                version: 1,
                frame_scope: self.frame_scope,
                quantizer: self.quantizer,
                dither: self.dither,
                delta: self.delta,
                custom_palette_text: self.custom_palette_text.clone(),
                custom_transparency_enabled: self.custom_transparency_enabled,
                custom_transparent_index: self.custom_transparent_index,
                finite_loop_count: self.finite_loop_count,
            }),
        };
        validate_export_preset(&preset)?;
        Ok(preset)
    }

    fn load_export_preset(&mut self, preset: &GifExportPreset) -> Result<(), String> {
        validate_export_preset(preset)?;
        let options = preset.options.clone().unwrap_or_else(|| GifPresetOptions {
            finite_loop_count: match preset.repeat {
                GifLoop::Finite(count) => count,
                GifLoop::Infinite => 1,
            },
            ..GifPresetOptions::default()
        });
        let candidate = Self {
            frame_scope: options.frame_scope,
            max_colors: preset.colors,
            palette: match preset.palette {
                GifPaletteStrategy::PerFrame => ExportPaletteChoice::Local,
                GifPaletteStrategy::Global => ExportPaletteChoice::Global,
                GifPaletteStrategy::Adaptive => return Err(
                    "This legacy preset uses Adaptive palette selection, which is not available in the editor. Choose an explicit palette and save a new preset.".to_owned()
                ),
            },
            quantizer: options.quantizer,
            custom_palette_text: options.custom_palette_text,
            custom_transparency_enabled: options.custom_transparency_enabled,
            custom_transparent_index: options.custom_transparent_index,
            dither: options.dither,
            delta: options.delta,
            alpha_threshold: preset.alpha_threshold,
            loop_choice: match preset.repeat {
                GifLoop::Infinite => ExportLoopChoice::Infinite,
                GifLoop::Finite(_) => ExportLoopChoice::Finite,
            },
            finite_loop_count: options.finite_loop_count,
            // Loading never grants permission to replace an existing file.
            overwrite: false,
        };
        build_project_export_options(&candidate, ProjectFrameSelection::All)?;
        *self = candidate;
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct PresetUiState {
    selected: String,
    name: String,
    notice: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum PresetAction {
    Load,
    SaveNew,
    Update,
    Rename,
    Delete,
}

pub(crate) fn show_export_presets(
    ui: &mut egui::Ui,
    settings: &mut EditorExportSettings,
    workspace: &mut EditorWorkspace,
) {
    let id = ui.id().with((
        "export-presets",
        workspace.manifest().project_id.to_string(),
    ));
    let mut state = ui.data_mut(|data| data.get_temp::<PresetUiState>(id).unwrap_or_default());
    if !workspace
        .manifest()
        .export_presets
        .contains_key(&state.selected)
    {
        state.selected.clear();
    }
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        ui.label("Project preset");
        egui::ComboBox::from_id_salt("export-preset-list")
            .selected_text(if state.selected.is_empty() {
                "Choose a preset"
            } else {
                &state.selected
            })
            .show_ui(ui, |ui| {
                for name in workspace.manifest().export_presets.keys() {
                    if ui
                        .selectable_value(&mut state.selected, name.clone(), name)
                        .changed()
                    {
                        state.name.clone_from(name);
                    }
                }
            });
        ui.add_enabled_ui(!state.selected.is_empty(), |ui| {
            for (label, requested) in [
                ("Load", PresetAction::Load),
                ("Update selected", PresetAction::Update),
                ("Delete", PresetAction::Delete),
            ] {
                if ui.button(label).clicked() {
                    action = Some(requested);
                }
            }
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.name)
                .hint_text("Preset name")
                .char_limit(64)
                .desired_width(220.0),
        );
        if ui.button("Save new").clicked() {
            action = Some(PresetAction::SaveNew);
        }
        if ui
            .add_enabled(
                !state.selected.is_empty(),
                egui::Button::new("Rename selected"),
            )
            .clicked()
        {
            action = Some(PresetAction::Rename);
        }
    });
    ui.weak("Saved inside this project · changes support Undo · loading clears overwrite permission and keeps the output path.");
    if let Some(action) = action {
        state.notice = Some(
            match apply_preset_action(action, &mut state, settings, workspace) {
                Ok(message) => message,
                Err(error) => format!("Preset: {error}"),
            },
        );
    }
    if let Some(notice) = &state.notice {
        ui.label(notice);
    }
    ui.add_space(8.0);
    ui.data_mut(|data| data.insert_temp(id, state));
}

fn apply_preset_action(
    action: PresetAction,
    state: &mut PresetUiState,
    settings: &mut EditorExportSettings,
    workspace: &mut EditorWorkspace,
) -> Result<String, String> {
    match action {
        PresetAction::Load => {
            let preset = workspace
                .manifest()
                .export_presets
                .get(&state.selected)
                .ok_or_else(|| "Select an existing preset first.".to_owned())?;
            settings.load_export_preset(preset)?;
            Ok(format!(
                "Loaded {}. Review the current frame selection before exporting.",
                state.selected
            ))
        }
        PresetAction::SaveNew => {
            workspace.save_export_preset(&state.name, settings.export_preset()?, false)?;
            state.selected = state.name.trim().to_owned();
            Ok(format!("Saved {} in this project.", state.selected))
        }
        PresetAction::Update => {
            workspace.save_export_preset(&state.selected, settings.export_preset()?, true)?;
            Ok(format!(
                "Updated {}. Undo restores its previous settings.",
                state.selected
            ))
        }
        PresetAction::Rename => {
            workspace.rename_export_preset(&state.selected, &state.name)?;
            state.selected = state.name.trim().to_owned();
            Ok(format!("Renamed preset to {}.", state.selected))
        }
        PresetAction::Delete => {
            workspace.delete_export_preset(&state.selected)?;
            state.selected.clear();
            Ok("Deleted preset. Undo restores it.".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use gif_from_screen_domain::{
        Canvas, CanvasBackground, ColorSpace, PhysicalSize, ProjectId, ProjectManifest, UnixTimeMs,
    };
    use gif_from_screen_project::{ActiveProject, LockPolicy};

    use crate::{ExportDitherChoice, ExportFrameScope, ExportQuantizerChoice};

    use super::*;

    fn workspace(root: &std::path::Path) -> EditorWorkspace {
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "preset-test",
            UnixTimeMs::new(0),
            Canvas {
                size: PhysicalSize::new(1, 1).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        EditorWorkspace::from_active(ActiveProject::create(root, manifest).unwrap(), 16).unwrap()
    }

    fn settings() -> EditorExportSettings {
        EditorExportSettings {
            frame_scope: ExportFrameScope::Selected,
            max_colors: 256,
            palette: ExportPaletteChoice::Global,
            quantizer: ExportQuantizerChoice::Custom,
            custom_palette_text: "#111111, #ACDC42\n#fedcba".to_owned(),
            custom_transparency_enabled: true,
            custom_transparent_index: 2,
            dither: ExportDitherChoice::BlueNoise,
            delta: true,
            alpha_threshold: 128,
            loop_choice: ExportLoopChoice::Finite,
            finite_loop_count: 17,
            overwrite: true,
        }
    }

    #[test]
    fn all_quantizers_and_dithers_round_trip_every_gui_option_without_overwrite() {
        for quantizer in [
            ExportQuantizerChoice::MedianCut,
            ExportQuantizerChoice::Octree,
            ExportQuantizerChoice::Wu,
            ExportQuantizerChoice::Grayscale,
            ExportQuantizerChoice::MostUsed,
            ExportQuantizerChoice::NeuQuant,
            ExportQuantizerChoice::WebSafe216,
            ExportQuantizerChoice::Windows16,
            ExportQuantizerChoice::Monochrome,
            ExportQuantizerChoice::Custom,
        ] {
            for dither in [
                ExportDitherChoice::None,
                ExportDitherChoice::Bayer,
                ExportDitherChoice::Dotted,
                ExportDitherChoice::BlueNoise,
                ExportDitherChoice::InterleavedNoise,
                ExportDitherChoice::FloydSteinberg,
                ExportDitherChoice::Atkinson,
                ExportDitherChoice::Burkes,
                ExportDitherChoice::Sierra,
                ExportDitherChoice::SierraLite,
                ExportDitherChoice::TwoRowSierra,
                ExportDitherChoice::JarvisJudiceNinke,
                ExportDitherChoice::Stucki,
                ExportDitherChoice::StevensonArce,
            ] {
                let original = EditorExportSettings {
                    quantizer,
                    dither,
                    ..settings()
                };
                let preset = original.export_preset().unwrap();
                let mut restored = EditorExportSettings {
                    overwrite: true,
                    ..EditorExportSettings::default()
                };
                restored.load_export_preset(&preset).unwrap();
                assert_eq!(
                    restored,
                    EditorExportSettings {
                        overwrite: false,
                        ..original
                    }
                );
            }
        }
    }

    #[test]
    fn ui_actions_save_update_rename_delete_and_undo_survive_project_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(directory.path());
        let mut settings = settings();
        let mut state = PresetUiState {
            name: "Demo".to_owned(),
            ..PresetUiState::default()
        };
        apply_preset_action(
            PresetAction::SaveNew,
            &mut state,
            &mut settings,
            &mut workspace,
        )
        .unwrap();
        let saved = workspace.manifest().export_presets.clone();
        assert_eq!(state.selected, "Demo");
        settings.dither = ExportDitherChoice::Stucki;
        apply_preset_action(
            PresetAction::Update,
            &mut state,
            &mut settings,
            &mut workspace,
        )
        .unwrap();
        let updated = workspace.manifest().export_presets.clone();
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().export_presets, saved);
        workspace.redo().unwrap();
        assert_eq!(workspace.manifest().export_presets, updated);
        state.name = "Share".to_owned();
        apply_preset_action(
            PresetAction::Rename,
            &mut state,
            &mut settings,
            &mut workspace,
        )
        .unwrap();
        let renamed = workspace.manifest().export_presets.clone();
        assert!(!renamed.contains_key("Demo"));
        assert!(renamed.contains_key("Share"));
        apply_preset_action(
            PresetAction::Delete,
            &mut state,
            &mut settings,
            &mut workspace,
        )
        .unwrap();
        assert!(workspace.manifest().export_presets.is_empty());
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().export_presets, renamed);
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().export_presets, updated);
        workspace.redo().unwrap();
        drop(workspace);
        let mut reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(reopened.manifest().export_presets, renamed);
        state.selected = "Share".to_owned();
        settings = EditorExportSettings::default();
        apply_preset_action(PresetAction::Load, &mut state, &mut settings, &mut reopened).unwrap();
        assert_eq!(settings.dither, ExportDitherChoice::Stucki);
        assert_eq!(settings.quantizer, ExportQuantizerChoice::Custom);
        assert_eq!(settings.custom_transparent_index, 2);
        assert!(!settings.overwrite);
    }

    #[test]
    fn invalid_save_colliding_names_and_failed_load_leave_state_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(directory.path());
        let mut settings = settings();
        let mut state = PresetUiState {
            name: "Demo".to_owned(),
            ..PresetUiState::default()
        };
        apply_preset_action(
            PresetAction::SaveNew,
            &mut state,
            &mut settings,
            &mut workspace,
        )
        .unwrap();
        let original = workspace.manifest().clone();
        assert!(
            apply_preset_action(
                PresetAction::SaveNew,
                &mut state,
                &mut settings,
                &mut workspace
            )
            .is_err()
        );
        assert!(workspace.rename_export_preset("Demo", "Demo").is_err());
        settings.custom_palette_text = "#BAD".to_owned();
        assert!(
            apply_preset_action(
                PresetAction::Update,
                &mut state,
                &mut settings,
                &mut workspace
            )
            .is_err()
        );
        assert_eq!(workspace.manifest(), &original);
        let mut unsupported = original.export_presets["Demo"].clone();
        unsupported.palette = GifPaletteStrategy::Adaptive;
        let before = settings.clone();
        assert!(settings.load_export_preset(&unsupported).is_err());
        assert_eq!(settings, before);
    }

    #[test]
    fn older_presets_load_explicit_defaults_and_infinite_preserves_remembered_count() {
        let legacy = GifExportPreset {
            colors: 17,
            palette: GifPaletteStrategy::PerFrame,
            repeat: GifLoop::Finite(3),
            alpha_threshold: 8,
            options: None,
        };
        let mut settings = settings();
        settings.load_export_preset(&legacy).unwrap();
        assert_eq!(settings.max_colors, 17);
        assert_eq!(settings.palette, ExportPaletteChoice::Local);
        assert_eq!(settings.finite_loop_count, 3);
        assert_eq!(settings.quantizer, ExportQuantizerChoice::MedianCut);
        assert_eq!(settings.dither, ExportDitherChoice::None);
        settings.loop_choice = ExportLoopChoice::Infinite;
        settings.finite_loop_count = 42;
        let preset = settings.export_preset().unwrap();
        settings.finite_loop_count = 1;
        settings.load_export_preset(&preset).unwrap();
        assert_eq!(settings.loop_choice, ExportLoopChoice::Infinite);
        assert_eq!(settings.finite_loop_count, 42);
    }
}
