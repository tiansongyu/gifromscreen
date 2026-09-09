//! New authors get explicit precision boundaries; existing pixels keep theirs.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};

use gif_from_screen_domain::{
    BlendMode, CompositePrecision, EditCommand, FrameClip, FrameGeometryPlan, FrameId,
    FrameRenderStep, MAX_FRAME_RENDER_STEPS, OverlayContent, OverlayTrack, ProjectManifest,
    TrackId, VECTOR_SHAPE_VERSION, WPF_VECTOR_SHAPE_VERSION, validate_frame_overlay_cells,
};

use crate::{EditorError, MAX_FRAME_BUNDLE_METADATA_BYTES};

/// Prepares a new frame-owned group and independent paint stages atomically.
/// Normal groups use WPF PBGRA8/PNG precision. Enhanced blend modes keep the
/// established straight-alpha algorithm, but still have independent stages.
/// All existing tail cells, including hidden/empty cells, retain legacy pixels.
///
/// The final command is always the new group's Upsert. Pending raster/replay
/// assets may be registered by earlier commands in the caller's same compound;
/// this function performs no pixel I/O and never clones the complete project.
///
/// # Errors
/// Rejects existing/nil identities, non-owned or malformed cells, unknown owners,
/// invalid source geometry, stage exhaustion, and more than 16 MiB of copied or
/// prepared command metadata. No project state is changed on failure.
pub fn author_frame_owned_track(
    project: &ProjectManifest,
    track: OverlayTrack,
) -> Result<Vec<EditCommand>, EditorError> {
    let precision = if track.blend_mode == BlendMode::Normal {
        CompositePrecision::WpfPbgra8PngV1
    } else {
        CompositePrecision::LegacyStraightRgba8
    };
    author_with_precision(project, track, precision)
}

/// Authors one isolated vector canvas, without changing existing author precision.
/// Every vector is painted into the new stage before that canvas is composited
/// once over preceding pixels, matching the shape Apply group's PM boundary.
/// All marks must carry one version: V1 retains the original vector stage and
/// V2 uses its explicit new stage. Hidden marks participate in this check.
///
/// # Errors
/// Rejects empty/non-vector/mixed-version groups, enhanced blend modes and the same ownership,
/// stage and metadata failures as [`author_frame_owned_track`].
pub fn author_vector_shape_track(
    project: &ProjectManifest,
    track: OverlayTrack,
) -> Result<Vec<EditCommand>, EditorError> {
    if track.blend_mode != BlendMode::Normal {
        return Err(track_error(
            track.id,
            "A vector canvas requires normal composition.",
        ));
    }
    if track.all_mark_contents().next().is_none() {
        return Err(track_error(
            track.id,
            "A new vector canvas requires at least one shape.",
        ));
    }
    let mut version = None;
    for (_, content) in track.all_mark_contents() {
        let OverlayContent::VectorShape { shape } = content else {
            return Err(track_error(
                track.id,
                "A vector canvas can only contain vector shapes.",
            ));
        };
        shape
            .validate()
            .map_err(|reason| track_error(track.id, reason))?;
        if version.is_some_and(|version| version != shape.version) {
            return Err(track_error(
                track.id,
                "A vector canvas cannot mix shape versions.",
            ));
        }
        version = Some(shape.version);
    }
    let precision = match version {
        Some(VECTOR_SHAPE_VERSION) => CompositePrecision::VectorCanvasPbgra8PngV1,
        Some(WPF_VECTOR_SHAPE_VERSION) => CompositePrecision::VectorCanvasPbgra8PngV2,
        _ => {
            return Err(track_error(
                track.id,
                "Unsupported vector canvas shape version.",
            ));
        }
    };
    author_with_precision(project, track, precision)
}

fn author_with_precision(
    project: &ProjectManifest,
    mut track: OverlayTrack,
    precision: CompositePrecision,
) -> Result<Vec<EditCommand>, EditorError> {
    project.validate()?;
    let track_id = track.id;
    let failure = |reason| track_error(track_id, reason);
    if track.id.is_nil()
        || project
            .timeline
            .overlay_tracks
            .iter()
            .any(|old| old.id == track.id)
    {
        return Err(failure(
            "A new authored group needs a distinct nonzero track identity.",
        ));
    }
    if track.name.trim().is_empty() {
        return Err(failure("A new authored group needs a name."));
    }
    let cells = track
        .frame_cells
        .as_ref()
        .ok_or_else(|| failure("New authored groups must be frame-owned."))?;
    let known: BTreeSet<_> = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    validate_frame_overlay_cells(&track, &known).map_err(|reason| track_error(track_id, reason))?;
    if cells.iter().any(|cell| cell.stage.is_some()) {
        return Err(failure(
            "New authored cells must not reuse an existing paint stage.",
        ));
    }
    let owners: BTreeSet<_> = cells.iter().map(|cell| cell.frame_id).collect();
    check_mark_identities(project, &track)?;
    let tail_owners: BTreeSet<_> = project
        .timeline
        .overlay_tracks
        .iter()
        .flat_map(|old| old.frame_cells.iter().flatten())
        .filter(|cell| cell.stage.is_none() && owners.contains(&cell.frame_id))
        .map(|cell| cell.frame_id)
        .collect();
    bound_source_metadata(project, &track, &owners, &tail_owners)?;
    let mut commands = Vec::new();
    let mut sealed = BTreeMap::new();
    let mut authored = BTreeMap::new();
    for frame in project
        .timeline
        .frames
        .iter()
        .filter(|frame| owners.contains(&frame.id))
    {
        let seal = frame.render_steps.is_empty() || tail_owners.contains(&frame.id);
        let prepared = author_owner(project, frame, seal, precision, track_id)?;
        if let Some(stage_id) = prepared.previous_tail_stage {
            sealed.insert(frame.id, stage_id);
        }
        authored.insert(frame.id, prepared.new_stage);
        commands.push(EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(prepared.replacement),
        });
    }
    commands.extend(seal_existing_tails(project, &sealed));
    for cell in track.frame_cells.iter_mut().flatten() {
        cell.stage = authored.get(&cell.frame_id).copied();
    }
    commands.push(EditCommand::UpsertOverlayTrack { track });
    let mut budget = MetadataBudget::default();
    // Count the actual eventual Compound wrapper, without cloning its commands.
    budget
        .write_all(br#"{"type":"compound","commands":"#)
        .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
    serde_json::to_writer(&mut budget, &commands)
        .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
    budget
        .write_all(b"}")
        .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
    Ok(commands)
}

struct AuthoredOwner {
    replacement: FrameClip,
    previous_tail_stage: Option<u32>,
    new_stage: u32,
}

fn author_owner(
    project: &ProjectManifest,
    frame: &FrameClip,
    seal: bool,
    precision: CompositePrecision,
    track_id: TrackId,
) -> Result<AuthoredOwner, EditorError> {
    let source_size = project
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
        })?;
    FrameGeometryPlan::new(frame, source_size).map_err(|reason| {
        EditorError::InvalidRenderPipeline {
            frame_id: frame.id,
            reason,
        }
    })?;
    if frame
        .render_steps
        .len()
        .checked_add(1 + usize::from(seal))
        .is_none_or(|count| count > MAX_FRAME_RENDER_STEPS)
    {
        return Err(track_error(
            track_id,
            "This authoring edit exceeds the 4,096-step frame limit.",
        ));
    }
    let mut occupied: BTreeSet<_> = frame
        .render_steps
        .iter()
        .filter_map(|step| {
            if let FrameRenderStep::Composite { stage_id, .. } = step {
                Some(*stage_id)
            } else {
                None
            }
        })
        .collect();
    let mut replacement = frame.clone();
    let previous_tail_stage = if seal {
        let id = allocate_stage(&mut occupied)
            .ok_or_else(|| track_error(track_id, "No bounded paint-stage identity remains."))?;
        replacement
            .render_steps
            .push(FrameRenderStep::composite(id));
        Some(id)
    } else {
        None
    };
    let new_stage = allocate_stage(&mut occupied)
        .ok_or_else(|| track_error(track_id, "No bounded paint-stage identity remains."))?;
    replacement.render_steps.push(FrameRenderStep::Composite {
        stage_id: new_stage,
        precision,
    });
    Ok(AuthoredOwner {
        replacement,
        previous_tail_stage,
        new_stage,
    })
}

fn check_mark_identities(
    project: &ProjectManifest,
    track: &OverlayTrack,
) -> Result<(), EditorError> {
    let mut identities = BTreeSet::new();
    for (id, _) in track.all_mark_contents() {
        if id.is_nil() || !identities.insert(id) {
            return Err(track_error(
                track.id,
                "New marks need distinct nonzero identities.",
            ));
        }
    }
    if project
        .timeline
        .overlay_tracks
        .iter()
        .flat_map(OverlayTrack::all_mark_contents)
        .any(|(id, _)| identities.contains(&id))
    {
        return Err(track_error(
            track.id,
            "A new mark identity is already used by the project.",
        ));
    }
    Ok(())
}

fn bound_source_metadata(
    project: &ProjectManifest,
    track: &OverlayTrack,
    owners: &BTreeSet<FrameId>,
    tail_owners: &BTreeSet<FrameId>,
) -> Result<(), EditorError> {
    let mut budget = MetadataBudget::default();
    serde_json::to_writer(&mut budget, track)
        .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
    for frame in project
        .timeline
        .frames
        .iter()
        .filter(|frame| owners.contains(&frame.id))
    {
        serde_json::to_writer(&mut budget, frame)
            .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
    }
    for old in &project.timeline.overlay_tracks {
        if old.frame_cells.as_ref().is_some_and(|cells| {
            cells
                .iter()
                .any(|cell| cell.stage.is_none() && tail_owners.contains(&cell.frame_id))
        }) {
            serde_json::to_writer(&mut budget, old)
                .map_err(|_| EditorError::RenderPipelineMetadataLimit)?;
        }
    }
    Ok(())
}

fn seal_existing_tails(
    project: &ProjectManifest,
    sealed: &BTreeMap<FrameId, u32>,
) -> Vec<EditCommand> {
    project
        .timeline
        .overlay_tracks
        .iter()
        .filter_map(|old| {
            if !old.frame_cells.as_ref().is_some_and(|cells| {
                cells
                    .iter()
                    .any(|cell| cell.stage.is_none() && sealed.contains_key(&cell.frame_id))
            }) {
                return None;
            }
            let mut track = old.clone();
            for cell in track.frame_cells.iter_mut().flatten() {
                if cell.stage.is_none() {
                    cell.stage = sealed.get(&cell.frame_id).copied();
                }
            }
            Some(EditCommand::UpsertOverlayTrack { track })
        })
        .collect()
}

fn allocate_stage(occupied: &mut BTreeSet<u32>) -> Option<u32> {
    (1..=u32::try_from(MAX_FRAME_RENDER_STEPS).ok()?).find(|id| occupied.insert(*id))
}

fn track_error(track_id: TrackId, reason: impl Into<String>) -> EditorError {
    EditorError::InvalidPaintTrack {
        track_id,
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
            .filter(|count| *count <= MAX_FRAME_BUNDLE_METADATA_BYTES)
            .ok_or_else(|| io::Error::other("paint-stage metadata budget exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "paint_stage_tests.rs"]
mod tests;
