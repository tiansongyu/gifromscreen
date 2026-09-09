use eframe::egui;
use gif_from_screen_domain::{Rgba, VectorShapeKind};
use gif_from_screen_localization::{Localizer, Message};

use super::{Intent, VectorShapes, draft::ShapeTool};
use crate::editor_workspace::EditorWorkspace;

pub(super) fn show(
    tool: &mut VectorShapes,
    ui: &mut egui::Ui,
    workspace: &EditorWorkspace,
    localizer: Localizer,
) -> Intent {
    tool.reconcile(workspace);
    tool.poll(ui.ctx());
    if !tool.is_active() {
        return Intent::None;
    }
    let mut intent = Intent::None;
    let mut apply = false;
    ui.push_id("vector-shape-controls", |ui| {
        ui.heading(localizer.text(Message::VectorShapesTitle));
        ui.label(localizer.text(Message::VectorHelp));
        ui.label(crate::format_message(
            localizer,
            Message::VectorCount,
            &[
                ("count", &tool.draft.objects.len().to_string()),
                ("selected", &tool.draft.selected.len().to_string()),
            ],
        ));
        if tool.is_stale() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                localizer.text(Message::VectorStale),
            );
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button(localizer.text(Message::VectorRestart)).clicked() {
                intent = Intent::Restart;
            }
            if ui.button(localizer.text(Message::VectorClose)).clicked() {
                intent = Intent::Close;
            }
            if ui
                .add_enabled(
                    !tool.is_stale()
                        && !tool.gesture_active()
                        && !tool.draft.objects.is_empty()
                        && tool.preview.current(tool.draft.generation),
                    egui::Button::new(localizer.text(Message::VectorApply)),
                )
                .clicked()
            {
                apply = true;
            }
        });
        ui.add_enabled_ui(!tool.is_stale(), |ui| {
            modes(tool, ui, localizer);
            ui.add_enabled_ui(!tool.gesture_active(), |ui| {
                styles(tool, ui, localizer);
                object_actions(tool, ui, localizer);
            });
        });
        preview_status(tool, ui, localizer);
    });
    // Resolve only after this pass's style editors. A late numeric edit cannot
    // apply the old, previously verified generation from above the form.
    if apply && matches!(intent, Intent::None) {
        match tool.draft.request(workspace) {
            Ok(request) if tool.preview.current(tool.draft.generation) => {
                intent = Intent::Apply(Box::new(request));
            }
            Ok(_) => tool.notice = Some(Message::VectorPreviewPending.into()),
            Err(error) => tool.notice = Some(error),
        }
    }
    intent
}

fn object_actions(tool: &mut VectorShapes, ui: &mut egui::Ui, localizer: Localizer) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(localizer.text(Message::VectorSelectAll))
            .clicked()
        {
            tool.notice = tool.draft.select_all().err();
            tool.draft.tool = ShapeTool::Select;
        }
        if ui
            .add_enabled(
                !tool.draft.selected.is_empty(),
                egui::Button::new(localizer.text(Message::VectorDeleteSelected)),
            )
            .clicked()
        {
            tool.notice = tool.draft.delete_selected().err();
        }
        if ui
            .add_enabled(
                !tool.draft.objects.is_empty(),
                egui::Button::new(localizer.text(Message::VectorClear)),
            )
            .clicked()
        {
            tool.notice = tool.draft.clear().err();
        }
        if ui
            .add_enabled(
                !tool.draft.selected.is_empty(),
                egui::Button::new(localizer.text(Message::VectorResetRotation)),
            )
            .clicked()
        {
            tool.notice = tool.draft.set_rotation(0).err();
        }
    });
}

fn preview_status(tool: &mut VectorShapes, ui: &mut egui::Ui, localizer: Localizer) {
    if let Some(notice) = &tool.notice {
        ui.colored_label(ui.visuals().error_fg_color, notice.render(localizer));
    }
    if let Some(notice) = &tool.preview.failure {
        ui.colored_label(ui.visuals().error_fg_color, notice.render(localizer));
        if ui.button(localizer.text(Message::SettingsRetry)).clicked() {
            tool.preview.retry();
        }
    } else if !tool.draft.objects.is_empty()
        && !tool.preview.current(tool.draft.generation)
        && !tool.is_stale()
    {
        ui.weak(localizer.text(Message::VectorPreviewPending));
    }
}

fn modes(tool: &mut VectorShapes, ui: &mut egui::Ui, localizer: Localizer) {
    let before = (tool.draft.tool, tool.draft.kind);
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(
            &mut tool.draft.tool,
            ShapeTool::Insert,
            localizer.text(Message::VectorInsertMode),
        );
        ui.selectable_value(
            &mut tool.draft.tool,
            ShapeTool::Select,
            localizer.text(Message::VectorSelectMode),
        );
        egui::ComboBox::from_id_salt("vector-shape-kind")
            .selected_text(localizer.text(kind_label(tool.draft.kind)))
            .show_ui(ui, |ui| {
                for kind in [
                    VectorShapeKind::Rectangle,
                    VectorShapeKind::Ellipse,
                    VectorShapeKind::Triangle,
                    VectorShapeKind::BlockArrow,
                ] {
                    ui.selectable_value(
                        &mut tool.draft.kind,
                        kind,
                        localizer.text(kind_label(kind)),
                    );
                }
            });
    });
    if before != (tool.draft.tool, tool.draft.kind) {
        tool.invalidate(ui.ctx());
    }
}

fn styles(tool: &mut VectorShapes, ui: &mut egui::Ui, localizer: Localizer) {
    let mut style = tool.draft.style;
    let mut rotation = tool
        .draft
        .primary()
        .map_or(0, |object| u32::from(object.shape.rotation_hundredths));
    let previous_rotation = rotation;
    ui.horizontal_wrapped(|ui| {
        number(
            ui,
            "stroke",
            Message::EditorStrokeWidth,
            &mut style.stroke_width_hundredths,
            100.0,
            localizer,
        );
        number(
            ui,
            "radius",
            Message::VectorCornerRadius,
            &mut style.corner_radius_hundredths,
            100.0,
            localizer,
        );
        ui.add_enabled_ui(!tool.draft.selected.is_empty(), |ui| {
            number(
                ui,
                "rotation",
                Message::VectorRotation,
                &mut rotation,
                359.99,
                localizer,
            );
        });
    });
    ui.horizontal_wrapped(|ui| {
        color(ui, Message::EditorStrokeRgba, &mut style.stroke, localizer);
        let mut fill = style.fill.is_some();
        if ui
            .checkbox(&mut fill, localizer.text(Message::EditorFill))
            .changed()
        {
            style.fill = fill.then_some(Rgba::TRANSPARENT);
        }
        if let Some(fill) = &mut style.fill {
            color(ui, Message::EditorFillRgba, fill, localizer);
        }
    });
    if style != tool.draft.style {
        tool.notice = tool.draft.set_style(style).err();
    }
    if rotation != previous_rotation
        && let Ok(rotation) = u16::try_from(rotation)
    {
        tool.notice = tool.draft.set_rotation(rotation).err();
    }
}

fn number(
    ui: &mut egui::Ui,
    id: &str,
    label: Message,
    value: &mut u32,
    maximum: f64,
    localizer: Localizer,
) {
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            ui.label(localizer.text(label));
            let mut displayed = f64::from(*value) / 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut displayed)
                        .range(0.0..=maximum)
                        .speed(0.25)
                        .min_decimals(2)
                        .max_decimals(2),
                )
                .changed()
                && displayed.is_finite()
            {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    *value = (displayed.clamp(0.0, maximum) * 100.0).round() as u32;
                }
            }
        })
    });
}

fn color(ui: &mut egui::Ui, label: Message, value: &mut Rgba, localizer: Localizer) {
    ui.horizontal(|ui| {
        ui.label(localizer.text(label));
        let mut bytes = [value.red, value.green, value.blue, value.alpha];
        if ui
            .color_edit_button_srgba_unmultiplied(&mut bytes)
            .changed()
        {
            *value = Rgba {
                red: bytes[0],
                green: bytes[1],
                blue: bytes[2],
                alpha: bytes[3],
            };
        }
    });
}

fn kind_label(kind: VectorShapeKind) -> Message {
    match kind {
        VectorShapeKind::Rectangle => Message::EditorShapeRectangle,
        VectorShapeKind::Ellipse => Message::EditorShapeEllipse,
        VectorShapeKind::Triangle => Message::VectorTriangle,
        VectorShapeKind::BlockArrow => Message::VectorBlockArrow,
    }
}
