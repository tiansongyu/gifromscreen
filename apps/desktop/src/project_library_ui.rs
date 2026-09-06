//! Recent-project navigation and explicit, snapshot-based Save As.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use gif_from_screen_application::{
    ProjectCopyProgress, ProjectCopyReport, ProjectCopySnapshot, SaveProjectCopyOptions,
    save_project_copy,
};
use gif_from_screen_domain::{ProjectId, UnixTimeMs};
use uuid::Uuid;

use crate::{background_task::BackgroundTask, editor_workspace::EditorWorkspace};

#[path = "recent_projects.rs"]
mod history;
#[cfg(not(test))]
use history::default_history_path;
use history::{RecentProjectEdit, RecentProjectStore};

#[derive(Clone, Debug)]
struct RecentEntry {
    path: PathBuf,
    available: bool,
}

pub(crate) struct ProjectLibraryTool {
    store: Option<RecentProjectStore>,
    recent: Vec<RecentEntry>,
    pending: VecDeque<RecentProjectEdit>,
    history_task: BackgroundTask<Vec<RecentEntry>, ()>,
    loaded: bool,
    closing: bool,
    target: String,
    copy_task: BackgroundTask<ProjectCopyReport, ProjectCopyProgress>,
    completed_copy: Option<ProjectCopyReport>,
    notice: Option<String>,
    #[cfg(test)]
    isolated_state: Option<tempfile::TempDir>,
}

impl Default for ProjectLibraryTool {
    fn default() -> Self {
        #[cfg(test)]
        {
            let directory = tempfile::tempdir().expect("test state directory");
            let mut tool = Self::with_history_path(directory.path().join("recent-projects.json"));
            tool.isolated_state = Some(directory);
            tool
        }
        #[cfg(not(test))]
        {
            match default_history_path() {
                Ok(path) => Self::with_history_path(path),
                Err(error) => Self {
                    store: None,
                    notice: Some(error),
                    ..Self::empty()
                },
            }
        }
    }
}

impl ProjectLibraryTool {
    fn empty() -> Self {
        Self {
            store: None,
            recent: Vec::new(),
            pending: VecDeque::new(),
            history_task: BackgroundTask::default(),
            loaded: false,
            closing: false,
            target: String::new(),
            copy_task: BackgroundTask::default(),
            completed_copy: None,
            notice: None,
            #[cfg(test)]
            isolated_state: None,
        }
    }

    /// Tests and embedders can inject their own isolated state file.
    pub(crate) fn with_history_path(path: PathBuf) -> Self {
        Self {
            store: Some(RecentProjectStore::new(path)),
            ..Self::empty()
        }
    }

    /// Called after successful project activation; never performs filesystem I/O here.
    pub(crate) fn remember(&mut self, path: &Path) {
        if self.closing {
            return;
        }
        if path.as_os_str().len() > 2048 {
            self.notice = Some("This project path is too long for recent history.".to_owned());
            return;
        }
        self.queue_history(RecentProjectEdit::Remember(path.to_owned()));
    }

    pub(crate) fn is_active(&self) -> bool {
        self.copy_task.is_running() || self.history_task.is_running() || !self.pending.is_empty()
    }

    /// Cancel a pending copy; queued bounded history updates are allowed to finish.
    pub(crate) fn shutdown(&mut self) {
        self.closing = true;
        self.copy_task.cancel();
    }

    /// Updates worker results without switching the active project.
    pub(crate) fn poll(&mut self) -> Option<String> {
        let mut notice = None;
        if let Some(result) = self.history_task.poll() {
            self.loaded = true;
            match result {
                Ok(recent) => self.recent = recent,
                Err(error) => notice = Some(format!("Recent projects: {error}")),
            }
        }
        if let Some(result) = self.copy_task.poll() {
            match result {
                Ok(report) => {
                    let message = format!(
                        "Saved snapshot revision {} to {}. Your current project was not switched.",
                        report.source_revision,
                        report.path.display()
                    );
                    self.remember(&report.path);
                    self.completed_copy = Some(report);
                    notice = Some(message);
                }
                Err(error) => notice = Some(format!("Save As: {error}")),
            }
        }
        if !self.history_task.is_running()
            && ((!self.loaded && !self.closing) || !self.pending.is_empty())
        {
            if let Some(store) = self.store.clone() {
                let edits: Vec<_> = self.pending.drain(..).collect();
                if let Err(error) = self
                    .history_task
                    .start("gfs-recent-projects", move |context| {
                        if context
                            .cancellation()
                            .load(std::sync::atomic::Ordering::Acquire)
                        {
                            return Err(
                                "Recent-project update stopped before accessing its state file."
                                    .to_owned(),
                            );
                        }
                        let paths = if edits.is_empty() {
                            store.load()?
                        } else {
                            store.update(&edits)?
                        };
                        Ok(paths
                            .into_iter()
                            .map(|path| RecentEntry {
                                available: path.join("manifest.json").is_file(),
                                path,
                            })
                            .collect())
                    })
                {
                    self.loaded = true;
                    notice = Some(error);
                }
            } else {
                self.pending.clear();
                self.loaded = true;
            }
        }
        if let Some(message) = &notice {
            self.notice = Some(message.clone());
        }
        notice
    }

    /// Returns a project only after an explicit Open click.
    pub(crate) fn show_recent(&mut self, ui: &mut egui::Ui) -> Option<PathBuf> {
        let mut open = None;
        let mut remove = None;
        ui.horizontal(|ui| {
            ui.heading("Recent projects");
            if ui
                .add_enabled(
                    !self.history_task.is_running(),
                    egui::Button::new("Refresh"),
                )
                .clicked()
            {
                self.loaded = false;
            }
        });
        if self.recent.is_empty() {
            ui.weak("Projects you open or create appear here. Removing an entry never deletes the project.");
        }
        for entry in &self.recent {
            ui.horizontal_wrapped(|ui| {
                let label = entry
                    .path
                    .file_name()
                    .unwrap_or(entry.path.as_os_str())
                    .to_string_lossy();
                if ui
                    .add_enabled(entry.available, egui::Button::new(label.as_ref()))
                    .on_hover_text(entry.path.display().to_string())
                    .clicked()
                {
                    open = Some(entry.path.clone());
                }
                ui.weak(entry.path.display().to_string());
                if !entry.available {
                    ui.colored_label(ui.visuals().warn_fg_color, "Missing or moved");
                }
                if ui.small_button("Remove from list").clicked() {
                    remove = Some(entry.path.clone());
                }
            });
        }
        if let Some(path) = remove {
            self.queue_history(RecentProjectEdit::Remove(path));
        }
        if let Some(notice) = &self.notice {
            ui.label(notice);
        }
        if self.is_active() {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        open
    }

    /// Starts a background copy from a frozen revision; opening a completed copy is explicit.
    pub(crate) fn show_save_as(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &EditorWorkspace,
    ) -> Option<PathBuf> {
        let mut open = None;
        egui::CollapsingHeader::new("Save project copy…").id_salt("save-project-copy").show(ui, |ui| {
            ui.label("Save this revision to a new .gfsproj. The current project stays open and editable.");
            ui.add_enabled_ui(!self.copy_task.is_running(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.target).desired_width(430.0).hint_text("/path/to/new-copy.gfsproj"));
                    if ui.button("Save As…").clicked() && let Err(error) = self.start_copy(workspace) { self.notice = Some(error); }
                });
            });
            if self.copy_task.is_running() {
                if let Some(progress) = self.copy_task.progress() { ui.label(format!("{} / {} assets · {} / {} MiB", progress.assets_copied, progress.total_assets, progress.bytes_copied / 1024 / 1024, progress.total_bytes / 1024 / 1024)); }
                if ui.add_enabled(!self.copy_task.is_cancelling(), egui::Button::new("Cancel copy")).clicked() { self.copy_task.cancel(); }
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
            if let Some(report) = &self.completed_copy {
                ui.weak(format!("Completed: {} (source revision {})", report.path.display(), report.source_revision));
                if ui.button("Open saved copy").clicked() { open = Some(report.path.clone()); }
            }
            if let Some(notice) = &self.notice { ui.label(notice); }
        });
        open
    }

    fn start_copy(&mut self, workspace: &EditorWorkspace) -> Result<(), String> {
        if self.target.trim().is_empty() {
            return Err("Choose a new .gfsproj path for the copy.".to_owned());
        }
        let snapshot = ProjectCopySnapshot::from_active(workspace.active_project());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?;
        let options = SaveProjectCopyOptions {
            target: PathBuf::from(self.target.trim()),
            project_id: ProjectId::from_u128(Uuid::new_v4().as_u128()),
            created_at: UnixTimeMs::new(
                i64::try_from(now.as_millis()).map_err(|error| error.to_string())?,
            ),
        };
        self.copy_task
            .start("gfs-save-project-copy", move |context| {
                save_project_copy(&snapshot, &options, context.cancellation(), |progress| {
                    context.report(progress);
                })
            })?;
        self.completed_copy = None;
        self.notice = None;
        Ok(())
    }

    fn queue_history(&mut self, edit: RecentProjectEdit) {
        if self.pending.len() == 40 {
            self.pending.pop_front();
        }
        self.pending.push_back(edit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::{
        Canvas, CanvasBackground, ColorSpace, PhysicalSize, ProjectManifest,
    };
    use gif_from_screen_project::ActiveProject;
    use std::{fs, thread, time::Instant};

    fn workspace(root: &Path) -> EditorWorkspace {
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "library-test",
            UnixTimeMs::new(0),
            Canvas {
                size: PhysicalSize::new(1, 1).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        EditorWorkspace::from_active(ActiveProject::create(root, manifest).unwrap(), 8).unwrap()
    }

    fn settle(library: &mut ProjectLibraryTool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            library.poll();
            if library.loaded && !library.is_active() {
                return;
            }
            assert!(Instant::now() < deadline, "library did not settle");
            thread::yield_now();
        }
    }

    #[test]
    fn remembered_projects_load_in_background_and_missing_paths_can_be_removed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source.gfsproj");
        let workspace = workspace(&root);
        let state = directory.path().join("state/recent.json");
        let mut library = ProjectLibraryTool::with_history_path(state.clone());
        library.remember(&root);
        assert!(!state.exists());
        settle(&mut library);
        assert_eq!(library.recent.len(), 1);
        assert!(library.recent[0].available);
        drop(workspace);
        fs::rename(&root, directory.path().join("moved.gfsproj")).unwrap();
        library.loaded = false;
        settle(&mut library);
        assert!(!library.recent[0].available);
        library.queue_history(RecentProjectEdit::Remove(root));
        settle(&mut library);
        assert!(library.recent.is_empty());
        assert!(
            directory
                .path()
                .join("moved.gfsproj/manifest.json")
                .exists()
        );
    }

    #[test]
    fn save_copy_completion_does_not_switch_or_revert_the_current_project() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source.gfsproj");
        let mut workspace = workspace(&root);
        let mut library =
            ProjectLibraryTool::with_history_path(directory.path().join("state/recent.json"));
        library.target = directory
            .path()
            .join("copy.gfsproj")
            .to_string_lossy()
            .into_owned();
        let captured_revision = workspace.manifest().revision;
        library.start_copy(&workspace).unwrap();
        let mut canvas = workspace.manifest().canvas.clone();
        canvas.size = PhysicalSize::new(2, 2).unwrap();
        workspace
            .execute(gif_from_screen_domain::EditCommand::SetCanvas { canvas })
            .unwrap();
        let edited = workspace.manifest().clone();
        settle(&mut library);
        let completed = library.completed_copy.as_ref().unwrap();
        assert_eq!(completed.source_revision, captured_revision);
        assert_ne!(completed.project_id, workspace.manifest().project_id);
        assert_eq!(workspace.manifest(), &edited);
        assert_eq!(workspace.project_root(), root);
        assert!(completed.path.join("manifest.json").exists());
    }

    #[test]
    fn default_in_unit_tests_uses_an_isolated_state_directory_and_queue_is_bounded() {
        let mut library = ProjectLibraryTool::default();
        assert!(library.isolated_state.is_some());
        for index in 0..100 {
            library.queue_history(RecentProjectEdit::Remove(PathBuf::from(format!(
                "/test/{index}"
            ))));
        }
        assert_eq!(library.pending.len(), 40);
        library.shutdown();
        assert!(library.closing);
    }
}
