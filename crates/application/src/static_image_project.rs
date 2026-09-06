use std::path::Path;

use gif_from_screen_domain::{FrameId, GifLoop, ProjectId, UnixTimeMs};
use gif_from_screen_media::{DecodedAnimation, LoopBehavior, StaticImageFormat};
use gif_from_screen_project::ActiveProject;
use thiserror::Error;

use crate::import_project::{
    DecodedAnimationProjectOptions, PersistDecodedAnimationError, persist_imported_animation,
};

/// Manifest key for the default preset attached to an imported static image.
pub const IMPORTED_STATIC_IMAGE_PRESET_NAME: &str = "Imported Image";

/// Injected identity and user-facing metadata for a decoded static-image project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticImageProjectOptions {
    /// Stable identifier assigned to the new project.
    pub project_id: ProjectId,
    /// Stable identifier assigned to the single timeline frame.
    pub frame_id: FrameId,
    /// Application version persisted in the project manifest.
    pub app_version: String,
    /// Wall-clock project creation timestamp.
    pub created_at: UnixTimeMs,
    /// User-facing source filename or import label.
    pub display_name: String,
}

/// Failure while converting one decoded static image into an editable project.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PersistStaticImageError {
    /// A static image must decode to exactly one display frame.
    #[error("static image import requires exactly one decoded frame, got {actual}")]
    FrameCount {
        /// Number of decoded frames supplied by the caller.
        actual: usize,
    },
    /// Static images must use one-shot playback metadata.
    #[error("static image import requires LoopBehavior::Once, got {actual:?}")]
    LoopBehavior {
        /// Unexpected decoded loop metadata.
        actual: LoopBehavior,
    },
    /// The source label was empty or whitespace-only.
    #[error("imported static-image display name must not be empty")]
    EmptyDisplayName,
    /// The injected project identity was nil.
    #[error("project id must not be nil")]
    NilProjectId,
    /// The injected frame identity was nil.
    #[error("static-image frame id must not be nil")]
    NilFrameId,
    /// A future static format is not mapped to a MIME type yet.
    #[error("static image format {format} has no project MIME mapping")]
    UnsupportedFormat {
        /// User-facing format name.
        format: String,
    },
    /// Shared decoded-RGBA validation or durable project persistence failed.
    #[error("could not persist decoded static image: {source}")]
    Persistence {
        /// Shared imported-animation failure retaining storage context.
        #[source]
        source: Box<PersistDecodedAnimationError>,
    },
}

/// Persists one decoded static raster image as a durable editable project.
///
/// This API accepts the common media-layer [`DecodedAnimation`] representation but requires exactly
/// one frame and [`LoopBehavior::Once`]. PNG, JPEG, BMP, and WebP are recorded with their standard
/// MIME types in [`gif_from_screen_domain::SourceProvenance::Imported`]. The `Imported Image`
/// export preset uses adaptive colors and [`GifLoop::Finite`] with a count of one, so an unchanged
/// export plays once.
///
/// Pixel storage, validation, content-addressed deduplication, command journaling, checkpointing,
/// and detailed storage-error conversion are shared with decoded GIF persistence.
///
/// # Errors
///
/// Returns [`PersistStaticImageError`] before creating the project for an invalid frame count,
/// loop behavior, label, project id, frame id, or unknown future format. Shared decoded-frame and
/// filesystem failures retain their typed source in [`PersistStaticImageError::Persistence`].
pub fn persist_decoded_static_image(
    root: impl AsRef<Path>,
    animation: DecodedAnimation,
    format: StaticImageFormat,
    options: StaticImageProjectOptions,
) -> Result<ActiveProject, PersistStaticImageError> {
    let frame_count = animation.frames().len();
    if frame_count != 1 {
        return Err(PersistStaticImageError::FrameCount {
            actual: frame_count,
        });
    }
    let loop_behavior = animation.loop_behavior();
    if loop_behavior != LoopBehavior::Once {
        return Err(PersistStaticImageError::LoopBehavior {
            actual: loop_behavior,
        });
    }
    if options.display_name.trim().is_empty() {
        return Err(PersistStaticImageError::EmptyDisplayName);
    }
    if options.project_id.is_nil() {
        return Err(PersistStaticImageError::NilProjectId);
    }
    if options.frame_id.is_nil() {
        return Err(PersistStaticImageError::NilFrameId);
    }
    let media_type = static_image_media_type(format)?;

    persist_imported_animation(
        root,
        animation,
        DecodedAnimationProjectOptions {
            project_id: options.project_id,
            frame_ids: vec![options.frame_id],
            app_version: options.app_version,
            created_at: options.created_at,
            display_name: options.display_name,
        },
        media_type,
        IMPORTED_STATIC_IMAGE_PRESET_NAME,
        GifLoop::Finite(1),
    )
    .map_err(|source| PersistStaticImageError::Persistence {
        source: Box::new(source),
    })
}

fn static_image_media_type(
    format: StaticImageFormat,
) -> Result<&'static str, PersistStaticImageError> {
    match format {
        StaticImageFormat::Png => Ok("image/png"),
        StaticImageFormat::Jpeg => Ok("image/jpeg"),
        StaticImageFormat::Bmp => Ok("image/bmp"),
        StaticImageFormat::WebP => Ok("image/webp"),
        _ => Err(PersistStaticImageError::UnsupportedFormat {
            format: format.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, fs, io::Cursor};

    use gif::{Encoder, Frame, Repeat};
    use gif_from_screen_domain::{
        GifExportPreset, GifPaletteStrategy, ProjectRevision, SourceProvenance,
    };
    use gif_from_screen_media::{GifDecodeOptions, decode_gif};
    use gif_from_screen_project::{ActiveProject, LockPolicy};
    use tempfile::tempdir;

    use super::*;

    const PALETTE: &[u8] = &[0, 0, 0, 255, 0, 0];

    fn decoded(frame_count: usize, repeat: Option<Repeat>) -> DecodedAnimation {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, 1, 1, PALETTE).unwrap();
            if let Some(repeat) = repeat {
                encoder.set_repeat(repeat).unwrap();
            }
            for _ in 0..frame_count {
                encoder
                    .write_frame(&Frame {
                        width: 1,
                        height: 1,
                        delay: 1,
                        buffer: Cow::Owned(vec![1]),
                        ..Frame::default()
                    })
                    .unwrap();
            }
        }
        decode_gif(Cursor::new(bytes), &GifDecodeOptions::default()).unwrap()
    }

    fn options(project_id: ProjectId, frame_id: FrameId) -> StaticImageProjectOptions {
        StaticImageProjectOptions {
            project_id,
            frame_id,
            app_version: "static-test-1.0".to_owned(),
            created_at: UnixTimeMs::new(42),
            display_name: "source.image".to_owned(),
        }
    }

    #[test]
    fn every_static_format_sets_its_mime_and_play_once_preset() {
        let cases = [
            (StaticImageFormat::Png, "image/png"),
            (StaticImageFormat::Jpeg, "image/jpeg"),
            (StaticImageFormat::Bmp, "image/bmp"),
            (StaticImageFormat::WebP, "image/webp"),
        ];
        for (index, (format, media_type)) in cases.into_iter().enumerate() {
            let directory = tempdir().unwrap();
            let project_id = ProjectId::from_u128(u128::try_from(index).unwrap() + 1);
            let frame_id = FrameId::from_u128(100 + u128::try_from(index).unwrap());
            let project = persist_decoded_static_image(
                directory.path(),
                decoded(1, None),
                format,
                options(project_id, frame_id),
            )
            .unwrap();
            let manifest = project.manifest();

            assert_eq!(manifest.project_id, project_id);
            assert_eq!(manifest.app_version, "static-test-1.0");
            assert_eq!(manifest.created_at, UnixTimeMs::new(42));
            assert_eq!(manifest.timeline.frames[0].id, frame_id);
            assert_eq!(
                manifest.source_provenance,
                [SourceProvenance::Imported {
                    display_name: "source.image".to_owned(),
                    media_type: media_type.to_owned(),
                }]
            );
            assert_eq!(
                manifest.export_presets[IMPORTED_STATIC_IMAGE_PRESET_NAME],
                GifExportPreset {
                    colors: 256,
                    palette: GifPaletteStrategy::Adaptive,
                    repeat: GifLoop::Finite(1),
                    alpha_threshold: 1,
                    options: None,
                }
            );
        }
    }

    #[test]
    fn rejects_multiframe_looping_empty_label_and_nil_ids_before_creation() {
        let base = tempdir().unwrap();
        let valid_options = options(ProjectId::from_u128(1), FrameId::from_u128(1));

        let root = base.path().join("multiple");
        assert!(matches!(
            persist_decoded_static_image(
                &root,
                decoded(2, None),
                StaticImageFormat::Png,
                valid_options.clone(),
            ),
            Err(PersistStaticImageError::FrameCount { actual: 2 })
        ));
        assert!(!root.exists());

        let root = base.path().join("looping");
        assert!(matches!(
            persist_decoded_static_image(
                &root,
                decoded(1, Some(Repeat::Infinite)),
                StaticImageFormat::Png,
                valid_options.clone(),
            ),
            Err(PersistStaticImageError::LoopBehavior {
                actual: LoopBehavior::Infinite
            })
        ));
        assert!(!root.exists());

        let root = base.path().join("empty-label");
        let mut empty_label = valid_options.clone();
        empty_label.display_name = "  ".to_owned();
        assert!(matches!(
            persist_decoded_static_image(
                &root,
                decoded(1, None),
                StaticImageFormat::Png,
                empty_label,
            ),
            Err(PersistStaticImageError::EmptyDisplayName)
        ));
        assert!(!root.exists());

        let root = base.path().join("nil-project");
        assert!(matches!(
            persist_decoded_static_image(
                &root,
                decoded(1, None),
                StaticImageFormat::Png,
                options(ProjectId::NIL, FrameId::from_u128(1)),
            ),
            Err(PersistStaticImageError::NilProjectId)
        ));
        assert!(!root.exists());

        let root = base.path().join("nil-frame");
        assert!(matches!(
            persist_decoded_static_image(
                &root,
                decoded(1, None),
                StaticImageFormat::Png,
                options(ProjectId::from_u128(1), FrameId::NIL),
            ),
            Err(PersistStaticImageError::NilFrameId)
        ));
        assert!(!root.exists());
    }

    #[test]
    fn content_addressed_pixels_deduplicate_and_project_reopens_cleanly() {
        let directory = tempdir().unwrap();
        let animation = decoded(1, None);
        let pixels = animation.frames()[0].rgba().to_vec();
        let project = persist_decoded_static_image(
            directory.path(),
            animation,
            StaticImageFormat::WebP,
            options(ProjectId::from_u128(7), FrameId::from_u128(9)),
        )
        .unwrap();
        let stored_id = project.manifest().timeline.frames[0].asset_id;
        assert_eq!(project.manifest().revision, ProjectRevision::new(1));
        assert_eq!(project.manifest().assets.len(), 1);
        assert_eq!(project.assets().put(&pixels).unwrap(), stored_id);
        assert_eq!(
            fs::read_dir(project.assets().directory()).unwrap().count(),
            1
        );
        drop(project);

        let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(reopened.journal_recovery.is_clean());
        assert!(reopened.asset_issues.is_empty());
        assert_eq!(reopened.project.manifest().timeline.frames.len(), 1);
        assert_eq!(reopened.project.assets().read(stored_id).unwrap(), pixels);
    }
}
