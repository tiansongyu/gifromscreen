use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
    DomainError, DurationUs, FrameClip, FrameId, PhysicalSize, ProjectId, ProjectManifest,
    RasterEncoding, SourceProvenance, UnitError, UnixTimeMs,
};
use gif_from_screen_gif::RgbaFrame;
use gif_from_screen_project::{ActiveProject, ProjectError};
use gif_from_screen_workflow::{CollectedRecording, RecordingMetadata};
use thiserror::Error;

use crate::rgba_project::{
    PersistRgbaProjectError, RgbaProjectFrame, RgbaProjectOptions,
    persist_rgba_project_with_metadata,
};

/// Deterministic identifiers and user-facing metadata for a captured project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingProjectOptions {
    /// Stable identifier assigned to the new project.
    pub project_id: ProjectId,
    /// Frame identifiers consumed in recording order. Extra identifiers are ignored.
    pub frame_ids: Vec<FrameId>,
    /// Application version persisted in the project manifest.
    pub app_version: String,
    /// Wall-clock project creation timestamp.
    pub created_at: UnixTimeMs,
    /// Optional display label for the captured screen/window source.
    pub source_label: Option<String>,
}

/// Metadata required before the first frame of an incrementally persisted recording arrives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalRecordingProjectOptions {
    /// Stable identifier assigned to the new project.
    pub project_id: ProjectId,
    /// Application version persisted in the project manifest.
    pub app_version: String,
    /// Wall-clock project creation timestamp.
    pub created_at: UnixTimeMs,
    /// Optional display label for the captured screen/window source.
    pub source_label: Option<String>,
}

/// Summary of frames durably journaled while capture is still active.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IncrementalRecordingSummary {
    /// Number of frames represented by the recoverable project journal.
    pub frames: usize,
    /// Total presentation duration represented by those frames.
    pub duration_us: u64,
}

/// Number of appended frames between automatic recording checkpoints.
///
/// This bounds crash-recovery replay without rewriting the complete manifest
/// for every captured frame.
pub const INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES: usize = 512;

/// Failure while creating, appending to, or finalizing an incremental recording project.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IncrementalRecordingProjectError {
    /// The caller attempted to finalize a recording before any frame was persisted.
    #[error("cannot finalize an empty incremental recording")]
    EmptyRecording,

    /// A frame identity may never use the reserved nil value.
    #[error("frame {frame_index} has the reserved nil identity")]
    NilFrameId {
        /// Zero-based position that the frame would have occupied.
        frame_index: usize,
    },

    /// A frame identity must be unique for the lifetime of the project.
    #[error("frame {frame_id} already exists in the incremental recording")]
    DuplicateFrameId {
        /// Repeated stable identity.
        frame_id: FrameId,
    },

    /// The bounded in-memory frame lookup could not grow before persistence.
    #[error("could not reserve the incremental recording index for frame {frame_index}")]
    FrameIndexAllocationFailed {
        /// Zero-based position that would have been indexed.
        frame_index: usize,
    },

    /// Every recording frame must match the fixed project canvas.
    #[error(
        "frame {frame_index} dimensions {actual_width}x{actual_height} do not match recording canvas {expected_width}x{expected_height}"
    )]
    DimensionMismatch {
        /// Zero-based position rejected by the writer.
        frame_index: usize,
        /// Canvas width established before recording.
        expected_width: u32,
        /// Canvas height established before recording.
        expected_height: u32,
        /// Supplied frame width.
        actual_width: u16,
        /// Supplied frame height.
        actual_height: u16,
    },

    /// A frame's byte count cannot be represented by the persisted descriptor.
    #[error("frame {frame_index} byte length cannot be represented as u64")]
    AssetLengthOutOfRange {
        /// Zero-based position rejected by the writer.
        frame_index: usize,
    },

    /// Content-identical raster bytes were already registered with incompatible dimensions.
    #[error("cursor image content conflicts with an existing raster asset's dimensions")]
    CursorAssetConflict,

    /// The supplied frame duration could not be represented by the project model.
    #[error("frame {frame_index} has invalid duration {duration_us} microseconds")]
    InvalidFrameDuration {
        /// Zero-based position rejected by the writer.
        frame_index: usize,
        /// Rejected duration.
        duration_us: u64,
    },

    /// A duration update referred to a frame that has not been persisted.
    #[error("cannot update unknown recording frame {frame_id}")]
    UnknownFrame {
        /// Stable identity requested by the caller.
        frame_id: FrameId,
    },

    /// Creating the empty, recoverable project failed.
    #[error("could not create incremental recording project: {source}")]
    CreateProject {
        /// Project storage or domain failure.
        #[source]
        source: ProjectError,
    },

    /// Storing one immutable RGBA asset failed.
    #[error("could not store incremental recording frame {frame_index}: {source}")]
    StoreAsset {
        /// Zero-based frame position.
        frame_index: usize,
        /// Content-addressed storage failure.
        #[source]
        source: ProjectError,
    },

    /// Appending a frame or duration update to the durable journal failed.
    #[error("could not journal incremental recording edit: {source}")]
    Commit {
        /// Project storage or domain failure.
        #[source]
        source: ProjectError,
    },

    /// Writing an explicit manifest checkpoint failed.
    #[error("could not checkpoint incremental recording project: {source}")]
    Checkpoint {
        /// Project storage failure.
        #[source]
        source: ProjectError,
    },

    /// Final snapshot or journal compaction failed. The journal remains the recovery source.
    #[error("could not finalize incremental recording project: {source}")]
    Finalize {
        /// Project storage failure.
        #[source]
        source: ProjectError,
    },
}

/// A project writer that durably appends captured frames before recording stops.
///
/// Creation writes an empty manifest immediately. Each successful [`Self::append_frame`] stores
/// immutable pixels and synchronously appends a checksummed journal record, so dropping the writer
/// without calling [`Self::finish`] intentionally leaves a project that can be recovered by
/// [`ActiveProject::open`]. At most the caller's not-yet-appended frame can be lost in a process
/// crash.
#[derive(Debug)]
pub struct IncrementalRecordingProject {
    project: ActiveProject,
    canvas: PhysicalSize,
    frame_durations: HashMap<FrameId, DurationUs>,
    duration_us: u64,
    capture_clock_id: gif_from_screen_domain::CaptureClockId,
}

impl IncrementalRecordingProject {
    /// Creates the empty snapshot used as the recovery anchor for an active recording.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid metadata/canvas values, an existing project, a lock conflict,
    /// or an I/O failure. Existing project contents are never replaced.
    pub fn create(
        root: impl AsRef<Path>,
        canvas: PhysicalSize,
        options: IncrementalRecordingProjectOptions,
    ) -> Result<Self, IncrementalRecordingProjectError> {
        let provenance = SourceProvenance::Screen {
            source_label: options.source_label.clone(),
        };
        Self::create_with_provenance(root, canvas, options, provenance)
    }

    pub(crate) fn create_with_provenance(
        root: impl AsRef<Path>,
        canvas: PhysicalSize,
        options: IncrementalRecordingProjectOptions,
        provenance: SourceProvenance,
    ) -> Result<Self, IncrementalRecordingProjectError> {
        let mut manifest = ProjectManifest::new(
            options.project_id,
            options.app_version,
            options.created_at,
            Canvas {
                size: canvas,
                color_space: gif_from_screen_domain::ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .map_err(ProjectError::from)
        .map_err(|source| IncrementalRecordingProjectError::CreateProject { source })?;
        manifest.source_provenance = vec![provenance];
        manifest
            .validate()
            .map_err(ProjectError::from)
            .map_err(|source| IncrementalRecordingProjectError::CreateProject { source })?;
        let project = ActiveProject::create(root, manifest)
            .map_err(|source| IncrementalRecordingProjectError::CreateProject { source })?;
        Ok(Self {
            project,
            canvas,
            frame_durations: HashMap::new(),
            duration_us: 0,
            capture_clock_id: fresh_capture_clock_id(),
        })
    }

    /// Stores and journals one complete frame at the end of the active timeline.
    ///
    /// If the pixels already exist, their content-addressed asset is reused. Input validation is
    /// performed before writing an asset, and a failed journal append never mutates the in-memory
    /// manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for nil/duplicate frame identities, a canvas mismatch, an unrepresentable
    /// asset length, or a storage/journal failure.
    pub fn append_frame(
        &mut self,
        frame_id: FrameId,
        frame: &RgbaFrame,
    ) -> Result<(), IncrementalRecordingProjectError> {
        self.append_frame_with_metadata(frame_id, frame, None)
    }

    /// Appends a frame together with its native input events and immutable cursor image.
    /// Native event timestamps retain the original active recording clock, excluding pauses.
    ///
    /// # Errors
    /// Returns validation, asset-storage, or journal errors without mutating the active timeline.
    pub fn append_frame_with_metadata(
        &mut self,
        frame_id: FrameId,
        frame: &RgbaFrame,
        metadata: Option<&RecordingMetadata>,
    ) -> Result<(), IncrementalRecordingProjectError> {
        let frame_index = self.project.manifest().timeline.frames.len();
        if frame_id.is_nil() {
            return Err(IncrementalRecordingProjectError::NilFrameId { frame_index });
        }
        if self.frame_durations.contains_key(&frame_id) {
            return Err(IncrementalRecordingProjectError::DuplicateFrameId { frame_id });
        }
        if u32::from(frame.width()) != self.canvas.width.get()
            || u32::from(frame.height()) != self.canvas.height.get()
        {
            return Err(IncrementalRecordingProjectError::DimensionMismatch {
                frame_index,
                expected_width: self.canvas.width.get(),
                expected_height: self.canvas.height.get(),
                actual_width: frame.width(),
                actual_height: frame.height(),
            });
        }
        let duration = DurationUs::new(frame.duration_us()).ok_or(
            IncrementalRecordingProjectError::InvalidFrameDuration {
                frame_index,
                duration_us: frame.duration_us(),
            },
        )?;
        let next_duration_us = self.duration_us.checked_add(duration.get()).ok_or(
            IncrementalRecordingProjectError::InvalidFrameDuration {
                frame_index,
                duration_us: frame.duration_us(),
            },
        )?;
        self.frame_durations.try_reserve(1).map_err(|_| {
            IncrementalRecordingProjectError::FrameIndexAllocationFailed { frame_index }
        })?;
        let byte_len = u64::try_from(frame.pixels().len())
            .map_err(|_| IncrementalRecordingProjectError::AssetLengthOutOfRange { frame_index })?;
        let asset_id = self
            .project
            .assets()
            .put(frame.pixels())
            .map_err(|source| IncrementalRecordingProjectError::StoreAsset {
                frame_index,
                source,
            })?;
        let (capture_metadata, new_cursor_asset) =
            self.store_cursor_metadata(metadata, asset_id, frame_index)?;
        let clip = FrameClip {
            capture_clock: capture_metadata.captured_at.map(|sampled_at| {
                gif_from_screen_domain::CaptureClockContext {
                    id: Some(self.capture_clock_id),
                    sampled_at,
                }
            }),
            capture_binding: if metadata.is_some() {
                gif_from_screen_domain::CaptureBinding::Original
            } else {
                gif_from_screen_domain::CaptureBinding::NotRecorded
            },
            id: frame_id,
            asset_id,
            duration,
            transform: ClipTransform::default(),
            capture_metadata,
            effects: Vec::new(),
        };
        let new_asset =
            (!self.project.manifest().assets.contains_key(&asset_id)).then_some(AssetDescriptor {
                id: asset_id,
                byte_len,
                kind: AssetKind::Frame {
                    size: self.canvas,
                    encoding: RasterEncoding::Rgba8,
                },
            });
        self.project
            .commit_recording_append_with_cursor(new_asset, new_cursor_asset, clip)
            .map_err(|source| IncrementalRecordingProjectError::Commit { source })?;
        self.frame_durations.insert(frame_id, duration);
        self.duration_us = next_duration_us;
        if self
            .frame_durations
            .len()
            .is_multiple_of(INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES)
        {
            self.project
                .checkpoint_and_compact()
                .map_err(|source| IncrementalRecordingProjectError::Checkpoint { source })?;
        }
        Ok(())
    }

    fn store_cursor_metadata(
        &self,
        metadata: Option<&RecordingMetadata>,
        asset_id: gif_from_screen_domain::AssetId,
        frame_index: usize,
    ) -> Result<(CaptureMetadata, Option<AssetDescriptor>), IncrementalRecordingProjectError> {
        let mut capture_metadata = metadata.map(domain_capture_metadata).unwrap_or_default();
        let mut new_cursor_asset = None;
        if let Some(image) = metadata.and_then(|metadata| metadata.cursor_image.as_ref()) {
            let size = PhysicalSize {
                width: gif_from_screen_domain::PhysicalPx::new(image.size().width()),
                height: gif_from_screen_domain::PhysicalPx::new(image.size().height()),
            };
            let cursor_id = gif_from_screen_project::AssetStore::id_for_bytes(image.pixels());
            if self
                .project
                .manifest()
                .assets
                .get(&cursor_id)
                .is_some_and(|asset| {
                    asset.kind.raster_descriptor() != Some((size, RasterEncoding::Rgba8))
                })
                || (cursor_id == asset_id && size != self.canvas)
            {
                return Err(IncrementalRecordingProjectError::CursorAssetConflict);
            }
            let cursor_id = self
                .project
                .assets()
                .put(image.pixels())
                .map_err(|source| IncrementalRecordingProjectError::StoreAsset {
                    frame_index,
                    source,
                })?;
            capture_metadata.cursor_asset = Some(cursor_id);
            if !self.project.manifest().assets.contains_key(&cursor_id) && cursor_id != asset_id {
                new_cursor_asset = Some(AssetDescriptor {
                    id: cursor_id,
                    byte_len: image.pixels().len() as u64,
                    kind: AssetKind::OverlayImage {
                        size,
                        encoding: RasterEncoding::Rgba8,
                    },
                });
            }
        }
        Ok((capture_metadata, new_cursor_asset))
    }

    /// Replaces the duration of a previously journaled frame.
    ///
    /// This supports capture pipelines that append the newest frame with a safe provisional tail
    /// duration and finalize it once the next capture timestamp is known.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame is unknown or the journal update fails.
    pub fn set_frame_duration(
        &mut self,
        frame_id: FrameId,
        duration: DurationUs,
    ) -> Result<bool, IncrementalRecordingProjectError> {
        let Some(previous) = self.frame_durations.get(&frame_id).copied() else {
            return Err(IncrementalRecordingProjectError::UnknownFrame { frame_id });
        };
        if previous == duration {
            return Ok(false);
        }
        let next_duration_us = self
            .duration_us
            .checked_sub(previous.get())
            .and_then(|total| total.checked_add(duration.get()))
            .ok_or(IncrementalRecordingProjectError::InvalidFrameDuration {
                frame_index: self.frame_durations.len(),
                duration_us: duration.get(),
            })?;
        self.project
            .commit_recording_duration(frame_id, duration)
            .map_err(|source| IncrementalRecordingProjectError::Commit { source })?;
        self.frame_durations.insert(frame_id, duration);
        self.duration_us = next_duration_us;
        Ok(true)
    }

    /// Returns the number and total duration currently protected by the journal.
    pub fn summary(&self) -> IncrementalRecordingSummary {
        IncrementalRecordingSummary {
            frames: self.project.manifest().timeline.frames.len(),
            duration_us: self.duration_us,
        }
    }

    /// Atomically checkpoints the current revision without truncating the recovery journal.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be validated or durably replaced.
    pub fn checkpoint(&self) -> Result<(), IncrementalRecordingProjectError> {
        self.project
            .checkpoint()
            .map_err(|source| IncrementalRecordingProjectError::Checkpoint { source })
    }

    /// Writes the final snapshot, compacts the represented journal, and yields the editor project.
    ///
    /// # Errors
    ///
    /// Empty recordings are rejected. If finalization fails, already-journaled frames remain
    /// recoverable on disk.
    pub fn finish(mut self) -> Result<ActiveProject, IncrementalRecordingProjectError> {
        if self.project.manifest().timeline.frames.is_empty() {
            return Err(IncrementalRecordingProjectError::EmptyRecording);
        }
        self.project
            .checkpoint_and_compact()
            .map_err(|source| IncrementalRecordingProjectError::Finalize { source })?;
        Ok(self.project)
    }

    /// Path layout of the live project, useful for user-facing recovery messages.
    pub fn root(&self) -> &Path {
        &self.project.layout().root
    }
}

/// Failure while converting an in-memory recording into an editable project.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PersistRecordingError {
    /// The workflow returned no frame to persist.
    #[error("cannot create a project from an empty recording")]
    EmptyRecording,

    /// The injected frame-id sequence ended before every frame had an id.
    #[error("recording has {required} frames but only {provided} frame ids were supplied")]
    InsufficientFrameIds {
        /// Number of recorded frames requiring identifiers.
        required: usize,
        /// Number of identifiers supplied by the caller.
        provided: usize,
    },

    /// A nil project id was supplied.
    #[error("project id must not be nil")]
    NilProjectId,

    /// A nil frame id was supplied for a recorded frame.
    #[error("frame id at recording index {frame_index} must not be nil")]
    NilFrameId {
        /// Zero-based recording index assigned the nil id.
        frame_index: usize,
    },

    /// The same injected frame id was assigned more than once.
    #[error(
        "frame id {frame_id} is duplicated at recording indices {first_index} and {duplicate_index}"
    )]
    DuplicateFrameId {
        /// Repeated stable frame identifier.
        frame_id: FrameId,
        /// First recording index using this identifier.
        first_index: usize,
        /// Later recording index using this identifier.
        duplicate_index: usize,
    },

    /// A frame does not match the fixed recording canvas.
    #[error(
        "frame {frame_index} dimensions {actual_width}x{actual_height} do not match recording canvas {expected_width}x{expected_height}"
    )]
    DimensionMismatch {
        /// Zero-based recording frame index.
        frame_index: usize,
        /// Width established by the first frame.
        expected_width: u16,
        /// Height established by the first frame.
        expected_height: u16,
        /// Width of the rejected frame.
        actual_width: u16,
        /// Height of the rejected frame.
        actual_height: u16,
    },

    /// A frame's packed pixels do not match its declared dimensions.
    #[error("frame {frame_index} has {actual} RGBA bytes, expected {expected}")]
    InvalidFramePixels {
        /// Zero-based recording frame index.
        frame_index: usize,
        /// Required tightly packed byte length.
        expected: usize,
        /// Supplied byte length.
        actual: usize,
    },

    /// Frame duration could not be represented by the domain model.
    #[error("frame {frame_index} has invalid duration {duration_us} microseconds: {source}")]
    InvalidFrameDuration {
        /// Zero-based recording frame index.
        frame_index: usize,
        /// Rejected duration in microseconds.
        duration_us: u64,
        /// Domain unit validation failure.
        #[source]
        source: UnitError,
    },

    /// Recording dimensions could not be represented by the domain model.
    #[error("recording canvas {width}x{height} is invalid: {source}")]
    InvalidCanvas {
        /// Recording width in physical pixels.
        width: u16,
        /// Recording height in physical pixels.
        height: u16,
        /// Domain unit validation failure.
        #[source]
        source: UnitError,
    },

    /// One frame's asset byte length could not be represented on disk.
    #[error("frame {frame_index} byte length cannot be represented as u64")]
    AssetLengthOutOfRange {
        /// Zero-based recording frame index.
        frame_index: usize,
    },

    /// Injected metadata failed domain manifest validation.
    #[error("recording project metadata is invalid: {source}")]
    InvalidManifest {
        /// Domain invariant failure with its original validation issues.
        #[source]
        source: DomainError,
    },

    /// Initial crash-recoverable project creation failed.
    #[error("could not create active project: {source}")]
    CreateProject {
        /// Project storage failure with path and operation context.
        #[source]
        source: ProjectError,
    },

    /// Writing immutable frame pixels failed.
    #[error("could not store pixels for frame {frame_index}: {source}")]
    StoreAsset {
        /// Zero-based recording frame index.
        frame_index: usize,
        /// Content-addressed asset-store failure.
        #[source]
        source: ProjectError,
    },

    /// The single compound asset/timeline command could not be journaled.
    #[error("could not commit the captured timeline: {source}")]
    CommitTimeline {
        /// Project/domain error retaining journal or invariant context.
        #[source]
        source: ProjectError,
    },

    /// Snapshot checkpoint or journal compaction failed.
    #[error("could not checkpoint the captured project: {source}")]
    CheckpointAndCompact {
        /// Project storage failure; the journal remains the recovery source.
        #[source]
        source: ProjectError,
    },
}

/// Persists a collected RGBA recording as a crash-recoverable editable project.
///
/// Immutable RGBA assets are written by content digest and reused when pixels
/// repeat. Every unique descriptor and every frame insertion is then journaled
/// as one compound domain command, followed by an atomic manifest checkpoint
/// and journal compaction. If a later stage fails, the already-created project
/// remains valid and can be reopened; uncommitted asset files are harmless.
///
/// # Errors
///
/// Returns [`PersistRecordingError`] for invalid injected identifiers or
/// metadata, an empty/inconsistent recording, or a contextual project storage
/// failure. An existing project manifest is never replaced.
pub fn persist_collected_recording(
    root: impl AsRef<Path>,
    recording: CollectedRecording,
    options: RecordingProjectOptions,
) -> Result<ActiveProject, PersistRecordingError> {
    let (recording_frames, metadata) = recording.into_parts();
    let frames: Vec<_> = recording_frames
        .iter()
        .map(|frame| RgbaProjectFrame {
            width: frame.width(),
            height: frame.height(),
            duration_us: frame.duration_us(),
            pixels: frame.pixels(),
        })
        .collect();
    persist_rgba_project_with_metadata(
        root,
        &frames,
        RgbaProjectOptions {
            project_id: options.project_id,
            frame_ids: options.frame_ids,
            app_version: options.app_version,
            created_at: options.created_at,
            source_provenance: vec![SourceProvenance::Screen {
                source_label: options.source_label,
            }],
            export_presets: BTreeMap::new(),
        },
        &metadata,
    )
    .map_err(map_persist_error)
}

fn map_persist_error(error: PersistRgbaProjectError) -> PersistRecordingError {
    match error {
        PersistRgbaProjectError::EmptyFrames => PersistRecordingError::EmptyRecording,
        PersistRgbaProjectError::InsufficientFrameIds { required, provided } => {
            PersistRecordingError::InsufficientFrameIds { required, provided }
        }
        PersistRgbaProjectError::NilProjectId => PersistRecordingError::NilProjectId,
        PersistRgbaProjectError::NilFrameId { frame_index } => {
            PersistRecordingError::NilFrameId { frame_index }
        }
        PersistRgbaProjectError::DuplicateFrameId {
            frame_id,
            first_index,
            duplicate_index,
        } => PersistRecordingError::DuplicateFrameId {
            frame_id,
            first_index,
            duplicate_index,
        },
        PersistRgbaProjectError::DimensionMismatch {
            frame_index,
            expected_width,
            expected_height,
            actual_width,
            actual_height,
        } => PersistRecordingError::DimensionMismatch {
            frame_index,
            expected_width,
            expected_height,
            actual_width,
            actual_height,
        },
        PersistRgbaProjectError::InvalidFramePixels {
            frame_index,
            expected,
            actual,
        } => PersistRecordingError::InvalidFramePixels {
            frame_index,
            expected,
            actual,
        },
        PersistRgbaProjectError::InvalidFrameDuration {
            frame_index,
            duration_us,
            source,
        } => PersistRecordingError::InvalidFrameDuration {
            frame_index,
            duration_us,
            source,
        },
        PersistRgbaProjectError::InvalidCanvas {
            width,
            height,
            source,
        } => PersistRecordingError::InvalidCanvas {
            width,
            height,
            source,
        },
        PersistRgbaProjectError::AssetLengthOutOfRange { frame_index } => {
            PersistRecordingError::AssetLengthOutOfRange { frame_index }
        }
        PersistRgbaProjectError::InvalidManifest { source } => {
            PersistRecordingError::InvalidManifest { source }
        }
        PersistRgbaProjectError::CreateProject { source } => {
            PersistRecordingError::CreateProject { source }
        }
        PersistRgbaProjectError::StoreAsset {
            frame_index,
            source,
        } => PersistRecordingError::StoreAsset {
            frame_index,
            source,
        },
        PersistRgbaProjectError::CommitTimeline { source } => {
            PersistRecordingError::CommitTimeline { source }
        }
        PersistRgbaProjectError::CheckpointAndCompact { source } => {
            PersistRecordingError::CheckpointAndCompact { source }
        }
    }
}

pub(crate) fn fresh_capture_clock_id() -> gif_from_screen_domain::CaptureClockId {
    gif_from_screen_domain::CaptureClockId::from_u128(uuid::Uuid::new_v4().as_u128())
}

pub(crate) fn domain_capture_metadata(metadata: &RecordingMetadata) -> CaptureMetadata {
    use gif_from_screen_capture::{
        ButtonState, InputEvent, KeyState, PhysicalPosition, PointerButton,
    };
    use gif_from_screen_domain::{KeyStroke, MouseButton, MouseInputEvent, PhysicalPoint, TimeUs};
    let point = |position: PhysicalPosition| {
        Some(PhysicalPoint {
            x: gif_from_screen_domain::PhysicalPx::new(u32::try_from(position.x).ok()?),
            y: gif_from_screen_domain::PhysicalPx::new(u32::try_from(position.y).ok()?),
        })
    };
    let mut result = CaptureMetadata {
        captured_at: Some(TimeUs::new(metadata.captured_at.as_micros())),
        capture_origin: metadata.capture_origin.map(|point| {
            gif_from_screen_domain::CaptureOrigin {
                x: point.x,
                y: point.y,
            }
        }),
        cursor_position: metadata
            .cursor
            .as_ref()
            .and_then(|cursor| point(cursor.position)),
        cursor_hotspot: metadata
            .cursor
            .as_ref()
            .and_then(|cursor| point(cursor.hotspot)),
        cursor_visible: metadata
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.visible),
        cursor_embedded: metadata.cursor_embedded,
        dropped_input_events: metadata.dropped_input_events,
        ..CaptureMetadata::default()
    };
    for event in &metadata.input_events {
        match event {
            InputEvent::Key {
                at,
                native_code,
                text,
                state,
                repeat,
                modifiers,
            } => {
                result.key_strokes.push(KeyStroke {
                    physical_key: format!("x11:{native_code}"),
                    display_text: text.clone(),
                    pressed: *state == KeyState::Pressed,
                    at: TimeUs::new(at.as_micros()),
                    repeat: *repeat,
                    modifiers: *modifiers,
                });
            }
            InputEvent::PointerButton {
                at,
                button,
                state,
                position,
            } => {
                let button = match button {
                    PointerButton::Primary => MouseButton::Left,
                    PointerButton::Middle => MouseButton::Middle,
                    PointerButton::Secondary => MouseButton::Right,
                    PointerButton::Other(8) => MouseButton::Back,
                    PointerButton::Other(9) => MouseButton::Forward,
                    PointerButton::Other(number) => MouseButton::Other(*number),
                    _ => continue,
                };
                let pressed = *state == ButtonState::Pressed;
                if pressed && !result.pressed_mouse_buttons.contains(&button) {
                    result.pressed_mouse_buttons.push(button);
                }
                result.mouse_events.push(MouseInputEvent {
                    at: TimeUs::new(at.as_micros()),
                    button,
                    pressed,
                    position: position.and_then(point),
                });
            }
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use gif_from_screen_capture::{
        CaptureCadence, CaptureRequest, CaptureSourceId, CaptureTarget, CaptureTimestamp,
        CapturedFrame, PhysicalSize as CaptureSize, PixelFormat, SyntheticCaptureBackend,
    };
    use gif_from_screen_domain::{
        DurationUs, EditCommand, FrameDurationChange, PhysicalSize, ProjectRevision,
        SourceProvenance,
    };
    use gif_from_screen_gif::NeverCancel;
    use gif_from_screen_project::{AssetStore, LockPolicy};
    use gif_from_screen_workflow::{
        CollectOptions, CollectionLimit, NoopWorkflowProgress, collect,
    };
    use tempfile::tempdir;

    use super::*;
    use crate::rgba_project::validate_rgba_frames;

    fn options(project_id: u128, frame_ids: &[u128]) -> RecordingProjectOptions {
        RecordingProjectOptions {
            project_id: ProjectId::from_u128(project_id),
            frame_ids: frame_ids.iter().copied().map(FrameId::from_u128).collect(),
            app_version: "test-1.0".to_owned(),
            created_at: UnixTimeMs::new(1_234),
            source_label: Some("Synthetic display".to_owned()),
        }
    }

    fn incremental_options(project_id: u128) -> IncrementalRecordingProjectOptions {
        IncrementalRecordingProjectOptions {
            project_id: ProjectId::from_u128(project_id),
            app_version: "test-1.0".to_owned(),
            created_at: UnixTimeMs::new(1_234),
            source_label: Some("Synthetic display".to_owned()),
        }
    }

    #[test]
    fn recording_metadata_and_deduplicated_cursor_survive_journal_recovery() {
        use gif_from_screen_capture::{
            ButtonState, CursorImage, CursorMetadata, InputEvent, KeyState, PhysicalPosition,
            PointerButton,
        };
        use gif_from_screen_domain::{MouseButton, TimeUs};
        let directory = tempdir().unwrap();
        let mut writer = IncrementalRecordingProject::create(
            directory.path(),
            PhysicalSize::new(1, 1).unwrap(),
            incremental_options(9),
        )
        .unwrap();
        let cursor = CursorImage::new(
            CaptureSize::new(2, 1).unwrap(),
            vec![255, 255, 255, 255, 0, 0, 0, 0],
        )
        .unwrap();
        let metadata = RecordingMetadata {
            captured_at: CaptureTimestamp::from_micros(20_000),
            capture_origin: Some(PhysicalPosition { x: -100, y: 50 }),
            cursor: Some(CursorMetadata {
                position: PhysicalPosition { x: 0, y: 0 },
                hotspot: PhysicalPosition { x: 1, y: 0 },
                visible: true,
                shape_id: Some("shape-1".into()),
            }),
            cursor_image: Some(cursor.clone()),
            cursor_embedded: true,
            dropped_input_events: 3,
            input_events: vec![
                InputEvent::Key {
                    at: CaptureTimestamp::from_micros(19_000),
                    native_code: 38,
                    text: Some("Ctrl+a".into()),
                    state: KeyState::Pressed,
                    repeat: true,
                    modifiers: 2,
                },
                InputEvent::PointerButton {
                    at: CaptureTimestamp::from_micros(19_500),
                    button: PointerButton::Primary,
                    state: ButtonState::Pressed,
                    position: Some(PhysicalPosition { x: 0, y: 0 }),
                },
            ],
        };
        let frame = RgbaFrame::new(1, 1, vec![255, 0, 0, 255], 10_000).unwrap();
        writer
            .append_frame_with_metadata(FrameId::from_u128(1), &frame, Some(&metadata))
            .unwrap();
        writer
            .append_frame_with_metadata(FrameId::from_u128(2), &frame, Some(&metadata))
            .unwrap();
        assert_eq!(writer.project.manifest().assets.len(), 2);
        assert_eq!(writer.project.manifest().revision, ProjectRevision::new(2));
        drop(writer);
        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        let metadata = &opened.project.manifest().timeline.frames[0].capture_metadata;
        assert_eq!(metadata.captured_at, Some(TimeUs::new(20_000)));
        assert_eq!(metadata.capture_origin.unwrap().x, -100);
        assert!(metadata.cursor_embedded);
        assert!(metadata.cursor_visible);
        assert_eq!(metadata.cursor_hotspot.unwrap().x.get(), 1);
        assert!(metadata.key_strokes[0].repeat);
        assert_eq!(metadata.key_strokes[0].modifiers, 2);
        assert_eq!(metadata.key_strokes[0].at, TimeUs::new(19_000));
        assert_eq!(metadata.mouse_events[0].button, MouseButton::Left);
        assert_eq!(metadata.dropped_input_events, 3);
        assert_eq!(
            opened
                .project
                .assets()
                .read(metadata.cursor_asset.unwrap())
                .unwrap(),
            cursor.pixels()
        );
    }

    #[test]
    fn empty_native_metadata_is_still_original_but_metadata_free_frames_are_not_recorded() {
        use gif_from_screen_domain::{CaptureBinding, TimeUs};
        let dir = tempdir().unwrap();
        let mut writer = IncrementalRecordingProject::create(
            dir.path(),
            PhysicalSize::new(1, 1).unwrap(),
            incremental_options(77),
        )
        .unwrap();
        let pixels = RgbaFrame::new(1, 1, vec![0, 0, 0, 255], 10_000).unwrap();
        writer.append_frame(FrameId::from_u128(1), &pixels).unwrap();
        let metadata = RecordingMetadata {
            captured_at: gif_from_screen_capture::CaptureTimestamp::from_micros(10_000),
            capture_origin: None,
            cursor: None,
            cursor_image: None,
            cursor_embedded: false,
            input_events: Vec::new(),
            dropped_input_events: 0,
        };
        writer
            .append_frame_with_metadata(FrameId::from_u128(2), &pixels, Some(&metadata))
            .unwrap();
        let project = writer.finish().unwrap();
        assert_eq!(
            project.manifest().timeline.frames[0].capture_binding,
            CaptureBinding::NotRecorded
        );
        assert_eq!(
            project.manifest().timeline.frames[1].capture_binding,
            CaptureBinding::Original
        );
        assert_eq!(
            project.manifest().timeline.frames[1]
                .capture_metadata
                .captured_at,
            Some(TimeUs::new(10_000))
        );
    }

    #[test]
    fn batch_recording_preserves_native_metadata_in_one_commit() {
        use gif_from_screen_capture::{
            CursorImage, CursorMetadata, InputEvent, KeyState, PhysicalPosition,
        };
        let directory = tempdir().unwrap();
        let cursor_image =
            CursorImage::new(CaptureSize::new(1, 1).unwrap(), vec![0, 0, 0, 255]).unwrap();
        let capture = CapturedFrame::new(
            1,
            CaptureTimestamp::from_micros(20_000),
            CaptureSize::new(1, 1).unwrap(),
            4,
            PixelFormat::Rgba8,
            vec![255, 255, 255, 255],
        )
        .unwrap()
        .with_cursor(CursorMetadata {
            position: PhysicalPosition::default(),
            hotspot: PhysicalPosition::default(),
            visible: true,
            shape_id: Some("batch-cursor".into()),
        })
        .with_cursor_image(cursor_image.clone(), false)
        .with_input_events(vec![InputEvent::Key {
            at: CaptureTimestamp::from_micros(19_000),
            native_code: 38,
            text: Some("a".into()),
            state: KeyState::Pressed,
            repeat: false,
            modifiers: 0,
        }]);
        let backend = SyntheticCaptureBackend::new(vec![capture]);
        let recording = collect(
            &backend,
            CaptureRequest::new(
                CaptureTarget::Monitor(CaptureSourceId::new("synthetic:monitor:0").unwrap()),
                CaptureCadence::Manual,
            ),
            &CollectOptions {
                limit: CollectionLimit::MaxFrames(1),
                ..CollectOptions::default()
            },
            &NeverCancel,
            &mut NoopWorkflowProgress,
        )
        .unwrap();
        assert_eq!(recording.metadata()[0].input_events.len(), 1);
        let project =
            persist_collected_recording(directory.path(), recording, options(9, &[1])).unwrap();
        assert_eq!(project.manifest().revision, ProjectRevision::new(1));
        let metadata = &project.manifest().timeline.frames[0].capture_metadata;
        assert_eq!(metadata.key_strokes[0].display_text.as_deref(), Some("a"));
        assert_eq!(metadata.captured_at.unwrap().get(), 20_000);
        assert_eq!(
            project
                .assets()
                .read(metadata.cursor_asset.unwrap())
                .unwrap(),
            cursor_image.pixels()
        );
    }

    fn collected(
        pixels: &[&[u8]],
        timestamps_us: &[u64],
        width: u32,
        height: u32,
        tail_us: u64,
    ) -> CollectedRecording {
        assert_eq!(pixels.len(), timestamps_us.len());
        let size = CaptureSize::new(width, height).unwrap();
        let stride = usize::try_from(width).unwrap() * 4;
        let frames = pixels
            .iter()
            .zip(timestamps_us)
            .enumerate()
            .map(|(index, (pixels, timestamp))| {
                CapturedFrame::new(
                    u64::try_from(index).unwrap(),
                    CaptureTimestamp::from_micros(*timestamp),
                    size,
                    stride,
                    PixelFormat::Rgba8,
                    pixels.to_vec(),
                )
                .unwrap()
            })
            .collect();
        let backend = SyntheticCaptureBackend::new(frames);
        let request = CaptureRequest::new(
            CaptureTarget::Monitor(
                CaptureSourceId::new("synthetic:monitor:0").expect("constant id"),
            ),
            CaptureCadence::Manual,
        );
        let mut progress = NoopWorkflowProgress;
        collect(
            &backend,
            request,
            &CollectOptions {
                limit: CollectionLimit::MaxFrames(u64::try_from(pixels.len()).unwrap()),
                tail_frame_duration: Duration::from_micros(tail_us),
                ..CollectOptions::default()
            },
            &NeverCancel,
            &mut progress,
        )
        .unwrap()
    }

    #[test]
    fn persists_deduplicated_assets_and_original_timing_order() {
        let directory = tempdir().unwrap();
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let recording = collected(&[&red, &red, &blue], &[0, 100, 300], 1, 1, 300);

        let project =
            persist_collected_recording(directory.path(), recording, options(42, &[1, 2, 3]))
                .unwrap();
        let manifest = project.manifest();
        assert_eq!(manifest.revision, ProjectRevision::new(1));
        assert_eq!(manifest.canvas.size, PhysicalSize::new(1, 1).unwrap());
        assert_eq!(manifest.assets.len(), 2);
        assert_eq!(manifest.timeline.frames.len(), 3);
        assert_eq!(
            manifest
                .timeline
                .frames
                .iter()
                .map(|frame| frame.id)
                .collect::<Vec<_>>(),
            [
                FrameId::from_u128(1),
                FrameId::from_u128(2),
                FrameId::from_u128(3)
            ]
        );
        assert_eq!(
            manifest
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [100, 200, 300]
        );
        assert_eq!(
            manifest.timeline.frames[0].asset_id,
            manifest.timeline.frames[1].asset_id
        );
        assert_ne!(
            manifest.timeline.frames[0].asset_id,
            manifest.timeline.frames[2].asset_id
        );
        assert_eq!(
            manifest.source_provenance,
            [SourceProvenance::Screen {
                source_label: Some("Synthetic display".to_owned())
            }]
        );
        assert_eq!(
            fs::read_dir(project.assets().directory()).unwrap().count(),
            2
        );
        assert!(fs::read(&project.layout().journal).unwrap().is_empty());
    }

    #[test]
    fn compacted_project_reopens_and_remains_editable() {
        let directory = tempdir().unwrap();
        let pixels = [1, 2, 3, 255];
        let recording = collected(&[&pixels], &[10], 1, 1, 250);
        let project =
            persist_collected_recording(directory.path(), recording, options(7, &[9])).unwrap();
        let asset_id = project.manifest().timeline.frames[0].asset_id;
        drop(project);

        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(opened.journal_recovery.is_clean());
        assert_eq!(opened.journal_recovery.replayed_records, 0);
        assert!(opened.asset_issues.is_empty());
        let mut project = opened.project;
        assert_eq!(project.assets().read(asset_id).unwrap(), pixels);
        project
            .commit(EditCommand::SetFrameDurations {
                changes: vec![FrameDurationChange {
                    frame_id: FrameId::from_u128(9),
                    duration: DurationUs::new(500).unwrap(),
                }],
            })
            .unwrap();
        assert_eq!(project.manifest().revision, ProjectRevision::new(2));
    }

    #[test]
    fn rejects_empty_mixed_dimensions_and_invalid_durations_before_io() {
        let valid_options = options(1, &[1, 2]);
        let validate = |frames: &[RgbaProjectFrame<'_>], options: &RecordingProjectOptions| {
            validate_rgba_frames(
                frames,
                &RgbaProjectOptions {
                    project_id: options.project_id,
                    frame_ids: options.frame_ids.clone(),
                    app_version: options.app_version.clone(),
                    created_at: options.created_at,
                    source_provenance: vec![SourceProvenance::Screen {
                        source_label: options.source_label.clone(),
                    }],
                    export_presets: BTreeMap::new(),
                },
            )
            .map_err(map_persist_error)
        };
        assert!(matches!(
            validate(&[], &valid_options),
            Err(PersistRecordingError::EmptyRecording)
        ));
        let one_pixel = [1, 2, 3, 255];
        let two_pixels = [1, 2, 3, 255, 4, 5, 6, 255];
        assert!(matches!(
            validate(
                &[
                    RgbaProjectFrame {
                        width: 1,
                        height: 1,
                        duration_us: 10,
                        pixels: &one_pixel,
                    },
                    RgbaProjectFrame {
                        width: 2,
                        height: 1,
                        duration_us: 10,
                        pixels: &two_pixels,
                    }
                ],
                &valid_options
            ),
            Err(PersistRecordingError::DimensionMismatch { frame_index: 1, .. })
        ));
        assert!(matches!(
            validate(
                &[RgbaProjectFrame {
                    width: 1,
                    height: 1,
                    duration_us: 0,
                    pixels: &one_pixel,
                }],
                &options(1, &[1])
            ),
            Err(PersistRecordingError::InvalidFrameDuration {
                frame_index: 0,
                source: UnitError::ZeroDuration,
                ..
            })
        ));
    }

    #[test]
    fn rejects_insufficient_duplicate_and_nil_ids_before_creating_a_project() {
        let directory = tempdir().unwrap();
        let pixels = [1, 2, 3, 255];
        let root = directory.path().join("insufficient");
        assert!(matches!(
            persist_collected_recording(
                &root,
                collected(&[&pixels, &pixels], &[0, 10], 1, 1, 10),
                options(1, &[1])
            ),
            Err(PersistRecordingError::InsufficientFrameIds {
                required: 2,
                provided: 1
            })
        ));
        assert!(!root.exists());

        let root = directory.path().join("duplicate");
        assert!(matches!(
            persist_collected_recording(
                &root,
                collected(&[&pixels, &pixels], &[0, 10], 1, 1, 10),
                options(1, &[1, 1])
            ),
            Err(PersistRecordingError::DuplicateFrameId {
                first_index: 0,
                duplicate_index: 1,
                ..
            })
        ));
        assert!(!root.exists());

        let root = directory.path().join("nil-frame");
        let mut nil_frame = options(1, &[1]);
        nil_frame.frame_ids[0] = FrameId::NIL;
        assert!(matches!(
            persist_collected_recording(&root, collected(&[&pixels], &[0], 1, 1, 10), nil_frame),
            Err(PersistRecordingError::NilFrameId { frame_index: 0 })
        ));
        assert!(!root.exists());

        let root = directory.path().join("nil-project");
        let mut nil_project = options(1, &[1]);
        nil_project.project_id = ProjectId::NIL;
        assert!(matches!(
            persist_collected_recording(&root, collected(&[&pixels], &[0], 1, 1, 10), nil_project),
            Err(PersistRecordingError::NilProjectId)
        ));
        assert!(!root.exists());
    }

    #[test]
    fn existing_project_is_not_replaced_and_remains_reopenable() {
        let directory = tempdir().unwrap();
        let pixels = [1, 2, 3, 255];
        let project = persist_collected_recording(
            directory.path(),
            collected(&[&pixels], &[0], 1, 1, 10),
            options(1, &[1]),
        )
        .unwrap();
        drop(project);

        let error = persist_collected_recording(
            directory.path(),
            collected(&[&pixels], &[0], 1, 1, 10),
            options(2, &[2]),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PersistRecordingError::CreateProject {
                source: ProjectError::ManifestAlreadyExists(_)
            }
        ));
        let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(
            reopened.project.manifest().project_id,
            ProjectId::from_u128(1)
        );
        assert_eq!(reopened.project.manifest().timeline.frames.len(), 1);
    }

    #[test]
    fn asset_failure_leaves_an_empty_valid_recoverable_project() {
        let directory = tempdir().unwrap();
        let scratch = tempdir().unwrap();
        let pixels = [9, 8, 7, 255];
        let asset_id = AssetStore::open(scratch.path())
            .unwrap()
            .put(&pixels)
            .unwrap();
        let assets = directory.path().join("assets");
        fs::create_dir_all(&assets).unwrap();
        fs::write(assets.join(format!("{asset_id}.frame")), b"corrupt").unwrap();

        let error = persist_collected_recording(
            directory.path(),
            collected(&[&pixels], &[0], 1, 1, 10),
            options(1, &[1]),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PersistRecordingError::StoreAsset {
                frame_index: 0,
                source: ProjectError::CorruptAsset { .. }
            }
        ));

        let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(reopened.journal_recovery.is_clean());
        assert!(reopened.asset_issues.is_empty());
        assert_eq!(reopened.project.manifest().revision, ProjectRevision::ZERO);
        assert!(reopened.project.manifest().timeline.frames.is_empty());
        assert!(reopened.project.manifest().assets.is_empty());
    }

    #[test]
    fn incremental_frames_survive_drop_through_journal_recovery() {
        let directory = tempdir().unwrap();
        let canvas = PhysicalSize::new(2, 1).unwrap();
        let pixels = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let first = RgbaFrame::new(2, 1, pixels.clone(), 100).unwrap();
        let second = RgbaFrame::new(2, 1, pixels.clone(), 200).unwrap();
        let mut writer =
            IncrementalRecordingProject::create(directory.path(), canvas, incremental_options(77))
                .unwrap();

        writer.append_frame(FrameId::from_u128(10), &first).unwrap();
        writer
            .append_frame(FrameId::from_u128(20), &second)
            .unwrap();
        assert!(
            writer
                .set_frame_duration(FrameId::from_u128(10), DurationUs::new(150).unwrap())
                .unwrap()
        );
        assert!(
            !writer
                .set_frame_duration(FrameId::from_u128(10), DurationUs::new(150).unwrap())
                .unwrap()
        );
        assert_eq!(
            writer.summary(),
            IncrementalRecordingSummary {
                frames: 2,
                duration_us: 350
            }
        );
        assert_eq!(writer.root(), directory.path());
        drop(writer);

        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(opened.journal_recovery.is_clean());
        assert_eq!(opened.journal_recovery.replayed_records, 3);
        assert!(opened.asset_issues.is_empty());
        assert_eq!(opened.project.manifest().revision, ProjectRevision::new(3));
        assert_eq!(opened.project.manifest().timeline.frames.len(), 2);
        assert_eq!(opened.project.manifest().assets.len(), 1);
        assert_eq!(
            opened
                .project
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [150, 200]
        );
        assert_eq!(
            opened.project.manifest().source_provenance,
            [SourceProvenance::Screen {
                source_label: Some("Synthetic display".to_owned())
            }]
        );
        assert_eq!(
            fs::read_dir(opened.project.assets().directory())
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn incremental_finish_compacts_only_after_a_nonempty_recording() {
        let directory = tempdir().unwrap();
        let empty_root = directory.path().join("empty");
        let writer = IncrementalRecordingProject::create(
            &empty_root,
            PhysicalSize::new(1, 1).unwrap(),
            incremental_options(1),
        )
        .unwrap();
        assert!(matches!(
            writer.finish(),
            Err(IncrementalRecordingProjectError::EmptyRecording)
        ));
        let reopened = ActiveProject::open(&empty_root, LockPolicy::FailIfPresent).unwrap();
        assert!(reopened.project.manifest().timeline.frames.is_empty());
        drop(reopened);

        let completed_root = directory.path().join("completed");
        let mut writer = IncrementalRecordingProject::create(
            &completed_root,
            PhysicalSize::new(1, 1).unwrap(),
            incremental_options(2),
        )
        .unwrap();
        writer
            .append_frame(
                FrameId::from_u128(1),
                &RgbaFrame::new(1, 1, vec![1, 2, 3, 255], 10_000).unwrap(),
            )
            .unwrap();
        writer.checkpoint().unwrap();
        let project = writer.finish().unwrap();
        assert_eq!(project.manifest().timeline.frames.len(), 1);
        assert!(fs::read(&project.layout().journal).unwrap().is_empty());
        drop(project);

        let reopened = ActiveProject::open(&completed_root, LockPolicy::FailIfPresent).unwrap();
        assert_eq!(reopened.journal_recovery.replayed_records, 0);
        assert_eq!(
            reopened.project.manifest().revision,
            ProjectRevision::new(1)
        );
    }

    #[test]
    fn incremental_validation_never_mutates_the_project() {
        let directory = tempdir().unwrap();
        let mut writer = IncrementalRecordingProject::create(
            directory.path(),
            PhysicalSize::new(1, 1).unwrap(),
            incremental_options(8),
        )
        .unwrap();
        let valid = RgbaFrame::new(1, 1, vec![9, 8, 7, 255], 100).unwrap();
        let wrong_size = RgbaFrame::new(2, 1, vec![0; 8], 100).unwrap();

        assert!(matches!(
            writer.append_frame(FrameId::NIL, &valid),
            Err(IncrementalRecordingProjectError::NilFrameId { frame_index: 0 })
        ));
        assert!(matches!(
            writer.append_frame(FrameId::from_u128(1), &wrong_size),
            Err(IncrementalRecordingProjectError::DimensionMismatch { frame_index: 0, .. })
        ));
        assert_eq!(writer.summary(), IncrementalRecordingSummary::default());
        assert_eq!(writer.project.manifest().revision, ProjectRevision::ZERO);
        assert_eq!(
            fs::read_dir(writer.project.assets().directory())
                .unwrap()
                .count(),
            0
        );

        writer.append_frame(FrameId::from_u128(1), &valid).unwrap();
        let revision = writer.project.manifest().revision;
        assert!(matches!(
            writer.append_frame(FrameId::from_u128(1), &valid),
            Err(IncrementalRecordingProjectError::DuplicateFrameId { .. })
        ));
        assert!(matches!(
            writer.set_frame_duration(FrameId::from_u128(99), DurationUs::new(1).unwrap()),
            Err(IncrementalRecordingProjectError::UnknownFrame { .. })
        ));
        assert_eq!(writer.project.manifest().revision, revision);
        assert_eq!(writer.summary().frames, 1);
    }

    #[test]
    fn incremental_checkpoint_interval_bounds_recovery_replay() {
        let directory = tempdir().unwrap();
        let mut writer = IncrementalRecordingProject::create(
            directory.path(),
            PhysicalSize::new(1, 1).unwrap(),
            incremental_options(99),
        )
        .unwrap();
        let frame = RgbaFrame::new(1, 1, vec![1, 2, 3, 255], 100).unwrap();
        for index in 0..=INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES {
            writer
                .append_frame(
                    FrameId::from_u128(u128::try_from(index + 1).unwrap()),
                    &frame,
                )
                .unwrap();
        }
        assert_eq!(
            writer.summary(),
            IncrementalRecordingSummary {
                frames: INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES + 1,
                duration_us: u64::try_from(
                    (INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES + 1) * 100
                )
                .unwrap(),
            }
        );
        drop(writer);

        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(opened.journal_recovery.replayed_records, 1);
        assert_eq!(
            opened.project.manifest().timeline.frames.len(),
            INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES + 1
        );
        assert_eq!(opened.project.manifest().assets.len(), 1);
    }

    #[test]
    #[ignore = "long-recording durability benchmark; run explicitly for release validation"]
    fn ten_thousand_frame_incremental_recording_has_bounded_replay_tail() {
        const FRAME_COUNT: usize = 10_000;
        let directory = tempdir().unwrap();
        let mut writer = IncrementalRecordingProject::create(
            directory.path(),
            PhysicalSize::new(1, 1).unwrap(),
            incremental_options(100),
        )
        .unwrap();
        let frame = RgbaFrame::new(1, 1, vec![9, 8, 7, 255], 100_000).unwrap();
        let started = std::time::Instant::now();
        for index in 0..FRAME_COUNT {
            if index > 0 {
                writer
                    .set_frame_duration(
                        FrameId::from_u128(u128::try_from(index).unwrap()),
                        DurationUs::new(33_333).unwrap(),
                    )
                    .unwrap();
            }
            writer
                .append_frame(
                    FrameId::from_u128(u128::try_from(index + 1).unwrap()),
                    &frame,
                )
                .unwrap();
        }
        writer
            .set_frame_duration(
                FrameId::from_u128(u128::try_from(FRAME_COUNT).unwrap()),
                DurationUs::new(33_333).unwrap(),
            )
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(writer.summary().frames, FRAME_COUNT);
        drop(writer);

        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(opened.project.manifest().timeline.frames.len(), FRAME_COUNT);
        assert!(
            opened.journal_recovery.replayed_records
                <= u64::try_from(INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES * 2 + 1).unwrap()
        );
        eprintln!(
            "persisted and duration-corrected {FRAME_COUNT} frames in {elapsed:?}; replayed {} tail records",
            opened.journal_recovery.replayed_records
        );
    }
}
