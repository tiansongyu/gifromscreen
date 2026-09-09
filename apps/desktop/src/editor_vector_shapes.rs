//! Atomic, bounded vector group authoring. Drafts do not own a mutable project.

use gif_from_screen_domain::{
    BlendMode, EditCommand, FrameId, FrameOverlayMark, MAX_FRAME_OVERLAY_CELLS,
    MAX_FRAME_OVERLAY_MARKS, OverlayContent, OverlayId, OverlayTrack, PhysicalSize, TrackId,
    VectorShape, selected_frame_cells,
};
use gif_from_screen_editor::{MAX_FRAME_BUNDLE_METADATA_BYTES, author_vector_shape_track};
use gif_from_screen_render::MAX_VECTOR_PREVIEW_SHAPES;
use uuid::Uuid;

use super::{EditorWorkspace, EditorWorkspaceError, OverlaySelectionAnchor};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VectorShapeRequest {
    pub(crate) anchor: OverlaySelectionAnchor,
    pub(crate) reference_frame: FrameId,
    pub(crate) canvas_size: PhysicalSize,
    pub(crate) shapes: Vec<VectorShape>,
}

impl EditorWorkspace {
    /// Prepare every mark and independent paint stage before a single journal edit.
    /// Shape order is the explicit draft paint order; selection gaps stay gaps.
    pub(crate) fn apply_vector_shapes(
        &mut self,
        request: &VectorShapeRequest,
    ) -> Result<TrackId, EditorWorkspaceError> {
        self.validate_vector_request(request)?;
        let contents: Vec<_> = request
            .shapes
            .iter()
            .map(|shape| OverlayContent::VectorShape { shape: *shape })
            .collect();
        validate_group_metadata(&contents, self.selection().len())?;
        // This helper checks the complete selection before invoking its factory,
        // and preserves noncontiguous ownership as distinct authoring runs.
        let mut cells = selected_frame_cells(
            &self.manifest().timeline,
            self.selection().selected(),
            |_| mark(contents[0].clone(), 0),
        )
        .map_err(preparation)?;
        for cell in &mut cells {
            cell.marks.extend(
                contents
                    .iter()
                    .enumerate()
                    .skip(1)
                    .map(|(index, content)| mark(content.clone(), index)),
            );
        }
        let id = TrackId::from_u128(Uuid::new_v4().as_u128());
        let track = OverlayTrack {
            id,
            name: "Shapes".to_owned(),
            visible: true,
            opacity: 255,
            blend_mode: BlendMode::Normal,
            annotation: None,
            annotation_scope: None,
            frame_cells: Some(cells),
            items: Vec::new(),
        };
        let commands = author_vector_shape_track(self.manifest(), track)?;
        self.execute(EditCommand::Compound { commands })?;
        Ok(id)
    }

    fn validate_vector_request(
        &self,
        request: &VectorShapeRequest,
    ) -> Result<(), EditorWorkspaceError> {
        if !request.anchor.matches(self) {
            return Err(preparation(
                "The shape draft's project, revision or target selection changed.",
            ));
        }
        if request.shapes.is_empty() || request.shapes.len() > MAX_VECTOR_PREVIEW_SHAPES {
            return Err(preparation(format!(
                "Apply between 1 and {MAX_VECTOR_PREVIEW_SHAPES} shapes; no shapes were truncated."
            )));
        }
        let selected = self.selected_frame_ids()?;
        if selected.len() > MAX_FRAME_OVERLAY_CELLS
            || selected
                .len()
                .checked_mul(request.shapes.len())
                .is_none_or(|marks| marks > MAX_FRAME_OVERLAY_MARKS)
        {
            return Err(preparation(
                "The complete shape group exceeds the frame or mark authoring limit.",
            ));
        }
        let reference = self
            .manifest()
            .timeline
            .frames
            .iter()
            .find(|frame| {
                frame.id == request.reference_frame && self.selection().contains(frame.id)
            })
            .ok_or_else(|| {
                preparation("The shape reference frame is no longer a selected target.")
            })?;
        let actual =
            crate::annotation_engine::authoring_stage_size(self.manifest(), reference, None)
                .map_err(preparation)?;
        if actual != request.canvas_size {
            return Err(preparation(
                "The shape reference paint-space dimensions changed.",
            ));
        }
        for shape in &request.shapes {
            shape.validate().map_err(preparation)?;
        }
        Ok(())
    }
}

fn mark(content: OverlayContent, index: usize) -> FrameOverlayMark {
    FrameOverlayMark {
        id: OverlayId::from_u128(Uuid::new_v4().as_u128()),
        z_index: i32::try_from(index).expect("validated bounded vector group"),
        content,
    }
}

fn validate_group_metadata(
    contents: &[OverlayContent],
    frames: usize,
) -> Result<(), EditorWorkspaceError> {
    let content_bytes = super::overlay_authoring::json_metadata_bytes(&contents)?;
    // Include every repeated value before cloning cells. Reserve explicit mark
    // IDs/order/content wrappers and whole-frame owner/scope metadata. The shared
    // author then measures the actual full Compound including previous frame data.
    let required = contents
        .len()
        .checked_mul(128)
        .and_then(|overhead| content_bytes.checked_add(overhead))
        .and_then(|per_frame| per_frame.checked_add(512))
        .and_then(|per_frame| per_frame.checked_mul(frames));
    if required.is_none_or(|bytes| bytes > MAX_FRAME_BUNDLE_METADATA_BYTES) {
        return Err(preparation(
            "The complete shape authoring command exceeds the 16 MiB metadata budget.",
        ));
    }
    Ok(())
}

fn preparation(reason: impl Into<String>) -> EditorWorkspaceError {
    EditorWorkspaceError::FrameOverlayPreparation(reason.into())
}
