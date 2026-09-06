use std::{collections::BTreeMap, path::Path};

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
    DomainError, DurationUs, EditCommand, FrameClip, FrameId, GifExportPreset, GifLoop,
    GifPaletteStrategy, PhysicalSize, ProjectId, ProjectManifest, RasterEncoding, Rgba, UnitError,
    UnixTimeMs,
};
use gif_from_screen_project::{ActiveProject, ProjectError};
use thiserror::Error;

/// Manifest key for the initial export preset attached to a blank animation.
pub const BLANK_ANIMATION_PRESET_NAME: &str = "Blank Animation";

/// Default upper bound for the single generated RGBA frame.
pub const DEFAULT_BLANK_FRAME_LIMIT_BYTES: u64 = 512 * 1024 * 1024;

/// Inputs for creating a one-frame editable animation without imported media.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlankAnimationProjectOptions {
    /// Stable project identity.
    pub project_id: ProjectId,
    /// Stable identity for the initial frame.
    pub frame_id: FrameId,
    /// Application version persisted in the manifest.
    pub app_version: String,
    /// Wall-clock project creation time.
    pub created_at: UnixTimeMs,
    /// Physical output canvas and generated frame dimensions.
    pub canvas: PhysicalSize,
    /// Straight-alpha sRGB fill color for every pixel.
    pub background: Rgba,
    /// Presentation duration of the initial frame.
    pub frame_duration: DurationUs,
    /// Maximum allocation accepted for the generated RGBA frame.
    pub frame_limit_bytes: u64,
}

/// Failure while validating or durably creating a blank animation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CreateBlankAnimationError {
    /// The reserved nil project identity was supplied.
    #[error("blank animation project id must not be nil")]
    NilProjectId,
    /// The reserved nil frame identity was supplied.
    #[error("blank animation frame id must not be nil")]
    NilFrameId,
    /// A persisted application version must contain visible text.
    #[error("blank animation application version must not be empty")]
    EmptyAppVersion,
    /// The canvas does not satisfy physical-size invariants.
    #[error("blank animation canvas is invalid: {source}")]
    InvalidCanvas {
        /// Unit validation failure.
        #[source]
        source: UnitError,
    },
    /// GIF output cannot represent a canvas edge above `u16::MAX`.
    #[error("blank animation canvas {width}x{height} exceeds GIF's 65535-pixel edge limit")]
    DimensionsOutOfRange {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// The requested frame would exceed its explicit allocation bound.
    #[error("blank RGBA frame needs {required_bytes} bytes, above the {limit_bytes}-byte limit")]
    FrameLimitExceeded {
        /// Required packed RGBA bytes.
        required_bytes: u64,
        /// Configured allocation limit.
        limit_bytes: u64,
    },
    /// The bounded allocation could not be reserved.
    #[error("could not allocate {required_bytes} bytes for the blank RGBA frame")]
    AllocationFailed {
        /// Requested allocation size.
        required_bytes: u64,
    },
    /// Injected metadata failed manifest validation.
    #[error("blank animation metadata is invalid: {source}")]
    InvalidManifest {
        /// Domain validation failure.
        #[source]
        source: DomainError,
    },
    /// Initial manifest/layout creation failed.
    #[error("could not create blank animation project: {source}")]
    CreateProject {
        /// Project storage failure.
        #[source]
        source: ProjectError,
    },
    /// Writing the immutable generated pixels failed.
    #[error("could not store blank animation pixels: {source}")]
    StoreAsset {
        /// Content-addressed storage failure.
        #[source]
        source: ProjectError,
    },
    /// The initial asset/frame command could not be journaled.
    #[error("could not journal blank animation frame: {source}")]
    Commit {
        /// Project or domain failure.
        #[source]
        source: ProjectError,
    },
    /// Final snapshot or journal compaction failed.
    #[error("could not finalize blank animation project: {source}")]
    Finalize {
        /// Project storage failure; the journal remains recoverable.
        #[source]
        source: ProjectError,
    },
}

/// Creates a bounded, one-frame project filled with a caller-selected color.
///
/// Validation and allocation checks finish before the project directory is created. Pixels are
/// stored as one immutable content-addressed RGBA8 asset, the descriptor and initial frame are
/// committed in one journal revision, and the result is checkpointed and compacted for immediate
/// editing. Existing projects are never replaced.
///
/// # Errors
///
/// Returns [`CreateBlankAnimationError`] for invalid identities/metadata, GIF-incompatible or
/// excessive dimensions, allocation failure, or contextual project storage failures.
pub fn create_blank_animation_project(
    root: impl AsRef<Path>,
    options: BlankAnimationProjectOptions,
) -> Result<ActiveProject, CreateBlankAnimationError> {
    validate_options(&options)?;
    let (required_bytes, required) = blank_buffer_size(&options)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(required)
        .map_err(|_| CreateBlankAnimationError::AllocationFailed { required_bytes })?;
    pixels.resize(required, 0);
    let fill = [
        options.background.red,
        options.background.green,
        options.background.blue,
        options.background.alpha,
    ];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        *pixel = fill;
    }

    let mut manifest = ProjectManifest::new(
        options.project_id,
        options.app_version,
        options.created_at,
        Canvas {
            size: options.canvas,
            color_space: gif_from_screen_domain::ColorSpace::Srgb,
            background: if options.background.alpha == 0 {
                CanvasBackground::Transparent
            } else {
                CanvasBackground::Solid(options.background)
            },
        },
    )
    .map_err(|source| CreateBlankAnimationError::InvalidManifest { source })?;
    manifest.export_presets = BTreeMap::from([(
        BLANK_ANIMATION_PRESET_NAME.to_owned(),
        GifExportPreset {
            colors: 256,
            palette: GifPaletteStrategy::Adaptive,
            repeat: GifLoop::Finite(1),
            alpha_threshold: 1,
            options: None,
        },
    )]);
    manifest
        .validate()
        .map_err(|source| CreateBlankAnimationError::InvalidManifest { source })?;

    let mut project = ActiveProject::create(root, manifest)
        .map_err(|source| CreateBlankAnimationError::CreateProject { source })?;
    let asset_id = project
        .assets()
        .put(&pixels)
        .map_err(|source| CreateBlankAnimationError::StoreAsset { source })?;
    let descriptor = AssetDescriptor {
        id: asset_id,
        byte_len: required_bytes,
        kind: AssetKind::Frame {
            size: options.canvas,
            encoding: RasterEncoding::Rgba8,
        },
    };
    let frame = FrameClip {
        capture_clock: None,
        capture_binding: gif_from_screen_domain::CaptureBinding::NotRecorded,
        id: options.frame_id,
        asset_id,
        duration: options.frame_duration,
        transform: ClipTransform::default(),
        capture_metadata: CaptureMetadata::default(),
        effects: Vec::new(),
    };
    project
        .commit(EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset { asset: descriptor },
                EditCommand::InsertFrames {
                    index: 0,
                    frames: vec![frame],
                },
            ],
        })
        .map_err(|source| CreateBlankAnimationError::Commit { source })?;
    project
        .checkpoint_and_compact()
        .map_err(|source| CreateBlankAnimationError::Finalize { source })?;
    Ok(project)
}

fn blank_buffer_size(
    options: &BlankAnimationProjectOptions,
) -> Result<(u64, usize), CreateBlankAnimationError> {
    let required_bytes = options
        .canvas
        .area()
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(CreateBlankAnimationError::InvalidCanvas {
            source: UnitError::PhysicalAreaOverflow,
        })?;
    if required_bytes > options.frame_limit_bytes {
        return Err(CreateBlankAnimationError::FrameLimitExceeded {
            required_bytes,
            limit_bytes: options.frame_limit_bytes,
        });
    }
    let required = usize::try_from(required_bytes).map_err(|_| {
        CreateBlankAnimationError::FrameLimitExceeded {
            required_bytes,
            limit_bytes: options.frame_limit_bytes,
        }
    })?;
    Ok((required_bytes, required))
}

fn validate_options(
    options: &BlankAnimationProjectOptions,
) -> Result<(), CreateBlankAnimationError> {
    if options.project_id.is_nil() {
        return Err(CreateBlankAnimationError::NilProjectId);
    }
    if options.frame_id.is_nil() {
        return Err(CreateBlankAnimationError::NilFrameId);
    }
    if options.app_version.trim().is_empty() {
        return Err(CreateBlankAnimationError::EmptyAppVersion);
    }
    options
        .canvas
        .validate()
        .map_err(|source| CreateBlankAnimationError::InvalidCanvas { source })?;
    if options.canvas.width.get() > u32::from(u16::MAX)
        || options.canvas.height.get() > u32::from(u16::MAX)
    {
        return Err(CreateBlankAnimationError::DimensionsOutOfRange {
            width: options.canvas.width.get(),
            height: options.canvas.height.get(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gif_from_screen_domain::{ProjectRevision, Rgba};
    use gif_from_screen_project::LockPolicy;
    use tempfile::tempdir;

    use super::*;

    fn options(background: Rgba) -> BlankAnimationProjectOptions {
        BlankAnimationProjectOptions {
            project_id: ProjectId::from_u128(1),
            frame_id: FrameId::from_u128(2),
            app_version: "blank-test".to_owned(),
            created_at: UnixTimeMs::new(3),
            canvas: PhysicalSize::new(2, 1).unwrap(),
            background,
            frame_duration: DurationUs::new(125_000).unwrap(),
            frame_limit_bytes: DEFAULT_BLANK_FRAME_LIMIT_BYTES,
        }
    }

    #[test]
    fn creates_exact_rgba_frame_preset_and_clean_reopen() {
        let directory = tempdir().unwrap();
        let color = Rgba {
            red: 10,
            green: 20,
            blue: 30,
            alpha: 200,
        };
        let project = create_blank_animation_project(directory.path(), options(color)).unwrap();
        let manifest = project.manifest();
        assert_eq!(manifest.revision, ProjectRevision::new(1));
        assert_eq!(manifest.timeline.frames.len(), 1);
        assert_eq!(manifest.timeline.frames[0].duration.get(), 125_000);
        assert_eq!(manifest.canvas.background, CanvasBackground::Solid(color));
        assert_eq!(manifest.assets.len(), 1);
        assert_eq!(
            manifest.export_presets[BLANK_ANIMATION_PRESET_NAME],
            GifExportPreset {
                colors: 256,
                palette: GifPaletteStrategy::Adaptive,
                repeat: GifLoop::Finite(1),
                alpha_threshold: 1,
                options: None,
            }
        );
        let asset_id = manifest.timeline.frames[0].asset_id;
        assert_eq!(
            project.assets().read(asset_id).unwrap(),
            [10, 20, 30, 200, 10, 20, 30, 200]
        );
        assert!(fs::read(&project.layout().journal).unwrap().is_empty());
        drop(project);

        let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(reopened.journal_recovery.is_clean());
        assert_eq!(reopened.journal_recovery.replayed_records, 0);
        assert!(reopened.asset_issues.is_empty());
        assert_eq!(reopened.project.assets().read(asset_id).unwrap().len(), 8);
    }

    #[test]
    fn transparent_fill_uses_transparent_canvas_semantics() {
        let directory = tempdir().unwrap();
        let transparent = Rgba {
            red: 99,
            green: 88,
            blue: 77,
            alpha: 0,
        };
        let project =
            create_blank_animation_project(directory.path(), options(transparent)).unwrap();
        assert_eq!(
            project.manifest().canvas.background,
            CanvasBackground::Transparent
        );
        let asset = project.manifest().timeline.frames[0].asset_id;
        assert_eq!(
            project.assets().read(asset).unwrap(),
            [99, 88, 77, 0, 99, 88, 77, 0]
        );
    }

    #[test]
    fn invalid_inputs_and_limits_finish_before_filesystem_creation() {
        let base = tempdir().unwrap();
        let cases = [
            (
                "nil-project",
                {
                    let mut value = options(Rgba::TRANSPARENT);
                    value.project_id = ProjectId::NIL;
                    value
                },
                "project id",
            ),
            (
                "nil-frame",
                {
                    let mut value = options(Rgba::TRANSPARENT);
                    value.frame_id = FrameId::NIL;
                    value
                },
                "frame id",
            ),
            (
                "empty-version",
                {
                    let mut value = options(Rgba::TRANSPARENT);
                    value.app_version = "  ".to_owned();
                    value
                },
                "version",
            ),
            (
                "oversized",
                {
                    let mut value = options(Rgba::TRANSPARENT);
                    value.canvas = PhysicalSize::new(65_536, 1).unwrap();
                    value
                },
                "65535",
            ),
            (
                "bounded",
                {
                    let mut value = options(Rgba::TRANSPARENT);
                    value.frame_limit_bytes = 7;
                    value
                },
                "above",
            ),
        ];
        for (name, options, expected) in cases {
            let root = base.path().join(name);
            let error = create_blank_animation_project(&root, options).unwrap_err();
            assert!(error.to_string().contains(expected));
            assert!(!root.exists());
        }
    }

    #[test]
    fn existing_project_is_never_replaced() {
        let directory = tempdir().unwrap();
        let project =
            create_blank_animation_project(directory.path(), options(Rgba::TRANSPARENT)).unwrap();
        drop(project);
        let mut replacement = options(Rgba {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        });
        replacement.project_id = ProjectId::from_u128(99);
        assert!(matches!(
            create_blank_animation_project(directory.path(), replacement),
            Err(CreateBlankAnimationError::CreateProject {
                source: ProjectError::ManifestAlreadyExists(_)
            })
        ));
        let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(
            reopened.project.manifest().project_id,
            ProjectId::from_u128(1)
        );
    }
}
