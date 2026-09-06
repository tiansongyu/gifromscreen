//! Compact, reversible provenance for a successfully applied editing-task chain.

use serde::{Deserialize, Serialize};

use crate::ProjectRevision;

pub const MAX_EDIT_TASK_RUNS: usize = 32;
pub const MAX_EDIT_TASKS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditTaskTrigger {
    ScreenRecording,
    CameraRecording,
    BoardRecording,
    Import,
    Manual,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditTaskRun {
    pub preset_name: String,
    pub source_revision: ProjectRevision,
    pub trigger: EditTaskTrigger,
    /// Enabled, applicable tasks, in the order in which they were executed.
    pub completed_tasks: Vec<String>,
    /// Inapplicable tasks are reported rather than silently treated as completed.
    pub skipped_tasks: Vec<String>,
}

pub fn validate_edit_task_runs(runs: &[EditTaskRun]) -> Result<(), &'static str> {
    if runs.len() > MAX_EDIT_TASK_RUNS {
        return Err("at most 32 editing-task run records are retained");
    }
    for run in runs {
        validate_task_name(&run.preset_name)?;
        let count = run.completed_tasks.len() + run.skipped_tasks.len();
        if !(1..=MAX_EDIT_TASKS).contains(&count) {
            return Err("an editing-task run must contain between 1 and 32 task results");
        }
        for name in run.completed_tasks.iter().chain(&run.skipped_tasks) {
            validate_task_name(name)?;
        }
    }
    Ok(())
}

pub fn validate_task_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || name.len() > 128
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        Err("names must contain 1..128 bytes without surrounding whitespace or controls")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditCommand, ProjectManifest, model::test_fixtures::manifest};

    fn run() -> EditTaskRun {
        EditTaskRun {
            preset_name: "Demo".into(),
            source_revision: ProjectRevision::ZERO,
            trigger: EditTaskTrigger::ScreenRecording,
            completed_tasks: vec!["Delay".into()],
            skipped_tasks: vec![],
        }
    }

    #[test]
    fn task_completion_records_have_exact_inverse_and_validate_atomically() {
        let mut project = manifest();
        let before = project.clone();
        let applied = project
            .apply_command(&EditCommand::SetTaskRuns { runs: vec![run()] })
            .unwrap();
        assert_eq!(project.task_runs.len(), 1);
        project.apply_command(&applied.inverse).unwrap();
        project.revision = before.revision;
        assert_eq!(project, before);
        assert!(
            project
                .apply_command(&EditCommand::SetTaskRuns {
                    runs: vec![run(); MAX_EDIT_TASK_RUNS + 1]
                })
                .is_err()
        );
        assert_eq!(project, before);
    }

    #[test]
    fn old_manifests_deserialize_without_inventing_task_runs() {
        let original = manifest();
        let json = serde_json::to_string(&original).unwrap();
        assert!(!json.contains("task_runs"));
        let restored: ProjectManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn boxed_frame_replacement_preserves_legacy_command_json_shape() {
        let frame = crate::model::test_fixtures::frame(1, crate::model::test_fixtures::asset(1).id);
        let command = EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame.clone()),
        };
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["replacement"], serde_json::to_value(frame).unwrap());
        assert_eq!(
            serde_json::from_value::<EditCommand>(value).unwrap(),
            command
        );
    }
}
