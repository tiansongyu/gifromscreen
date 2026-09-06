//! Project-owned export presets share the ordinary edit journal and undo stack.

use gif_from_screen_domain::{
    EditCommand, GifExportPreset, validate_export_preset, validate_export_preset_name,
    validate_export_presets,
};

use super::EditorWorkspace;

impl EditorWorkspace {
    pub(crate) fn save_export_preset(
        &mut self,
        name: &str,
        preset: GifExportPreset,
        replace: bool,
    ) -> Result<(), String> {
        let name = name.trim();
        validate_export_preset_name(name)?;
        validate_export_preset(&preset)?;
        if !replace && self.manifest().export_presets.contains_key(name) {
            return Err(
                "That preset already exists; use Update selected to replace it.".to_owned(),
            );
        }
        if replace && !self.manifest().export_presets.contains_key(name) {
            return Err("The selected preset no longer exists.".to_owned());
        }
        let mut presets = self.manifest().export_presets.clone();
        presets.insert(name.to_owned(), preset.clone());
        validate_export_presets(&presets)?;
        self.execute(EditCommand::UpsertExportPreset {
            name: name.to_owned(),
            preset,
        })
        .map_err(|error| error.to_string())
    }

    pub(crate) fn rename_export_preset(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = to.trim();
        validate_export_preset_name(to)?;
        if from == to {
            return Err("Choose a different preset name.".to_owned());
        }
        if self.manifest().export_presets.contains_key(to) {
            return Err("That name already belongs to another preset.".to_owned());
        }
        let preset = self
            .manifest()
            .export_presets
            .get(from)
            .ok_or_else(|| "The selected preset no longer exists.".to_owned())?
            .clone();
        let mut presets = self.manifest().export_presets.clone();
        presets.remove(from);
        presets.insert(to.to_owned(), preset.clone());
        validate_export_presets(&presets)?;
        self.execute(EditCommand::Compound {
            commands: vec![
                EditCommand::RemoveExportPreset {
                    name: from.to_owned(),
                },
                EditCommand::UpsertExportPreset {
                    name: to.to_owned(),
                    preset,
                },
            ],
        })
        .map_err(|error| error.to_string())
    }

    pub(crate) fn delete_export_preset(&mut self, name: &str) -> Result<(), String> {
        if !self.manifest().export_presets.contains_key(name) {
            return Err("The selected preset no longer exists.".to_owned());
        }
        self.execute(EditCommand::RemoveExportPreset {
            name: name.to_owned(),
        })
        .map_err(|error| error.to_string())
    }
}
