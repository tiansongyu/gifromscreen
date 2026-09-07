//! Edits to the currently composed pixels, in the order the user applies them.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};

use gif_from_screen_domain::{
    EditCommand, FrameClip, FrameGeometryPlan, FrameId, FrameRenderStep, PhysicalRect,
    PhysicalSize, ProjectManifest, QuarterTurn,
};

use crate::{
    EditorError, FrameEffectEdit, MAX_FRAME_BUNDLE_METADATA_BYTES, ensure_known_selection,
};

#[path = "composed_effect.rs"]
mod effects;
pub use effects::{ComposedEffectEdit, ComposedImageEffect, MAX_COMPOSED_IMAGE_BYTES};

/// Current-image operations. Unlike the legacy clip-transform property API, an
/// appended operation affects artwork already present, but not later artwork.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComposedFrameEdit {
    /// Crop the complete current image on every animation frame.
    Crop(PhysicalRect),
    /// Resize the complete current image on every animation frame.
    Resize(PhysicalSize),
    /// Rotate every animation frame relative to its current pixels.
    Rotate(QuarterTurn),
    /// Flip the current composed pixels of selected frames horizontally.
    FlipHorizontal,
    /// Flip the current composed pixels of selected frames vertically.
    FlipVertical,
    /// Remove the most recent crop, retaining later artwork's stage coordinates.
    ClearCrop,
    /// Remove the most recent resize, retaining later artwork's stage coordinates.
    ClearResize,
    /// Add, replace or remove effects on selected frames.
    Effect(FrameEffectEdit),
    /// Add, replace or clear current-image effects, including canvas expansion.
    ImageEffect(ComposedEffectEdit),
}

impl ComposedFrameEdit {
    fn changes_canvas(&self) -> bool {
        matches!(
            self,
            Self::Crop(_) | Self::Resize(_) | Self::Rotate(_) | Self::ClearCrop | Self::ClearResize
        )
    }

    fn appended_step(&self) -> Option<FrameRenderStep> {
        Some(match self {
            Self::Crop(rect) => FrameRenderStep::Crop { rect: *rect },
            Self::Resize(size) => FrameRenderStep::Resize { size: *size },
            Self::Rotate(rotation) => FrameRenderStep::Rotate {
                rotation: *rotation,
            },
            Self::FlipHorizontal => FrameRenderStep::FlipHorizontal,
            Self::FlipVertical => FrameRenderStep::FlipVertical,
            Self::Effect(FrameEffectEdit::Add(effect)) => FrameRenderStep::Effect {
                effect: effect.clone(),
            },
            Self::ImageEffect(ComposedEffectEdit::Add(effect)) => effect.step(),
            _ => return None,
        })
    }
}

/// Number of editable effects in chronological order (legacy prefix, then stages).
pub fn frame_effect_count(frame: &FrameClip) -> usize {
    frame.effects.len()
        + frame
            .render_steps
            .iter()
            .filter(|step| effects::is_effect(step))
            .count()
}

/// Build an atomic, pixel-I/O-free edit with stable compositing boundaries.
///
/// Crop, resize and rotation always address the complete animation and update
/// its canvas in the same undo step, as in `ScreenToGif`. Flip and effect edits use
/// the explicit selection. A boundary seals every existing tail cell, including
/// hidden and empty authoring cells. New artwork remains at the tail. Legacy
/// timed layers are sampled at the first boundary, still using their original
/// time and z-order semantics.
///
/// Removing an earlier crop/resize keeps later layers in their authored stage
/// coordinates; it does not silently re-author them. If a later crop or effect
/// no longer fits, the entire edit is rejected. Undo is the exact inverse.
///
/// # Errors
/// Rejects stale/empty selections, invalid stage geometry or effect settings,
/// inconsistent animation output sizes, stage exhaustion, or excessive metadata.
pub fn edit_composed_frames(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    edit: &ComposedFrameEdit,
) -> Result<EditCommand, EditorError> {
    let requested: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &requested)?;
    let effect_edit = match edit {
        ComposedFrameEdit::Effect(edit) => Some(ComposedEffectEdit::from(edit)),
        ComposedFrameEdit::ImageEffect(edit) => Some(edit.clone()),
        _ => None,
    };
    let changes_canvas = edit.changes_canvas()
        || effect_edit
            .as_ref()
            .is_some_and(|edit| edit.requires_all_frames(project, &requested));
    let selected: BTreeSet<_> = if changes_canvas {
        project
            .timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .collect()
    } else {
        requested
    };
    let appending = edit.appended_step();
    let tail_owners: BTreeSet<_> = project
        .timeline
        .overlay_tracks
        .iter()
        .filter_map(|track| track.frame_cells.as_ref())
        .flatten()
        .filter(|cell| cell.stage.is_none() && selected.contains(&cell.frame_id))
        .map(|cell| cell.frame_id)
        .collect();

    bound_source_metadata(project, &selected, appending.is_some())?;

    let mut commands = Vec::new();
    let mut sealed = BTreeMap::new();
    let mut output_size = None;
    for frame in project
        .timeline
        .frames
        .iter()
        .filter(|frame| selected.contains(&frame.id))
    {
        let source_size = source_size(project, frame)?;
        let mut replacement = frame.clone();
        if let Some(step) = &appending {
            let before = geometry(frame, source_size)?;
            if let Some(effect) = effect_edit.as_ref().and_then(ComposedEffectEdit::appended) {
                effect.validate(frame.id, before.output_size())?;
            }
            if replacement.render_steps.is_empty() || tail_owners.contains(&frame.id) {
                let stage_id = next_stage_id(frame)?;
                replacement
                    .render_steps
                    .push(FrameRenderStep::composite(stage_id));
                sealed.insert(frame.id, stage_id);
            }
            replacement.render_steps.push(step.clone());
        } else if let Some(effect_edit) = &effect_edit {
            effect_edit.apply(&mut replacement, source_size)?;
        } else {
            modify_previous(&mut replacement, edit);
        }
        let after = geometry(&replacement, source_size)?;
        effects::validate_program(&replacement, source_size, &after)?;
        if changes_canvas {
            if output_size.is_some_and(|size| size != after.output_size()) {
                return Err(pipeline_error(
                    frame.id,
                    "This operation would leave different frame sizes. Resize the whole animation to a common size first.",
                ));
            }
            output_size = Some(after.output_size());
        }
        if replacement != *frame {
            commands.push(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(replacement),
            });
        }
    }
    commands.extend(seal_tail_cells(project, &sealed));
    if let Some(size) = output_size.filter(|size| *size != project.canvas.size) {
        let mut canvas = project.canvas.clone();
        canvas.size = size;
        commands.push(EditCommand::SetCanvas { canvas });
    }
    if commands.is_empty() {
        return Err(pipeline_error(
            selected
                .first()
                .copied()
                .ok_or(EditorError::EmptySelection)?,
            "The frames already match this edit; nothing was changed.",
        ));
    }
    let command = EditCommand::Compound { commands };
    serde_json::to_writer(&mut MetadataBudget::default(), &command)
        .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
    Ok(command)
}

/// Bound source cloning before preparing edits. The final command is checked
/// again, accounting for newly allocated stages and command wrappers.
fn bound_source_metadata(
    project: &ProjectManifest,
    selected: &BTreeSet<FrameId>,
    appending: bool,
) -> Result<(), EditorError> {
    let mut budget = MetadataBudget::default();
    for frame in project
        .timeline
        .frames
        .iter()
        .filter(|frame| selected.contains(&frame.id))
    {
        serde_json::to_writer(&mut budget, frame)
            .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
    }
    if appending {
        for track in &project.timeline.overlay_tracks {
            if track.frame_cells.as_ref().is_some_and(|cells| {
                cells
                    .iter()
                    .any(|cell| cell.stage.is_none() && selected.contains(&cell.frame_id))
            }) {
                serde_json::to_writer(&mut budget, track)
                    .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
            }
        }
    }
    Ok(())
}

fn seal_tail_cells(project: &ProjectManifest, sealed: &BTreeMap<FrameId, u32>) -> Vec<EditCommand> {
    let mut commands = Vec::new();
    for track in &project.timeline.overlay_tracks {
        let Some(cells) = &track.frame_cells else {
            continue;
        };
        if !cells
            .iter()
            .any(|cell| cell.stage.is_none() && sealed.contains_key(&cell.frame_id))
        {
            continue;
        }
        let mut track = track.clone();
        for cell in track.frame_cells.iter_mut().flatten() {
            if cell.stage.is_none() {
                cell.stage = sealed.get(&cell.frame_id).copied();
            }
        }
        commands.push(EditCommand::UpsertOverlayTrack { track });
    }
    commands
}

fn source_size(project: &ProjectManifest, frame: &FrameClip) -> Result<PhysicalSize, EditorError> {
    project
        .assets
        .get(&frame.asset_id)
        .ok_or(EditorError::MissingFrameAsset {
            frame_id: frame.id,
            asset_id: frame.asset_id,
        })?
        .kind
        .raster_size()
        .ok_or(EditorError::UnsupportedFrameAsset {
            frame_id: frame.id,
            asset_id: frame.asset_id,
        })
}

fn geometry(
    frame: &FrameClip,
    source_size: PhysicalSize,
) -> Result<FrameGeometryPlan, EditorError> {
    FrameGeometryPlan::new(frame, source_size).map_err(|reason| pipeline_error(frame.id, reason))
}

fn next_stage_id(frame: &FrameClip) -> Result<u32, EditorError> {
    let allocated: BTreeSet<_> = frame
        .render_steps
        .iter()
        .filter_map(|step| match step {
            FrameRenderStep::Composite { stage_id, .. } => Some(*stage_id),
            _ => None,
        })
        .collect();
    // IDs are opaque, not a counter. A valid imported stage may be u32::MAX;
    // bounded gap search still finds space without rejecting that program.
    let search_end = u32::try_from(allocated.len())
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| pipeline_error(frame.id, "Compositing stage count exceeds its bound."))?;
    (1..=search_end)
        .find(|id| !allocated.contains(id))
        .ok_or_else(|| pipeline_error(frame.id, "Compositing stage identities are exhausted."))
}

fn modify_previous(frame: &mut FrameClip, edit: &ComposedFrameEdit) {
    match edit {
        ComposedFrameEdit::ClearCrop => {
            if let Some(index) = frame
                .render_steps
                .iter()
                .rposition(|step| matches!(step, FrameRenderStep::Crop { .. }))
            {
                frame.render_steps.remove(index);
            } else {
                frame.transform.crop = None;
            }
        }
        ComposedFrameEdit::ClearResize => {
            if let Some(index) = frame
                .render_steps
                .iter()
                .rposition(|step| matches!(step, FrameRenderStep::Resize { .. }))
            {
                frame.render_steps.remove(index);
            } else {
                frame.transform.output_size = None;
            }
        }
        _ => unreachable!("append-only edits handled by caller"),
    }
}

fn pipeline_error(frame_id: FrameId, reason: impl Into<String>) -> EditorError {
    EditorError::InvalidRenderPipeline {
        frame_id,
        reason: reason.into(),
    }
}

#[derive(Default)]
struct MetadataBudget(usize);

impl Write for MetadataBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|size| *size <= MAX_FRAME_BUNDLE_METADATA_BYTES)
            .ok_or_else(|| io::Error::other("frame edit metadata budget exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "composed_frame_tests.rs"]
mod tests;
