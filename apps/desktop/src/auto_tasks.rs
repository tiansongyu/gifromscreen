//! Ordered, cancellable editing tasks with one durable undo boundary.

use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use gif_from_screen_domain::{
    AnnotationMode, AssetDescriptor, AssetId, DurationUs, EditCommand, EditTaskRun,
    EditTaskTrigger, EditingTaskAction, EditingTaskPreset, EditingTaskSettings, Effect,
    MAX_EDIT_TASK_RUNS, ProjectManifest, RasterEncoding, TaskDelay,
};
use gif_from_screen_editor::{
    FrameEffectEdit, adjust_duration, edit_frame_effects, override_duration, scale_duration,
};
use gif_from_screen_project::ActiveProject;
use gif_from_screen_render::RgbaSurface;

use crate::{
    annotation_engine::{load_annotation_asset, prepare_annotations_with_assets},
    background_task::BackgroundTask,
    editor_workspace::{EditorWorkspace, OverlaySelectionAnchor},
};

#[path = "auto_task_store.rs"]
mod store;
#[path = "auto_task_ui.rs"]
mod ui;

use store::{AutoTaskStore, Snapshot};

type WorkspaceLoan = Arc<Mutex<Option<EditorWorkspace>>>;

const MAX_COMMAND_BYTES: usize = 64 * 1024 * 1024;

struct ByteBudget(usize);

impl std::io::Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.checked_sub(bytes.len()).ok_or_else(|| {
            std::io::Error::other("Editing tasks exceed the 64 MiB metadata budget; split the chain into smaller presets.")
        })?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PendingRun {
    anchor: OverlaySelectionAnchor,
    trigger: EditTaskTrigger,
}

#[derive(Clone, Debug, Default)]
struct TaskProgress {
    task: usize,
    total: usize,
    name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TaskSummary {
    pub completed: Vec<String>,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) struct AutoTasks {
    store: AutoTaskStore,
    snapshot: Option<Snapshot>,
    draft: EditingTaskSettings,
    settings_job: BackgroundTask<Snapshot, ()>,
    settings_error: Option<String>,
    selected: usize,
    new_name: String,
    new_kind: usize,
    pending: Option<PendingRun>,
    loan: Option<WorkspaceLoan>,
    job: BackgroundTask<TaskSummary, TaskProgress>,
    completed: Option<Result<TaskSummary, String>>,
    notice: Option<String>,
    #[cfg(test)]
    test_directory: Option<tempfile::TempDir>,
}

impl Default for AutoTasks {
    fn default() -> Self {
        #[cfg(test)]
        {
            let directory = tempfile::tempdir().expect("isolated editing-task settings");
            let mut state = Self::new(directory.path().join("editing-tasks.json"));
            state.test_directory = Some(directory);
            state
        }
        #[cfg(not(test))]
        {
            match default_settings_path() {
                Ok(path) => Self::new(path),
                Err(error) => {
                    // No fallback into the working directory and no filesystem access.
                    let mut state = Self::unloaded(PathBuf::new());
                    state.settings_error = Some(error);
                    state
                }
            }
        }
    }
}

#[cfg(not(test))]
fn default_settings_path() -> Result<PathBuf, String> {
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err(
                "XDG_STATE_HOME must be an absolute directory; editing presets are disabled."
                    .to_owned(),
            );
        }
        return Ok(root.join("gifromscreen/editing-tasks.json"));
    }
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("Set an absolute HOME or XDG_STATE_HOME to enable saved editing presets.")?;
    Ok(root.join(".local/state/gifromscreen/editing-tasks.json"))
}

impl AutoTasks {
    pub(crate) fn new(path: PathBuf) -> Self {
        let mut state = Self::unloaded(path);
        state.reload();
        state
    }

    fn unloaded(path: PathBuf) -> Self {
        Self {
            store: AutoTaskStore::new(path),
            snapshot: None,
            draft: EditingTaskSettings::default(),
            settings_job: BackgroundTask::default(),
            settings_error: None,
            selected: 0,
            new_name: "My editing preset".to_owned(),
            new_kind: 0,
            pending: None,
            loan: None,
            job: BackgroundTask::default(),
            completed: None,
            notice: None,
            #[cfg(test)]
            test_directory: None,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.pending.is_some() || self.loan.is_some() || self.job.is_running()
    }
    pub(crate) fn is_loading(&self) -> bool {
        self.settings_job.is_running()
    }

    pub(crate) fn cancel(&mut self) {
        if self.pending.take().is_some() {
            self.notice = Some(
                "Editing tasks cancelled before preparation. The original project is unchanged."
                    .to_owned(),
            );
        }
        self.job.cancel();
    }

    /// Only new captures/imports call this. Ordinary project opening never reruns tasks.
    pub(crate) fn queue_created(
        &mut self,
        workspace: &EditorWorkspace,
        trigger: EditTaskTrigger,
    ) -> Result<(), String> {
        if self.is_running() {
            return Err("Another editing-task chain is already pending.".to_owned());
        }
        self.pending = Some(PendingRun {
            anchor: workspace.project_edit_anchor(),
            trigger,
        });
        self.notice = None;
        Ok(())
    }

    fn reload(&mut self) {
        if self.is_loading() || self.is_running() {
            return;
        }
        let store = self.store.clone();
        if let Err(error) = self
            .settings_job
            .start("gfs-load-editing-tasks", move |_| store.load())
        {
            self.settings_error = Some(error);
        }
    }

    fn save(&mut self) {
        let Some(previous) = self.snapshot.clone() else {
            return;
        };
        if let Err(error) = self.draft.validate() {
            self.notice = Some(error);
            return;
        }
        let store = self.store.clone();
        let config = self.draft.clone();
        if let Err(error) = self.settings_job.start("gfs-save-editing-tasks", move |_| {
            store.save(&previous, config)
        }) {
            self.notice = Some(error);
        }
    }

    /// Poll on the UI thread, handing the workspace exclusively to the worker when ready.
    pub(crate) fn poll(&mut self, workspace: &mut Option<EditorWorkspace>) -> Option<String> {
        if let Some(result) = self.settings_job.poll() {
            match result {
                Ok(snapshot) => {
                    self.draft = snapshot.config.clone();
                    self.snapshot = Some(snapshot);
                    self.settings_error = None;
                    self.notice =
                        Some("Editing presets loaded and saved settings are ready.".to_owned());
                }
                Err(error) => {
                    self.settings_error = Some(error.clone());
                    self.notice = Some(error);
                }
            }
        }
        if let Some(result) = self.job.poll() {
            self.completed = Some(result);
        }
        if self.completed.is_some() {
            if workspace.is_some() {
                return Some("Editing tasks completed, but another project occupies the editor. The original workspace remains held safely.".to_owned());
            }
            let loan = self.loan.take()?;
            *workspace = loan.lock().unwrap_or_else(PoisonError::into_inner).take();
            let message = match self.completed.take()? {
                Ok(summary) if summary.completed.is_empty() && summary.skipped.is_empty() => "No automatic editing tasks apply to this source; the original project is ready.".to_owned(),
                Ok(summary) => format!("Editing tasks complete: {} applied, {} skipped. One undo restores the original project.{} {}", summary.completed.len(), summary.skipped.len(), if summary.skipped.is_empty() { String::new() } else { format!(" Skipped: {}.", summary.skipped.join(", ")) }, summary.warnings.join(" ")),
                Err(error) => format!("Editing tasks stopped: {error} The workspace and undo history were returned. If the journal could not be saved, reopen the project to confirm its recovered state. Verified unreferenced pixel assets may remain."),
            };
            self.notice = Some(message.clone());
            return Some(message);
        }
        if self.settings_job.is_running() {
            return None;
        }
        let pending = self.pending.take()?;
        let Some(snapshot) = &self.snapshot else {
            let error = format!(
                "Automatic editing tasks could not load: {}. The new project is preserved without edits.",
                self.settings_error
                    .as_deref()
                    .unwrap_or("settings unavailable")
            );
            self.notice = Some(error.clone());
            return Some(error);
        };
        if self.settings_error.is_some() {
            let error = "Editing presets have a load/save error; automatic edits were not applied. Reload the settings before retrying. The original project is preserved.".to_owned();
            self.notice = Some(error.clone());
            return Some(error);
        }
        let Some(current) = workspace.as_ref() else {
            return Some("The editing-task project is unavailable.".to_owned());
        };
        if !pending.anchor.matches(current) {
            return Some(
                "The project changed before editing tasks could start; no edits were applied."
                    .to_owned(),
            );
        }
        let config = &snapshot.config;
        if pending.trigger != EditTaskTrigger::Manual && !config.enabled {
            return None;
        }
        let preset = config
            .active_preset
            .as_ref()
            .and_then(|name| config.presets.iter().find(|p| &p.name == name))
            .cloned();
        let Some(preset) = preset else {
            return Some("Choose and save an active editing preset first.".to_owned());
        };
        if !preset.sources.includes(pending.trigger) {
            return None;
        }
        let loan = Arc::new(Mutex::new(workspace.take()));
        let worker_loan = Arc::clone(&loan);
        let started = self.job.start("gfs-editing-tasks", move |context| {
            let mut slot = worker_loan.lock().unwrap_or_else(PoisonError::into_inner);
            let workspace = slot
                .as_mut()
                .ok_or("The editing-task workspace is unavailable.")?;
            apply_task_chain(
                workspace,
                &preset,
                pending.trigger,
                context.cancellation(),
                |progress| context.report(progress),
            )
        });
        if let Err(error) = started {
            *workspace = loan.lock().unwrap_or_else(PoisonError::into_inner).take();
            return Some(error);
        }
        self.loan = Some(loan);
        None
    }
}

fn apply_task_chain(
    workspace: &mut EditorWorkspace,
    preset: &EditingTaskPreset,
    trigger: EditTaskTrigger,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(TaskProgress),
) -> Result<TaskSummary, String> {
    preset.validate()?;
    if !preset.sources.includes(trigger) {
        return Ok(TaskSummary::default());
    }
    serde_json::to_writer(&mut ByteBudget(MAX_COMMAND_BYTES), workspace.manifest())
        .map_err(|error| error.to_string())?;
    let mut staged = workspace.manifest().clone();
    let source_revision = staged.revision;
    let mut commands = Vec::new();
    let mut assets = Vec::new();
    let mut asset_bytes = 0_usize;
    let mut command_budget = ByteBudget(MAX_COMMAND_BYTES);
    let mut summary = TaskSummary::default();
    for (index, task) in preset
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.enabled)
    {
        if cancelled.load(Ordering::Acquire) {
            return Err("Cancelled before committing the task chain.".to_owned());
        }
        progress(TaskProgress {
            task: index + 1,
            total: preset.tasks.len(),
            name: task.name.clone(),
        });
        let prepared = prepare_task(&staged, &task.action, trigger, cancelled, &|id| {
            load_task_asset(&staged, workspace.active_project(), &assets, id)
        })
        .map_err(|error| format!("Task {} ({}): {error}", index + 1, task.name))?;
        if !prepared.replay_skips.is_empty() {
            summary.warnings.push(format!(
                "Task {} ({}): {}",
                index + 1,
                task.name,
                prepared.replay_skips.message()
            ));
        }
        for (_, bytes) in &prepared.assets {
            asset_bytes = asset_bytes.checked_add(bytes.len())
                .filter(|bytes| *bytes <= 256 * 1024 * 1024)
                .ok_or_else(|| format!("Task {} ({}): The editing chain exceeds its 256 MiB generated-asset budget.", index + 1, task.name))?;
        }
        assets.extend(prepared.assets);
        let Some(command) = prepared.command else {
            summary.skipped.push(task.name.clone());
            continue;
        };
        serde_json::to_writer(&mut command_budget, &command)
            .map_err(|error| format!("Task {} ({}): {error}", index + 1, task.name))?;
        staged
            .apply_command(&command)
            .map_err(|error| format!("Task {} ({}): {error}", index + 1, task.name))?;
        // Simulation is one eventual revision, regardless of the number of tasks.
        staged.revision = source_revision;
        commands.push(command);
        summary.completed.push(task.name.clone());
    }
    if summary.completed.is_empty() && summary.skipped.is_empty() {
        return Ok(summary);
    }
    let mut runs = workspace.manifest().task_runs.clone();
    runs.push(EditTaskRun {
        preset_name: preset.name.clone(),
        source_revision,
        trigger,
        completed_tasks: summary.completed.clone(),
        skipped_tasks: summary.skipped.clone(),
    });
    if runs.len() > MAX_EDIT_TASK_RUNS {
        runs.remove(0);
    }
    commands.push(EditCommand::SetTaskRuns { runs });
    if cancelled.load(Ordering::Acquire) {
        return Err("Cancelled before saving assets.".to_owned());
    }
    for (descriptor, bytes) in assets {
        if cancelled.load(Ordering::Acquire) {
            return Err("Cancelled while saving assets.".to_owned());
        }
        let stored = workspace
            .active_project()
            .assets()
            .put(&bytes)
            .map_err(|e| e.to_string())?;
        if stored != descriptor.id {
            return Err("Generated annotation asset digest mismatch.".to_owned());
        }
    }
    if cancelled.load(Ordering::Acquire) {
        return Err("Cancelled before committing the task chain.".to_owned());
    }
    workspace
        .execute(EditCommand::Compound { commands })
        .map_err(|e| e.to_string())?;
    Ok(summary)
}

#[derive(Default)]
struct PreparedTask {
    command: Option<EditCommand>,
    assets: Vec<(AssetDescriptor, Vec<u8>)>,
    replay_skips: crate::annotation_engine::AnnotationReplaySkips,
}

fn prepare_task(
    project: &ProjectManifest,
    action: &EditingTaskAction,
    trigger: EditTaskTrigger,
    cancelled: &AtomicBool,
    provider: &dyn Fn(AssetId) -> Result<RgbaSurface, String>,
) -> Result<PreparedTask, String> {
    let selected: BTreeSet<_> = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    let command = match action {
        EditingTaskAction::Delay { mode } => match mode {
            TaskDelay::Override { milliseconds } => override_duration(
                project,
                selected,
                DurationUs::new(milliseconds * 1000).ok_or("Duration must be positive.")?,
            ),
            TaskDelay::Adjust { milliseconds } => {
                adjust_duration(project, selected, milliseconds * 1000)
            }
            TaskDelay::Scale { percent } => scale_duration(project, selected, *percent),
        }
        .map_err(|error| error.to_string())?,
        EditingTaskAction::Border { widths, color } => edit_frame_effects(
            project,
            selected,
            &FrameEffectEdit::Add(Effect::Border {
                widths: *widths,
                color: *color,
            }),
        )
        .map_err(|error| error.to_string())?,
        EditingTaskAction::Shadow {
            offset_x,
            offset_y,
            blur_radius,
            color,
        } => edit_frame_effects(
            project,
            selected,
            &FrameEffectEdit::Add(Effect::Shadow {
                offset_x: *offset_x,
                offset_y: *offset_y,
                blur_radius: *blur_radius,
                color: *color,
            }),
        )
        .map_err(|error| error.to_string())?,
        EditingTaskAction::Annotation { request } => {
            if trigger != EditTaskTrigger::Manual
                && trigger != EditTaskTrigger::ScreenRecording
                && matches!(
                    request.mode,
                    AnnotationMode::RecordedKeys
                        | AnnotationMode::RecordedClicks
                        | AnnotationMode::RecordedCursor
                )
            {
                return Ok(PreparedTask::default());
            }
            let prepared = prepare_annotations_with_assets(
                project,
                &selected,
                request,
                cancelled,
                |_| {},
                provider,
            )?;
            return Ok(PreparedTask {
                command: (!prepared.commands.is_empty()).then_some(EditCommand::Compound {
                    commands: prepared.commands,
                }),
                assets: prepared.assets,
                replay_skips: prepared.replay_skips,
            });
        }
    };
    Ok(PreparedTask {
        command: Some(command),
        assets: Vec::new(),
        replay_skips: crate::annotation_engine::AnnotationReplaySkips::default(),
    })
}

fn load_task_asset(
    manifest: &ProjectManifest,
    project: &ActiveProject,
    prepared: &[(AssetDescriptor, Vec<u8>)],
    id: AssetId,
) -> Result<RgbaSurface, String> {
    let Some((descriptor, bytes)) = prepared.iter().find(|(asset, _)| asset.id == id) else {
        return load_annotation_asset(manifest, project.assets(), id);
    };
    let Some((size, RasterEncoding::Rgba8)) = descriptor.kind.raster_descriptor() else {
        return Err("Prepared cursor pixels must be RGBA8.".to_owned());
    };
    if bytes.len() > 64 * 1024 * 1024
        || bytes.len() as u64 != descriptor.byte_len
        || manifest.assets.get(&id) != Some(descriptor)
    {
        return Err(
            "Prepared cursor pixels exceed the 64 MiB limit or do not match their descriptor."
                .to_owned(),
        );
    }
    RgbaSurface::new(size, bytes.clone()).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "auto_task_tests.rs"]
mod tests;
