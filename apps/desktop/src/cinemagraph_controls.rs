//! Cinemagraph authoring controls; input and geometry previews stay independent.

use super::{EditorWorkspace, Mode, MotionTools, egui};
use crate::cinemagraph_draft::CinemagraphTool;
use gif_from_screen_domain::FrameId;
use gif_from_screen_render::{InkAttributes, InkTip};

impl MotionTools {
    pub(crate) fn reconcile_cinemagraph(&mut self, workspace: &EditorWorkspace) {
        self.cine.reconcile(workspace);
        if self.mode != Mode::Cinemagraph || self.cine.is_stale() {
            self.cine.cancel_gesture();
            self.cine_preview.cancel();
        }
    }

    pub(crate) fn cinemagraph_editing(&self) -> bool {
        self.mode == Mode::Cinemagraph && self.cine.is_active()
    }

    pub(crate) fn cinemagraph_reference(&self) -> Option<FrameId> {
        (self.cinemagraph_editing() && !self.cine.is_stale())
            .then(|| self.cine.reference_frame())
            .flatten()
    }

    pub(crate) fn show_cinemagraph_preview(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        rendered_size: [u32; 2],
        enabled: bool,
    ) {
        self.cine_preview
            .show(ui, response, &mut self.cine, rendered_size, enabled);
    }

    pub(super) fn show_cinemagraph_controls(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &EditorWorkspace,
    ) {
        self.reconcile_cinemagraph(workspace);
        ui.label("Paint the motion area on the first frame. Its unpainted area is composited over the selected frames.");
        ui.weak("Transparent reference pixels do not erase the animation. Pen color is only a guide; geometry determines the region.");
        if !self.cine.is_active() {
            if ui.button("Draw motion region").clicked() {
                self.notice = self.cine.begin(workspace).err();
            }
            return;
        }
        if let Some(reason) = self.cine.stale_reason() {
            ui.colored_label(ui.visuals().warn_fg_color, reason);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Restart with current targets").clicked() {
                    self.notice = self.cine.begin(workspace).err();
                }
                if ui.button("Close draft").clicked() {
                    self.close_cinemagraph();
                }
            });
            return;
        }
        ui.label(format!(
            "Reference: frame 1 · Apply to {} selected frame(s) · {} stroke(s)",
            workspace.selection().len(),
            self.cine.strokes().len()
        ));
        let old_tool = self.cine.tool;
        ui.horizontal_wrapped(|ui| {
            for (tool, label) in [
                (CinemagraphTool::Pen, "Pen"),
                (CinemagraphTool::PointEraser, "Erase part"),
                (CinemagraphTool::StrokeEraser, "Erase stroke"),
                (CinemagraphTool::Select, "Select"),
            ] {
                ui.selectable_value(&mut self.cine.tool, tool, label);
            }
        });
        if old_tool != self.cine.tool {
            self.cine.cancel_gesture();
        }
        egui::ScrollArea::vertical().id_salt("cinemagraph-tip-controls")
            .max_height(150.0).auto_shrink([false,true]).show(ui, |ui| {
                match self.cine.tool {
                    CinemagraphTool::Pen => {
                        tip_controls(ui, &mut self.cine.pen);
                        ui.checkbox(&mut self.cine.pen.fit_to_curve, "Fit new strokes to curves");
                        ui.weak("Pen settings apply to new strokes. Click to place a dot; drag to paint.");
                    }
                    CinemagraphTool::PointEraser | CinemagraphTool::StrokeEraser => {
                        tip_controls(ui, &mut self.cine.eraser);
                        ui.weak("Drag across strokes. Partial erasing splits ink; stroke erasing removes a complete hit stroke.");
                        ui.weak("Elliptical eraser boundaries currently use a bounded polygon approximation; exact WPF cut fidelity remains under verification.");
                    }
                    CinemagraphTool::Select => {
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Select all strokes").clicked() { self.notice = self.cine.select_all().err(); }
                            if ui.add_enabled(!self.cine.selected_ids().is_empty(), egui::Button::new("Delete selected strokes")).clicked() {
                                self.notice = self.cine.delete_selected().err();
                            }
                        });
                        ui.weak("Click or drag to select. Drag the selection to move it; use its corner handle to resize point coordinates. Pen width and pressure stay unchanged.");
                    }
                }
            });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.cine.strokes().is_empty() && !self.cine.gesture_active(),
                    egui::Button::new("Apply Cinemagraph"),
                )
                .clicked()
                && let Err(error) = self.queue(workspace)
            {
                self.notice = Some(error);
            }
            if ui.button("Clear strokes").clicked() {
                self.notice = self.cine.clear().err();
            }
            if ui.button("Close draft").clicked() {
                self.close_cinemagraph();
            }
        });
        ui.weak("Escape cancels the current gesture. Final clipping and saving run in the background; one project Undo restores the edit.");
    }

    fn close_cinemagraph(&mut self) {
        self.cine.close();
        self.cine_preview.cancel();
    }
}

fn tip_controls(ui: &mut egui::Ui, attributes: &mut InkAttributes) {
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::DragValue::new(&mut attributes.width)
                .prefix("Width ")
                .range(1.0..=100.0)
                .fixed_decimals(0),
        );
        ui.add(
            egui::DragValue::new(&mut attributes.height)
                .prefix("Height ")
                .range(1.0..=100.0)
                .fixed_decimals(0),
        );
        ui.selectable_value(&mut attributes.tip, InkTip::Ellipse, "Ellipse tip");
        ui.selectable_value(&mut attributes.tip, InkTip::Rectangle, "Rectangle tip");
    });
}

#[cfg(test)]
#[path = "cinemagraph_controls/tests.rs"]
mod tests;
