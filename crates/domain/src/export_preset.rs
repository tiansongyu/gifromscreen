//! Versioned encoder settings, independent of output paths and overwrite consent.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{GifExportPreset, GifLoop};

pub const MAX_EXPORT_PRESETS: usize = 32;
pub const MAX_EXPORT_PRESET_BYTES: usize = 64 * 1024;
pub const MAX_EXPORT_PRESET_NAME_BYTES: usize = 64;
pub const MAX_CUSTOM_PALETTE_TEXT_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GifPresetFrameScope {
    #[default]
    All,
    Selected,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GifPresetQuantizer {
    #[default]
    MedianCut,
    Octree,
    Wu,
    Grayscale,
    MostUsed,
    NeuQuant,
    WebSafe216,
    Monochrome,
    Windows16,
    Custom,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GifPresetDither {
    #[default]
    None,
    Bayer,
    Dotted,
    BlueNoise,
    InterleavedNoise,
    FloydSteinberg,
    Atkinson,
    Burkes,
    Sierra,
    SierraLite,
    TwoRowSierra,
    JarvisJudiceNinke,
    Stucki,
    StevensonArce,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GifPresetOptions {
    pub version: u16,
    pub frame_scope: GifPresetFrameScope,
    pub quantizer: GifPresetQuantizer,
    pub dither: GifPresetDither,
    pub delta: bool,
    pub custom_palette_text: String,
    pub custom_transparency_enabled: bool,
    pub custom_transparent_index: u16,
    /// Remembered even while infinite looping is selected.
    pub finite_loop_count: u16,
}

impl Default for GifPresetOptions {
    fn default() -> Self {
        Self {
            version: 1,
            frame_scope: GifPresetFrameScope::All,
            quantizer: GifPresetQuantizer::MedianCut,
            dither: GifPresetDither::None,
            delta: false,
            custom_palette_text: "#000000\n#FFFFFF".to_owned(),
            custom_transparency_enabled: false,
            custom_transparent_index: 0,
            finite_loop_count: 1,
        }
    }
}

pub fn validate_export_preset_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || name.trim() != name
        || name.len() > MAX_EXPORT_PRESET_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(
            "preset names must contain 1..64 bytes without surrounding whitespace or control characters",
        );
    }
    Ok(())
}

pub fn validate_export_presets(
    presets: &BTreeMap<String, GifExportPreset>,
) -> Result<(), &'static str> {
    if presets.len() > MAX_EXPORT_PRESETS {
        return Err("a project can contain at most 32 export presets");
    }
    for (name, preset) in presets {
        validate_export_preset_name(name)?;
        validate_export_preset(preset)?;
    }
    // Text and counts are bounded before serialization, including escaped input.
    if serde_json::to_vec(presets)
        .map_err(|_| "could not serialize export presets")?
        .len()
        > MAX_EXPORT_PRESET_BYTES
    {
        return Err("project export presets exceed the 64 KiB serialized limit");
    }
    Ok(())
}

pub fn validate_export_preset(preset: &GifExportPreset) -> Result<(), &'static str> {
    if !(2..=256).contains(&preset.colors) {
        return Err("preset colors must be between 2 and 256");
    }
    if matches!(preset.repeat, GifLoop::Finite(0)) {
        return Err("finite loop count must be at least one");
    }
    let Some(options) = &preset.options else {
        return Ok(());
    };
    if options.version != 1 {
        return Err("unsupported export preset options version");
    }
    if options.finite_loop_count == 0
        || matches!(preset.repeat, GifLoop::Finite(count) if count != options.finite_loop_count)
    {
        return Err("preset finite loop count is invalid or inconsistent");
    }
    validate_custom_palette(options, preset.colors)?;
    let minimum = match options.quantizer {
        GifPresetQuantizer::WebSafe216 => 217,
        GifPresetQuantizer::Windows16 => 17,
        GifPresetQuantizer::Monochrome => 3,
        _ => 2,
    };
    if preset.colors < minimum {
        return Err("preset color limit is too small for its fixed palette including transparency");
    }
    Ok(())
}

fn validate_custom_palette(
    options: &GifPresetOptions,
    maximum_colors: u16,
) -> Result<(), &'static str> {
    if options.custom_palette_text.len() > MAX_CUSTOM_PALETTE_TEXT_BYTES {
        return Err("custom palette text exceeds 4096 bytes");
    }
    let mut count = 0;
    for color in options
        .custom_palette_text
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|color| !color.is_empty())
    {
        count += 1;
        if count > 256
            || color.len() != 7
            || !color.starts_with('#')
            || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
        {
            return Err(
                "custom palette must contain 2..256 strict #RRGGBB colors, including remembered inactive settings",
            );
        }
    }
    if count < 2
        || (options.quantizer == GifPresetQuantizer::Custom && count > usize::from(maximum_colors))
    {
        return Err("custom palette color count is invalid or exceeds the active color limit");
    }
    if options.custom_transparent_index > 255
        || (options.custom_transparency_enabled
            && usize::from(options.custom_transparent_index) >= count)
    {
        return Err("custom transparent index is outside the palette");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{EditCommand, GifPaletteStrategy, model::test_fixtures::manifest};

    use super::*;

    fn preset() -> GifExportPreset {
        GifExportPreset {
            colors: 128,
            palette: GifPaletteStrategy::Global,
            repeat: GifLoop::Finite(7),
            alpha_threshold: 42,
            options: Some(GifPresetOptions {
                finite_loop_count: 7,
                ..GifPresetOptions::default()
            }),
        }
    }

    #[test]
    fn legacy_presets_deserialize_without_inventing_persisted_advanced_settings() {
        let json = r#"{"colors":256,"palette":"global","repeat":"infinite","alpha_threshold":1}"#;
        let legacy: GifExportPreset = serde_json::from_str(json).unwrap();
        assert!(legacy.options.is_none());
        validate_export_preset(&legacy).unwrap();
        assert!(!serde_json::to_string(&legacy).unwrap().contains("options"));
        let partial = json.replace(
            "\"alpha_threshold\":1",
            "\"alpha_threshold\":1,\"options\":{\"version\":1}",
        );
        assert!(serde_json::from_str::<GifExportPreset>(&partial).is_err());
    }

    #[test]
    fn versioned_presets_round_trip_and_reject_inconsistent_or_invalid_settings() {
        let original = preset();
        assert_eq!(
            serde_json::from_str::<GifExportPreset>(&serde_json::to_string(&original).unwrap())
                .unwrap(),
            original
        );
        let mut invalid = original.clone();
        invalid.options.as_mut().unwrap().version = 2;
        assert!(validate_export_preset(&invalid).is_err());
        invalid = original.clone();
        invalid.options.as_mut().unwrap().finite_loop_count = 4;
        assert!(validate_export_preset(&invalid).is_err());
        invalid = original.clone();
        invalid.options.as_mut().unwrap().custom_palette_text = "#abc #123456".to_owned();
        assert!(validate_export_preset(&invalid).is_err());
        invalid = original.clone();
        invalid.options.as_mut().unwrap().quantizer = GifPresetQuantizer::WebSafe216;
        assert!(validate_export_preset(&invalid).is_err());
        invalid.colors = 217;
        assert!(validate_export_preset(&invalid).is_ok());
        invalid = original;
        invalid
            .options
            .as_mut()
            .unwrap()
            .custom_transparency_enabled = true;
        invalid.options.as_mut().unwrap().custom_transparent_index = 2;
        assert!(validate_export_preset(&invalid).is_err());
    }

    #[test]
    fn project_preset_count_serialized_budget_and_names_are_bounded() {
        let mut presets = (0..MAX_EXPORT_PRESETS)
            .map(|index| (format!("Preset {index}"), preset()))
            .collect::<BTreeMap<_, _>>();
        validate_export_presets(&presets).unwrap();
        presets.insert("One too many".to_owned(), preset());
        assert!(validate_export_presets(&presets).is_err());
        presets.remove("One too many");
        for preset in presets.values_mut() {
            preset.options.as_mut().unwrap().custom_palette_text =
                format!("#000000 #FFFFFF{}", " ".repeat(4000));
        }
        assert!(validate_export_presets(&presets).is_err());
        for name in ["", " leading", "trailing ", "new\nline", &"a".repeat(65)] {
            assert!(validate_export_preset_name(name).is_err());
        }
        assert!(validate_export_preset_name("演示动画").is_ok());
    }

    #[test]
    fn preset_commands_are_atomic_and_exactly_reversible() {
        let mut project = manifest();
        let before = project.clone();
        let edit = project
            .apply_command(&EditCommand::UpsertExportPreset {
                name: "Demo".to_owned(),
                preset: preset(),
            })
            .unwrap();
        let saved = project.clone();
        let redo = project.apply_command(&edit.inverse).unwrap();
        project.revision = before.revision;
        assert_eq!(project, before);
        project.apply_command(&redo.inverse).unwrap();
        project.revision = saved.revision;
        assert_eq!(project, saved);
        let mut invalid = preset();
        invalid.colors = 1;
        assert!(
            project
                .apply_command(&EditCommand::UpsertExportPreset {
                    name: "Broken".to_owned(),
                    preset: invalid
                })
                .is_err()
        );
        assert_eq!(project, saved);
    }
}
