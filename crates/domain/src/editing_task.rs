//! Versioned data-only editing presets. They cannot execute external programs.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    AnnotationRequest, EdgeWidths, EditTaskTrigger, ImageBorderStyle, ImageShadowStyle,
    MAX_EDIT_TASKS, Rgba, validate_task_name,
};

pub const EDITING_TASKS_VERSION: u16 = 2;
pub const MAX_EDITING_PRESETS: usize = 32;
pub const MAX_EDITING_SETTINGS_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditingTaskSettings {
    pub version: u16,
    pub enabled: bool,
    pub active_preset: Option<String>,
    pub presets: Vec<EditingTaskPreset>,
}

impl Default for EditingTaskSettings {
    fn default() -> Self {
        Self {
            version: EDITING_TASKS_VERSION,
            enabled: false,
            active_preset: None,
            presets: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditingTaskPreset {
    pub name: String,
    pub sources: EditingTaskSources,
    pub tasks: Vec<EditingTask>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditingTaskSources {
    pub screen: bool,
    pub camera: bool,
    pub board: bool,
    pub import: bool,
}

impl Default for EditingTaskSources {
    fn default() -> Self {
        Self {
            screen: true,
            camera: true,
            board: true,
            import: true,
        }
    }
}

impl EditingTaskSources {
    pub const fn includes(&self, trigger: EditTaskTrigger) -> bool {
        match trigger {
            EditTaskTrigger::ScreenRecording => self.screen,
            EditTaskTrigger::CameraRecording => self.camera,
            EditTaskTrigger::BoardRecording => self.board,
            EditTaskTrigger::Import => self.import,
            EditTaskTrigger::Manual => true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditingTask {
    pub name: String,
    pub enabled: bool,
    pub action: EditingTaskAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditingTaskAction {
    Delay {
        mode: TaskDelay,
    },
    Border {
        widths: EdgeWidths,
        color: Rgba,
    },
    Shadow {
        offset_x: i32,
        offset_y: i32,
        blur_radius: u16,
        color: Rgba,
    },
    ImageBorder {
        style: ImageBorderStyle,
    },
    ImageShadow {
        style: ImageShadowStyle,
    },
    Annotation {
        request: AnnotationRequest,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskDelay {
    Override { milliseconds: u64 },
    Adjust { milliseconds: i64 },
    Scale { percent: u32 },
}

impl EditingTaskSettings {
    /// Minimum settings format required by every stored task, including
    /// disabled tasks and presets that are not currently active.
    pub fn required_version(&self) -> u16 {
        self.presets
            .iter()
            .flat_map(|preset| &preset.tasks)
            .map(|task| match task.action {
                EditingTaskAction::ImageBorder { .. } | EditingTaskAction::ImageShadow { .. } => 2,
                _ => 1,
            })
            .max()
            .unwrap_or(1)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=EDITING_TASKS_VERSION).contains(&self.version) {
            return Err(
                "Unsupported editing-task settings version; the file was left unchanged."
                    .to_owned(),
            );
        }
        if self.presets.len() > MAX_EDITING_PRESETS {
            return Err("At most 32 editing presets are supported.".to_owned());
        }
        let mut names = BTreeSet::new();
        for preset in &self.presets {
            validate_task_name(&preset.name)?;
            if !names.insert(preset.name.as_str()) {
                return Err("Editing preset names must be unique.".to_owned());
            }
            preset.validate()?;
        }
        if self.version < self.required_version() {
            return Err("Expanded-image tasks require editing-task settings version 2.".to_owned());
        }
        if self
            .active_preset
            .as_deref()
            .is_some_and(|name| !names.contains(name))
        {
            return Err("The active editing preset does not exist.".to_owned());
        }
        if self.enabled && self.active_preset.is_none() {
            return Err(
                "Choose an active editing preset before enabling automatic tasks.".to_owned(),
            );
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAX_EDITING_SETTINGS_BYTES {
            return Err("Editing presets exceed the 256 KiB settings budget.".to_owned());
        }
        Ok(())
    }
}

impl EditingTaskPreset {
    pub fn validate(&self) -> Result<(), String> {
        validate_task_name(&self.name)?;
        if self.tasks.len() > MAX_EDIT_TASKS {
            return Err("An editing preset may contain at most 32 tasks.".to_owned());
        }
        for (index, task) in self.tasks.iter().enumerate() {
            validate_task_name(&task.name).map_err(|e| format!("Task {}: {e}", index + 1))?;
            task.action
                .validate()
                .map_err(|e| format!("Task {} ({}): {e}", index + 1, task.name))?;
        }
        Ok(())
    }
}

impl EditingTaskAction {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Delay { mode } => match mode {
                TaskDelay::Override { milliseconds } if !(1..=3_600_000).contains(milliseconds) => {
                    return Err("Frame duration must be between 1 ms and 1 hour.".to_owned());
                }
                TaskDelay::Adjust { milliseconds } if milliseconds.unsigned_abs() > 3_600_000 => {
                    return Err(
                        "Duration adjustment must be between -1 hour and +1 hour.".to_owned()
                    );
                }
                TaskDelay::Scale { percent } if !(1..=100_000).contains(percent) => {
                    return Err("Duration scale must be between 1 and 100000 percent.".to_owned());
                }
                _ => {}
            },
            Self::Border { widths, color } => {
                if *widths == EdgeWidths::default() || color.alpha == 0 {
                    return Err(
                        "Border needs a visible color and at least one nonzero edge.".to_owned(),
                    );
                }
            }
            Self::Shadow {
                blur_radius,
                color,
                offset_x,
                offset_y,
            } => {
                if *blur_radius > 256
                    || color.alpha == 0
                    || offset_x.unsigned_abs() > 16_384
                    || offset_y.unsigned_abs() > 16_384
                {
                    return Err("Shadow radius is limited to 256 px, offsets to 16384 px, and color must be visible.".to_owned());
                }
            }
            Self::Annotation { request } => request.validate_settings()?,
            Self::ImageBorder { style } => style.validate()?,
            Self::ImageShadow { style } => style.validate()?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SignedEdgeWidths;

    fn settings() -> EditingTaskSettings {
        EditingTaskSettings {
            enabled: true,
            active_preset: Some("Demo".to_owned()),
            presets: vec![EditingTaskPreset {
                name: "Demo".to_owned(),
                sources: EditingTaskSources::default(),
                tasks: vec![EditingTask {
                    name: "Delay".to_owned(),
                    enabled: true,
                    action: EditingTaskAction::Delay {
                        mode: TaskDelay::Override { milliseconds: 100 },
                    },
                }],
            }],
            ..EditingTaskSettings::default()
        }
    }

    #[test]
    fn versioned_data_round_trips_and_rejects_unknown_executable_tasks() {
        let original = settings();
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(
            serde_json::from_str::<EditingTaskSettings>(&json).unwrap(),
            original
        );
        assert!(
            serde_json::from_str::<EditingTaskSettings>(
                &json.replace("\"type\":\"delay\"", "\"type\":\"run_script\"")
            )
            .is_err()
        );
        let mut invalid = original.clone();
        invalid.version += 1;
        assert!(invalid.validate().is_err());
        invalid = original;
        invalid.active_preset = Some("missing".into());
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn task_count_names_numeric_parameters_and_presets_are_bounded() {
        let mut config = settings();
        config.presets[0].tasks = vec![config.presets[0].tasks[0].clone(); MAX_EDIT_TASKS];
        config.validate().unwrap();
        let extra = config.presets[0].tasks[0].clone();
        config.presets[0].tasks.push(extra);
        assert!(config.validate().is_err());
        config = settings();
        config.presets.push(config.presets[0].clone());
        assert!(config.validate().is_err());
        for name in ["", " trailing ", "bad\nname"] {
            config = settings();
            config.presets[0].tasks[0].name = name.to_owned();
            assert!(config.validate().is_err());
        }
        for mode in [
            TaskDelay::Override { milliseconds: 0 },
            TaskDelay::Override {
                milliseconds: u64::MAX,
            },
            TaskDelay::Adjust {
                milliseconds: i64::MIN,
            },
            TaskDelay::Scale { percent: 0 },
        ] {
            assert!(EditingTaskAction::Delay { mode }.validate().is_err());
        }
    }

    #[test]
    fn automatic_source_filters_do_not_block_explicit_manual_runs() {
        let sources = EditingTaskSources {
            screen: false,
            camera: false,
            board: false,
            import: false,
        };
        for trigger in [
            EditTaskTrigger::ScreenRecording,
            EditTaskTrigger::CameraRecording,
            EditTaskTrigger::BoardRecording,
            EditTaskTrigger::Import,
        ] {
            assert!(!sources.includes(trigger));
        }
        assert!(sources.includes(EditTaskTrigger::Manual));
    }

    #[test]
    fn legacy_settings_keep_their_version_and_actions_without_implicit_conversion() {
        let mut original = settings();
        original.version = 1;
        original.presets[0].tasks[0].action = EditingTaskAction::Border {
            widths: EdgeWidths {
                top: 1,
                right: 0,
                bottom: 0,
                left: 0,
            },
            color: ImageBorderStyle::default().color,
        };
        original.validate().unwrap();
        assert_eq!(original.required_version(), 1);
        let bytes = serde_json::to_vec(&original).unwrap();
        let restored: EditingTaskSettings = serde_json::from_slice(&bytes).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, original);
        assert_eq!(serde_json::to_vec(&restored).unwrap(), bytes);
        assert_eq!(EditingTaskSettings::default().version, 2);
        assert_eq!(EditingTaskSettings::default().required_version(), 1);
    }

    #[test]
    fn image_actions_require_v2_even_when_disabled_and_outside_the_active_preset() {
        for action in [
            EditingTaskAction::ImageBorder {
                style: ImageBorderStyle::default(),
            },
            EditingTaskAction::ImageShadow {
                style: ImageShadowStyle::default(),
            },
        ] {
            let mut original = settings();
            original.presets.push(EditingTaskPreset {
                name: "Inactive".to_owned(),
                sources: EditingTaskSources::default(),
                tasks: vec![EditingTask {
                    name: "Disabled".to_owned(),
                    enabled: false,
                    action,
                }],
            });
            assert_eq!(original.required_version(), 2);
            original.version = 1;
            assert!(original.validate().unwrap_err().contains("version 2"));
            original.version = original.version.max(original.required_version());
            original.validate().unwrap();
            let bytes = serde_json::to_vec(&original).unwrap();
            let restored: EditingTaskSettings = serde_json::from_slice(&bytes).unwrap();
            restored.validate().unwrap();
            assert_eq!(restored, original);
            assert!(bytes.len() < MAX_EDITING_SETTINGS_BYTES);
        }
    }

    #[test]
    fn image_task_validation_preserves_supported_transparency_and_rejects_invalid_shadow() {
        EditingTaskAction::ImageBorder {
            style: ImageBorderStyle {
                widths: SignedEdgeWidths::default(),
                color: Rgba::TRANSPARENT,
                background: Rgba::TRANSPARENT,
            },
        }
        .validate()
        .unwrap();
        let mut original = settings();
        original.presets[0].tasks[0].action = EditingTaskAction::ImageShadow {
            style: ImageShadowStyle {
                direction_hundredths: 36_001,
                ..ImageShadowStyle::default()
            },
        };
        assert!(original.validate().unwrap_err().contains("direction"));
        original.version = 0;
        assert!(original.validate().unwrap_err().contains("Unsupported"));
    }
}
