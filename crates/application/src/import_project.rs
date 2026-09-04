use std::{collections::BTreeMap, path::Path};

use gif_from_screen_domain::{
    DomainError, FrameId, GifExportPreset, GifLoop, GifPaletteStrategy, ProjectId,
    SourceProvenance, UnitError, UnixTimeMs,
};
use gif_from_screen_media::{DecodedAnimation, LoopBehavior};
use gif_from_screen_project::{ActiveProject, ProjectError};
use thiserror::Error;

use crate::rgba_project::{
    PersistRgbaProjectError, RgbaProjectFrame, RgbaProjectOptions, persist_rgba_project,
};

/// Manifest key used for the export preset derived from an imported GIF.
pub const IMPORTED_GIF_PRESET_NAME: &str = "Imported GIF";

/// Injected identities and user-facing metadata for a decoded GIF project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedAnimationProjectOptions {
    /// Stable identifier assigned to the new project.
    pub project_id: ProjectId,
    /// Stable frame identifiers consumed in animation order. Extra identifiers are ignored.
    pub frame_ids: Vec<FrameId>,
    /// Application version persisted in the project manifest.
    pub app_version: String,
    /// Wall-clock project creation timestamp.
    pub created_at: UnixTimeMs,
    /// User-facing source filename or import label.
    pub display_name: String,
}

/// Failure while converting a safely decoded GIF into an editable project.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PersistDecodedAnimationError {
    /// The decoded animation contained no display frame.
    #[error("cannot create a project from an empty decoded animation")]
    EmptyAnimation,
    /// The injected frame-id sequence ended before every decoded frame had an id.
    #[error("animation has {required} frames but only {provided} frame ids were supplied")]
    InsufficientFrameIds {
        /// Number of decoded frames requiring identifiers.
        required: usize,
        /// Number of supplied identifiers.
        provided: usize,
    },
    /// A nil project id was supplied.
    #[error("project id must not be nil")]
    NilProjectId,
    /// A nil frame id was supplied.
    #[error("frame id at animation index {frame_index} must not be nil")]
    NilFrameId {
        /// Zero-based decoded-frame index.
        frame_index: usize,
    },
    /// The same frame id was injected more than once.
    #[error(
        "frame id {frame_id} is duplicated at animation indices {first_index} and {duplicate_index}"
    )]
    DuplicateFrameId {
        /// Repeated stable identifier.
        frame_id: FrameId,
        /// First animation index using the identifier.
        first_index: usize,
        /// Later animation index using the identifier.
        duplicate_index: usize,
    },
    /// A decoded frame did not cover the declared logical GIF canvas.
    #[error(
        "frame {frame_index} dimensions {actual_width}x{actual_height} do not match animation canvas {expected_width}x{expected_height}"
    )]
    DimensionMismatch {
        /// Zero-based decoded-frame index.
        frame_index: usize,
        /// GIF logical canvas width.
        expected_width: u16,
        /// GIF logical canvas height.
        expected_height: u16,
        /// Frame width.
        actual_width: u16,
        /// Frame height.
        actual_height: u16,
    },
    /// A decoded frame did not contain tightly packed full-canvas RGBA8.
    #[error("frame {frame_index} has {actual} RGBA bytes, expected {expected}")]
    InvalidFramePixels {
        /// Zero-based decoded-frame index.
        frame_index: usize,
        /// Required byte length.
        expected: usize,
        /// Supplied byte length.
        actual: usize,
    },
    /// A decoded frame duration was outside the domain's valid range.
    #[error("frame {frame_index} has invalid duration {duration_us} microseconds: {source}")]
    InvalidFrameDuration {
        /// Zero-based decoded-frame index.
        frame_index: usize,
        /// Rejected duration.
        duration_us: u64,
        /// Domain unit validation failure.
        #[source]
        source: UnitError,
    },
    /// The decoded logical canvas was invalid.
    #[error("animation canvas {width}x{height} is invalid: {source}")]
    InvalidCanvas {
        /// Canvas width.
        width: u16,
        /// Canvas height.
        height: u16,
        /// Domain unit validation failure.
        #[source]
        source: UnitError,
    },
    /// One frame's byte length could not be represented by project storage.
    #[error("frame {frame_index} byte length cannot be represented as u64")]
    AssetLengthOutOfRange {
        /// Zero-based decoded-frame index.
        frame_index: usize,
    },
    /// The imported source label was empty or whitespace-only.
    #[error("imported GIF display name must not be empty")]
    EmptyDisplayName,
    /// Injected metadata failed manifest validation.
    #[error("imported GIF project metadata is invalid: {source}")]
    InvalidManifest {
        /// Domain validation failure.
        #[source]
        source: DomainError,
    },
    /// Initial project creation failed.
    #[error("could not create imported GIF project: {source}")]
    CreateProject {
        /// Project storage failure.
        #[source]
        source: ProjectError,
    },
    /// Writing one immutable composed frame failed.
    #[error("could not store pixels for imported frame {frame_index}: {source}")]
    StoreAsset {
        /// Zero-based decoded-frame index.
        frame_index: usize,
        /// Content-addressed store failure.
        #[source]
        source: ProjectError,
    },
    /// The compound descriptor/timeline command could not be journaled.
    #[error("could not commit imported GIF timeline: {source}")]
    CommitTimeline {
        /// Project/domain failure retaining recovery context.
        #[source]
        source: ProjectError,
    },
    /// Manifest checkpoint or journal compaction failed.
    #[error("could not checkpoint imported GIF project: {source}")]
    CheckpointAndCompact {
        /// Project storage failure; the journal remains authoritative.
        #[source]
        source: ProjectError,
    },
}

/// Persists decoded, full-canvas GIF frames as an editable project.
///
/// The media decoder has already applied local rectangles, transparency, and
/// disposal, so every stored immutable asset is one complete straight-alpha
/// RGBA8 canvas. Content-addressed storage deduplicates repeated composed
/// frames while timeline clips retain their individual durations. The source
/// GIF loop extension seeds the `Imported GIF` export preset.
///
/// # Errors
///
/// Returns [`PersistDecodedAnimationError`] before filesystem creation for bad
/// injected identifiers, labels, frame shapes, or durations. Storage failures
/// retain the same recoverable project semantics as captured recordings, and
/// an existing project manifest is never replaced.
pub fn persist_decoded_animation(
    root: impl AsRef<Path>,
    animation: DecodedAnimation,
    options: DecodedAnimationProjectOptions,
) -> Result<ActiveProject, PersistDecodedAnimationError> {
    if options.display_name.trim().is_empty() {
        return Err(PersistDecodedAnimationError::EmptyDisplayName);
    }
    let loop_behavior = animation.loop_behavior();
    let width = animation.width();
    let height = animation.height();
    let decoded_frames = animation.into_frames();
    let frames = decoded_frames
        .iter()
        .map(|frame| RgbaProjectFrame {
            width,
            height,
            duration_us: frame.duration_us(),
            pixels: frame.rgba(),
        })
        .collect::<Vec<_>>();
    let mut export_presets = BTreeMap::new();
    export_presets.insert(
        IMPORTED_GIF_PRESET_NAME.to_owned(),
        GifExportPreset {
            colors: 256,
            palette: GifPaletteStrategy::Adaptive,
            repeat: map_loop_behavior(loop_behavior),
            alpha_threshold: 1,
        },
    );
    persist_rgba_project(
        root,
        &frames,
        RgbaProjectOptions {
            project_id: options.project_id,
            frame_ids: options.frame_ids,
            app_version: options.app_version,
            created_at: options.created_at,
            source_provenance: vec![SourceProvenance::Imported {
                display_name: options.display_name,
                media_type: "image/gif".to_owned(),
            }],
            export_presets,
        },
    )
    .map_err(map_persist_error)
}

const fn map_loop_behavior(loop_behavior: LoopBehavior) -> GifLoop {
    match loop_behavior {
        LoopBehavior::Once | LoopBehavior::Finite(0) => GifLoop::Finite(1),
        LoopBehavior::Infinite => GifLoop::Infinite,
        LoopBehavior::Finite(repeats) => GifLoop::Finite(repeats),
    }
}

fn map_persist_error(error: PersistRgbaProjectError) -> PersistDecodedAnimationError {
    match error {
        PersistRgbaProjectError::EmptyFrames => PersistDecodedAnimationError::EmptyAnimation,
        PersistRgbaProjectError::InsufficientFrameIds { required, provided } => {
            PersistDecodedAnimationError::InsufficientFrameIds { required, provided }
        }
        PersistRgbaProjectError::NilProjectId => PersistDecodedAnimationError::NilProjectId,
        PersistRgbaProjectError::NilFrameId { frame_index } => {
            PersistDecodedAnimationError::NilFrameId { frame_index }
        }
        PersistRgbaProjectError::DuplicateFrameId {
            frame_id,
            first_index,
            duplicate_index,
        } => PersistDecodedAnimationError::DuplicateFrameId {
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
        } => PersistDecodedAnimationError::DimensionMismatch {
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
        } => PersistDecodedAnimationError::InvalidFramePixels {
            frame_index,
            expected,
            actual,
        },
        PersistRgbaProjectError::InvalidFrameDuration {
            frame_index,
            duration_us,
            source,
        } => PersistDecodedAnimationError::InvalidFrameDuration {
            frame_index,
            duration_us,
            source,
        },
        PersistRgbaProjectError::InvalidCanvas {
            width,
            height,
            source,
        } => PersistDecodedAnimationError::InvalidCanvas {
            width,
            height,
            source,
        },
        PersistRgbaProjectError::AssetLengthOutOfRange { frame_index } => {
            PersistDecodedAnimationError::AssetLengthOutOfRange { frame_index }
        }
        PersistRgbaProjectError::InvalidManifest { source } => {
            PersistDecodedAnimationError::InvalidManifest { source }
        }
        PersistRgbaProjectError::CreateProject { source } => {
            PersistDecodedAnimationError::CreateProject { source }
        }
        PersistRgbaProjectError::StoreAsset {
            frame_index,
            source,
        } => PersistDecodedAnimationError::StoreAsset {
            frame_index,
            source,
        },
        PersistRgbaProjectError::CommitTimeline { source } => {
            PersistDecodedAnimationError::CommitTimeline { source }
        }
        PersistRgbaProjectError::CheckpointAndCompact { source } => {
            PersistDecodedAnimationError::CheckpointAndCompact { source }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, fs, io::Cursor};

    use gif::{DisposalMethod, Encoder, Frame, Repeat};
    use gif_from_screen_domain::{GifPaletteStrategy, PhysicalSize, ProjectRevision};
    use gif_from_screen_media::{GifDecodeOptions, decode_gif};
    use gif_from_screen_project::LockPolicy;
    use tempfile::tempdir;

    use super::*;

    const PALETTE: &[u8] = &[
        0, 0, 0, // transparent/black
        255, 0, 0, // red
        0, 255, 0, // green
        0, 0, 255, // blue
    ];

    #[derive(Clone)]
    struct InputFrame {
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        indices: Vec<u8>,
        transparent: Option<u8>,
        delay: u16,
        disposal: DisposalMethod,
    }

    fn make_gif(width: u16, height: u16, repeat: Option<Repeat>, frames: &[InputFrame]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, width, height, PALETTE).unwrap();
            if let Some(repeat) = repeat {
                encoder.set_repeat(repeat).unwrap();
            }
            for input in frames {
                encoder
                    .write_frame(&Frame {
                        left: input.left,
                        top: input.top,
                        width: input.width,
                        height: input.height,
                        delay: input.delay,
                        dispose: input.disposal,
                        transparent: input.transparent,
                        buffer: Cow::Owned(input.indices.clone()),
                        ..Frame::default()
                    })
                    .unwrap();
            }
        }
        bytes
    }

    fn decode(
        width: u16,
        height: u16,
        repeat: Option<Repeat>,
        frames: &[InputFrame],
    ) -> DecodedAnimation {
        decode_gif(
            Cursor::new(make_gif(width, height, repeat, frames)),
            &GifDecodeOptions::default(),
        )
        .unwrap()
    }

    fn full_frame(indices: Vec<u8>, delay: u16) -> InputFrame {
        InputFrame {
            left: 0,
            top: 0,
            width: u16::try_from(indices.len()).unwrap(),
            height: 1,
            indices,
            transparent: None,
            delay,
            disposal: DisposalMethod::Keep,
        }
    }

    fn options(project_id: u128, frame_ids: &[u128]) -> DecodedAnimationProjectOptions {
        DecodedAnimationProjectOptions {
            project_id: ProjectId::from_u128(project_id),
            frame_ids: frame_ids.iter().copied().map(FrameId::from_u128).collect(),
            app_version: "test-1.0".to_owned(),
            created_at: UnixTimeMs::new(1_234),
            display_name: "source.gif".to_owned(),
        }
    }

    #[test]
    fn persists_composited_disposal_transparency_and_variable_timing() {
        let scratch = tempdir().unwrap();
        let animation = decode(
            2,
            1,
            Some(Repeat::Infinite),
            &[
                full_frame(vec![1, 1], 1),
                InputFrame {
                    left: 0,
                    top: 0,
                    width: 1,
                    height: 1,
                    indices: vec![3],
                    transparent: None,
                    delay: 2,
                    disposal: DisposalMethod::Background,
                },
                InputFrame {
                    left: 1,
                    top: 0,
                    width: 1,
                    height: 1,
                    indices: vec![2],
                    transparent: None,
                    delay: 3,
                    disposal: DisposalMethod::Keep,
                },
            ],
        );
        assert_eq!(animation.frames()[2].rgba(), [0, 0, 0, 0, 0, 255, 0, 255]);

        let project =
            persist_decoded_animation(scratch.path(), animation, options(7, &[1, 2, 3])).unwrap();
        let manifest = project.manifest();
        assert_eq!(manifest.revision, ProjectRevision::new(1));
        assert_eq!(manifest.canvas.size, PhysicalSize::new(2, 1).unwrap());
        assert_eq!(
            manifest
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [10_000, 20_000, 30_000]
        );
        assert_eq!(
            manifest.source_provenance,
            [SourceProvenance::Imported {
                display_name: "source.gif".to_owned(),
                media_type: "image/gif".to_owned(),
            }]
        );
        assert_eq!(
            manifest.export_presets[IMPORTED_GIF_PRESET_NAME],
            GifExportPreset {
                colors: 256,
                palette: GifPaletteStrategy::Adaptive,
                repeat: GifLoop::Infinite,
                alpha_threshold: 1,
            }
        );
        let stored_frames = manifest
            .timeline
            .frames
            .iter()
            .map(|clip| project.assets().read(clip.asset_id).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            stored_frames,
            [
                vec![255, 0, 0, 255, 255, 0, 0, 255],
                vec![0, 0, 255, 255, 255, 0, 0, 255],
                vec![0, 0, 0, 0, 0, 255, 0, 255],
            ]
        );
        assert!(fs::read(&project.layout().journal).unwrap().is_empty());
    }

    #[test]
    fn repeated_composited_frames_share_one_asset_and_reopen_cleanly() {
        let scratch = tempdir().unwrap();
        let animation = decode(
            1,
            1,
            None,
            &[full_frame(vec![1], 1), full_frame(vec![1], 2)],
        );
        let project =
            persist_decoded_animation(scratch.path(), animation, options(9, &[10, 11])).unwrap();
        assert_eq!(project.manifest().assets.len(), 1);
        assert_eq!(project.manifest().timeline.frames.len(), 2);
        assert_eq!(
            project.manifest().timeline.frames[0].asset_id,
            project.manifest().timeline.frames[1].asset_id
        );
        assert_eq!(
            fs::read_dir(project.assets().directory()).unwrap().count(),
            1
        );
        drop(project);

        let reopened = ActiveProject::open(scratch.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(reopened.journal_recovery.is_clean());
        assert!(reopened.asset_issues.is_empty());
        assert_eq!(reopened.project.manifest().timeline.frames.len(), 2);
    }

    #[test]
    fn maps_once_finite_and_infinite_loops_into_the_default_preset() {
        let cases = [
            (None, GifLoop::Finite(1)),
            (Some(Repeat::Finite(7)), GifLoop::Finite(7)),
            (Some(Repeat::Infinite), GifLoop::Infinite),
        ];
        for (index, (repeat, expected)) in cases.into_iter().enumerate() {
            let scratch = tempdir().unwrap();
            let animation = decode(1, 1, repeat, &[full_frame(vec![1], 1)]);
            let project = persist_decoded_animation(
                scratch.path(),
                animation,
                options(index as u128 + 1, &[1]),
            )
            .unwrap();
            assert_eq!(
                project.manifest().export_presets[IMPORTED_GIF_PRESET_NAME].repeat,
                expected
            );
        }
    }

    #[test]
    fn rejects_bad_injected_ids_and_label_before_creating_files() {
        let base = tempdir().unwrap();
        let animation = decode(
            1,
            1,
            None,
            &[full_frame(vec![1], 1), full_frame(vec![2], 1)],
        );

        let root = base.path().join("insufficient");
        assert!(matches!(
            persist_decoded_animation(&root, animation.clone(), options(1, &[1])),
            Err(PersistDecodedAnimationError::InsufficientFrameIds {
                required: 2,
                provided: 1,
            })
        ));
        assert!(!root.exists());

        let root = base.path().join("duplicate");
        assert!(matches!(
            persist_decoded_animation(&root, animation.clone(), options(1, &[1, 1])),
            Err(PersistDecodedAnimationError::DuplicateFrameId { .. })
        ));
        assert!(!root.exists());

        let root = base.path().join("nil-project");
        let mut nil_project = options(1, &[1, 2]);
        nil_project.project_id = ProjectId::NIL;
        assert!(matches!(
            persist_decoded_animation(&root, animation.clone(), nil_project),
            Err(PersistDecodedAnimationError::NilProjectId)
        ));
        assert!(!root.exists());

        let root = base.path().join("nil-frame");
        let mut nil_frame = options(1, &[1, 2]);
        nil_frame.frame_ids[1] = FrameId::NIL;
        assert!(matches!(
            persist_decoded_animation(&root, animation.clone(), nil_frame),
            Err(PersistDecodedAnimationError::NilFrameId { frame_index: 1 })
        ));
        assert!(!root.exists());

        let root = base.path().join("empty-label");
        let mut empty_label = options(1, &[1, 2]);
        empty_label.display_name = "  ".to_owned();
        assert!(matches!(
            persist_decoded_animation(&root, animation, empty_label),
            Err(PersistDecodedAnimationError::EmptyDisplayName)
        ));
        assert!(!root.exists());
    }
}
