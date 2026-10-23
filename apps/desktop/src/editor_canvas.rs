//! Display-only editor zoom/pan and an independent, explicitly applied crop draft.

use eframe::egui;
use gif_from_screen_localization::{Localizer, Message};

use crate::{editor_preview::EditorPreview, editor_workspace::EditorWorkspace, ui_notice::Notice};

#[path = "editor_crop.rs"]
mod crop;
pub(crate) use crop::DirectCropDraft;

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
    height_limit: Option<f32>,
}

impl EditorCanvasState {
    pub(crate) fn limit_height(&mut self, height: Option<f32>) {
        self.height_limit = height;
    }
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
                    ui.selectable_value(&mut self.zoom, zoom, localizer.text(message))
                        .on_hover_text(localizer.text(match zoom {
                            PreviewZoom::Fit => Message::PreviewFitHint,
                            PreviewZoom::Native | PreviewZoom::Double => {
                                Message::PreviewExactPixelHint
                            }
                        }));
                }
            });
        });
        if before != self.zoom {
            self.scroll_offset = egui::Vec2::ZERO;
            self.panning = false;
            self.crop.cancel_gesture();
        }
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
        self.update_pan(ui);
        let output = preview_viewport(
            ui,
            preview.rendered_size,
            self.zoom,
            self.scroll_offset,
            self.height_limit,
            egui::scroll_area::ScrollSource {
                drag: false,
                ..egui::scroll_area::ScrollSource::ALL
            },
            |ui, size| {
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
            },
        )?;
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

    /// Keep downstream controls in place while no current pixels are ready.
    /// The child is clipped and scrollable so long errors cannot grow the page.
    pub(crate) fn show_placeholder<R>(
        &mut self,
        ui: &mut egui::Ui,
        rendered_size: [u32; 2],
        contents: impl FnOnce(&mut egui::Ui, egui::Rect) -> R,
    ) -> Result<R, Notice> {
        self.cancel_layout_gestures();
        self.viewport = None;
        // Allocate the same viewport and natural scroll content as the real
        // image, including centering and scrollbar geometry.
        let output = preview_viewport(
            ui,
            rendered_size,
            self.zoom,
            self.scroll_offset,
            self.height_limit,
            egui::scroll_area::ScrollSource::NONE,
            |ui, size| ui.allocate_exact_size(size, egui::Sense::hover()).0,
        )?;
        let rect = output.inner_rect;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("editor-preview-placeholder")
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        Ok(egui::ScrollArea::vertical()
            .id_salt("editor-preview-placeholder-scroll")
            .max_height(rect.height())
            .auto_shrink([false, false])
            .show(&mut child, |ui| contents(ui, rect))
            .inner)
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

fn preview_viewport<R>(
    ui: &mut egui::Ui,
    rendered_size: [u32; 2],
    zoom: PreviewZoom,
    scroll_offset: egui::Vec2,
    height_limit: Option<f32>,
    scroll_source: egui::scroll_area::ScrollSource,
    contents: impl FnOnce(&mut egui::Ui, egui::Vec2) -> R,
) -> Result<egui::scroll_area::ScrollAreaOutput<R>, Notice> {
    let available = preview_viewport_size(ui, height_limit);
    let size = zoom.extent(rendered_size, available, ui.ctx().pixels_per_point())?;
    ui.painter().rect_filled(
        egui::Rect::from_min_size(ui.cursor().min, available),
        0,
        crate::appearance::Palette::of(ui).surface,
    );
    Ok(egui::ScrollArea::both()
        .id_salt("editor-image-viewport")
        .max_height(available.y)
        .auto_shrink([false, false])
        .scroll_source(scroll_source)
        .scroll_offset(scroll_offset)
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(available.x.max(size.x), available.y.max(size.y)),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.add_space(((available.y - size.y) * 0.5).max(0.0));
                    contents(ui, size)
                },
            )
            .inner
        }))
}

fn preview_viewport_size(ui: &egui::Ui, height_limit: Option<f32>) -> egui::Vec2 {
    let available = crate::editor_shell_layout::available_work_height(ui);
    let height = height_limit.map_or(available, |limit| available.min(limit.max(100.0)));
    egui::vec2(ui.available_width().max(1.0), height)
}

#[cfg(test)]
#[path = "editor_canvas_tests.rs"]
mod tests;
