use std::collections::BTreeMap;
use std::path::Path;

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
    DomainError, DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize, ProjectId,
    ProjectManifest, RasterEncoding, SourceProvenance, UnitError, UnixTimeMs,
};
use gif_from_screen_project::{ActiveProject, ProjectError};
use gif_from_screen_workflow::CollectedRecording;
use thiserror::Error;

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

#[derive(Clone, Copy, Debug)]
struct FrameFacts {
    width: u16,
    height: u16,
    duration_us: u64,
}

#[derive(Debug)]
struct ValidatedRecording {
    canvas: PhysicalSize,
    frame_ids: Vec<FrameId>,
    durations: Vec<DurationUs>,
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
    let facts: Vec<_> = recording
        .frames()
        .iter()
        .map(|frame| FrameFacts {
            width: frame.width(),
            height: frame.height(),
            duration_us: frame.duration_us(),
        })
        .collect();
    let validated = validate_recording(&facts, &options)?;

    let mut manifest = ProjectManifest::new(
        options.project_id,
        options.app_version,
        options.created_at,
        Canvas {
            size: validated.canvas,
            color_space: gif_from_screen_domain::ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    )
    .map_err(|source| PersistRecordingError::InvalidManifest { source })?;
    manifest.source_provenance.push(SourceProvenance::Screen {
        source_label: options.source_label,
    });
    manifest
        .validate()
        .map_err(|source| PersistRecordingError::InvalidManifest { source })?;

    let mut project = ActiveProject::create(root, manifest)
        .map_err(|source| PersistRecordingError::CreateProject { source })?;
    let frames = recording.into_frames();
    let mut descriptors = BTreeMap::new();
    let mut clips = Vec::with_capacity(frames.len());
    for (frame_index, ((frame, frame_id), duration)) in frames
        .into_iter()
        .zip(validated.frame_ids)
        .zip(validated.durations)
        .enumerate()
    {
        let asset_id = project.assets().put(frame.pixels()).map_err(|source| {
            PersistRecordingError::StoreAsset {
                frame_index,
                source,
            }
        })?;
        let byte_len = u64::try_from(frame.pixels().len())
            .map_err(|_| PersistRecordingError::AssetLengthOutOfRange { frame_index })?;
        descriptors.entry(asset_id).or_insert(AssetDescriptor {
            id: asset_id,
            byte_len,
            kind: AssetKind::Frame {
                size: validated.canvas,
                encoding: RasterEncoding::Rgba8,
            },
        });
        clips.push(FrameClip {
            id: frame_id,
            asset_id,
            duration,
            transform: ClipTransform::default(),
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
        });
    }

    let mut commands: Vec<_> = descriptors
        .into_values()
        .map(|asset| EditCommand::RegisterAsset { asset })
        .collect();
    commands.push(EditCommand::InsertFrames {
        index: 0,
        frames: clips,
    });
    project
        .commit(EditCommand::Compound { commands })
        .map_err(|source| PersistRecordingError::CommitTimeline { source })?;
    project
        .checkpoint_and_compact()
        .map_err(|source| PersistRecordingError::CheckpointAndCompact { source })?;
    Ok(project)
}

fn validate_recording(
    frames: &[FrameFacts],
    options: &RecordingProjectOptions,
) -> Result<ValidatedRecording, PersistRecordingError> {
    let Some(first) = frames.first() else {
        return Err(PersistRecordingError::EmptyRecording);
    };
    if options.project_id.is_nil() {
        return Err(PersistRecordingError::NilProjectId);
    }
    if options.frame_ids.len() < frames.len() {
        return Err(PersistRecordingError::InsufficientFrameIds {
            required: frames.len(),
            provided: options.frame_ids.len(),
        });
    }

    let canvas =
        PhysicalSize::new(u32::from(first.width), u32::from(first.height)).map_err(|source| {
            PersistRecordingError::InvalidCanvas {
                width: first.width,
                height: first.height,
                source,
            }
        })?;
    let mut seen = BTreeMap::new();
    let mut frame_ids = Vec::with_capacity(frames.len());
    let mut durations = Vec::with_capacity(frames.len());
    for (frame_index, (frame, frame_id)) in frames
        .iter()
        .zip(options.frame_ids.iter().copied())
        .enumerate()
    {
        if frame.width != first.width || frame.height != first.height {
            return Err(PersistRecordingError::DimensionMismatch {
                frame_index,
                expected_width: first.width,
                expected_height: first.height,
                actual_width: frame.width,
                actual_height: frame.height,
            });
        }
        if frame_id.is_nil() {
            return Err(PersistRecordingError::NilFrameId { frame_index });
        }
        if let Some(first_index) = seen.insert(frame_id, frame_index) {
            return Err(PersistRecordingError::DuplicateFrameId {
                frame_id,
                first_index,
                duplicate_index: frame_index,
            });
        }
        let duration = DurationUs::try_from(frame.duration_us).map_err(|source| {
            PersistRecordingError::InvalidFrameDuration {
                frame_index,
                duration_us: frame.duration_us,
                source,
            }
        })?;
        frame_ids.push(frame_id);
        durations.push(duration);
    }
    Ok(ValidatedRecording {
        canvas,
        frame_ids,
        durations,
    })
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
        EditCommand, FrameDurationChange, ProjectRevision, SourceProvenance,
    };
    use gif_from_screen_gif::NeverCancel;
    use gif_from_screen_project::{AssetStore, LockPolicy};
    use gif_from_screen_workflow::{
        CollectOptions, CollectionLimit, NoopWorkflowProgress, collect,
    };
    use tempfile::tempdir;

    use super::*;

    fn options(project_id: u128, frame_ids: &[u128]) -> RecordingProjectOptions {
        RecordingProjectOptions {
            project_id: ProjectId::from_u128(project_id),
            frame_ids: frame_ids.iter().copied().map(FrameId::from_u128).collect(),
            app_version: "test-1.0".to_owned(),
            created_at: UnixTimeMs::new(1_234),
            source_label: Some("Synthetic display".to_owned()),
        }
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
        assert!(matches!(
            validate_recording(&[], &valid_options),
            Err(PersistRecordingError::EmptyRecording)
        ));
        assert!(matches!(
            validate_recording(
                &[
                    FrameFacts {
                        width: 1,
                        height: 1,
                        duration_us: 10,
                    },
                    FrameFacts {
                        width: 2,
                        height: 1,
                        duration_us: 10,
                    }
                ],
                &valid_options
            ),
            Err(PersistRecordingError::DimensionMismatch { frame_index: 1, .. })
        ));
        assert!(matches!(
            validate_recording(
                &[FrameFacts {
                    width: 1,
                    height: 1,
                    duration_us: 0,
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
}
