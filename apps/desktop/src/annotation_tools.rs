//! Annotation authoring with bounded background preparation and a recoverable workspace loan.

use crate::{
    annotation_engine::{AnnotationProgress, annotation_name},
    background_task::BackgroundTask,
    editor_workspace::{EditorWorkspace, OverlaySelectionAnchor},
};
use eframe::egui;
use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, MouseButton, PhysicalPoint, PhysicalPx, PhysicalSize,
    ProgressDirection, ProgressMeasure, ProgressOptions, ProjectId, Rgba, TrackId,
};
use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

type WorkspaceLoan = Arc<Mutex<Option<EditorWorkspace>>>;

#[derive(Default)]
pub(crate) struct AnnotationTools {
    request: AnnotationRequest,
    project_id: Option<ProjectId>,
    pending: Option<(OverlaySelectionAnchor, AnnotationRequest, Option<TrackId>)>,
    replacing: Option<TrackId>,
    loan: Option<WorkspaceLoan>,
    completed: Option<Result<usize, String>>,
    task: BackgroundTask<usize, AnnotationProgress>,
    cancel_pending: AtomicBool,
    notice: Option<String>,
}

impl AnnotationTools {
    pub(crate) fn is_running(&self) -> bool {
        self.pending.is_some() || self.loan.is_some() || self.task.is_running()
    }
    pub(crate) fn cancel(&self) {
        self.cancel_pending.store(true, Ordering::Release);
        self.task.cancel();
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui, workspace: &EditorWorkspace) {
        egui::CollapsingHeader::new("Progress & input annotations").id_salt("annotation-tools").show(ui,|ui| {
            if self.project_id != Some(workspace.manifest().project_id) {
                self.project_id = Some(workspace.manifest().project_id);
                self.replacing = None;
                let canvas = workspace.manifest().canvas.size;
                self.request.size = PhysicalSize::new(canvas.width.get().min(240),canvas.height.get().min(40)).expect("valid canvas");
                self.request.position = PhysicalPoint {x:PhysicalPx::ZERO,y:PhysicalPx::new(canvas.height.get()-self.request.size.height.get())};
            }
            ui.add_enabled_ui(!self.is_running(),|ui| {
                let editing_name=self.replacing.and_then(|id|workspace.manifest().timeline.overlay_tracks.iter().find(|track|track.id==id)).map_or("New annotation group",|track|track.name.as_str());
                egui::ComboBox::from_id_salt("annotation-existing-group").selected_text(editing_name).show_ui(ui,|ui|{
                    if ui.selectable_label(self.replacing.is_none(),"New annotation group").clicked(){self.replacing=None;}
                    for track in &workspace.manifest().timeline.overlay_tracks {
                        if let Some(request)=&track.annotation && ui.selectable_label(self.replacing==Some(track.id),&track.name).clicked(){self.replacing=Some(track.id);self.request=request.clone();}
                    }
                });
                show_annotation_options(ui,&mut self.request);
                ui.weak("Applies to selected frames only. Gaps stay untouched. Labels use original timeline frame numbers and frame-end times; transition in-betweens are not numbered separately.");
                ui.weak("Recorded events are sampled per frame. Captured cursor pixels follow crop, resize, rotation and flips exactly. Events cannot cross selection gaps.");
                ui.weak("Annotations are frozen at authoring time. Duration edits ripple their spans; reordering keeps their timeline times. Update the group to regenerate numbers or event positions.");
                if self.replacing.is_some(){ui.weak("Updating preserves the group's exact time coverage and visibility, independent of the frame selection. Values regenerate from the current timeline.");}
                if ui.button(if self.replacing.is_some(){"Update annotation group"}else{"Add annotations to selection"}).clicked() {
                    let result = self.request.validate(workspace.manifest().canvas.size).and_then(|()| if self.replacing.is_some(){Ok(workspace.project_edit_anchor())}else{workspace.overlay_selection_anchor().map_err(|e| e.to_string())});
                    match result {
                        Ok(anchor) => {self.pending=Some((anchor,self.request.clone(),self.replacing));self.cancel_pending.store(false,Ordering::Release);self.notice=None;}
                        Err(error) => self.notice=Some(error),
                    }
                }
            });
            if self.is_running() {self.show_running(ui);}
            if let Some(notice)=&self.notice {ui.label(notice);}
        });
    }

    pub(crate) fn show_running(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Preparing and saving annotations…");
        });
        if let Some(progress) = self.task.progress() {
            ui.label(format!(
                "{} / {} selected frames",
                progress.completed, progress.total
            ));
        }
        if ui.button("Cancel annotation preparation").clicked() {
            self.cancel();
        }
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }

    pub(crate) fn poll(&mut self, workspace: &mut Option<EditorWorkspace>) -> Option<String> {
        if let Some(result) = self.task.poll() {
            self.completed = Some(result);
        }
        if self.completed.is_some() {
            if workspace.is_some() {
                return Some("Annotations finished, but another project occupies the editor. The original workspace is held safely.".to_owned());
            }
            let loan = self.loan.take()?;
            *workspace = loan.lock().unwrap_or_else(PoisonError::into_inner).take();
            let notice = match self.completed.take()? {
                Ok(count) => format!(
                    "Annotations saved across {count} frames. One undo restores the previous group."
                ),
                Err(error) => format!(
                    "Annotation operation reported: {error} The workspace has been returned. If journal recovery is required, reopen before editing; verified unreferenced pixel assets may remain."
                ),
            };
            self.notice = Some(notice.clone());
            return Some(notice);
        }
        let (anchor, request, replacing) = self.pending.take()?;
        if self.cancel_pending.load(Ordering::Acquire) {
            return Some("Annotations cancelled before preparation.".to_owned());
        }
        let Some(current) = workspace.as_ref() else {
            return Some("Open a project before adding annotations.".to_owned());
        };
        if !anchor.matches(current) {
            return Some(
                "The annotation project or selection changed before preparation.".to_owned(),
            );
        }
        let loan = Arc::new(Mutex::new(workspace.take()));
        let worker = Arc::clone(&loan);
        if let Err(error) = self.task.start("gfs-annotations", move |context| {
            let mut slot = worker.lock().unwrap_or_else(PoisonError::into_inner);
            slot.as_mut()
                .ok_or_else(|| "Annotation workspace is unavailable.".to_owned())?
                .apply_annotation_group(
                    &anchor,
                    &request,
                    replacing,
                    context.cancellation(),
                    |progress| context.report(progress),
                )
        }) {
            *workspace = loan.lock().unwrap_or_else(PoisonError::into_inner).take();
            return Some(error);
        }
        self.loan = Some(loan);
        None
    }
}

/// Shared with automatic tasks; editing settings never launches work or writes files.
pub(crate) fn show_annotation_options(ui: &mut egui::Ui, request: &mut AnnotationRequest) {
    egui::ComboBox::from_id_salt("annotation-mode")
        .selected_text(annotation_name(&request.mode))
        .show_ui(ui, |ui| {
            for mode in [
                AnnotationMode::Progress(ProgressOptions::default()),
                AnnotationMode::ManualKeys {
                    text: "Ctrl+C".to_owned(),
                },
                AnnotationMode::RecordedKeys,
                AnnotationMode::ManualClick {
                    button: MouseButton::Left,
                },
                AnnotationMode::RecordedClicks,
                AnnotationMode::RecordedCursor,
                AnnotationMode::BuiltinCursor,
            ] {
                let selected =
                    std::mem::discriminant(&mode) == std::mem::discriminant(&request.mode);
                if ui
                    .selectable_label(selected, annotation_name(&mode))
                    .clicked()
                    && !selected
                {
                    request.mode = mode;
                }
            }
        });
    match &mut request.mode {
        AnnotationMode::Progress(options) => {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut options.measure, ProgressMeasure::Frames, "Frame count");
                ui.selectable_value(
                    &mut options.measure,
                    ProgressMeasure::ElapsedTime,
                    "Elapsed time",
                );
                ui.checkbox(&mut options.remaining, "Count down");
                ui.checkbox(&mut options.show_bar, "Show bar");
            });
            egui::ComboBox::from_id_salt("progress-direction")
                .selected_text(format!("{:?}", options.direction))
                .show_ui(ui, |ui| {
                    for (direction, name) in [
                        (ProgressDirection::LeftToRight, "Left → right"),
                        (ProgressDirection::RightToLeft, "Right → left"),
                        (ProgressDirection::TopToBottom, "Top → bottom"),
                        (ProgressDirection::BottomToTop, "Bottom → top"),
                    ] {
                        ui.selectable_value(&mut options.direction, direction, name);
                    }
                });
            ui.horizontal(|ui| {
                ui.label("Bar color");
                edit_color(ui, &mut options.bar_color);
            });
            ui.label("Label format (empty = bar only)");
            ui.add(egui::TextEdit::singleline(&mut options.format).char_limit(1024));
            ui.weak("{frame}  {frames}  {elapsed}  {total}  {remaining}  {percent}");
        }
        AnnotationMode::ManualKeys { text } => {
            ui.label("Key label (including modifiers)");
            ui.add(egui::TextEdit::singleline(text).char_limit(4096));
        }
        AnnotationMode::ManualClick { button } => {
            ui.horizontal(|ui| {
                ui.selectable_value(button, MouseButton::Left, "Left");
                ui.selectable_value(button, MouseButton::Middle, "Middle");
                ui.selectable_value(button, MouseButton::Right, "Right");
            });
        }
        AnnotationMode::RecordedCursor => {
            ui.weak("Uses recorded cursor pixels when available. Frames that already contain the cursor are skipped to avoid duplicate pointers.");
        }
        AnnotationMode::RecordedKeys | AnnotationMode::RecordedClicks => {
            ui.weak("Requires input events in the recorded frames. Backends without global input access can use the manual annotation modes.");
        }
        AnnotationMode::BuiltinCursor => {}
    }
    show_common_annotation_options(ui, request);
}

fn show_common_annotation_options(ui: &mut egui::Ui, request: &mut AnnotationRequest) {
    ui.horizontal_wrapped(|ui| {
        let (mut x, mut y, mut width, mut height) = (
            request.position.x.get(),
            request.position.y.get(),
            request.size.width.get(),
            request.size.height.get(),
        );
        ui.add(egui::DragValue::new(&mut x).prefix("X ").range(0..=16_383));
        ui.add(egui::DragValue::new(&mut y).prefix("Y ").range(0..=16_383));
        ui.add(
            egui::DragValue::new(&mut width)
                .prefix("Width ")
                .range(1..=4096),
        );
        ui.add(
            egui::DragValue::new(&mut height)
                .prefix("Height ")
                .range(1..=4096),
        );
        request.position = PhysicalPoint {
            x: PhysicalPx::new(x),
            y: PhysicalPx::new(y),
        };
        request.size = PhysicalSize::new(width, height).unwrap_or(request.size);
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Foreground");
        edit_color(ui, &mut request.foreground);
        ui.label("Background");
        edit_color(ui, &mut request.background);
        ui.add(
            egui::DragValue::new(&mut request.opacity)
                .prefix("Opacity ")
                .range(1..=255),
        );
        ui.add(
            egui::DragValue::new(&mut request.z_index)
                .prefix("Layer ")
                .range(-1000..=1000),
        );
    });
    if matches!(
        request.mode,
        AnnotationMode::ManualKeys { .. }
            | AnnotationMode::RecordedKeys
            | AnnotationMode::Progress(_)
    ) {
        ui.horizontal(|ui| {
            ui.label("Font");
            ui.add(
                egui::TextEdit::singleline(&mut request.font_family)
                    .desired_width(130.0)
                    .char_limit(256),
            );
            ui.add(
                egui::DragValue::new(&mut request.font_size_px)
                    .suffix(" px")
                    .range(1..=512),
            );
        });
    }
    if matches!(
        request.mode,
        AnnotationMode::ManualClick { .. } | AnnotationMode::RecordedClicks
    ) {
        ui.add(
            egui::DragValue::new(&mut request.click_radius)
                .prefix("Click radius ")
                .suffix(" px")
                .range(1..=1024),
        );
    }
    if matches!(
        request.mode,
        AnnotationMode::RecordedKeys | AnnotationMode::RecordedClicks
    ) {
        ui.add(
            egui::DragValue::new(&mut request.hold_ms)
                .prefix("Hold ")
                .suffix(" ms")
                .range(1..=60_000),
        );
    }
    ui.weak("Up to 10,000 selected frames, 40,000 markers, and 256 MiB of saved label pixels.");
}

fn edit_color(ui: &mut egui::Ui, color: &mut Rgba) {
    let mut rgba = [color.red, color.green, color.blue, color.alpha];
    if ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed() {
        *color = Rgba {
            red: rgba[0],
            green: rgba[1],
            blue: rgba[2],
            alpha: rgba[3],
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::{
        Canvas, CanvasBackground, ColorSpace, ProjectManifest, UnixTimeMs,
    };
    use gif_from_screen_project::ActiveProject;
    use std::time::Instant;

    fn empty_workspace(root: &std::path::Path, id: u128) -> EditorWorkspace {
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(id),
            "annotation-loan-test",
            UnixTimeMs::new(0),
            Canvas {
                size: PhysicalSize::new(240, 40).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        EditorWorkspace::from_active(ActiveProject::create(root, manifest).unwrap(), 16).unwrap()
    }

    fn finish(tool: &mut AnnotationTools, workspace: &mut Option<EditorWorkspace>) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(message) = tool.poll(workspace) {
                return message;
            }
            assert!(
                Instant::now() < deadline,
                "annotation worker did not return its loan"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn errors_and_prelaunch_cancellation_return_the_original_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = Some(empty_workspace(&dir.path().join("error.gfsproj"), 1));
        let before = workspace.as_ref().unwrap().manifest().clone();
        let mut tool = AnnotationTools {
            pending: Some((
                workspace.as_ref().unwrap().project_edit_anchor(),
                AnnotationRequest::default(),
                None,
            )),
            ..AnnotationTools::default()
        };
        assert!(tool.poll(&mut workspace).is_none());
        assert!(workspace.is_none());
        assert!(finish(&mut tool, &mut workspace).contains("operation reported"));
        assert_eq!(workspace.as_ref().unwrap().manifest(), &before);
        assert!(!tool.is_running());
        tool.pending = Some((
            workspace.as_ref().unwrap().project_edit_anchor(),
            AnnotationRequest::default(),
            None,
        ));
        tool.cancel();
        assert!(tool.poll(&mut workspace).unwrap().contains("cancelled"));
        assert_eq!(workspace.as_ref().unwrap().manifest(), &before);
    }

    #[test]
    fn panic_poisoned_loan_is_returned_without_losing_project() {
        let dir = tempfile::tempdir().unwrap();
        let original = empty_workspace(&dir.path().join("panic.gfsproj"), 1);
        let loan = Arc::new(Mutex::new(Some(original)));
        let worker = Arc::clone(&loan);
        let mut tool = AnnotationTools {
            loan: Some(loan),
            ..AnnotationTools::default()
        };
        tool.task
            .start("annotation-panic-test", move |_| {
                let _guard = worker.lock().unwrap();
                panic!("injected authoring panic")
            })
            .unwrap();
        let mut workspace = None;
        assert!(finish(&mut tool, &mut workspace).contains("without a result"));
        assert_eq!(
            workspace.unwrap().manifest().project_id,
            ProjectId::from_u128(1)
        );
        assert!(!tool.is_running());
    }

    #[test]
    fn completed_job_keeps_loan_if_another_workspace_occupies_editor() {
        let dir = tempfile::tempdir().unwrap();
        let original = empty_workspace(&dir.path().join("held.gfsproj"), 1);
        let mut tool = AnnotationTools {
            loan: Some(Arc::new(Mutex::new(Some(original)))),
            completed: Some(Ok(1)),
            ..AnnotationTools::default()
        };
        let mut other = Some(empty_workspace(&dir.path().join("other.gfsproj"), 2));
        assert!(tool.poll(&mut other).unwrap().contains("held safely"));
        assert!(tool.is_running());
        assert_eq!(
            other.as_ref().unwrap().manifest().project_id,
            ProjectId::from_u128(2)
        );
        drop(other.take());
        assert!(tool.poll(&mut other).unwrap().contains("Annotations saved"));
        assert_eq!(
            other.unwrap().manifest().project_id,
            ProjectId::from_u128(1)
        );
    }

    #[test]
    fn options_form_renders_every_mode_without_starting_work() {
        let ctx = egui::Context::default();
        for mode in [
            AnnotationMode::Progress(ProgressOptions::default()),
            AnnotationMode::ManualKeys {
                text: "Ctrl+C".to_owned(),
            },
            AnnotationMode::RecordedKeys,
            AnnotationMode::RecordedClicks,
            AnnotationMode::RecordedCursor,
            AnnotationMode::BuiltinCursor,
            AnnotationMode::ManualClick {
                button: MouseButton::Left,
            },
        ] {
            let mut request = AnnotationRequest {
                mode,
                ..AnnotationRequest::default()
            };
            let original = request.clone();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| show_annotation_options(ui, &mut request));
            });
            assert_eq!(request, original);
        }
    }
}
