//! An independent, bounded object draft. Only the root application commits it.

use eframe::egui;
use gif_from_screen_domain::FrameId;
use gif_from_screen_localization::Localizer;

pub(crate) use crate::editor_workspace::VectorShapeRequest;
use crate::{editor_workspace::EditorWorkspace, ui_notice::Notice};

mod boundary;
mod controls;
mod draft;
mod geometry;
mod input;
mod paint;
mod preview;

use draft::Draft;
use draft::DraftObject;

pub(crate) const MAX_VECTOR_DRAFT_OBJECTS: usize =
    gif_from_screen_render::MAX_VECTOR_PREVIEW_SHAPES;

#[derive(Debug, Default)]
pub(crate) enum Intent {
    #[default]
    None,
    Apply(Box<VectorShapeRequest>),
    Restart,
    Close,
}

#[derive(Default)]
pub(crate) struct VectorShapes {
    draft: Draft,
    boundary: boundary::InputBoundary,
    input: input::PreviewInput,
    geometry: geometry::GeometryCache,
    preview: preview::RasterPreview,
    notice: Option<Notice>,
}

impl VectorShapes {
    pub(crate) fn begin(
        &mut self,
        workspace: &EditorWorkspace,
        frame: FrameId,
        rendered: [u32; 2],
    ) -> Result<(), Notice> {
        if self.is_active() {
            return Err(gif_from_screen_localization::Message::VectorRestartRequired.into());
        }
        self.start(workspace, frame, rendered)
    }

    /// A separate, explicitly requested operation; reconciliation never calls it.
    pub(crate) fn restart(
        &mut self,
        workspace: &EditorWorkspace,
        frame: FrameId,
        rendered: [u32; 2],
    ) -> Result<(), Notice> {
        self.start(workspace, frame, rendered)
    }

    fn start(
        &mut self,
        workspace: &EditorWorkspace,
        frame: FrameId,
        rendered: [u32; 2],
    ) -> Result<(), Notice> {
        self.draft.begin(workspace, frame, rendered)?;
        self.input = input::PreviewInput::default();
        self.preview.invalidate();
        self.geometry.clear();
        self.notice = None;
        Ok(())
    }

    pub(crate) fn reconcile(&mut self, workspace: &EditorWorkspace) {
        self.draft.reconcile(workspace);
        if self.is_stale() {
            self.preview.invalidate();
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.draft.is_active()
    }
    pub(crate) fn is_stale(&self) -> bool {
        self.draft.stale
    }
    pub(crate) fn gesture_active(&self) -> bool {
        self.draft.gesture.is_some()
    }
    pub(crate) fn reference_frame(&self) -> Option<FrameId> {
        self.draft.reference_frame()
    }
    #[cfg(test)]
    pub(crate) fn objects(&self) -> &[DraftObject] {
        &self.draft.objects
    }
    pub(crate) fn close(&mut self) {
        self.draft.close();
        self.preview.invalidate();
        self.geometry.clear();
        self.input = input::PreviewInput::default();
        self.notice = None;
    }

    pub(crate) fn shutdown(&mut self) {
        self.close();
    }
    pub(crate) fn has_pending_work(&self) -> bool {
        self.preview.is_running()
    }
    pub(crate) fn poll(&mut self, context: &egui::Context) {
        self.preview.poll(context);
    }
    pub(crate) fn validate_apply(
        &self,
        workspace: &EditorWorkspace,
        request: &VectorShapeRequest,
    ) -> Result<(), Notice> {
        let current = self.draft.request(workspace)?;
        if &current != request || !self.preview.current(self.draft.generation) {
            return Err(gif_from_screen_localization::Message::VectorPreviewPending.into());
        }
        Ok(())
    }
    pub(crate) fn invalidate(&mut self, context: &egui::Context) {
        self.input.invalidate(context, &mut self.draft);
        self.preview.invalidate();
    }
    pub(crate) fn filter_raw_input(
        &mut self,
        context: &egui::Context,
        input: &mut egui::RawInput,
        enabled: bool,
    ) {
        self.boundary
            .filter(context, input, enabled, &mut self.input, &mut self.draft);
    }
    pub(crate) fn show_controls(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &EditorWorkspace,
        localizer: Localizer,
    ) -> Intent {
        controls::show(self, ui, workspace, localizer)
    }

    /// Response must describe the actual painted frame, not a justified larger allocation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show_preview(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        rendered: [u32; 2],
        output_size: [u32; 2],
        enabled: bool,
        localizer: Localizer,
    ) {
        self.poll(ui.ctx());
        if !self.is_active() {
            return;
        }
        let mapping = match self.input.update(
            ui,
            response,
            rendered,
            enabled,
            &mut self.draft,
            &mut self.geometry,
        ) {
            Ok(mapping) => mapping,
            Err(error) => {
                self.notice = Some(error);
                None
            }
        };
        let Some(mapping) = mapping else {
            self.preview.invalidate();
            return;
        };
        if self.is_stale() || self.draft.canvas() != Some(rendered) {
            self.preview.invalidate();
            return;
        }
        if let Err(error) = self.geometry.ensure(&self.draft) {
            self.preview.invalidate();
            self.notice = Some(error);
            return;
        }
        self.preview.request(ui.ctx(), &self.draft, output_size);
        let painter = ui.painter().with_clip_rect(mapping.visible);
        self.preview
            .paint(&painter, mapping.painted, self.draft.generation);
        paint::guides(&painter, mapping, &self.draft, &self.geometry);
        if !self.preview.current(self.draft.generation)
            && self.preview.failure.is_none()
            && !self.draft.objects.is_empty()
        {
            painter.text(
                mapping.visible.left_top() + egui::vec2(6.0, 6.0),
                egui::Align2::LEFT_TOP,
                localizer.text(gif_from_screen_localization::Message::VectorPreviewPending),
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            );
        }
    }
}

fn error(reason: impl std::fmt::Display) -> Notice {
    Notice::new(
        gif_from_screen_localization::Message::VectorOperationFailed,
        &[("error", &reason.to_string())],
    )
}

#[cfg(test)]
mod tests;
