use eframe::egui;
use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, EditTaskTrigger, EditingTask, EditingTaskAction,
    EditingTaskPreset, EditingTaskSources, MAX_EDIT_TASKS, MAX_EDITING_PRESETS, Rgba, TaskDelay,
};

use super::AutoTasks;
use crate::editor_workspace::EditorWorkspace;

impl AutoTasks {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, workspace: Option<&EditorWorkspace>) {
        egui::CollapsingHeader::new("Automatic tasks & editing presets").id_salt("automatic-editing-tasks").default_open(true).show(ui, |ui| {
            ui.label("Apply a saved, ordered editing chain after recording or import. Reuse it manually with one undo for the whole chain.");
            if self.is_loading() { ui.horizontal(|ui| { ui.spinner(); ui.label("Loading or saving editing presets…"); }); }
            if let Some(error) = &self.settings_error { ui.colored_label(ui.visuals().error_fg_color, error); }
            let editable = !self.is_loading() && !self.is_running() && self.snapshot.is_some();
            ui.add_enabled_ui(editable, |ui| {
                ui.add_enabled_ui(self.draft.active_preset.is_some(), |ui| { ui.checkbox(&mut self.draft.enabled, "Apply automatically to new recordings and imports"); });
                egui::ComboBox::from_id_salt("active-editing-preset").selected_text(self.draft.active_preset.as_deref().unwrap_or("No active preset")).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.draft.active_preset, None, "No active preset");
                    for preset in &self.draft.presets { ui.selectable_value(&mut self.draft.active_preset, Some(preset.name.clone()), &preset.name); }
                });
                if self.draft.active_preset.is_none() { self.draft.enabled = false; }
                ui.horizontal_wrapped(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("New preset name").desired_width(160.0));
                    if ui.add_enabled(self.draft.presets.len() < MAX_EDITING_PRESETS, egui::Button::new("Create preset")).clicked() {
                        let name = self.new_name.trim().to_owned();
                        if let Err(error) = gif_from_screen_domain::validate_task_name(&name) { self.notice = Some(error.to_owned()); }
                        else if self.draft.presets.iter().any(|p| p.name == name) { self.notice = Some("That editing preset name already exists.".to_owned()); }
                        else { self.selected = self.draft.presets.len(); self.draft.presets.push(EditingTaskPreset { name: name.clone(), sources: EditingTaskSources::default(), tasks: vec![] }); self.draft.active_preset.get_or_insert(name); }
                    }
                });
                self.selected = self.selected.min(self.draft.presets.len().saturating_sub(1));
                if !self.draft.presets.is_empty() {
                    egui::ComboBox::from_id_salt("edit-editing-preset").selected_text(&self.draft.presets[self.selected].name).show_ui(ui, |ui| { for (index, preset) in self.draft.presets.iter().enumerate() { ui.selectable_value(&mut self.selected, index, &preset.name); } });
                    let preset = &mut self.draft.presets[self.selected];
                    let previous_name = preset.name.clone();
                    ui.horizontal(|ui| { ui.label("Name"); ui.text_edit_singleline(&mut preset.name); });
                    if self.draft.active_preset.as_deref() == Some(previous_name.as_str()) { self.draft.active_preset = Some(preset.name.clone()); }
                    ui.horizontal_wrapped(|ui| {
                        ui.label("After"); ui.checkbox(&mut preset.sources.screen, "Screen recording"); ui.checkbox(&mut preset.sources.camera, "Camera"); ui.checkbox(&mut preset.sources.board, "Board"); ui.checkbox(&mut preset.sources.import, "Import");
                    });
                    ui.weak("Recorded input tasks are only automatic for screen recordings. Tasks with no matching events are skipped, and later tasks continue; input is never invented.");
                    let mut operation = None;
                    for (index, task) in preset.tasks.iter_mut().enumerate() {
                        ui.push_id(index, |ui| {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.checkbox(&mut task.enabled, format!("{}.", index + 1));
                                    ui.add(egui::TextEdit::singleline(&mut task.name).desired_width(180.0));
                                    if ui.add_enabled(index > 0, egui::Button::new("Move up")).clicked() { operation = Some((index, -1)); }
                                    if ui.button("Move down").clicked() { operation = Some((index, 1)); }
                                    if ui.button("Delete task").clicked() { operation = Some((index, 0)); }
                                });
                                ui.add_enabled_ui(task.enabled, |ui| show_task(ui, &mut task.action));
                            });
                        });
                    }
                    if let Some((index, direction)) = operation { match direction { -1 if index > 0 => preset.tasks.swap(index, index - 1), 1 if index + 1 < preset.tasks.len() => preset.tasks.swap(index, index + 1), 0 => { preset.tasks.remove(index); }, _ => {} } }
                    ui.horizontal_wrapped(|ui| {
                        egui::ComboBox::from_id_salt("new-editing-task-type").selected_text(task_label(self.new_kind)).show_ui(ui, |ui| { for kind in 0..7 { ui.selectable_value(&mut self.new_kind, kind, task_label(kind)); } });
                        if ui.add_enabled(preset.tasks.len() < MAX_EDIT_TASKS, egui::Button::new("Add task")).clicked() { preset.tasks.push(new_task(self.new_kind, workspace.map(|workspace| workspace.manifest().canvas.size))); }
                    });
                    if ui.button("Delete this preset").clicked() {
                        let removed = self.draft.presets.remove(self.selected);
                        if self.draft.active_preset.as_ref() == Some(&removed.name) { self.draft.active_preset = None; self.draft.enabled = false; }
                        self.selected = self.selected.saturating_sub(1);
                    }
                }
                let unsaved = self.snapshot.as_ref().is_some_and(|s| s.config != self.draft);
                if unsaved { ui.weak("Unsaved changes. Automatic and manual runs use saved settings only."); }
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(unsaved, egui::Button::new("Save presets")).clicked() { self.save(); }
                    let runnable = !unsaved && workspace.is_some() && self.draft.active_preset.is_some();
                    if ui.add_enabled(runnable, egui::Button::new("Run saved preset now")).clicked() && let Some(workspace) = workspace && let Err(error) = self.queue_created(workspace, EditTaskTrigger::Manual) { self.notice = Some(error); }
                });
            });
            if ui.add_enabled(!self.is_loading() && !self.is_running(), egui::Button::new("Reload saved presets")).clicked() { self.reload(); }
            if self.is_running() { self.show_running(ui); }
            if let Some(notice) = &self.notice { ui.label(notice); }
            ui.weak("Up to 32 presets with 32 tasks each. Presets contain editing data only; they cannot launch scripts or programs.");
        });
    }

    pub(crate) fn show_running(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Preparing automatic editing tasks…");
        });
        if let Some(progress) = self.job.progress() {
            ui.label(format!(
                "Task {} / {} · {}",
                progress.task, progress.total, progress.name
            ));
        }
        if ui.button("Cancel editing tasks").clicked() {
            self.cancel();
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
}

fn show_task(ui: &mut egui::Ui, action: &mut EditingTaskAction) {
    match action {
        EditingTaskAction::ImageBorder { style } => crate::image_effect_ui::show_border(ui, style),
        EditingTaskAction::ImageShadow { style } => crate::image_effect_ui::show_shadow(ui, style),
        EditingTaskAction::Delay { mode } => {
            let mut selected = match mode {
                TaskDelay::Override { .. } => 0,
                TaskDelay::Adjust { .. } => 1,
                TaskDelay::Scale { .. } => 2,
            };
            let previous = selected;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut selected, 0, "Set delay");
                ui.selectable_value(&mut selected, 1, "Add / subtract");
                ui.selectable_value(&mut selected, 2, "Scale");
            });
            if selected != previous {
                *mode = match selected {
                    0 => TaskDelay::Override { milliseconds: 100 },
                    1 => TaskDelay::Adjust { milliseconds: 10 },
                    _ => TaskDelay::Scale { percent: 100 },
                };
            }
            match mode {
                TaskDelay::Override { milliseconds } => {
                    ui.add(
                        egui::DragValue::new(milliseconds)
                            .range(1..=3_600_000)
                            .suffix(" ms per frame"),
                    );
                }
                TaskDelay::Adjust { milliseconds } => {
                    ui.add(
                        egui::DragValue::new(milliseconds)
                            .range(-3_600_000..=3_600_000)
                            .suffix(" ms per frame"),
                    );
                }
                TaskDelay::Scale { percent } => {
                    ui.add(
                        egui::DragValue::new(percent)
                            .range(1..=100_000)
                            .suffix(" % of original delay"),
                    );
                }
            }
        }
        EditingTaskAction::Border { widths, color } => {
            ui.weak("Legacy inset border: preserves this preset's original fixed-canvas behavior.");
            ui.horizontal_wrapped(|ui| {
                for (label, edge) in [
                    ("Top ", &mut widths.top),
                    ("Right ", &mut widths.right),
                    ("Bottom ", &mut widths.bottom),
                    ("Left ", &mut widths.left),
                ] {
                    ui.add(
                        egui::DragValue::new(edge)
                            .range(0..=u16::MAX)
                            .prefix(label)
                            .suffix(" px"),
                    );
                }
            });
            color_input(ui, "Border color", color);
        }
        EditingTaskAction::Shadow {
            offset_x,
            offset_y,
            blur_radius,
            color,
        } => {
            ui.weak("Legacy clipped shadow: preserves this preset's original box-blur behavior.");
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::DragValue::new(offset_x)
                        .range(-16_384..=16_384)
                        .prefix("X "),
                );
                ui.add(
                    egui::DragValue::new(offset_y)
                        .range(-16_384..=16_384)
                        .prefix("Y "),
                );
                ui.add(
                    egui::DragValue::new(blur_radius)
                        .range(0..=256)
                        .prefix("Blur ")
                        .suffix(" px"),
                );
            });
            color_input(ui, "Shadow color", color);
        }
        EditingTaskAction::Annotation { request } => {
            crate::annotation_tools::show_annotation_options(ui, request);
        }
    }
}

fn color_input(ui: &mut egui::Ui, label: &str, color: &mut Rgba) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut bytes = [color.red, color.green, color.blue, color.alpha];
        if ui
            .color_edit_button_srgba_unmultiplied(&mut bytes)
            .changed()
        {
            *color = Rgba {
                red: bytes[0],
                green: bytes[1],
                blue: bytes[2],
                alpha: bytes[3],
            };
        }
    });
}

fn task_label(kind: usize) -> &'static str {
    match kind {
        0 => "Frame delay",
        1 => "Progress",
        2 => "Mouse clicks",
        3 => "Key strokes",
        4 => "Border",
        5 => "Shadow",
        _ => "Recorded cursor",
    }
}

fn new_task(kind: usize, canvas: Option<gif_from_screen_domain::PhysicalSize>) -> EditingTask {
    let action = match kind {
        0 => EditingTaskAction::Delay {
            mode: TaskDelay::Override { milliseconds: 100 },
        },
        4 => EditingTaskAction::ImageBorder {
            style: gif_from_screen_domain::ImageBorderStyle::default(),
        },
        5 => EditingTaskAction::ImageShadow {
            style: gif_from_screen_domain::ImageShadowStyle::default(),
        },
        _ => {
            let mut request = AnnotationRequest::default();
            if let Some(canvas) = canvas {
                request.size = gif_from_screen_domain::PhysicalSize::new(
                    canvas.width.get().min(request.size.width.get()),
                    canvas.height.get().min(request.size.height.get()),
                )
                .expect("a project canvas and default annotation have nonzero dimensions");
                request.font_size_px = u16::try_from(request.size.height.get())
                    .unwrap_or(u16::MAX)
                    .min(request.font_size_px);
            }
            request.mode = match kind {
                2 => AnnotationMode::RecordedClicks,
                3 => AnnotationMode::RecordedKeys,
                6 => AnnotationMode::RecordedCursor,
                _ => request.mode,
            };
            EditingTaskAction::Annotation { request }
        }
    };
    EditingTask {
        name: task_label(kind).to_owned(),
        enabled: true,
        action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::PhysicalSize;

    #[test]
    fn new_border_and_shadow_presets_use_expanded_effects_with_unambiguous_units() {
        let EditingTaskAction::ImageBorder { style } = new_task(4, None).action else {
            panic!("new border")
        };
        assert_eq!(style.widths.left_milli, 1000);
        assert_eq!(style.background.alpha, 255);
        let EditingTaskAction::ImageShadow { style } = new_task(5, None).action else {
            panic!("new shadow")
        };
        assert_eq!(style.opacity_basis_points, 6000);
        style.validate().unwrap();
    }

    #[test]
    fn initial_annotation_options_fit_the_open_canvas_without_mutating_saved_parameters() {
        for size in [
            PhysicalSize::new(1, 1).unwrap(),
            PhysicalSize::new(100, 25).unwrap(),
            PhysicalSize::new(640, 480).unwrap(),
        ] {
            for kind in [1, 2, 3, 6] {
                let task = new_task(kind, Some(size));
                let EditingTaskAction::Annotation { request } = task.action else {
                    panic!("annotation task expected");
                };
                request.validate(size).unwrap();
                assert!(request.size.width.get() <= size.width.get());
                assert!(request.size.height.get() <= size.height.get());
                assert!(u32::from(request.font_size_px) <= size.height.get());
            }
        }
        let task = new_task(1, None);
        let EditingTaskAction::Annotation { request } = task.action else {
            panic!("annotation task expected");
        };
        assert_eq!(request.size, AnnotationRequest::default().size);
    }
}
