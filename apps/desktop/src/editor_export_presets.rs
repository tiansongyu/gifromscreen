//! Presets restore encoder choices only, never a path or overwrite permission.

use crate::ui_notice::Notice;
use eframe::egui;
use gif_from_screen_domain::{
    GifExportPreset, GifLoop, GifPaletteStrategy, GifPresetOptions, validate_export_preset,
};
use gif_from_screen_localization::{Localizer, Message};

use crate::{
    EditorExportSettings, ExportLoopChoice, ExportPaletteChoice, ProjectFrameSelection,
    build_project_export_options, editor_workspace::EditorWorkspace,
};

impl EditorExportSettings {
    fn export_preset(&self) -> Result<GifExportPreset, Notice> {
        if self.custom_palette_text.len() > gif_from_screen_domain::MAX_CUSTOM_PALETTE_TEXT_BYTES {
            return Err(Message::ExportPresetPaletteLimit.into());
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

    fn load_export_preset(&mut self, preset: &GifExportPreset) -> Result<(), Notice> {
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
                GifPaletteStrategy::Adaptive => {
                    return Err(Message::ExportPresetAdaptiveUnsupported.into());
                }
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
    notice: Option<Result<Notice, Notice>>,
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
    localizer: Localizer,
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
        ui.label(localizer.text(Message::ExportProjectPreset));
        egui::ComboBox::from_id_salt("export-preset-list")
            .selected_text(if state.selected.is_empty() {
                localizer.text(Message::ExportChoosePreset)
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
                (
                    localizer.text(Message::ExportPresetLoad),
                    PresetAction::Load,
                ),
                (
                    localizer.text(Message::ExportPresetUpdate),
                    PresetAction::Update,
                ),
                (localizer.text(Message::EditorDelete), PresetAction::Delete),
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
                .hint_text(localizer.text(Message::ExportPresetName))
                .char_limit(64)
                .desired_width(220.0),
        );
        if ui
            .button(localizer.text(Message::ExportPresetSaveNew))
            .clicked()
        {
            action = Some(PresetAction::SaveNew);
        }
        if ui
            .add_enabled(
                !state.selected.is_empty(),
                egui::Button::new(localizer.text(Message::ExportPresetRename)),
            )
            .clicked()
        {
            action = Some(PresetAction::Rename);
        }
    });
    ui.weak(localizer.text(Message::ExportPresetStorageHint));
    if let Some(action) = action {
        state.notice = Some(apply_preset_action(action, &mut state, settings, workspace));
    }
    if let Some(notice) = &state.notice {
        ui.label(preset_notice_text(notice, localizer));
    }
    ui.add_space(8.0);
    ui.data_mut(|data| data.insert_temp(id, state));
}

fn preset_notice_text(notice: &Result<Notice, Notice>, localizer: Localizer) -> String {
    match notice {
        Ok(notice) => notice.render(localizer),
        Err(error) => crate::format_message(
            localizer,
            Message::ExportPresetFailed,
            &[("error", &error.render(localizer))],
        ),
    }
}

fn apply_preset_action(
    action: PresetAction,
    state: &mut PresetUiState,
    settings: &mut EditorExportSettings,
    workspace: &mut EditorWorkspace,
) -> Result<Notice, Notice> {
    match action {
        PresetAction::Load => {
            let preset = workspace
                .manifest()
                .export_presets
                .get(&state.selected)
                .ok_or_else(|| Notice::from(Message::ExportPresetSelectFirst))?;
            settings.load_export_preset(preset)?;
            Ok(Notice::new(
                Message::ExportPresetLoaded,
                &[("name", &state.selected)],
            ))
        }
        PresetAction::SaveNew => {
            workspace.save_export_preset(&state.name, settings.export_preset()?, false)?;
            state.selected = state.name.trim().to_owned();
            Ok(Notice::new(
                Message::ExportPresetSaved,
                &[("name", &state.selected)],
            ))
        }
        PresetAction::Update => {
            workspace.save_export_preset(&state.selected, settings.export_preset()?, true)?;
            Ok(Notice::new(
                Message::ExportPresetUpdated,
                &[("name", &state.selected)],
            ))
        }
        PresetAction::Rename => {
            workspace.rename_export_preset(&state.selected, &state.name)?;
            state.selected = state.name.trim().to_owned();
            Ok(Notice::new(
                Message::ExportPresetRenamed,
                &[("name", &state.selected)],
            ))
        }
        PresetAction::Delete => {
            workspace.delete_export_preset(&state.selected)?;
            state.selected.clear();
            Ok(Message::ExportPresetDeleted.into())
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

    fn language(tag: &str) -> Localizer {
        Localizer::new(gif_from_screen_localization::find_language(tag).unwrap())
    }

    fn assert_success_languages(
        result: &Result<Notice, Notice>,
        message: Message,
        name: Option<&str>,
    ) {
        assert_eq!(result.as_ref().unwrap().message_id(), Some(message));
        let arguments: Vec<_> = name.map(|name| ("name", name)).into_iter().collect();
        let first = preset_notice_text(result, language("en"));
        for tag in ["en", "zh", "en"] {
            let localizer = language(tag);
            assert_eq!(
                preset_notice_text(result, localizer),
                localizer.format(message, &arguments).unwrap()
            );
        }
        assert_eq!(preset_notice_text(result, language("en")), first);
    }

    fn assert_failure_languages(result: &Result<Notice, Notice>, message: Message) {
        assert_eq!(result.as_ref().unwrap_err().message_id(), Some(message));
        let first = preset_notice_text(result, language("en"));
        for tag in ["en", "zh", "en"] {
            let localizer = language(tag);
            let expected = localizer
                .format(
                    Message::ExportPresetFailed,
                    &[("error", localizer.text(message))],
                )
                .unwrap();
            assert_eq!(preset_notice_text(result, localizer), expected);
        }
        assert_eq!(preset_notice_text(result, language("en")), first);
    }

    #[test]
    fn real_save_load_delete_receipts_switch_language_without_changing_user_names_or_options() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(directory.path());
        let original = settings();
        let mut settings = original.clone();
        let name = "用户 {name}";
        let mut state = PresetUiState {
            name: name.into(),
            ..Default::default()
        };
        let saved = apply_preset_action(
            PresetAction::SaveNew,
            &mut state,
            &mut settings,
            &mut workspace,
        );
        let manifest = workspace.manifest().clone();
        assert_success_languages(&saved, Message::ExportPresetSaved, Some(name));
        assert_eq!(
            preset_notice_text(&saved, language("zh")),
            "已在此工程中保存 用户 {name}。"
        );
        assert_eq!(workspace.manifest(), &manifest);
        assert_eq!(state.name, name);
        assert_eq!(state.selected, name);
        assert_eq!(settings, original);
        let stored = &manifest.export_presets[name];
        assert_eq!(
            stored.options.as_ref().unwrap().custom_palette_text,
            original.custom_palette_text
        );

        settings.dither = ExportDitherChoice::Atkinson;
        let loaded = apply_preset_action(
            PresetAction::Load,
            &mut state,
            &mut settings,
            &mut workspace,
        );
        assert_success_languages(&loaded, Message::ExportPresetLoaded, Some(name));
        assert_eq!(
            settings,
            EditorExportSettings {
                overwrite: false,
                ..original
            }
        );
        assert_eq!(workspace.manifest(), &manifest);
        assert_eq!(state.selected, name);

        let before_delete = settings.clone();
        let deleted = apply_preset_action(
            PresetAction::Delete,
            &mut state,
            &mut settings,
            &mut workspace,
        );
        let deleted_manifest = workspace.manifest().clone();
        assert_success_languages(&deleted, Message::ExportPresetDeleted, None);
        assert!(state.selected.is_empty());
        assert!(workspace.manifest().export_presets.is_empty());
        assert_eq!(workspace.manifest(), &deleted_manifest);
        assert_eq!(settings, before_delete);
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().export_presets, manifest.export_presets);
    }

    #[test]
    fn no_selection_and_legacy_adaptive_errors_relocalize_without_mutating_settings_or_project() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(directory.path());
        let mut settings = settings();
        let original = settings.clone();
        let mut state = PresetUiState::default();
        let before = workspace.manifest().clone();
        let missing = apply_preset_action(
            PresetAction::Load,
            &mut state,
            &mut settings,
            &mut workspace,
        );
        assert_failure_languages(&missing, Message::ExportPresetSelectFirst);
        assert_eq!(
            preset_notice_text(&missing, language("zh")),
            "预设：请先选择一个已有预设。"
        );
        assert_eq!(workspace.manifest(), &before);
        assert_eq!(settings, original);
        let mut legacy = settings.export_preset().unwrap();
        legacy.palette = GifPaletteStrategy::Adaptive;
        workspace
            .save_export_preset("旧预设 {name}", legacy, false)
            .unwrap();
        state.selected = "旧预设 {name}".into();
        let before = workspace.manifest().clone();
        let unsupported = apply_preset_action(
            PresetAction::Load,
            &mut state,
            &mut settings,
            &mut workspace,
        );
        assert_failure_languages(&unsupported, Message::ExportPresetAdaptiveUnsupported);
        assert_eq!(workspace.manifest(), &before);
        assert_eq!(settings, original);
        assert!(
            settings.overwrite,
            "a rejected load must not change the current permission"
        );
        assert_eq!(state.selected, "旧预设 {name}");
    }

    #[test]
    fn preset_palette_limit_remains_4096_bytes_and_failure_notice_changes_language_later() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(directory.path());
        let mut settings = settings();
        settings.custom_palette_text = format!("{:<4096}", "#000000,#FFFFFF");
        settings.custom_transparent_index = 0;
        assert_eq!(settings.custom_palette_text.len(), 4096);
        let mut state = PresetUiState {
            name: "最大文本".into(),
            ..Default::default()
        };
        let saved = apply_preset_action(
            PresetAction::SaveNew,
            &mut state,
            &mut settings,
            &mut workspace,
        );
        assert!(saved.is_ok());
        let before = workspace.manifest().clone();
        settings.custom_palette_text.push(' ');
        let original = settings.clone();
        let too_large = apply_preset_action(
            PresetAction::Update,
            &mut state,
            &mut settings,
            &mut workspace,
        );
        assert_failure_languages(&too_large, Message::ExportPresetPaletteLimit);
        assert_eq!(workspace.manifest(), &before);
        assert_eq!(settings, original);
        assert_eq!(state.selected, "最大文本");
        assert!(settings.overwrite);
    }
}
