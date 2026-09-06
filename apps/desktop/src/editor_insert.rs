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
    AssetDescriptor, AssetId, DurationUs, EditCommand, Effect, FrameClip, FrameId, OverlayId,
    ProjectManifest, ProjectRevision, RasterEncoding, TimeUs, Timeline, TrackId, TransitionId,
};
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
    let needed = referenced_assets(source);
    let descriptors = checked_descriptors(&target, source, &needed)?;
    let (command, inserted_frames) = insertion_command(&target, source, &descriptors)?;
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
    let mut bytes = Vec::with_capacity(descriptors.len());
    for descriptor in &descriptors {
        let pixels = read_asset(source_assets, descriptor, cancellation)?;
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
            .map(|track| track.items.len())
            .try_fold(0_usize, usize::checked_add)
            .is_none_or(|items| items > MAX_OVERLAY_ITEMS)
    {
        return Err(ProjectInsertionError::OverlayLimit);
    }
    source.validate().map_err(ProjectError::from)?;
    if source.canvas != target.manifest.canvas {
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

fn referenced_assets(source: &ProjectManifest) -> BTreeSet<AssetId> {
    let mut needed = BTreeSet::new();
    for frame in &source.timeline.frames {
        needed.insert(frame.asset_id);
        needed.extend(frame.capture_metadata.cursor_asset);
        needed.extend(frame.effects.iter().filter_map(Effect::referenced_asset));
    }
    needed.extend(
        source
            .timeline
            .overlay_tracks
            .iter()
            .flat_map(|track| &track.items)
            .filter_map(|item| item.content.referenced_asset()),
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
            let Some((size, RasterEncoding::Rgba8)) = descriptor.kind.raster_descriptor() else {
                return Err(ProjectInsertionError::UnsupportedAsset(*id));
            };
            let expected = u64::from(size.width.get())
                .checked_mul(u64::from(size.height.get()))
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or(ProjectInsertionError::AssetBudget)?;
            if expected != descriptor.byte_len {
                return Err(ProjectInsertionError::InvalidAssetLength(*id));
            }
            total = total
                .checked_add(descriptor.byte_len)
                .filter(|total| *total <= MAX_INSERTED_ASSET_BYTES)
                .ok_or(ProjectInsertionError::AssetBudget)?;
            if let Some(existing) = target.manifest.assets.get(id)
                && (existing.kind.raster_descriptor() != descriptor.kind.raster_descriptor()
                    || existing.byte_len != descriptor.byte_len)
            {
                return Err(ProjectInsertionError::AssetCollision(*id));
            }
            Ok(descriptor.clone())
        })
        .collect()
}

fn insertion_command(
    target: &ProjectInsertionTarget,
    source: &ProjectManifest,
    descriptors: &[AssetDescriptor],
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
    } = remap_frames(timeline, &source.timeline, start)?;
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
    for track in &timeline.overlay_tracks {
        let mut shifted = track.clone();
        shifted.items = super::text::exclude_inserted_title(&track.items, start, duration)?;
        commands.push(EditCommand::UpsertOverlayTrack { track: shifted });
    }
    append_source_tracks(&mut commands, timeline, &source.timeline, start)?;
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
    Ok((EditCommand::Compound { commands }, inserted_frames))
}

fn append_source_tracks(
    commands: &mut Vec<EditCommand>,
    destination: &Timeline,
    source: &Timeline,
    start: u64,
) -> Result<(), ProjectInsertionError> {
    let mut used_tracks = destination
        .overlay_tracks
        .iter()
        .chain(&source.overlay_tracks)
        .map(|track| track.id)
        .collect::<BTreeSet<_>>();
    let mut used_items = destination
        .overlay_tracks
        .iter()
        .chain(&source.overlay_tracks)
        .flat_map(|track| &track.items)
        .map(|item| item.id)
        .collect::<BTreeSet<_>>();
    for track in &source.overlay_tracks {
        let mut imported = track.clone();
        imported.id = loop {
            let id = TrackId::from_u128(Uuid::new_v4().as_u128());
            if used_tracks.insert(id) {
                break id;
            }
        };
        for item in &mut imported.items {
            item.id = loop {
                let id = OverlayId::from_u128(Uuid::new_v4().as_u128());
                if used_items.insert(id) {
                    break id;
                }
            };
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
    start: u64,
) -> Result<RemappedFrames, ProjectInsertionError> {
    let mut used_frames = destination
        .frames
        .iter()
        .chain(&source.frames)
        .map(|frame| frame.id)
        .collect::<BTreeSet<_>>();
    let mut by_source = BTreeMap::new();
    let mut frames = source.frames.clone();
    for frame in &mut frames {
        let id = loop {
            let id = FrameId::from_u128(Uuid::new_v4().as_u128());
            if used_frames.insert(id) {
                break id;
            }
        };
        by_source.insert(frame.id, id);
        frame.id = id;
        for key in &mut frame.capture_metadata.key_strokes {
            key.at = TimeUs::new(
                key.at
                    .get()
                    .checked_add(start)
                    .ok_or(ProjectInsertionError::DurationOverflow)?,
            );
        }
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
    #[error("source and destination canvas size, color space, and background must match")]
    CanvasMismatch,
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
mod tests {
    use gif_from_screen_domain::{
        AssetKind, BlendMode, Canvas, CanvasBackground, CaptureMetadata, ClipTransform, ColorSpace,
        FrameClip, PhysicalPoint, PhysicalSize, ProjectId, Rgba, Transition, TransitionKind,
        UnixTimeMs,
    };
    use gif_from_screen_gif::{CancellationFlag, NeverCancel};

    use super::*;

    fn frame_id(number: u128) -> FrameId {
        FrameId::from_u128(number)
    }

    fn workspace(root: &Path, frames: usize, color: [u8; 4]) -> EditorWorkspace {
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

    fn prepare(
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

    fn same_content(actual: &ProjectManifest, expected: &ProjectManifest) {
        let mut actual = actual.clone();
        actual.revision = expected.revision;
        assert_eq!(&actual, expected);
    }

    fn annotate(workspace: &mut EditorWorkspace, color: Rgba) {
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
        resized.canvas.size = PhysicalSize::new(3, 1).unwrap();
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
    fn cursor_and_effect_assets_are_imported_and_unused_assets_are_skipped() {
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
        replacement
            .effects
            .push(gif_from_screen_domain::Effect::Cinemagraph {
                mask_asset: mask,
                invert_mask: false,
            });
        commands.push(EditCommand::ReplaceFrame {
            frame_id: replacement.id,
            replacement,
        });
        source.execute(EditCommand::Compound { commands }).unwrap();
        dest.insert_prepared_project(prepare(&dest, &source, None))
            .unwrap();
        assert!(dest.manifest().assets.contains_key(&cursor));
        assert!(dest.manifest().assets.contains_key(&mask));
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
