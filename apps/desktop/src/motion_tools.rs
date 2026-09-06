//! Background motion tools with an exclusive, recoverable workspace loan.

use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use eframe::egui;
use gif_from_screen_domain::{PhysicalPoint, PhysicalPx, PhysicalRect, PhysicalSize, ProjectId};

use crate::{
    background_task::BackgroundTask,
    editor_workspace::{EditorWorkspace, MotionOperation, MotionProgress, OverlaySelectionAnchor},
};

type WorkspaceLoan = Arc<Mutex<Option<EditorWorkspace>>>;

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Mode {
    #[default]
    Cinemagraph,
    SmoothLoop,
}

struct PendingEdit {
    anchor: OverlaySelectionAnchor,
    operation: MotionOperation,
}

pub(crate) struct MotionTools {
    mode: Mode,
    project_id: Option<ProjectId>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    invert: bool,
    frames: u16,
    duration_ms: u64,
    pending: Option<PendingEdit>,
    loan: Option<WorkspaceLoan>,
    completed: Option<Result<usize, String>>,
    label: &'static str,
    cancel_pending: AtomicBool,
    task: BackgroundTask<usize, MotionProgress>,
    notice: Option<String>,
}

impl Default for MotionTools {
    fn default() -> Self {
        Self {
            mode: Mode::default(),
            project_id: None,
            x: 0,
            y: 0,
            width: 160,
            height: 120,
            invert: false,
            frames: 8,
            duration_ms: 400,
            pending: None,
            loan: None,
            completed: None,
            label: "Motion edit",
            cancel_pending: AtomicBool::new(false),
            task: BackgroundTask::default(),
            notice: None,
        }
    }
}

impl MotionTools {
    pub(crate) fn is_running(&self) -> bool {
        self.pending.is_some() || self.loan.is_some() || self.task.is_running()
    }

    pub(crate) fn cancel(&self) {
        self.cancel_pending.store(true, Ordering::Release);
        self.task.cancel();
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui, workspace: &EditorWorkspace) {
        egui::CollapsingHeader::new("Motion tools").id_salt("motion-tools").show(ui, |ui| {
            self.sync_canvas(workspace);
            ui.add_enabled_ui(!self.is_running(), |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.mode, Mode::Cinemagraph, "Cinemagraph");
                    ui.selectable_value(&mut self.mode, Mode::SmoothLoop, "Smooth loop");
                });
                match self.mode {
                    Mode::Cinemagraph => {
                        ui.label("Use the current frame as a frozen image. Only the rectangular motion area continues animating in selected frames.");
                        let size = workspace.manifest().canvas.size;
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::DragValue::new(&mut self.x).prefix("X ").range(0..=size.width.get().saturating_sub(1)));
                            ui.add(egui::DragValue::new(&mut self.y).prefix("Y ").range(0..=size.height.get().saturating_sub(1)));
                            ui.add(egui::DragValue::new(&mut self.width).prefix("Width ").range(1..=size.width.get().saturating_sub(self.x).max(1)));
                            ui.add(egui::DragValue::new(&mut self.height).prefix("Height ").range(1..=size.height.get().saturating_sub(self.y).max(1)));
                        });
                        ui.checkbox(&mut self.invert, "Invert: freeze inside the rectangle");
                        ui.weak("Selected frames and their visible overlays are baked into pixels. Gaps in the selection stay untouched. Rectangles only, not freeform masks.");
                    }
                    Mode::SmoothLoop => {
                        ui.label("Append a cross-fade from the last frame back to the first frame of the whole timeline.");
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::DragValue::new(&mut self.frames).prefix("Added frames ").range(1..=120));
                            ui.add(egui::DragValue::new(&mut self.duration_ms).suffix(" ms total").range(1..=60_000));
                        });
                        ui.weak("Creates baked new frames, not a live transition. Original frames stay unchanged; the final added frame exactly matches the first.");
                    }
                }
                ui.weak("One undo restores the original edit. Up to 256 MiB of generated pixels. Rendering and saving run in the background.");
                if ui.button("Apply motion edit").clicked() && let Err(error) = self.queue(workspace) { self.notice = Some(error); }
            });
            if self.is_running() { self.show_running(ui); }
            if let Some(notice) = &self.notice { ui.label(notice); }
        });
    }

    pub(crate) fn show_running(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("{} — preparing and saving…", self.label));
        });
        if let Some(progress) = self.task.progress() {
            ui.label(format!(
                "{} / {} frames prepared",
                progress.completed, progress.total
            ));
        }
        if ui.button("Cancel motion edit").clicked() {
            self.cancel();
        }
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }

    /// Called last in the root's background completion handler. The root must
    /// disable other jobs/navigation while this tool is pending or holds a loan.
    pub(crate) fn poll(&mut self, workspace: &mut Option<EditorWorkspace>) -> Option<String> {
        if let Some(result) = self.task.poll() {
            self.completed = Some(result);
        }
        if self.completed.is_some() {
            if workspace.is_some() {
                return Some("Motion edit is ready, but another project occupies the editor. The original workspace and undo history remain held safely.".to_owned());
            }
            let loan = self.loan.take()?;
            *workspace = loan.lock().unwrap_or_else(PoisonError::into_inner).take();
            let result = self.completed.take()?;
            let notice = match result {
                Ok(count) => format!(
                    "{} applied to {count} frames. Undo restores the original timeline.",
                    self.label
                ),
                Err(error) => format!(
                    "{} did not complete: {error} The project and its history were restored; verified unreferenced pixel assets may remain.",
                    self.label
                ),
            };
            self.notice = Some(notice.clone());
            return Some(notice);
        }
        let pending = self.pending.take()?;
        if self.cancel_pending.load(Ordering::Acquire) {
            return Some("Motion edit cancelled before preparation.".to_owned());
        }
        let Some(current) = workspace.as_ref() else {
            return Some("Open a project before applying a motion edit.".to_owned());
        };
        if !pending.anchor.matches(current) {
            return Some(
                "The project or selection changed before the motion edit could start.".to_owned(),
            );
        }
        let loan = Arc::new(Mutex::new(workspace.take()));
        let worker_loan = Arc::clone(&loan);
        let started = self.task.start("gfs-motion-edit", move |context| {
            let mut slot = worker_loan.lock().unwrap_or_else(PoisonError::into_inner);
            let current = slot
                .as_mut()
                .ok_or_else(|| "Motion workspace is unavailable.".to_owned())?;
            current.apply_motion_edit(
                &pending.anchor,
                pending.operation,
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

    fn queue(&mut self, workspace: &EditorWorkspace) -> Result<(), String> {
        if self.is_running() {
            return Err("A motion edit is already pending or running.".to_owned());
        }
        let operation = match self.mode {
            Mode::Cinemagraph => {
                if workspace.selection().is_empty() {
                    return Err("Select frames and a current frozen reference first.".to_owned());
                }
                MotionOperation::Cinemagraph {
                    region: PhysicalRect {
                        origin: PhysicalPoint {
                            x: PhysicalPx::new(self.x),
                            y: PhysicalPx::new(self.y),
                        },
                        size: PhysicalSize::new(self.width, self.height)
                            .map_err(|error| error.to_string())?,
                    },
                    invert: self.invert,
                }
            }
            Mode::SmoothLoop => MotionOperation::SmoothLoop {
                frames: self.frames,
                duration_us: self.duration_ms.saturating_mul(1000),
            },
        };
        self.label = operation.label();
        self.pending = Some(PendingEdit {
            anchor: workspace.project_edit_anchor(),
            operation,
        });
        self.cancel_pending.store(false, Ordering::Release);
        self.notice = None;
        Ok(())
    }

    fn sync_canvas(&mut self, workspace: &EditorWorkspace) {
        if self.project_id != Some(workspace.manifest().project_id) {
            self.project_id = Some(workspace.manifest().project_id);
            let size = workspace.manifest().canvas.size;
            self.x = size.width.get() / 4;
            self.y = size.height.get() / 4;
            self.width = (size.width.get() / 2).max(1);
            self.height = (size.height.get() / 2).max(1);
        }
        let size = workspace.manifest().canvas.size;
        self.x = self.x.min(size.width.get().saturating_sub(1));
        self.y = self.y.min(size.height.get().saturating_sub(1));
        self.width = self
            .width
            .max(1)
            .min(size.width.get().saturating_sub(self.x).max(1));
        self.height = self
            .height
            .max(1)
            .min(size.height.get().saturating_sub(self.y).max(1));
    }
}

#[cfg(test)]
mod tests;
