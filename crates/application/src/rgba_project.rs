use std::{collections::BTreeMap, path::Path};

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
    DomainError, DurationUs, EditCommand, FrameClip, FrameId, GifExportPreset, PhysicalSize,
    ProjectId, ProjectManifest, RasterEncoding, SourceProvenance, UnitError, UnixTimeMs,
};
use gif_from_screen_project::{ActiveProject, ProjectError};
use thiserror::Error;

#[derive(Clone, Copy, Debug)]
pub(crate) struct RgbaProjectFrame<'a> {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) duration_us: u64,
    pub(crate) pixels: &'a [u8],
}

#[derive(Clone, Debug)]
pub(crate) struct RgbaProjectOptions {
    pub(crate) project_id: ProjectId,
    pub(crate) frame_ids: Vec<FrameId>,
    pub(crate) app_version: String,
    pub(crate) created_at: UnixTimeMs,
    pub(crate) source_provenance: Vec<SourceProvenance>,
    pub(crate) export_presets: BTreeMap<String, GifExportPreset>,
}

#[derive(Debug, Error)]
pub(crate) enum PersistRgbaProjectError {
    #[error("cannot create a project from an empty frame sequence")]
    EmptyFrames,
    #[error("frame sequence has {required} frames but only {provided} frame ids were supplied")]
    InsufficientFrameIds { required: usize, provided: usize },
    #[error("project id must not be nil")]
    NilProjectId,
    #[error("frame id at index {frame_index} must not be nil")]
    NilFrameId { frame_index: usize },
    #[error("frame id {frame_id} is duplicated at indices {first_index} and {duplicate_index}")]
    DuplicateFrameId {
        frame_id: FrameId,
        first_index: usize,
        duplicate_index: usize,
    },
    #[error(
        "frame {frame_index} dimensions {actual_width}x{actual_height} do not match canvas {expected_width}x{expected_height}"
    )]
    DimensionMismatch {
        frame_index: usize,
        expected_width: u16,
        expected_height: u16,
        actual_width: u16,
        actual_height: u16,
    },
    #[error("frame {frame_index} has {actual} RGBA bytes, expected {expected}")]
    InvalidFramePixels {
        frame_index: usize,
        expected: usize,
        actual: usize,
    },
    #[error("frame {frame_index} has invalid duration {duration_us} microseconds: {source}")]
    InvalidFrameDuration {
        frame_index: usize,
        duration_us: u64,
        #[source]
        source: UnitError,
    },
    #[error("frame canvas {width}x{height} is invalid: {source}")]
    InvalidCanvas {
        width: u16,
        height: u16,
        #[source]
        source: UnitError,
    },
    #[error("frame {frame_index} byte length cannot be represented as u64")]
    AssetLengthOutOfRange { frame_index: usize },
    #[error("project metadata is invalid: {source}")]
    InvalidManifest {
        #[source]
        source: DomainError,
    },
    #[error("could not create active project: {source}")]
    CreateProject {
        #[source]
        source: ProjectError,
    },
    #[error("could not store pixels for frame {frame_index}: {source}")]
    StoreAsset {
        frame_index: usize,
        #[source]
        source: ProjectError,
    },
    #[error("could not commit the frame timeline: {source}")]
    CommitTimeline {
        #[source]
        source: ProjectError,
    },
    #[error("could not checkpoint the project: {source}")]
    CheckpointAndCompact {
        #[source]
        source: ProjectError,
    },
}

struct ValidatedFrames {
    canvas: PhysicalSize,
    frame_ids: Vec<FrameId>,
    durations: Vec<DurationUs>,
}

pub(crate) fn persist_rgba_project(
    root: impl AsRef<Path>,
    frames: &[RgbaProjectFrame<'_>],
    options: RgbaProjectOptions,
) -> Result<ActiveProject, PersistRgbaProjectError> {
    persist_rgba_project_with_metadata(root, frames, options, &[])
}

pub(crate) fn persist_rgba_project_with_metadata(
    root: impl AsRef<Path>,
    frames: &[RgbaProjectFrame<'_>],
    options: RgbaProjectOptions,
    metadata: &[gif_from_screen_workflow::RecordingMetadata],
) -> Result<ActiveProject, PersistRgbaProjectError> {
    if !metadata.is_empty() && metadata.len() != frames.len() {
        return Err(PersistRgbaProjectError::CommitTimeline {
            source: ProjectError::InvalidRecordingMutation(
                "native metadata count differs from frame count".into(),
            ),
        });
    }
    let validated = validate_rgba_frames_inner(frames, &options)?;
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
    .map_err(|source| PersistRgbaProjectError::InvalidManifest { source })?;
    manifest.source_provenance = options.source_provenance;
    manifest.export_presets = options.export_presets;
    manifest
        .validate()
        .map_err(|source| PersistRgbaProjectError::InvalidManifest { source })?;

    let mut project = ActiveProject::create(root, manifest)
        .map_err(|source| PersistRgbaProjectError::CreateProject { source })?;
    let mut descriptors = BTreeMap::new();
    let mut clips = Vec::with_capacity(frames.len());
    let capture_clock_id = crate::recording_project::fresh_capture_clock_id();
    for (frame_index, ((frame, frame_id), duration)) in frames
        .iter()
        .zip(validated.frame_ids)
        .zip(validated.durations)
        .enumerate()
    {
        let asset_id = project.assets().put(frame.pixels).map_err(|source| {
            PersistRgbaProjectError::StoreAsset {
                frame_index,
                source,
            }
        })?;
        let byte_len = u64::try_from(frame.pixels.len())
            .map_err(|_| PersistRgbaProjectError::AssetLengthOutOfRange { frame_index })?;
        register_raster_descriptor(
            &mut descriptors,
            AssetDescriptor {
                id: asset_id,
                byte_len,
                kind: AssetKind::Frame {
                    size: validated.canvas,
                    encoding: RasterEncoding::Rgba8,
                },
            },
            frame_index,
        )?;
        let capture_metadata = store_frame_metadata(
            &project,
            &mut descriptors,
            metadata.get(frame_index),
            frame_index,
        )?;
        clips.push(FrameClip {
            capture_clock: capture_metadata.captured_at.map(|sampled_at| {
                gif_from_screen_domain::CaptureClockContext {
                    id: Some(capture_clock_id),
                    sampled_at,
                }
            }),
            capture_binding: if metadata.get(frame_index).is_some() {
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
        .map_err(|source| PersistRgbaProjectError::CommitTimeline { source })?;
    project
        .checkpoint_and_compact()
        .map_err(|source| PersistRgbaProjectError::CheckpointAndCompact { source })?;
    Ok(project)
}

fn register_raster_descriptor(
    descriptors: &mut BTreeMap<gif_from_screen_domain::AssetId, AssetDescriptor>,
    descriptor: AssetDescriptor,
    frame_index: usize,
) -> Result<(), PersistRgbaProjectError> {
    if descriptors.get(&descriptor.id).is_some_and(|previous| {
        previous.kind.raster_descriptor() != descriptor.kind.raster_descriptor()
    }) {
        return Err(PersistRgbaProjectError::StoreAsset {
            frame_index,
            source: ProjectError::InvalidRecordingMutation(
                "content-identical RGBA assets have incompatible dimensions".into(),
            ),
        });
    }
    descriptors.entry(descriptor.id).or_insert(descriptor);
    Ok(())
}

fn store_frame_metadata(
    project: &ActiveProject,
    descriptors: &mut BTreeMap<gif_from_screen_domain::AssetId, AssetDescriptor>,
    metadata: Option<&gif_from_screen_workflow::RecordingMetadata>,
    frame_index: usize,
) -> Result<CaptureMetadata, PersistRgbaProjectError> {
    let mut capture = metadata
        .map(crate::recording_project::domain_capture_metadata)
        .unwrap_or_default();
    if let Some(image) = metadata.and_then(|metadata| metadata.cursor_image.as_ref()) {
        let asset_id = project.assets().put(image.pixels()).map_err(|source| {
            PersistRgbaProjectError::StoreAsset {
                frame_index,
                source,
            }
        })?;
        let size = PhysicalSize {
            width: gif_from_screen_domain::PhysicalPx::new(image.size().width()),
            height: gif_from_screen_domain::PhysicalPx::new(image.size().height()),
        };
        register_raster_descriptor(
            descriptors,
            AssetDescriptor {
                id: asset_id,
                byte_len: image.pixels().len() as u64,
                kind: AssetKind::OverlayImage {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
            frame_index,
        )?;
        capture.cursor_asset = Some(asset_id);
    }
    Ok(capture)
}

#[cfg(test)]
pub(crate) fn validate_rgba_frames(
    frames: &[RgbaProjectFrame<'_>],
    options: &RgbaProjectOptions,
) -> Result<(), PersistRgbaProjectError> {
    validate_rgba_frames_inner(frames, options).map(|_| ())
}

fn validate_rgba_frames_inner(
    frames: &[RgbaProjectFrame<'_>],
    options: &RgbaProjectOptions,
) -> Result<ValidatedFrames, PersistRgbaProjectError> {
    let Some(first) = frames.first() else {
        return Err(PersistRgbaProjectError::EmptyFrames);
    };
    if options.project_id.is_nil() {
        return Err(PersistRgbaProjectError::NilProjectId);
    }
    if options.frame_ids.len() < frames.len() {
        return Err(PersistRgbaProjectError::InsufficientFrameIds {
            required: frames.len(),
            provided: options.frame_ids.len(),
        });
    }

    let canvas =
        PhysicalSize::new(u32::from(first.width), u32::from(first.height)).map_err(|source| {
            PersistRgbaProjectError::InvalidCanvas {
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
            return Err(PersistRgbaProjectError::DimensionMismatch {
                frame_index,
                expected_width: first.width,
                expected_height: first.height,
                actual_width: frame.width,
                actual_height: frame.height,
            });
        }
        let expected_pixels = usize::from(frame.width) * usize::from(frame.height) * 4;
        if frame.pixels.len() != expected_pixels {
            return Err(PersistRgbaProjectError::InvalidFramePixels {
                frame_index,
                expected: expected_pixels,
                actual: frame.pixels.len(),
            });
        }
        if frame_id.is_nil() {
            return Err(PersistRgbaProjectError::NilFrameId { frame_index });
        }
        if let Some(first_index) = seen.insert(frame_id, frame_index) {
            return Err(PersistRgbaProjectError::DuplicateFrameId {
                frame_id,
                first_index,
                duplicate_index: frame_index,
            });
        }
        let duration = DurationUs::try_from(frame.duration_us).map_err(|source| {
            PersistRgbaProjectError::InvalidFrameDuration {
                frame_index,
                duration_us: frame.duration_us,
                source,
            }
        })?;
        frame_ids.push(frame_id);
        durations.push(duration);
    }
    Ok(ValidatedFrames {
        canvas,
        frame_ids,
        durations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(frame_ids: &[u128]) -> RgbaProjectOptions {
        RgbaProjectOptions {
            project_id: ProjectId::from_u128(1),
            frame_ids: frame_ids.iter().copied().map(FrameId::from_u128).collect(),
            app_version: "test".to_owned(),
            created_at: UnixTimeMs::new(0),
            source_provenance: Vec::new(),
            export_presets: BTreeMap::new(),
        }
    }

    #[test]
    fn rejects_empty_mismatched_and_malformed_frame_batches() {
        assert!(matches!(
            validate_rgba_frames(&[], &options(&[])),
            Err(PersistRgbaProjectError::EmptyFrames)
        ));

        let pixel = [1, 2, 3, 255];
        let two_pixels = [1, 2, 3, 255, 4, 5, 6, 255];
        assert!(matches!(
            validate_rgba_frames(
                &[
                    RgbaProjectFrame {
                        width: 1,
                        height: 1,
                        duration_us: 10,
                        pixels: &pixel,
                    },
                    RgbaProjectFrame {
                        width: 2,
                        height: 1,
                        duration_us: 10,
                        pixels: &two_pixels,
                    },
                ],
                &options(&[1, 2]),
            ),
            Err(PersistRgbaProjectError::DimensionMismatch { frame_index: 1, .. })
        ));
        assert!(matches!(
            validate_rgba_frames(
                &[RgbaProjectFrame {
                    width: 1,
                    height: 1,
                    duration_us: 10,
                    pixels: &[],
                }],
                &options(&[1]),
            ),
            Err(PersistRgbaProjectError::InvalidFramePixels {
                frame_index: 0,
                expected: 4,
                actual: 0,
            })
        ));
        assert!(matches!(
            validate_rgba_frames(
                &[RgbaProjectFrame {
                    width: 0,
                    height: 1,
                    duration_us: 10,
                    pixels: &[],
                }],
                &options(&[1]),
            ),
            Err(PersistRgbaProjectError::InvalidCanvas { width: 0, .. })
        ));
        assert!(matches!(
            validate_rgba_frames(
                &[RgbaProjectFrame {
                    width: 1,
                    height: 1,
                    duration_us: 0,
                    pixels: &pixel,
                }],
                &options(&[1]),
            ),
            Err(PersistRgbaProjectError::InvalidFrameDuration {
                frame_index: 0,
                source: UnitError::ZeroDuration,
                ..
            })
        ));
    }
}
