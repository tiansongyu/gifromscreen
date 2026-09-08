//! Display-only editor zoom/pan and an independent, explicitly applied crop draft.

use eframe::egui;
use gif_from_screen_localization::{Localizer, Message};

use crate::{editor_preview::EditorPreview, editor_workspace::EditorWorkspace, ui_notice::Notice};

#[path = "editor_crop.rs"]
mod crop;
pub(crate) use crop::DirectCropDraft;

const CANVAS_HEIGHT: f32 = 360.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PreviewZoom {
    #[default]
    Fit,
    Native,
    Double,
}

impl PreviewZoom {
    /// Input dimensions are the current composed image, not a downsampled texture.
    pub(crate) fn extent(
        self,
        rendered: [u32; 2],
        available: egui::Vec2,
        ppp: f32,
    ) -> Result<egui::Vec2, Notice> {
        if rendered.contains(&0)
            || !available.is_finite()
            || available.min_elem() <= 0.0
            || !ppp.is_finite()
            || ppp <= 0.0
        {
            return Err(Message::PreviewInvalidGeometry.into());
        }
        let width = f64::from(rendered[0]);
        let height = f64::from(rendered[1]);
        let scale = match self {
            Self::Fit => (f64::from(available.x) / width).min(f64::from(available.y) / height),
            Self::Native => 1.0 / f64::from(ppp),
            Self::Double => 2.0 / f64::from(ppp),
        };
        checked_extent(width * scale, height * scale)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn checked_extent(width: f64, height: f64) -> Result<egui::Vec2, Notice> {
    let size = egui::vec2(width as f32, height as f32);
    if !size.is_finite() || size.min_elem() <= 0.0 {
        return Err(Message::PreviewUnrepresentableExtent.into());
    }
    Ok(size)
}

#[derive(Debug, Default)]
pub(crate) struct EditorCanvasState {
    pub(crate) zoom: PreviewZoom,
    pub(crate) crop: DirectCropDraft,
    scroll_offset: egui::Vec2,
    viewport: Option<egui::Rect>,
    panning: bool,
}

impl EditorCanvasState {
    pub(crate) fn reconcile(&mut self, workspace: &EditorWorkspace) {
        self.crop.reconcile(workspace);
    }

    /// A UI-layout change must not reinterpret an in-flight pointer sequence.
    /// Preserve the display scale, scroll position and already-confirmed crop.
    pub(crate) fn cancel_layout_gestures(&mut self) {
        self.panning = false;
        self.crop.cancel_layout_gesture();
    }

    pub(crate) fn show_zoom(&mut self, ui: &mut egui::Ui, localizer: Localizer) {
        let before = self.zoom;
        ui.push_id("editor-preview-zoom", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (zoom, message) in [
                    (PreviewZoom::Fit, Message::PreviewZoomFit),
                    (PreviewZoom::Native, Message::PreviewZoomNative),
                    (PreviewZoom::Double, Message::PreviewZoomDouble),
                ] {
                    ui.selectable_value(&mut self.zoom, zoom, localizer.text(message));
                }
            });
        });
        if before != self.zoom {
            self.scroll_offset = egui::Vec2::ZERO;
            self.panning = false;
            self.crop.cancel_gesture();
        }
        ui.small(localizer.text(match self.zoom {
            PreviewZoom::Fit => Message::PreviewFitHint,
            PreviewZoom::Native | PreviewZoom::Double => Message::PreviewExactPixelHint,
        }));
    }

    /// Calls every overlay painter within the same clipped scrolling viewport.
    /// The Response is always the actual painted image, even in justified columns.
    pub(crate) fn show_image(
        &mut self,
        ui: &mut egui::Ui,
        preview: &EditorPreview,
        sense: egui::Sense,
        editable: bool,
        overlay: impl FnOnce(&mut egui::Ui, &egui::Response),
    ) -> Result<egui::Response, Notice> {
        let available = egui::vec2(ui.available_width().max(1.0), CANVAS_HEIGHT);
        let size = self.zoom.extent(
            preview.rendered_size,
            available,
            ui.ctx().pixels_per_point(),
        )?;
        self.update_pan(ui);
        let output = egui::ScrollArea::both()
            .id_salt("editor-image-viewport")
            .max_height(CANVAS_HEIGHT)
            .auto_shrink([false, true])
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: false,
                ..egui::scroll_area::ScrollSource::ALL
            })
            .scroll_offset(self.scroll_offset)
            .show(ui, |ui| {
                let response = crate::show_editor_preview_image(ui, &preview.texture, size, sense);
                let crop_input = editable
                    && !self.panning
                    && !ui.input(|input| input.pointer.button_down(egui::PointerButton::Middle));
                self.crop
                    .interact(ui, &response, preview.rendered_size, crop_input);
                self.crop
                    .paint(ui.painter(), response.rect, preview.rendered_size);
                overlay(ui, &response);
                response
            });
        self.scroll_offset = output.state.offset;
        self.viewport = Some(output.inner_rect);
        let (focused, pressed, position) = ui.input(|input| {
            (
                input.focused,
                input.pointer.button_pressed(egui::PointerButton::Middle),
                input.pointer.interact_pos(),
            )
        });
        let start_pan = focused
            && pressed
            && position.is_some_and(|point| {
                output.inner_rect.contains(point)
                    && ui.clip_rect().contains(point)
                    && ui.ctx().layer_id_at(point) == Some(ui.layer_id())
            });
        if start_pan {
            self.panning = true;
            self.crop.cancel_gesture();
        }
        Ok(output.inner)
    }

    fn update_pan(&mut self, ui: &egui::Ui) {
        let (focused, down, delta) = ui.input(|input| {
            (
                input.focused,
                input.pointer.button_down(egui::PointerButton::Middle),
                input.pointer.delta(),
            )
        });
        if !focused || !down {
            self.panning = false;
        }
        if self.panning && delta.is_finite() {
            self.scroll_offset = (self.scroll_offset - delta).max(egui::Vec2::ZERO);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }
}

#[cfg(test)]
#[path = "editor_canvas_tests.rs"]
mod tests;
