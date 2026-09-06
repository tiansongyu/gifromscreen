//! Import a previously recorded or decoded project into the current timeline.

use eframe::egui;
use gif_from_screen_gif::CancellationToken;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use crate::{
    background_task::BackgroundTask,
    editor_workspace::{
        EditorWorkspace, PreparedProjectInsertion, ProjectInsertionTarget,
        prepare_project_insertion_from_path,
    },
    path_picker::{PathKind, PathPicker},
};

struct Cancellation<'a>(&'a AtomicBool);
impl CancellationToken for Cancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Default)]
pub(crate) struct ProjectInsertTool {
    path: String,
    at_start: bool,
    picker: PathPicker,
    task: BackgroundTask<PreparedProjectInsertion, ()>,
    notice: Option<String>,
}

impl ProjectInsertTool {
    pub(crate) fn is_running(&self) -> bool {
        self.task.is_running()
    }
    pub(crate) fn cancel(&self) {
        self.task.cancel();
    }

    pub(crate) fn poll(&mut self, workspace: Option<&mut EditorWorkspace>) -> Option<String> {
        let result = self.task.poll()?;
        Some(match result {
            Ok(_) if self.task.is_cancelling() => "Insertion cancelled. The timeline is unchanged; verified unreferenced assets may remain in the project store.".to_owned(),
            Ok(prepared) => {
                let source = prepared.source_label.clone();
                let frames = prepared.frame_count();
                match workspace.ok_or_else(|| "The destination project was closed.".to_owned())
                    .and_then(|workspace| workspace.insert_prepared_project(prepared).map_err(|error| error.to_string()))
                {
                    Ok(_) => format!("Inserted {frames} frames from {source}. Undo restores the previous timeline."),
                    Err(error) => format!("Could not insert project: {error}. The timeline is unchanged; unreferenced verified assets may remain."),
                }
            }
            Err(error) => format!("Could not prepare insertion: {error}. The timeline is unchanged; unreferenced verified assets may remain."),
        })
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui, workspace: &EditorWorkspace) {
        egui::CollapsingHeader::new("Insert recording or imported project").id_salt("insert-project").show(ui, |ui| {
            ui.label("Choose a .gfsproj with the same canvas size. Frames, overlays, and transitions are inserted together.");
            ui.weak("Import video/images or stop a recording to create its project first. Close that project in other windows before inserting it here.");
            ui.add_enabled_ui(!self.is_running(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(300.0).hint_text("/path/to/source.gfsproj"));
                    if let Some(notice) = self.picker.show(ui, &mut self.path, PathKind::Project) { self.notice = Some(notice); }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut self.at_start, true, "At start");
                    ui.radio_value(&mut self.at_start, false, "After current frame");
                });
                if ui.button("Insert project").clicked() && let Err(error) = self.start(workspace) { self.notice = Some(error); }
            });
            if self.is_running() {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Verifying and preparing source frames…"); });
                if ui.add_enabled(!self.task.is_cancelling(), egui::Button::new("Cancel insertion")).clicked() { self.cancel(); }
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
            if let Some(notice) = &self.notice { ui.label(notice); }
        });
    }

    fn start(&mut self, workspace: &EditorWorkspace) -> Result<(), String> {
        if self.path.trim().is_empty() {
            return Err("Choose a source project first.".to_owned());
        }
        let after = if self.at_start {
            None
        } else {
            Some(
                workspace
                    .selection()
                    .current()
                    .ok_or_else(|| "Select a frame or choose At start.".to_owned())?,
            )
        };
        let target: ProjectInsertionTarget = workspace
            .project_insertion_target(after)
            .map_err(|error| error.to_string())?;
        let source = PathBuf::from(self.path.trim());
        self.task.start("gfs-insert-project", move |context| {
            prepare_project_insertion_from_path(
                target,
                &source,
                &Cancellation(context.cancellation()),
            )
            .map_err(|error| error.to_string())
        })?;
        self.notice = None;
        Ok(())
    }
}
