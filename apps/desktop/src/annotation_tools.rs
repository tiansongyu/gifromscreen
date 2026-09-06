//! Annotation authoring with bounded background preparation and a recoverable workspace loan.

use crate::{
    annotation_engine::{
        AnnotationEditReport, AnnotationProgress, AnnotationReplaySkips, annotation_name,
    },
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

enum PendingAnnotation {
    Apply {
        anchor: OverlaySelectionAnchor,
        request: AnnotationRequest,
        replacing: Option<TrackId>,
    },
    ConfirmBinding {
        anchor: OverlaySelectionAnchor,
    },
}

impl PendingAnnotation {
    fn matches(&self, workspace: &EditorWorkspace) -> bool {
        match self {
            Self::Apply {
                anchor,
                replacing: Some(_),
                ..
            } => anchor.matches_project(workspace),
            Self::Apply { anchor, .. } | Self::ConfirmBinding { anchor } => {
                anchor.matches(workspace)
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct AnnotationTools {
    request: AnnotationRequest,
    project_id: Option<ProjectId>,
    pending: Option<PendingAnnotation>,
    replacing: Option<TrackId>,
    loan: Option<WorkspaceLoan>,
    completed: Option<Result<AnnotationEditReport, String>>,
    task: BackgroundTask<AnnotationEditReport, AnnotationProgress>,
    confirming: bool,
    cancel_pending: AtomicBool,
    notice: Option<String>,
}

impl AnnotationTools {
    pub(crate) fn queue_binding_confirmation(
        &mut self,
        workspace: &EditorWorkspace,
    ) -> Result<(), String> {
        if self.is_running() {
            return Err("Another annotation operation is already running.".to_owned());
        }
        let anchor = workspace
            .overlay_selection_anchor()
            .map_err(|error| error.to_string())?;
        self.pending = Some(PendingAnnotation::ConfirmBinding { anchor });
        self.confirming = true;
        self.cancel_pending.store(false, Ordering::Release);
        self.notice = None;
        Ok(())
    }
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
                if self.replacing.is_some(){ui.weak("Updating uses the saved authoring scope, including unmarked frames, independently of the current selection. Legacy groups without a saved scope remain limited to their existing marker coverage.");}
                if ui.button(if self.replacing.is_some(){"Update annotation group"}else{"Add annotations to selection"}).clicked() {
                    let result = self.request.validate(workspace.manifest().canvas.size).and_then(|()| if self.replacing.is_some(){Ok(workspace.project_edit_anchor())}else{workspace.overlay_selection_anchor().map_err(|e| e.to_string())});
                    match result {
                        Ok(anchor) => {self.pending=Some(PendingAnnotation::Apply {anchor,request:self.request.clone(),replacing:self.replacing});self.confirming=false;self.cancel_pending.store(false,Ordering::Release);self.notice=None;}
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
            ui.label(if self.confirming {
                "Confirming original input coordinates…"
            } else {
                "Preparing and saving annotations…"
            });
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
                Ok(report) if self.confirming => format!(
                    "Confirmed original input coordinates on {} frames; no annotations were added. One undo restores the previous binding.",
                    report.frames
                ),
                Ok(report) => format!(
                    "Annotations saved across {} frames. One undo restores the previous group. {}",
                    report.frames,
                    if report.replay_skips.is_empty() {
                        String::new()
                    } else {
                        report.replay_skips.message()
                    }
                ),
                Err(error) => format!(
                    "Annotation operation reported: {error} The workspace has been returned. If journal recovery is required, reopen before editing; verified unreferenced pixel assets may remain."
                ),
            };
            self.notice = Some(notice.clone());
            return Some(notice);
        }
        let pending = self.pending.take()?;
        if self.cancel_pending.load(Ordering::Acquire) {
            return Some("Annotations cancelled before preparation.".to_owned());
        }
        let Some(current) = workspace.as_ref() else {
            return Some("Open a project before adding annotations.".to_owned());
        };
        if !pending.matches(current) {
            return Some(
                "The annotation project or selection changed before preparation.".to_owned(),
            );
        }
        let loan = Arc::new(Mutex::new(workspace.take()));
        let worker = Arc::clone(&loan);
        self.confirming = matches!(pending, PendingAnnotation::ConfirmBinding { .. });
        if let Err(error) = self.task.start("gfs-annotations", move |context| {
            let mut slot = worker.lock().unwrap_or_else(PoisonError::into_inner);
            let workspace = slot
                .as_mut()
                .ok_or_else(|| "Annotation workspace is unavailable.".to_owned())?;
            match pending {
                PendingAnnotation::Apply {
                    anchor,
                    request,
                    replacing,
                } => workspace.apply_annotation_group(
                    &anchor,
                    &request,
                    replacing,
                    context.cancellation(),
                    |progress| context.report(progress),
                ),
                PendingAnnotation::ConfirmBinding { anchor } => workspace
                    .confirm_original_capture_binding(&anchor, context.cancellation())
                    .map(|frames| AnnotationEditReport {
                        frames,
                        replay_skips: AnnotationReplaySkips::default(),
                    }),
            }
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
                ui.checkbox(&mut options.remaining, "Reverse bar / percent");
                ui.checkbox(&mut options.show_bar, "Show bar");
            });
            egui::ComboBox::from_id_salt("progress-direction")
                .selected_text(match options.direction {
                    ProgressDirection::LeftToRight => "Left to right",
                    ProgressDirection::RightToLeft => "Right to left",
                    ProgressDirection::TopToBottom => "Top to bottom",
                    ProgressDirection::BottomToTop => "Bottom to top",
                })
                .show_ui(ui, |ui| {
                    for (direction, name) in [
                        (ProgressDirection::LeftToRight, "Left to right"),
                        (ProgressDirection::RightToLeft, "Right to left"),
                        (ProgressDirection::TopToBottom, "Top to bottom"),
                        (ProgressDirection::BottomToTop, "Bottom to top"),
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
            ui.weak("{remaining} shows time left. {frame} and {elapsed} always count up; reversing does not change your format.");
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
    let uses_text = request.mode.uses_text();
    let uses_box = matches!(
        request.mode,
        AnnotationMode::Progress(_)
            | AnnotationMode::ManualKeys { .. }
            | AnnotationMode::RecordedKeys
    );
    let uses_clicks = matches!(
        request.mode,
        AnnotationMode::ManualClick { .. } | AnnotationMode::RecordedClicks
    );
    if uses_box
        || matches!(
            request.mode,
            AnnotationMode::ManualClick { .. } | AnnotationMode::BuiltinCursor
        )
    {
        ui.horizontal_wrapped(|ui| {
            let (mut x, mut y, mut width, mut height) = (
                request.position.x.get(),
                request.position.y.get(),
                request.size.width.get(),
                request.size.height.get(),
            );
            ui.add(egui::DragValue::new(&mut x).prefix("X ").range(0..=16_383));
            ui.add(egui::DragValue::new(&mut y).prefix("Y ").range(0..=16_383));
            if uses_box {
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
            }
            request.position = PhysicalPoint {
                x: PhysicalPx::new(x),
                y: PhysicalPx::new(y),
            };
            request.size = PhysicalSize::new(width, height).unwrap_or(request.size);
        });
    }
    ui.horizontal_wrapped(|ui| {
        if uses_text || uses_clicks {
            ui.label("Foreground");
            edit_color(ui, &mut request.foreground);
        }
        if uses_box {
            ui.label("Background");
            edit_color(ui, &mut request.background);
        }
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
    if uses_text {
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
    if uses_clicks {
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

    fn legacy_workspace(root: &std::path::Path) -> EditorWorkspace {
        use gif_from_screen_domain::{
            AssetDescriptor, AssetKind, CaptureBinding, CaptureMetadata, ClipTransform, DurationUs,
            EditCommand, FrameClip, FrameId, KeyStroke, RasterEncoding, TimeUs,
        };
        let mut workspace = empty_workspace(root, 19);
        let size = workspace.manifest().canvas.size;
        let bytes = vec![0; 240 * 40 * 4];
        let asset_id = workspace.active_project().assets().put(&bytes).unwrap();
        let frame = FrameClip {
            id: FrameId::from_u128(1),
            asset_id,
            capture_binding: CaptureBinding::LegacyUnknown,
            duration: DurationUs::new(100_000).unwrap(),
            transform: ClipTransform::default(),
            effects: Vec::new(),
            capture_metadata: CaptureMetadata {
                key_strokes: vec![KeyStroke {
                    physical_key: "C".to_owned(),
                    display_text: Some("Ctrl+C".to_owned()),
                    pressed: true,
                    at: TimeUs::ZERO,
                    repeat: false,
                    modifiers: 2,
                }],
                ..CaptureMetadata::default()
            },
        };
        workspace
            .execute(EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: AssetDescriptor {
                            id: asset_id,
                            byte_len: bytes.len() as u64,
                            kind: AssetKind::Frame {
                                size,
                                encoding: RasterEncoding::Rgba8,
                            },
                        },
                    },
                    EditCommand::InsertFrames {
                        index: 0,
                        frames: vec![frame],
                    },
                ],
            })
            .unwrap();
        workspace.select_first().unwrap();
        workspace
    }

    #[test]
    fn explicit_legacy_confirmation_reuses_the_loan_and_never_adds_annotations() {
        use gif_from_screen_domain::CaptureBinding;
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = Some(legacy_workspace(&dir.path().join("confirm.gfsproj")));
        let original = workspace.as_ref().unwrap().manifest().timeline.frames[0]
            .capture_metadata
            .clone();
        let mut tool = AnnotationTools::default();
        tool.queue_binding_confirmation(workspace.as_ref().unwrap())
            .unwrap();
        assert!(tool.is_running());
        assert!(
            tool.queue_binding_confirmation(workspace.as_ref().unwrap())
                .is_err()
        );
        assert!(tool.poll(&mut workspace).is_none());
        assert!(workspace.is_none());
        let notice = finish(&mut tool, &mut workspace);
        assert!(notice.contains("Confirmed original input coordinates on 1"));
        assert!(notice.contains("no annotations were added"));
        let workspace = workspace.as_mut().unwrap();
        assert_eq!(
            workspace.manifest().timeline.frames[0].capture_binding,
            CaptureBinding::Original
        );
        assert_eq!(
            workspace.manifest().timeline.frames[0].capture_metadata,
            original
        );
        assert!(workspace.manifest().timeline.overlay_tracks.is_empty());
        workspace.undo().unwrap();
        assert_eq!(
            workspace.manifest().timeline.frames[0].capture_binding,
            CaptureBinding::LegacyUnknown
        );
    }

    #[test]
    fn confirmation_cancellation_and_selection_changes_cannot_weaken_the_binding_guard() {
        use gif_from_screen_domain::{CaptureBinding, FrameId};
        let dir = tempfile::tempdir().unwrap();
        let mut workspace = Some(legacy_workspace(&dir.path().join("cancel-confirm.gfsproj")));
        let before = workspace.as_ref().unwrap().manifest().clone();
        let mut tool = AnnotationTools::default();
        tool.queue_binding_confirmation(workspace.as_ref().unwrap())
            .unwrap();
        tool.cancel();
        assert!(tool.poll(&mut workspace).unwrap().contains("cancelled"));
        assert_eq!(workspace.as_ref().unwrap().manifest(), &before);
        tool.queue_binding_confirmation(workspace.as_ref().unwrap())
            .unwrap();
        workspace
            .as_mut()
            .unwrap()
            .toggle_selection(FrameId::from_u128(1))
            .unwrap();
        assert!(tool.poll(&mut workspace).unwrap().contains("changed"));
        assert_eq!(
            workspace.as_ref().unwrap().manifest().timeline.frames[0].capture_binding,
            CaptureBinding::LegacyUnknown
        );
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
            pending: Some(PendingAnnotation::Apply {
                anchor: workspace.as_ref().unwrap().project_edit_anchor(),
                request: AnnotationRequest::default(),
                replacing: None,
            }),
            ..AnnotationTools::default()
        };
        assert!(tool.poll(&mut workspace).is_none());
        assert!(workspace.is_none());
        assert!(finish(&mut tool, &mut workspace).contains("operation reported"));
        assert_eq!(workspace.as_ref().unwrap().manifest(), &before);
        assert!(!tool.is_running());
        tool.pending = Some(PendingAnnotation::Apply {
            anchor: workspace.as_ref().unwrap().project_edit_anchor(),
            request: AnnotationRequest::default(),
            replacing: None,
        });
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
            completed: Some(Ok(AnnotationEditReport {
                frames: 1,
                replay_skips: AnnotationReplaySkips::default(),
            })),
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
    fn cursor_forms_hide_unused_controls_and_leave_hidden_settings_unchanged() {
        fn collect(shape: &egui::epaint::Shape, text: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(shape) => text.push(shape.galley.job.text.clone()),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, text);
                    }
                }
                _ => {}
            }
        }
        for mode in [
            AnnotationMode::RecordedCursor,
            AnnotationMode::BuiltinCursor,
        ] {
            let mut request = AnnotationRequest {
                mode,
                foreground: Rgba::TRANSPARENT,
                font_family: String::new(),
                font_size_px: 0,
                click_radius: 0,
                hold_ms: 0,
                size: PhysicalSize {
                    width: PhysicalPx::ZERO,
                    height: PhysicalPx::ZERO,
                },
                ..AnnotationRequest::default()
            };
            let before = request.clone();
            let ctx = egui::Context::default();
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| show_annotation_options(ui, &mut request));
            });
            let mut texts = Vec::new();
            for shape in output.shapes {
                collect(&shape.shape, &mut texts);
            }
            for label in [
                "Foreground",
                "Background",
                "Font",
                "Click radius",
                "Hold ",
                "Width ",
                "Height ",
            ] {
                assert!(
                    !texts
                        .iter()
                        .any(|text| text == label || text.starts_with(label)),
                    "cursor mode exposed {label}: {texts:?}"
                );
            }
            assert_eq!(request, before);
        }
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
