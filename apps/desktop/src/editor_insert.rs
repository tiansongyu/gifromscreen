//! Import an editable timeline as one journaled revision.
//!
//! Preparation runs off the UI thread. Immutable assets are verified before any
//! destination writes; interrupted preparation may leave unreferenced blobs,
//! but never changes either timeline. Commit accepts only the frozen revision.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use gif_from_screen_domain::{
    AssetDescriptor, AssetId, DurationUs, EditCommand, FrameClip, FrameId, ProjectManifest,
    ProjectRevision, RasterEncoding, TimeUs, Timeline, TrackId, TransitionId,
};
use gif_from_screen_editor::{FrameBundle, FrameBundleIdentities};
use gif_from_screen_gif::CancellationToken;
use gif_from_screen_project::{ActiveProject, AssetStore, LockPolicy, ProjectError};
use thiserror::Error;
use uuid::Uuid;

use super::{EditorWorkspace, EditorWorkspaceError, asset_issue_id};

pub(crate) const MAX_INSERTED_FRAMES: usize = 1_000;
const MAX_RESULTING_FRAMES: usize = 100_000;
const MAX_OVERLAY_ITEMS: usize = 10_000;
const MAX_INSERTED_ASSET_BYTES: u64 = 512 * 1024 * 1024;
const READ_CHUNK: usize = 16 * 1024;

/// The original insertion point survives selection changes, but not edits.
#[derive(Clone, Debug)]
pub(crate) struct ProjectInsertionTarget {
    root: PathBuf,
    manifest: ProjectManifest,
    assets: AssetStore,
    index: usize,
}

/// Contains metadata only: large immutable pixels were prepared off-thread.
#[derive(Debug)]
pub(crate) struct PreparedProjectInsertion {
    pub(crate) source_label: String,
    pub(crate) inserted_frame_count: usize,
    root: PathBuf,
    project_id: gif_from_screen_domain::ProjectId,
    revision: ProjectRevision,
    command: EditCommand,
    inserted_frames: Vec<FrameId>,
    imported_assets: BTreeSet<AssetId>,
}

impl PreparedProjectInsertion {
    pub(crate) fn frame_count(&self) -> usize {
        self.inserted_frame_count
    }
}

impl EditorWorkspace {
    /// None inserts at the beginning; Some inserts after the specified frame.
    pub(crate) fn project_insertion_target(
        &self,
        after: Option<FrameId>,
    ) -> Result<ProjectInsertionTarget, ProjectInsertionError> {
        let index = match after {
            None => 0,
            Some(id) => self
                .manifest()
                .timeline
                .frames
                .iter()
                .position(|frame| frame.id == id)
                .map(|index| index + 1)
                .ok_or(ProjectInsertionError::UnknownAnchor(id))?,
        };
        Ok(ProjectInsertionTarget {
            root: self.project_root().to_owned(),
            manifest: self.manifest().clone(),
            assets: self.project.assets().clone(),
            index,
        })
    }

    /// Journals the validated insertion once, without reading large asset files.
    pub(crate) fn insert_prepared_project(
        &mut self,
        prepared: PreparedProjectInsertion,
    ) -> Result<usize, ProjectInsertionError> {
        if self.project_root() != prepared.root
            || self.manifest().project_id != prepared.project_id
            || self.manifest().revision != prepared.revision
        {
            return Err(ProjectInsertionError::StaleTarget);
        }
        self.execute(prepared.command)?;
        self.asset_issues
            .retain(|issue| !prepared.imported_assets.contains(&asset_issue_id(issue)));
        let first = prepared.inserted_frames[0];
        self.select_only(first)?;
        Ok(prepared.inserted_frames.len())
    }
}

/// Opens and recovers a source under its ordinary exclusive lock. Existing or
/// stale locks are never taken over, and the destination cannot be its source.
pub(crate) fn prepare_project_insertion_from_path(
    target: ProjectInsertionTarget,
    source_root: &Path,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedProjectInsertion, ProjectInsertionError> {
    check_cancelled(cancellation)?;
    if canonicalize(source_root)? == canonicalize(&target.root)? {
        return Err(ProjectInsertionError::SameProject);
    }
    let source = ActiveProject::open(source_root, LockPolicy::FailIfPresent)?;
    if !source.journal_recovery.is_clean() {
        return Err(ProjectInsertionError::SourceRequiresRepair);
    }
    let mut prepared = prepare_project_insertion(
        target,
        source.project.manifest(),
        source.project.assets(),
        cancellation,
    )?;
    prepared.source_label = source_root
        .file_name()
        .unwrap_or(source_root.as_os_str())
        .to_string_lossy()
        .into_owned();
    Ok(prepared)
}

/// The caller must hold the source project's lock for the duration of this call.
/// This permits inserting a just-recorded/imported active project without
/// reacquiring its lock. Destination files remain immutable and content-addressed.
pub(crate) fn prepare_project_insertion(
    target: ProjectInsertionTarget,
    source: &ProjectManifest,
    source_assets: &AssetStore,
    cancellation: &dyn CancellationToken,
) -> Result<PreparedProjectInsertion, ProjectInsertionError> {
    check_cancelled(cancellation)?;
    if source.project_id == target.manifest.project_id
        || canonicalize(source_assets.directory())? == canonicalize(target.assets.directory())?
    {
        return Err(ProjectInsertionError::SameProject);
    }
    validate_source(&target, source)?;
    let output_size = validate_rendered_sizes(source, &target.manifest, cancellation)?;
    let needed = referenced_assets(source);
    let descriptors = checked_descriptors(&target, source, &needed)?;
    let (command, inserted_frames) = insertion_command(&target, source, &descriptors, output_size)?;
    let mut candidate = target.manifest.clone();
    candidate
        .apply_command(&command)
        .map_err(ProjectError::from)?;
    candidate
        .timeline
        .transitions
        .iter()
        .try_fold(
            candidate.timeline.total_duration().unwrap().get(),
            |total, transition| total.checked_add(transition.duration.get()),
        )
        .ok_or(ProjectInsertionError::DurationOverflow)?;

    // Read and verify the entire bounded input before storing the first blob.
    // A corrupt late asset cannot leave a partially imported project behind.
    let mut replay_refs: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for reference in source
        .timeline
        .overlay_tracks
        .iter()
        .flat_map(|track| track.frame_cells.iter().flatten())
        .flat_map(|cell| cell.input_replay.iter())
        .flat_map(|replay| &replay.runs)
    {
        replay_refs
            .entry(reference.asset_id)
            .or_default()
            .push(reference);
    }
    let mut bytes = Vec::with_capacity(descriptors.len());
    for descriptor in &descriptors {
        let pixels = read_asset(source_assets, descriptor, cancellation)?;
        validate_input_replay_asset(descriptor, &pixels, replay_refs.get(&descriptor.id))?;
        if target.assets.contains(descriptor.id) {
            // Reuse must never silently accept damaged destination content.
            verify_existing_asset(&target.assets, descriptor, &pixels, cancellation)?;
        }
        bytes.push(pixels);
    }
    for (descriptor, pixels) in descriptors.iter().zip(bytes) {
        check_cancelled(cancellation)?;
        if !target.assets.contains(descriptor.id) {
            let id = target.assets.put(&pixels)?;
            debug_assert_eq!(id, descriptor.id);
        }
    }
    check_cancelled(cancellation)?;
    Ok(PreparedProjectInsertion {
        source_label: source.project_id.to_string(),
        inserted_frame_count: inserted_frames.len(),
        root: target.root,
        project_id: target.manifest.project_id,
        revision: target.manifest.revision,
        command,
        inserted_frames,
        imported_assets: needed,
    })
}

fn validate_source(
    target: &ProjectInsertionTarget,
    source: &ProjectManifest,
) -> Result<(), ProjectInsertionError> {
    if source.timeline.frames.is_empty() {
        return Err(ProjectInsertionError::EmptySource);
    }
    if source.timeline.frames.len() > MAX_INSERTED_FRAMES
        || target
            .manifest
            .timeline
            .frames
            .len()
            .saturating_add(source.timeline.frames.len())
            > MAX_RESULTING_FRAMES
    {
        return Err(ProjectInsertionError::FrameLimit);
    }
    if source.timeline.overlay_tracks.len() > MAX_OVERLAY_ITEMS
        || source
            .timeline
            .overlay_tracks
            .iter()
            .map(|track| track.all_mark_contents().count())
            .try_fold(0_usize, usize::checked_add)
            .is_none_or(|items| items > MAX_OVERLAY_ITEMS)
    {
        return Err(ProjectInsertionError::OverlayLimit);
    }
    source.validate().map_err(ProjectError::from)?;
    if source.canvas.color_space != target.manifest.canvas.color_space
        || source.canvas.background != target.manifest.canvas.background
    {
        return Err(ProjectInsertionError::CanvasMismatch);
    }
    source
        .timeline
        .total_duration()
        .and_then(|duration| {
            target
                .manifest
                .timeline
                .total_duration()?
                .get()
                .checked_add(duration.get())
        })
        .ok_or(ProjectInsertionError::DurationOverflow)?;
    Ok(())
}

/// Borrow clip metadata directly: large raw input arrays are not cloned just
/// to inspect the geometry prefix. This pass precedes every pixel/asset read.
fn validate_rendered_sizes(
    source: &ProjectManifest,
    destination: &ProjectManifest,
    cancellation: &dyn CancellationToken,
) -> Result<gif_from_screen_domain::PhysicalSize, ProjectInsertionError> {
    let mut expected = None;
    for (side, project) in [("source", source), ("destination", destination)] {
        for frame in &project.timeline.frames {
            check_cancelled(cancellation)?;
            let source_size = project
                .assets
                .get(&frame.asset_id)
                .ok_or(ProjectInsertionError::MissingAsset(frame.asset_id))?
                .kind
                .raster_size()
                .ok_or(ProjectInsertionError::UnsupportedAsset(frame.asset_id))?;
            let actual = gif_from_screen_domain::FrameGeometryPlan::new(frame, source_size)
                .map_err(|reason| ProjectInsertionError::RenderedGeometry {
                    side,
                    frame_id: frame.id,
                    reason,
                })?
                .output_size();
            if let Some(expected) = expected {
                if actual != expected {
                    return Err(ProjectInsertionError::RenderedSizeMismatch {
                        side,
                        frame_id: frame.id,
                        expected,
                        actual,
                    });
                }
            } else {
                expected = Some(actual);
            }
        }
    }
    check_cancelled(cancellation)?;
    expected.ok_or(ProjectInsertionError::EmptySource)
}

fn referenced_assets(source: &ProjectManifest) -> BTreeSet<AssetId> {
    let mut needed = BTreeSet::new();
    for frame in &source.timeline.frames {
        needed.insert(frame.asset_id);
        needed.extend(frame.capture_metadata.cursor_asset);
        needed.extend(frame.referenced_effect_assets());
    }
    needed.extend(
        source
            .timeline
            .overlay_tracks
            .iter()
            .flat_map(gif_from_screen_domain::OverlayTrack::referenced_assets),
    );
    needed
}

fn checked_descriptors(
    target: &ProjectInsertionTarget,
    source: &ProjectManifest,
    needed: &BTreeSet<AssetId>,
) -> Result<Vec<AssetDescriptor>, ProjectInsertionError> {
    let mut total = 0_u64;
    needed
        .iter()
        .map(|id| {
            let descriptor = source
                .assets
                .get(id)
                .ok_or(ProjectInsertionError::MissingAsset(*id))?;
            validate_inserted_asset_descriptor(descriptor)?;
            total = total
                .checked_add(descriptor.byte_len)
                .filter(|total| *total <= MAX_INSERTED_ASSET_BYTES)
                .ok_or(ProjectInsertionError::AssetBudget)?;
            if let Some(existing) = target.manifest.assets.get(id)
                && (!same_asset_storage(existing, descriptor)
                    || existing.byte_len != descriptor.byte_len)
            {
                return Err(ProjectInsertionError::AssetCollision(*id));
            }
            Ok(descriptor.clone())
        })
        .collect()
}

fn same_asset_storage(left: &AssetDescriptor, right: &AssetDescriptor) -> bool {
    match (
        left.kind.raster_descriptor(),
        right.kind.raster_descriptor(),
    ) {
        (Some(a), Some(b)) => a == b,
        (None, None) => left.kind == right.kind,
        _ => false,
    }
}

fn validate_inserted_asset_descriptor(
    descriptor: &AssetDescriptor,
) -> Result<(), ProjectInsertionError> {
    if let Some((size, RasterEncoding::Rgba8)) = descriptor.kind.raster_descriptor() {
        let expected = u64::from(size.width.get())
            .checked_mul(u64::from(size.height.get()))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(ProjectInsertionError::AssetBudget)?;
        if expected != descriptor.byte_len {
            return Err(ProjectInsertionError::InvalidAssetLength(descriptor.id));
        }
        return Ok(());
    }
    if matches!(&descriptor.kind, gif_from_screen_domain::AssetKind::ImportedSource { media_type } if media_type == gif_from_screen_domain::INPUT_REPLAY_MEDIA_TYPE)
        && (1..=gif_from_screen_domain::MAX_INPUT_REPLAY_POOL_BYTES).contains(&descriptor.byte_len)
    {
        return Ok(());
    }
    Err(ProjectInsertionError::UnsupportedAsset(descriptor.id))
}

fn validate_input_replay_asset(
    descriptor: &AssetDescriptor,
    bytes: &[u8],
    references: Option<&Vec<&gif_from_screen_domain::FrameInputReplayRef>>,
) -> Result<(), ProjectInsertionError> {
    let Some(references) = references else {
        return Ok(());
    };
    let failure = |message: String| ProjectInsertionError::InvalidInputReplay {
        asset_id: descriptor.id,
        message,
    };
    let pool: gif_from_screen_domain::FrameInputReplayPool =
        serde_json::from_slice(bytes).map_err(|error| failure(error.to_string()))?;
    pool.validate().map_err(failure)?;
    for reference in references {
        pool.validate_reference(reference).map_err(failure)?;
    }
    Ok(())
}

fn insertion_command(
    target: &ProjectInsertionTarget,
    source: &ProjectManifest,
    descriptors: &[AssetDescriptor],
    output_size: gif_from_screen_domain::PhysicalSize,
) -> Result<(EditCommand, Vec<FrameId>), ProjectInsertionError> {
    let timeline = &target.manifest.timeline;
    let start = timeline.frames[..target.index]
        .iter()
        .try_fold(0_u64, |sum, frame| sum.checked_add(frame.duration.get()))
        .ok_or(ProjectInsertionError::DurationOverflow)?;
    let duration = source
        .timeline
        .total_duration()
        .and_then(|time| DurationUs::new(time.get()))
        .ok_or(ProjectInsertionError::DurationOverflow)?;
    let RemappedFrames {
        frames,
        by_source: frame_ids,
    } = remap_frames(timeline, &source.timeline)?;
    let inserted_frames = frames.iter().map(|frame| frame.id).collect();
    let mut commands = descriptors
        .iter()
        .filter(|asset| !target.manifest.assets.contains_key(&asset.id))
        .cloned()
        .map(|asset| EditCommand::RegisterAsset { asset })
        .collect::<Vec<_>>();
    let preceding = target
        .index
        .checked_sub(1)
        .map(|index| timeline.frames[index].id);
    let following = timeline.frames.get(target.index).map(|frame| frame.id);
    let mut transitions = timeline
        .transitions
        .iter()
        .filter(|transition| {
            Some(transition.from_frame) != preceding || Some(transition.to_frame) != following
        })
        .cloned()
        .collect::<Vec<_>>();
    commands.push(EditCommand::InsertFrames {
        index: target.index,
        frames,
    });
    // Preserve destination annotation coverage exactly, excluding the inserted
    // interval. Generic timeline insertion intentionally extends crossing spans.
    for track in timeline
        .overlay_tracks
        .iter()
        .filter(|track| track.frame_cells.is_none())
    {
        let mut shifted = track.clone();
        shifted.items = super::text::exclude_inserted_title(&track.items, start, duration)?;
        if let Some(scope) = &track.annotation_scope {
            shifted.annotation_scope = Some(
                gif_from_screen_domain::shift_annotation_scope_for_insert(scope, start, duration)
                    .map_err(|_| ProjectInsertionError::DurationOverflow)?,
            );
        }
        commands.push(EditCommand::UpsertOverlayTrack { track: shifted });
    }
    append_source_tracks(&mut commands, timeline, source, start, &frame_ids)?;
    let mut used_transitions = timeline
        .transitions
        .iter()
        .chain(&source.timeline.transitions)
        .map(|transition| transition.id)
        .collect::<BTreeSet<_>>();
    for transition in &source.timeline.transitions {
        let mut imported = transition.clone();
        imported.id = loop {
            let id = TransitionId::from_u128(Uuid::new_v4().as_u128());
            if used_transitions.insert(id) {
                break id;
            }
        };
        imported.from_frame = frame_ids[&transition.from_frame];
        imported.to_frame = frame_ids[&transition.to_frame];
        transitions.push(imported);
    }
    commands.push(EditCommand::SetTransitions { transitions });
    if output_size != target.manifest.canvas.size {
        let mut canvas = target.manifest.canvas.clone();
        canvas.size = output_size;
        commands.push(EditCommand::SetCanvas { canvas });
    }
    Ok((EditCommand::Compound { commands }, inserted_frames))
}

fn append_source_tracks(
    commands: &mut Vec<EditCommand>,
    destination: &Timeline,
    source: &ProjectManifest,
    start: u64,
    frame_ids: &BTreeMap<FrameId, FrameId>,
) -> Result<(), ProjectInsertionError> {
    let mut identities = FrameBundleIdentities::new(
        destination
            .overlay_tracks
            .iter()
            .chain(&source.timeline.overlay_tracks),
    )?;
    let selected = source
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    let bundle = FrameBundle::capture(source, &selected)?;
    let mut generate = || FrameId::from_u128(Uuid::new_v4().as_u128());
    let remapped = bundle.remap(frame_ids, &mut identities, &mut generate)?;
    let mut frame_tracks: BTreeMap<TrackId, _> = bundle
        .tracks()
        .iter()
        .map(|track| track.id)
        .zip(remapped)
        .collect();
    // Preserve mixed legacy/frame-owned source track order: it participates
    // in z-order tie breaking. All branches share the same identity allocator.
    for track in &source.timeline.overlay_tracks {
        if track.frame_cells.is_some() {
            let imported = frame_tracks
                .remove(&track.id)
                .expect("full source bundle includes every frame-owned track");
            commands.push(EditCommand::UpsertOverlayTrack { track: imported });
            continue;
        }
        let mut imported = track.clone();
        if let Some(scope) = &mut imported.annotation_scope {
            for span in scope {
                span.start = TimeUs::new(
                    span.start
                        .get()
                        .checked_add(start)
                        .ok_or(ProjectInsertionError::DurationOverflow)?,
                );
            }
        }
        imported.id = identities.track(&mut generate)?;
        for item in &mut imported.items {
            item.id = identities.mark(&mut generate)?;
            item.span.start = TimeUs::new(
                item.span
                    .start
                    .get()
                    .checked_add(start)
                    .ok_or(ProjectInsertionError::DurationOverflow)?,
            );
        }
        commands.push(EditCommand::UpsertOverlayTrack { track: imported });
    }
    Ok(())
}

struct RemappedFrames {
    frames: Vec<FrameClip>,
    by_source: BTreeMap<FrameId, FrameId>,
}

fn remap_frames(
    destination: &Timeline,
    source: &Timeline,
) -> Result<RemappedFrames, ProjectInsertionError> {
    let mut used_frames = destination
        .frames
        .iter()
        .chain(&source.frames)
        .map(|frame| frame.id)
        .collect::<BTreeSet<_>>();
    let mut by_source = BTreeMap::new();
    let mut frames = source.frames.clone();
    let mut source_time = 0_u64;
    for frame in &mut frames {
        frame.freeze_capture_clock(TimeUs::new(source_time));
        source_time = source_time
            .checked_add(frame.duration.get())
            .ok_or(ProjectInsertionError::DurationOverflow)?;
        let id = loop {
            let id = FrameId::from_u128(Uuid::new_v4().as_u128());
            if used_frames.insert(id) {
                break id;
            }
        };
        by_source.insert(frame.id, id);
        frame.id = id;
    }
    Ok(RemappedFrames { frames, by_source })
}

fn read_asset(
    store: &AssetStore,
    descriptor: &AssetDescriptor,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<u8>, ProjectInsertionError> {
    let path = store.asset_path(descriptor.id);
    let io_error = |source| ProjectInsertionError::Io {
        path: path.clone(),
        source,
    };
    let mut file = File::open(&path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.len() != descriptor.byte_len {
        return Err(ProjectInsertionError::InvalidAssetLength(descriptor.id));
    }
    let expected =
        usize::try_from(descriptor.byte_len).map_err(|_| ProjectInsertionError::AssetBudget)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected)
        .map_err(|_| ProjectInsertionError::AssetBudget)?;
    let mut buffer = [0_u8; READ_CHUNK];
    loop {
        check_cancelled(cancellation)?;
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > expected {
            return Err(ProjectInsertionError::InvalidAssetLength(descriptor.id));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    if bytes.len() != expected {
        return Err(ProjectInsertionError::InvalidAssetLength(descriptor.id));
    }
    if AssetStore::id_for_bytes(&bytes) != descriptor.id {
        return Err(ProjectInsertionError::CorruptAsset(descriptor.id));
    }
    Ok(bytes)
}

fn verify_existing_asset(
    store: &AssetStore,
    descriptor: &AssetDescriptor,
    expected: &[u8],
    cancellation: &dyn CancellationToken,
) -> Result<(), ProjectInsertionError> {
    let path = store.asset_path(descriptor.id);
    let io_error = |source| ProjectInsertionError::Io {
        path: path.clone(),
        source,
    };
    let mut file = File::open(&path).map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() || metadata.len() != descriptor.byte_len {
        return Err(ProjectInsertionError::InvalidAssetLength(descriptor.id));
    }
    let mut buffer = [0_u8; READ_CHUNK];
    let mut offset = 0;
    loop {
        check_cancelled(cancellation)?;
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        let end = offset + read;
        if expected.get(offset..end) != Some(&buffer[..read]) {
            return Err(ProjectInsertionError::CorruptAsset(descriptor.id));
        }
        offset = end;
    }
    if offset != expected.len() {
        return Err(ProjectInsertionError::InvalidAssetLength(descriptor.id));
    }
    Ok(())
}

fn canonicalize(path: &Path) -> Result<PathBuf, ProjectInsertionError> {
    fs::canonicalize(path).map_err(|source| ProjectInsertionError::Io {
        path: path.to_owned(),
        source,
    })
}

fn check_cancelled(cancellation: &dyn CancellationToken) -> Result<(), ProjectInsertionError> {
    if cancellation.is_cancelled() {
        Err(ProjectInsertionError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum ProjectInsertionError {
    #[error("input replay asset {asset_id} is invalid: {message}")]
    InvalidInputReplay { asset_id: AssetId, message: String },
    #[error(transparent)]
    FrameBundle(#[from] gif_from_screen_editor::FrameBundleError),
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Workspace(#[from] EditorWorkspaceError),
    #[error("could not read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("project insertion cancelled")]
    Cancelled,
    #[error("the source project is the destination; use Copy and Paste to duplicate its frames")]
    SameProject,
    #[error("the source project journal needs repair before inserting it")]
    SourceRequiresRepair,
    #[error("the source project has no frames")]
    EmptySource,
    #[error("insertion supports at most 1,000 source frames and 100,000 resulting frames")]
    FrameLimit,
    #[error("insertion supports at most 10,000 source overlay items")]
    OverlayLimit,
    #[error("source and destination canvas color space and background must match")]
    CanvasMismatch,
    #[error("{side} frame {frame_id} cannot be rendered: {reason}")]
    RenderedGeometry {
        side: &'static str,
        frame_id: FrameId,
        reason: String,
    },
    #[error("{side} frame {frame_id} renders at {}x{}, expected {}x{}; normalize both animations to a common output size before insertion", .actual.width.get(), .actual.height.get(), .expected.width.get(), .expected.height.get())]
    RenderedSizeMismatch {
        side: &'static str,
        frame_id: FrameId,
        expected: gif_from_screen_domain::PhysicalSize,
        actual: gif_from_screen_domain::PhysicalSize,
    },
    #[error("the inserted duration would overflow the project timeline")]
    DurationOverflow,
    #[error("inserted assets exceed the 512 MiB input budget or cannot be allocated")]
    AssetBudget,
    #[error("source asset {0} is missing its descriptor")]
    MissingAsset(AssetId),
    #[error("source asset {0} is not a supported raw RGBA raster")]
    UnsupportedAsset(AssetId),
    #[error("asset {0} has an invalid byte length")]
    InvalidAssetLength(AssetId),
    #[error("asset {0} has a damaged content digest")]
    CorruptAsset(AssetId),
    #[error("asset {0} collides with incompatible destination raster metadata")]
    AssetCollision(AssetId),
    #[error("insertion anchor frame {0} no longer exists")]
    UnknownAnchor(FrameId),
    #[error("the destination project changed while insertion was being prepared; prepare it again")]
    StaleTarget,
}

#[cfg(test)]
#[path = "editor_insert_frame_owned_tests.rs"]
mod frame_owned_tests;

#[cfg(test)]
#[path = "editor_insert_input_replay_tests.rs"]
mod input_replay_tests;

#[cfg(test)]
#[path = "editor_insert_geometry_tests.rs"]
mod geometry_tests;

#[cfg(test)]
mod tests {
    use gif_from_screen_domain::{
        AssetKind, BlendMode, Canvas, CanvasBackground, CaptureMetadata, ClipTransform, ColorSpace,
        Effect, FrameClip, PhysicalPoint, PhysicalSize, ProjectId, Rgba, Transition,
        TransitionKind, UnixTimeMs,
    };
    use gif_from_screen_gif::{CancellationFlag, NeverCancel};

    use super::*;

    fn frame_id(number: u128) -> FrameId {
        FrameId::from_u128(number)
    }

    #[test]
    fn insertion_keeps_native_event_clocks_paired_with_capture_timestamps() {
        use gif_from_screen_domain::{KeyStroke, MouseButton, MouseInputEvent};
        let directory = tempfile::tempdir().unwrap();
        let source = workspace(directory.path(), 1, [255, 0, 0, 255]);
        let mut timeline = source.manifest().timeline.clone();
        let metadata = &mut timeline.frames[0].capture_metadata;
        metadata.captured_at = Some(TimeUs::new(20_000));
        metadata.key_strokes.push(KeyStroke {
            physical_key: "x11:38".into(),
            display_text: Some("a".into()),
            pressed: true,
            at: TimeUs::new(19_000),
            repeat: false,
            modifiers: 0,
        });
        metadata.mouse_events.push(MouseInputEvent {
            at: TimeUs::new(19_500),
            button: MouseButton::Left,
            pressed: true,
            position: Some(PhysicalPoint::default()),
        });
        let remapped = remap_frames(&Timeline::default(), &timeline).unwrap();
        assert_eq!(
            remapped.frames[0].capture_metadata,
            timeline.frames[0].capture_metadata
        );
        timeline.frames[0].capture_metadata.captured_at = None;
        timeline.frames[0].capture_metadata.mouse_events.clear();
        let legacy = remap_frames(&Timeline::default(), &timeline).unwrap();
        assert_eq!(
            legacy.frames[0].capture_metadata.key_strokes[0].at,
            TimeUs::new(19_000)
        );
        assert_eq!(legacy.frames[0].capture_clock.unwrap().id, None);
        assert_eq!(legacy.frames[0].capture_sample_time(), Some(TimeUs::ZERO));
    }

    pub(super) fn workspace(root: &Path, frames: usize, color: [u8; 4]) -> EditorWorkspace {
        let size = PhysicalSize::new(2, 1).unwrap();
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(Uuid::new_v4().as_u128()),
            "insertion-test",
            UnixTimeMs::new(0),
            Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        let mut active = ActiveProject::create(root, manifest).unwrap();
        let rgba = [color, color].concat();
        let asset_id = active.assets().put(&rgba).unwrap();
        active
            .commit(EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: AssetDescriptor {
                            id: asset_id,
                            byte_len: 8,
                            kind: AssetKind::Frame {
                                size,
                                encoding: RasterEncoding::Rgba8,
                            },
                        },
                    },
                    EditCommand::InsertFrames {
                        index: 0,
                        frames: (0..frames)
                            .map(|index| FrameClip {
                                render_steps: Vec::new(),
                                capture_clock: None,
                                capture_binding: gif_from_screen_domain::CaptureBinding::Original,
                                id: frame_id(index as u128 + 1),
                                asset_id,
                                duration: DurationUs::new(100_000).unwrap(),
                                transform: ClipTransform::default(),
                                capture_metadata: CaptureMetadata::default(),
                                effects: Vec::new(),
                            })
                            .collect(),
                    },
                ],
            })
            .unwrap();
        EditorWorkspace::from_active(active, 16).unwrap()
    }

    pub(super) fn prepare(
        destination: &EditorWorkspace,
        source: &EditorWorkspace,
        after: Option<FrameId>,
    ) -> PreparedProjectInsertion {
        prepare_project_insertion(
            destination.project_insertion_target(after).unwrap(),
            source.manifest(),
            source.active_project().assets(),
            &NeverCancel,
        )
        .unwrap()
    }

    pub(super) fn same_content(actual: &ProjectManifest, expected: &ProjectManifest) {
        let mut actual = actual.clone();
        actual.revision = expected.revision;
        assert_eq!(&actual, expected);
    }

    pub(super) fn annotate(workspace: &mut EditorWorkspace, color: Rgba) {
        workspace.select_only(frame_id(1)).unwrap();
        workspace.toggle_selection(frame_id(2)).unwrap();
        workspace
            .add_raster_overlay_for_selection(
                super::super::RasterOverlayEdit {
                    name: "Annotation".to_owned(),
                    source_size: PhysicalSize::new(1, 1).unwrap(),
                    display_size: PhysicalSize::new(1, 1).unwrap(),
                    position: PhysicalPoint::default(),
                    item_opacity: 255,
                    track_opacity: 255,
                    blend_mode: BlendMode::Normal,
                    z_index: 1,
                },
                &[color.red, color.green, color.blue, color.alpha],
            )
            .unwrap();
        // These existing insertion tests exercise legacy time-anchor shifting,
        // not the representation chosen by the current new-artwork UI.
        let mut legacy = workspace
            .manifest()
            .timeline
            .overlay_tracks
            .last()
            .unwrap()
            .clone();
        let cells = legacy.frame_cells.take().unwrap();
        let mark = cells[0].marks[0].clone();
        legacy.items = vec![gif_from_screen_domain::OverlayItem {
            id: mark.id,
            span: gif_from_screen_domain::TimelineSpan {
                start: TimeUs::ZERO,
                duration: DurationUs::new(
                    workspace.manifest().timeline.frames[0].duration.get()
                        + workspace.manifest().timeline.frames[1].duration.get(),
                )
                .unwrap(),
            },
            z_index: mark.z_index,
            content: mark.content,
        }];
        workspace
            .execute(EditCommand::UpsertOverlayTrack { track: legacy })
            .unwrap();
        workspace
            .execute(EditCommand::SetTransitions {
                transitions: vec![Transition {
                    id: TransitionId::from_u128(1),
                    from_frame: frame_id(1),
                    to_frame: frame_id(2),
                    duration: DurationUs::new(20_000).unwrap(),
                    steps: 1,
                    kind: TransitionKind::FadeToNext,
                }],
            })
            .unwrap();
    }

    #[test]
    fn insertion_at_start_middle_and_end_remaps_frames_and_reuses_content() {
        for after in [None, Some(frame_id(1)), Some(frame_id(3))] {
            let dest_root = tempfile::tempdir().unwrap();
            let source_root = tempfile::tempdir().unwrap();
            let mut dest = workspace(dest_root.path(), 3, [255, 0, 0, 255]);
            let source = workspace(source_root.path(), 2, [255, 0, 0, 255]);
            let original = dest.manifest().clone();
            let prepared = prepare(&dest, &source, after);
            assert_eq!(prepared.frame_count(), 2);
            let inserted_ids = prepared.inserted_frames.clone();
            assert_eq!(dest.insert_prepared_project(prepared).unwrap(), 2);
            assert_eq!(dest.manifest().assets.len(), 1);
            let index = after.map_or(0, |id| {
                original
                    .timeline
                    .frames
                    .iter()
                    .position(|frame| frame.id == id)
                    .unwrap()
                    + 1
            });
            let ids = dest
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.id)
                .collect::<Vec<_>>();
            assert_eq!(&ids[index..index + 2], &inserted_ids);
            assert!(
                inserted_ids.iter().all(|id| !original
                    .timeline
                    .frames
                    .iter()
                    .any(|frame| frame.id == *id))
            );
            assert_eq!(dest.selection().current(), Some(inserted_ids[0]));
            let edited = dest.manifest().clone();
            for _ in 0..10 {
                assert!(dest.undo().unwrap());
                same_content(dest.manifest(), &original);
                assert!(dest.redo().unwrap());
                same_content(dest.manifest(), &edited);
            }
        }
    }

    #[test]
    fn source_annotations_and_transitions_are_preserved_without_destination_bleed() {
        let dest_root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let mut dest = workspace(dest_root.path(), 3, [255, 0, 0, 255]);
        let mut source = workspace(source_root.path(), 2, [0, 0, 0, 255]);
        annotate(
            &mut dest,
            Rgba {
                red: 255,
                green: 255,
                blue: 0,
                alpha: 255,
            },
        );
        annotate(
            &mut source,
            Rgba {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 255,
            },
        );
        let original = dest.manifest().clone();
        let source_before = source.manifest().clone();
        let prepared = prepare(&dest, &source, Some(frame_id(1)));
        let inserted_ids = prepared.inserted_frames.clone();
        dest.insert_prepared_project(prepared).unwrap();
        let edited = dest.manifest().clone();
        let timeline = &edited.timeline;
        assert_eq!(timeline.transitions.len(), 1);
        assert_eq!(timeline.transitions[0].from_frame, inserted_ids[0]);
        assert_eq!(timeline.transitions[0].to_frame, inserted_ids[1]);
        assert_ne!(
            timeline.transitions[0].id,
            source_before.timeline.transitions[0].id
        );
        let spans = timeline.overlay_tracks[0]
            .items
            .iter()
            .map(|item| (item.span.start.get(), item.span.end().unwrap().get()))
            .collect::<Vec<_>>();
        assert_eq!(spans, [(0, 100_000), (300_000, 400_000)]);
        let imported_track = &timeline.overlay_tracks[1];
        let source_track = &source_before.timeline.overlay_tracks[0];
        assert_ne!(imported_track.id, source_track.id);
        assert_ne!(imported_track.items[0].id, source_track.items[0].id);
        assert_eq!(
            imported_track.items[0].content,
            source_track.items[0].content
        );
        assert_eq!(imported_track.items[0].span.start.get(), 100_000);
        assert_eq!(imported_track.items[0].span.duration.get(), 200_000);
        same_content(source.manifest(), &source_before);
        for _ in 0..10 {
            dest.undo().unwrap();
            same_content(dest.manifest(), &original);
            dest.redo().unwrap();
            same_content(dest.manifest(), &edited);
        }
        drop(dest);
        let reopened =
            EditorWorkspace::open(dest_root.path(), LockPolicy::FailIfPresent, 16).unwrap();
        same_content(reopened.manifest(), &edited);
        let output = dest_root.path().join("inserted.gif");
        gif_from_screen_application::export_project_snapshot_to_gif(
            &gif_from_screen_application::ProjectExportSnapshot::from_active(
                reopened.active_project(),
            ),
            &output,
            &gif_from_screen_application::ProjectGifExportOptions::default(),
            &NeverCancel,
            &mut gif_from_screen_application::NoopProjectExportProgress,
        )
        .unwrap();
        let decoded = gif_from_screen_media::decode_gif(
            File::open(output).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap();
        // Identical source/transition pixels merge without losing their timing.
        assert_eq!(decoded.frames().len(), 4);
        assert_eq!(
            decoded
                .frames()
                .iter()
                .map(gif_from_screen_media::DecodedFrame::duration_us)
                .sum::<u64>(),
            520_000
        );
        assert_eq!(&decoded.frames()[1].rgba()[..4], &[0, 0, 255, 255]);
    }

    fn legacy_progress_fixture(current: &EditorWorkspace) -> gif_from_screen_domain::OverlayTrack {
        let mut track = current.manifest().timeline.overlay_tracks[0].clone();
        // Keep the legacy absolute-scope insertion fixture independent of the
        // representation chosen by current annotation authoring.
        track.items = track
            .frame_cells
            .take()
            .unwrap()
            .into_iter()
            .flat_map(|cell| {
                assert!(cell.input_replay.is_none());
                let owner = current
                    .manifest()
                    .timeline
                    .frames
                    .iter()
                    .find(|frame| frame.id == cell.frame_id)
                    .unwrap();
                let span = gif_from_screen_domain::TimelineSpan {
                    start: current.manifest().timeline.frame_start(owner.id).unwrap(),
                    duration: owner.duration,
                };
                cell.marks
                    .into_iter()
                    .map(move |mark| gif_from_screen_domain::OverlayItem {
                        id: mark.id,
                        span,
                        z_index: mark.z_index,
                        content: mark.content,
                    })
            })
            .collect();
        track
    }

    #[test]
    fn whole_project_insertion_offsets_source_scope_and_excludes_it_from_destination_scope() {
        use gif_from_screen_domain::{
            AnnotationMode, AnnotationRequest, ProgressOptions, TimelineSpan,
        };
        let dest_root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let mut dest = workspace(dest_root.path(), 3, [255, 0, 0, 255]);
        let mut source = workspace(source_root.path(), 2, [0, 0, 0, 255]);
        for current in [&mut dest, &mut source] {
            current.select_all();
            let request = AnnotationRequest {
                size: current.manifest().canvas.size,
                mode: AnnotationMode::Progress(ProgressOptions {
                    format: String::new(),
                    ..ProgressOptions::default()
                }),
                ..AnnotationRequest::default()
            };
            current
                .apply_annotation_edit(
                    &current.project_edit_anchor(),
                    &request,
                    &std::sync::atomic::AtomicBool::new(false),
                    |_| {},
                )
                .unwrap();
            let mut track = legacy_progress_fixture(current);
            track.annotation_scope = Some(vec![TimelineSpan {
                start: TimeUs::ZERO,
                duration: DurationUs::new(
                    current.manifest().timeline.total_duration().unwrap().get(),
                )
                .unwrap(),
            }]);
            current
                .execute(EditCommand::UpsertOverlayTrack { track })
                .unwrap();
        }
        let before = dest.manifest().clone();
        let prepared = prepare(&dest, &source, Some(frame_id(1)));
        dest.insert_prepared_project(prepared).unwrap();
        let scopes: Vec<_> = dest
            .manifest()
            .timeline
            .overlay_tracks
            .iter()
            .map(|track| {
                track
                    .annotation_scope
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|span| (span.start.get(), span.end().unwrap().get()))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(
            scopes,
            [
                vec![(0, 100_000), (300_000, 500_000)],
                vec![(100_000, 300_000)]
            ]
        );
        let updated = dest.manifest().clone();
        dest.undo().unwrap();
        same_content(dest.manifest(), &before);
        dest.redo().unwrap();
        same_content(dest.manifest(), &updated);
        let saved = dest.manifest().clone();
        drop(dest);
        let reopened =
            EditorWorkspace::open(dest_root.path(), LockPolicy::FailIfPresent, 32).unwrap();
        assert_eq!(reopened.manifest(), &saved);
    }

    #[test]
    fn stale_target_rejects_without_an_extra_revision_or_history_entry() {
        let dest_root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let mut dest = workspace(dest_root.path(), 2, [255, 0, 0, 255]);
        let source = workspace(source_root.path(), 1, [0, 0, 0, 255]);
        let prepared = prepare(&dest, &source, None);
        dest.select_only(frame_id(1)).unwrap();
        dest.override_selection_duration(DurationUs::new(200_000).unwrap())
            .unwrap();
        let edited = dest.manifest().clone();
        let undo_len = dest.undo.len();
        assert!(matches!(
            dest.insert_prepared_project(prepared),
            Err(ProjectInsertionError::StaleTarget)
        ));
        assert_eq!(dest.manifest(), &edited);
        assert_eq!(dest.undo.len(), undo_len);
    }

    #[test]
    fn cancellation_corrupt_asset_and_canvas_mismatch_do_not_mutate_destination() {
        let dest_root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let dest = workspace(dest_root.path(), 2, [255, 0, 0, 255]);
        let source = workspace(source_root.path(), 1, [0, 0, 0, 255]);
        let original = dest.manifest().clone();
        let cancelled = CancellationFlag::default();
        cancelled.cancel();
        assert!(matches!(
            prepare_project_insertion(
                dest.project_insertion_target(None).unwrap(),
                source.manifest(),
                source.active_project().assets(),
                &cancelled
            ),
            Err(ProjectInsertionError::Cancelled)
        ));
        let mut resized = source.manifest().clone();
        resized.canvas.background = CanvasBackground::Solid(Rgba {
            red: 1,
            green: 2,
            blue: 3,
            alpha: 255,
        });
        assert!(matches!(
            prepare_project_insertion(
                dest.project_insertion_target(None).unwrap(),
                &resized,
                source.active_project().assets(),
                &NeverCancel
            ),
            Err(ProjectInsertionError::CanvasMismatch)
        ));
        let id = source.manifest().timeline.frames[0].asset_id;
        fs::write(source.active_project().assets().asset_path(id), [1; 8]).unwrap();
        assert!(matches!(
            prepare_project_insertion(
                dest.project_insertion_target(None).unwrap(),
                source.manifest(),
                source.active_project().assets(),
                &NeverCancel
            ),
            Err(ProjectInsertionError::CorruptAsset(_))
        ));
        assert_eq!(dest.manifest(), &original);
        assert_eq!(
            fs::read_dir(dest.active_project().assets().directory())
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn incompatible_hash_metadata_is_rejected_before_storing_assets() {
        let dest_root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let dest = workspace(dest_root.path(), 2, [255, 0, 0, 255]);
        let source = workspace(source_root.path(), 1, [255, 0, 0, 255]);
        let mut manifest = source.manifest().clone();
        let descriptor = manifest.assets.values_mut().next().unwrap();
        descriptor.kind = AssetKind::Frame {
            size: PhysicalSize::new(1, 2).unwrap(),
            encoding: RasterEncoding::Rgba8,
        };
        manifest.timeline.frames[0].transform.output_size = Some(manifest.canvas.size);
        assert!(matches!(
            prepare_project_insertion(
                dest.project_insertion_target(None).unwrap(),
                &manifest,
                source.active_project().assets(),
                &NeverCancel
            ),
            Err(ProjectInsertionError::AssetCollision(_))
        ));
    }

    #[test]
    fn path_preparation_honors_source_lock_and_rejects_self_alias() {
        let dest_root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let mut dest = workspace(dest_root.path(), 2, [255, 0, 0, 255]);
        let source = workspace(source_root.path(), 1, [0, 0, 0, 255]);
        assert!(matches!(
            prepare_project_insertion_from_path(
                dest.project_insertion_target(None).unwrap(),
                source_root.path(),
                &NeverCancel
            ),
            Err(ProjectInsertionError::Project(
                ProjectError::AlreadyLocked { .. }
            ))
        ));
        assert!(matches!(
            prepare_project_insertion_from_path(
                dest.project_insertion_target(None).unwrap(),
                &dest_root.path().join("."),
                &NeverCancel
            ),
            Err(ProjectInsertionError::SameProject)
        ));
        assert!(matches!(
            prepare_project_insertion(
                dest.project_insertion_target(None).unwrap(),
                dest.manifest(),
                dest.active_project().assets(),
                &NeverCancel
            ),
            Err(ProjectInsertionError::SameProject)
        ));
        drop(source);
        let prepared = prepare_project_insertion_from_path(
            dest.project_insertion_target(None).unwrap(),
            source_root.path(),
            &NeverCancel,
        )
        .unwrap();
        assert!(!prepared.source_label.is_empty());
        dest.insert_prepared_project(prepared).unwrap();
        assert_eq!(dest.manifest().timeline.frames.len(), 3);
    }

    #[test]
    fn cursor_assets_are_imported_unused_assets_skipped_and_effect_references_remain_complete() {
        let dest_root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let mut dest = workspace(dest_root.path(), 1, [255, 0, 0, 255]);
        let mut source = workspace(source_root.path(), 1, [0, 0, 0, 255]);
        let cursor = source
            .active_project()
            .assets()
            .put(&[9, 9, 9, 255])
            .unwrap();
        let mask = source
            .active_project()
            .assets()
            .put(&[8, 8, 8, 255])
            .unwrap();
        let unused = source
            .active_project()
            .assets()
            .put(&[7, 7, 7, 255])
            .unwrap();
        let mut commands = [cursor, mask, unused]
            .into_iter()
            .map(|id| EditCommand::RegisterAsset {
                asset: AssetDescriptor {
                    id,
                    byte_len: 4,
                    kind: AssetKind::Mask {
                        size: PhysicalSize::new(1, 1).unwrap(),
                        encoding: RasterEncoding::Rgba8,
                    },
                },
            })
            .collect::<Vec<_>>();
        let mut replacement = source.manifest().timeline.frames[0].clone();
        replacement.capture_metadata.cursor_asset = Some(cursor);
        commands.push(EditCommand::ReplaceFrame {
            frame_id: replacement.id,
            replacement: Box::new(replacement),
        });
        source.execute(EditCommand::Compound { commands }).unwrap();
        // Enumeration must still retain unsupported effect references, without
        // using an unrenderable effect as the successful insertion fixture.
        let mut raw = source.manifest().clone();
        raw.timeline.frames[0].effects.push(Effect::Cinemagraph {
            mask_asset: mask,
            invert_mask: false,
        });
        raw.validate().unwrap();
        assert!(referenced_assets(&raw).contains(&mask));
        raw.timeline.frames[0].effects.clear();
        raw.timeline.frames[0].render_steps = vec![
            gif_from_screen_domain::FrameRenderStep::Composite { stage_id: 1 },
            gif_from_screen_domain::FrameRenderStep::Effect {
                effect: Effect::Cinemagraph {
                    mask_asset: mask,
                    invert_mask: false,
                },
            },
        ];
        assert!(referenced_assets(&raw).contains(&mask));
        dest.insert_prepared_project(prepare(&dest, &source, None))
            .unwrap();
        assert!(dest.manifest().assets.contains_key(&cursor));
        assert!(!dest.manifest().assets.contains_key(&mask));
        assert!(!dest.manifest().assets.contains_key(&unused));
    }

    #[test]
    fn frame_limits_overflow_and_missing_cursor_metadata_fail_before_writes() {
        let dest_root = tempfile::tempdir().unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let dest = workspace(dest_root.path(), 1, [255, 0, 0, 255]);
        let source = workspace(source_root.path(), 1, [0, 0, 0, 255]);
        let mut excessive = source.manifest().clone();
        excessive.timeline.frames =
            vec![excessive.timeline.frames[0].clone(); MAX_INSERTED_FRAMES + 1];
        assert!(matches!(
            prepare_project_insertion(
                dest.project_insertion_target(None).unwrap(),
                &excessive,
                source.active_project().assets(),
                &NeverCancel
            ),
            Err(ProjectInsertionError::FrameLimit)
        ));
        let mut overflow = source.manifest().clone();
        overflow.timeline.frames[0].duration = DurationUs::new(u64::MAX).unwrap();
        assert!(matches!(
            prepare_project_insertion(
                dest.project_insertion_target(None).unwrap(),
                &overflow,
                source.active_project().assets(),
                &NeverCancel
            ),
            Err(ProjectInsertionError::DurationOverflow)
        ));
        let mut missing = source.manifest().clone();
        missing.timeline.frames[0].capture_metadata.cursor_asset =
            Some(AssetId::from_digest([42; 32]));
        assert!(
            prepare_project_insertion(
                dest.project_insertion_target(None).unwrap(),
                &missing,
                source.active_project().assets(),
                &NeverCancel
            )
            .is_err()
        );
        assert_eq!(
            fs::read_dir(dest.active_project().assets().directory())
                .unwrap()
                .count(),
            1
        );
    }
}
