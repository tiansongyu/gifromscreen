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
    cinemagraph_draft::CinemagraphDraft,
    cinemagraph_preview::CinemagraphPreview,
    editor_workspace::{
        EditorWorkspace, MotionOperation, MotionOutcome, MotionProgress, OverlaySelectionAnchor,
    },
};

type WorkspaceLoan = Arc<Mutex<Option<EditorWorkspace>>>;

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Mode {
    #[default]
    RectangularFreeze,
    Cinemagraph,
    SmoothLoop,
    LoopCrossfade,
}

struct PendingEdit {
    anchor: OverlaySelectionAnchor,
    operation: MotionOperation,
}

pub(crate) struct MotionTools {
    cine: CinemagraphDraft,
    cine_preview: CinemagraphPreview,
    applying_cinemagraph: bool,
    mode: Mode,
    project_id: Option<ProjectId>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    invert: bool,
    frames: u16,
    duration_ms: u64,
    skip_first: usize,
    similarity_tenths: u16,
    search_from_end: bool,
    pending: Option<PendingEdit>,
    loan: Option<WorkspaceLoan>,
    completed: Option<Result<MotionOutcome, String>>,
    label: &'static str,
    cancel_pending: AtomicBool,
    task: BackgroundTask<MotionOutcome, MotionProgress>,
    notice: Option<String>,
}

impl Default for MotionTools {
    fn default() -> Self {
        Self {
            cine: CinemagraphDraft::default(),
            cine_preview: CinemagraphPreview::default(),
            applying_cinemagraph: false,
            mode: Mode::default(),
            project_id: None,
            x: 0,
            y: 0,
            width: 160,
            height: 120,
            invert: false,
            frames: 8,
            duration_ms: 400,
            skip_first: 1,
            similarity_tenths: 1000,
            search_from_end: true,
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
                let previous_mode = self.mode;
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.mode, Mode::Cinemagraph, "Cinemagraph");
                    ui.selectable_value(&mut self.mode, Mode::RectangularFreeze, "Rectangular freeze");
                    ui.selectable_value(&mut self.mode, Mode::SmoothLoop, "Smooth loop");
                    ui.selectable_value(&mut self.mode, Mode::LoopCrossfade, "Loop crossfade");
                });
                if self.mode != previous_mode {
                    self.reconcile_cinemagraph(workspace);
                }
                match self.mode {
                    Mode::Cinemagraph => self.show_cinemagraph_controls(ui, workspace),
                    Mode::RectangularFreeze => {
                        ui.label("Use the current frame as a frozen image. Only the rectangular motion area continues animating in selected frames.");
                        let size = workspace.manifest().canvas.size;
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::DragValue::new(&mut self.x).prefix("X ").range(0..=size.width.get().saturating_sub(1)));
                            ui.add(egui::DragValue::new(&mut self.y).prefix("Y ").range(0..=size.height.get().saturating_sub(1)));
                            ui.add(egui::DragValue::new(&mut self.width).prefix("Width ").range(1..=size.width.get().saturating_sub(self.x).max(1)));
                            ui.add(egui::DragValue::new(&mut self.height).prefix("Height ").range(1..=size.height.get().saturating_sub(self.y).max(1)));
                        });
                        ui.checkbox(&mut self.invert, "Invert: freeze inside the rectangle");
                        ui.weak("The frozen image is saved once. Original frames and all layers, including hidden artwork, stay editable. Revealing earlier artwork changes only the live region; the frozen region keeps its saved pixels.");
                        ui.weak("Existing recorded-input annotations remain editable before the freeze. New annotations after it must be manual. Selection gaps stay untouched. Rectangles only, not freeform masks.");
                    }
                    Mode::SmoothLoop => {
                        ui.label("Find an ending frame similar to the first frame, then remove everything after the match.");
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Matching pixels");
                            ui.add(egui::DragValue::new(&mut self.similarity_tenths).range(1..=1000)
                                .custom_formatter(|value, _| format!("{:.1}%", value / 10.0))
                                .custom_parser(|text| text.trim_end_matches('%').trim().parse::<f64>().ok().map(|value| value * 10.0)));
                            ui.add(egui::DragValue::new(&mut self.skip_first).prefix("Skip initial frames ")
                                .range(1..=workspace.manifest().timeline.frames.len().saturating_sub(1).max(1)));
                        });
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.search_from_end, true, "Search from end");
                            ui.radio_value(&mut self.search_from_end, false, "Search from start");
                        });
                        ui.weak("The matching frame is retained. No match or an already-matching final frame leaves the project unchanged. Uses the whole timeline and final rendered pixels.");
                    }
                    Mode::LoopCrossfade => {
                        ui.label("Append a cross-fade from the last frame back to the first frame of the whole timeline.");
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::DragValue::new(&mut self.frames).prefix("Added frames ").range(1..=120));
                            ui.add(egui::DragValue::new(&mut self.duration_ms).suffix(" ms total").range(1..=60_000));
                        });
                        ui.weak("Creates baked new frames, not a live transition. Original frames stay unchanged; the final added frame exactly matches the first.");
                    }
                }
                ui.weak("One undo restores the original edit. Freeze uses a 64 MiB image-plus-reference working budget; crossfade can generate up to 256 MiB. Rendering and saving run in the background.");
                if self.mode != Mode::Cinemagraph && ui.button("Apply motion edit").clicked() && let Err(error) = self.queue(workspace) { self.notice = Some(error); }
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
            if self.applying_cinemagraph && result.is_ok() {
                self.cine.close();
                self.cine_preview.cancel();
            }
            self.applying_cinemagraph = false;
            let notice = match result {
                Ok(MotionOutcome::Edited(count)) => format!(
                    "{} applied to {count} frames. Undo restores the original timeline.",
                    self.label
                ),
                Ok(MotionOutcome::AlreadySmooth) => "The final frame already meets the similarity threshold. No frames were removed.".to_owned(),
                Ok(MotionOutcome::TrimmedTail(count)) => format!("Removed {count} trailing frames after the matching loop endpoint. Undo restores them."),
                Ok(MotionOutcome::NoMatchingEnd) => "No matching end frame was found at this threshold. The project is unchanged.".to_owned(),
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
            self.applying_cinemagraph = false;
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
                MotionOperation::Cinemagraph(Box::new(self.cine.request(workspace)?))
            }
            Mode::RectangularFreeze => {
                if workspace.selection().is_empty() {
                    return Err("Select frames and a current frozen reference first.".to_owned());
                }
                MotionOperation::RectangularFreeze {
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
            Mode::SmoothLoop => MotionOperation::FindSmoothLoop {
                skip_first: self.skip_first,
                similarity_tenths: self.similarity_tenths,
                from_end: self.search_from_end,
            },
            Mode::LoopCrossfade => MotionOperation::LoopCrossfade {
                frames: self.frames,
                duration_us: self.duration_ms.saturating_mul(1000),
            },
        };
        self.label = operation.label();
        self.applying_cinemagraph = matches!(&operation, MotionOperation::Cinemagraph(_));
        self.cine_preview.cancel();
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

#[path = "cinemagraph_controls.rs"]
mod cinemagraph_controls;

#[cfg(test)]
mod tests;
