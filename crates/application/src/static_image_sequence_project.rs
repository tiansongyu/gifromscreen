use std::{collections::BTreeMap, path::Path};

use gif_from_screen_domain::{
    FrameId, GifExportPreset, GifLoop, GifPaletteStrategy, ProjectId, SourceProvenance, UnixTimeMs,
};
use gif_from_screen_media::{DecodedStaticImageSequence, LoopBehavior};
use gif_from_screen_project::ActiveProject;
use thiserror::Error;

use crate::{
    import_project::{PersistDecodedAnimationError, map_persist_error},
    rgba_project::{RgbaProjectFrame, RgbaProjectOptions, persist_rgba_project},
};

/// Manifest key for the initial export preset attached to an imported image sequence.
pub const IMPORTED_STATIC_SEQUENCE_PRESET_NAME: &str = "Imported Image Sequence";

/// Injected identities and source labels for a decoded static-image sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaticImageSequenceProjectOptions {
    /// Stable project identity.
    pub project_id: ProjectId,
    /// Stable identities consumed in sequence order. Extra identities are ignored.
    pub frame_ids: Vec<FrameId>,
    /// Application version persisted in the project manifest.
    pub app_version: String,
    /// Wall-clock project creation timestamp.
    pub created_at: UnixTimeMs,
    /// One non-empty source filename or display label per sequence frame.
    pub display_names: Vec<String>,
}

/// Failure while converting a decoded static-image sequence into an editable project.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PersistStaticImageSequenceError {
    /// The normalized sequence unexpectedly contained no frame.
    #[error("cannot create a project from an empty static-image sequence")]
    EmptySequence,
    /// Format provenance no longer has one entry per decoded frame.
    #[error("static-image sequence has {frames} frames but {formats} detected format entries")]
    FormatCountMismatch {
        /// Number of decoded frames.
        frames: usize,
        /// Number of detected source formats.
        formats: usize,
    },
    /// Caller metadata must identify every decoded source.
    #[error(
        "static-image sequence has {required} frames but {provided} source labels were supplied"
    )]
    DisplayNameCountMismatch {
        /// Number of labels required.
        required: usize,
        /// Number of labels supplied.
        provided: usize,
    },
    /// A source label contained no visible text.
    #[error("static-image source label at index {image_index} must not be empty")]
    EmptyDisplayName {
        /// Zero-based source position.
        image_index: usize,
    },
    /// A zero finite repeat count is not a valid imported sequence policy.
    #[error("static-image sequence finite loop count must be positive")]
    ZeroFiniteLoopCount,
    /// Shared frame validation or durable persistence failed.
    #[error("could not persist decoded static-image sequence: {source}")]
    Persistence {
        /// Shared imported-animation failure retaining precise storage context.
        #[source]
        source: Box<PersistDecodedAnimationError>,
    },
}

/// Persists a decoded image sequence with per-frame source provenance.
///
/// Every frame remains in source order with its assembled duration. Each display name is paired
/// with the corresponding content-detected MIME type. Pixels use the same content-addressed asset
/// store and crash-recoverable journal path as GIF and single-image imports, so identical images
/// share one immutable asset without merging their timeline entries.
///
/// # Errors
///
/// Returns [`PersistStaticImageSequenceError`] before filesystem creation for missing/malformed
/// source metadata or an invalid loop count. Shared identity, frame, domain, and storage failures
/// retain their typed source through [`PersistStaticImageSequenceError::Persistence`].
pub fn persist_decoded_static_image_sequence(
    root: impl AsRef<Path>,
    sequence: DecodedStaticImageSequence,
    options: StaticImageSequenceProjectOptions,
) -> Result<ActiveProject, PersistStaticImageSequenceError> {
    let (animation, formats) = sequence.into_parts();
    let frame_count = animation.frames().len();
    if frame_count == 0 {
        return Err(PersistStaticImageSequenceError::EmptySequence);
    }
    if formats.len() != frame_count {
        return Err(PersistStaticImageSequenceError::FormatCountMismatch {
            frames: frame_count,
            formats: formats.len(),
        });
    }
    if options.display_names.len() != frame_count {
        return Err(PersistStaticImageSequenceError::DisplayNameCountMismatch {
            required: frame_count,
            provided: options.display_names.len(),
        });
    }
    if let Some(image_index) = options
        .display_names
        .iter()
        .position(|display_name| display_name.trim().is_empty())
    {
        return Err(PersistStaticImageSequenceError::EmptyDisplayName { image_index });
    }
    let repeat = match animation.loop_behavior() {
        LoopBehavior::Once => GifLoop::Finite(1),
        LoopBehavior::Infinite => GifLoop::Infinite,
        LoopBehavior::Finite(0) => {
            return Err(PersistStaticImageSequenceError::ZeroFiniteLoopCount);
        }
        LoopBehavior::Finite(repeats) => GifLoop::Finite(repeats),
    };

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
    let source_provenance = options
        .display_names
        .into_iter()
        .zip(formats)
        .map(|(display_name, format)| SourceProvenance::Imported {
            display_name,
            media_type: format.media_type().to_owned(),
        })
        .collect();
    let export_presets = BTreeMap::from([(
        IMPORTED_STATIC_SEQUENCE_PRESET_NAME.to_owned(),
        GifExportPreset {
            colors: 256,
            palette: GifPaletteStrategy::Adaptive,
            repeat,
            alpha_threshold: 1,
        },
    )]);
    persist_rgba_project(
        root,
        &frames,
        RgbaProjectOptions {
            project_id: options.project_id,
            frame_ids: options.frame_ids,
            app_version: options.app_version,
            created_at: options.created_at,
            source_provenance,
            export_presets,
        },
    )
    .map_err(map_persist_error)
    .map_err(|source| PersistStaticImageSequenceError::Persistence {
        source: Box::new(source),
    })
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, num::NonZeroU64};

    use gif_from_screen_domain::{ProjectRevision, SourceProvenance};
    use gif_from_screen_media::{
        StaticImageDecodeOptions, StaticImageSequenceOptions, assemble_static_image_sequence,
        decode_static_image_with_format,
    };
    use gif_from_screen_project::{LockPolicy, ProjectError};
    use tempfile::tempdir;

    use super::*;

    const PNG_ALPHA: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1, 1, 3,
        0, 0, 0, 206, 236, 237, 201, 0, 0, 0, 6, 80, 76, 84, 69, 0, 255, 0, 255, 0, 0, 209, 155,
        74, 174, 0, 0, 0, 1, 116, 82, 78, 83, 64, 54, 58, 153, 246, 0, 0, 0, 10, 73, 68, 65, 84, 8,
        215, 99, 104, 0, 0, 0, 130, 0, 129, 221, 67, 106, 244, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
        96, 130,
    ];
    const WEBP_ALPHA: &[u8] = &[
        82, 73, 70, 70, 32, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 19, 0, 0, 0, 47, 1, 0, 0, 16,
        15, 48, 255, 251, 31, 15, 250, 15, 7, 21, 136, 232, 127, 0, 0,
    ];

    fn decoded(bytes: &[u8], duration_us: u64) -> gif_from_screen_media::DecodedStaticImage {
        decode_static_image_with_format(
            Cursor::new(bytes),
            &StaticImageDecodeOptions {
                frame_duration_us: NonZeroU64::new(duration_us).unwrap(),
                ..StaticImageDecodeOptions::default()
            },
        )
        .unwrap()
    }

    fn sequence(
        images: Vec<gif_from_screen_media::DecodedStaticImage>,
        loop_behavior: LoopBehavior,
    ) -> DecodedStaticImageSequence {
        assemble_static_image_sequence(
            images,
            &StaticImageSequenceOptions {
                loop_behavior,
                ..StaticImageSequenceOptions::default()
            },
        )
        .unwrap()
    }

    fn options(names: &[&str], frame_ids: &[u128]) -> StaticImageSequenceProjectOptions {
        StaticImageSequenceProjectOptions {
            project_id: ProjectId::from_u128(1),
            frame_ids: frame_ids.iter().copied().map(FrameId::from_u128).collect(),
            app_version: "sequence-project-test".to_owned(),
            created_at: UnixTimeMs::new(42),
            display_names: names.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn persists_order_timing_mime_loop_and_clean_reopen() {
        let directory = tempdir().unwrap();
        let sequence = sequence(
            vec![decoded(PNG_ALPHA, 10_000), decoded(WEBP_ALPHA, 20_000)],
            LoopBehavior::Infinite,
        );
        let project = persist_decoded_static_image_sequence(
            directory.path(),
            sequence,
            options(&["first.png", "second.webp"], &[11, 22]),
        )
        .unwrap();
        let manifest = project.manifest();
        assert_eq!(manifest.revision, ProjectRevision::new(1));
        assert_eq!(
            manifest
                .timeline
                .frames
                .iter()
                .map(|frame| (frame.id, frame.duration.get()))
                .collect::<Vec<_>>(),
            [
                (FrameId::from_u128(11), 10_000),
                (FrameId::from_u128(22), 20_000)
            ]
        );
        assert_eq!(
            manifest.source_provenance,
            [
                SourceProvenance::Imported {
                    display_name: "first.png".to_owned(),
                    media_type: "image/png".to_owned(),
                },
                SourceProvenance::Imported {
                    display_name: "second.webp".to_owned(),
                    media_type: "image/webp".to_owned(),
                }
            ]
        );
        assert_eq!(
            manifest.export_presets[IMPORTED_STATIC_SEQUENCE_PRESET_NAME].repeat,
            GifLoop::Infinite
        );
        drop(project);

        let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(reopened.journal_recovery.is_clean());
        assert!(reopened.asset_issues.is_empty());
        assert_eq!(reopened.project.manifest().timeline.frames.len(), 2);
    }

    #[test]
    fn repeated_source_pixels_share_one_asset_without_losing_frames() {
        let directory = tempdir().unwrap();
        let sequence = sequence(
            vec![decoded(PNG_ALPHA, 10), decoded(PNG_ALPHA, 20)],
            LoopBehavior::Finite(3),
        );
        let project = persist_decoded_static_image_sequence(
            directory.path(),
            sequence,
            options(&["a.png", "b.png"], &[1, 2]),
        )
        .unwrap();
        assert_eq!(project.manifest().timeline.frames.len(), 2);
        assert_eq!(project.manifest().assets.len(), 1);
        assert_eq!(
            project.manifest().timeline.frames[0].asset_id,
            project.manifest().timeline.frames[1].asset_id
        );
        assert_eq!(
            project.manifest().export_presets[IMPORTED_STATIC_SEQUENCE_PRESET_NAME].repeat,
            GifLoop::Finite(3)
        );
    }

    #[test]
    fn labels_and_shared_identity_errors_finish_before_project_creation() {
        let base = tempdir().unwrap();
        let make_sequence = || {
            sequence(
                vec![decoded(PNG_ALPHA, 10), decoded(PNG_ALPHA, 20)],
                LoopBehavior::Once,
            )
        };

        let missing_label_root = base.path().join("missing-label");
        assert!(matches!(
            persist_decoded_static_image_sequence(
                &missing_label_root,
                make_sequence(),
                options(&["one.png"], &[1, 2])
            ),
            Err(PersistStaticImageSequenceError::DisplayNameCountMismatch {
                required: 2,
                provided: 1
            })
        ));
        assert!(!missing_label_root.exists());

        let empty_label_root = base.path().join("empty-label");
        assert!(matches!(
            persist_decoded_static_image_sequence(
                &empty_label_root,
                make_sequence(),
                options(&["one.png", "  "], &[1, 2])
            ),
            Err(PersistStaticImageSequenceError::EmptyDisplayName { image_index: 1 })
        ));
        assert!(!empty_label_root.exists());

        let short_ids_root = base.path().join("short-ids");
        assert!(matches!(
            persist_decoded_static_image_sequence(
                &short_ids_root,
                make_sequence(),
                options(&["one.png", "two.png"], &[1])
            ),
            Err(PersistStaticImageSequenceError::Persistence { source })
                if matches!(
                    source.as_ref(),
                    PersistDecodedAnimationError::InsufficientFrameIds {
                        required: 2,
                        provided: 1
                    }
                )
        ));
        assert!(!short_ids_root.exists());
    }

    #[test]
    fn existing_project_is_not_replaced() {
        let directory = tempdir().unwrap();
        let first = sequence(vec![decoded(PNG_ALPHA, 10)], LoopBehavior::Once);
        let project = persist_decoded_static_image_sequence(
            directory.path(),
            first,
            options(&["first.png"], &[1]),
        )
        .unwrap();
        drop(project);

        let second = sequence(vec![decoded(WEBP_ALPHA, 20)], LoopBehavior::Once);
        assert!(matches!(
            persist_decoded_static_image_sequence(
                directory.path(),
                second,
                options(&["second.webp"], &[2])
            ),
            Err(PersistStaticImageSequenceError::Persistence { source })
                if matches!(
                    source.as_ref(),
                    PersistDecodedAnimationError::CreateProject {
                        source: ProjectError::ManifestAlreadyExists(_)
                    }
                )
        ));
        let reopened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(reopened.project.manifest().timeline.frames.len(), 1);
        assert_eq!(
            reopened.project.manifest().project_id,
            ProjectId::from_u128(1)
        );
    }
}
