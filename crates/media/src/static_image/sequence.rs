use std::num::NonZeroU64;

use thiserror::Error;

use crate::{DecodeLimits, DecodedAnimation, LoopBehavior};

use super::{DecodedStaticImage, StaticImageFormat};

/// Duration assignment applied while assembling decoded static images.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StaticImageSequenceDurationPolicy {
    /// Keep the positive duration attached by each image's decoder.
    #[default]
    PreserveDecoded,
    /// Assign the same caller-selected positive duration to every frame.
    Uniform(NonZeroU64),
}

/// Bounds, timing, and playback metadata for a static-image sequence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StaticImageSequenceOptions {
    /// Limits re-applied to the complete assembled animation.
    pub limits: DecodeLimits,
    /// Whether decoded durations are preserved or replaced uniformly.
    pub duration: StaticImageSequenceDurationPolicy,
    /// Playback behavior for the assembled animation.
    pub loop_behavior: LoopBehavior,
}

/// A normalized animation plus the content-detected format of every source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedStaticImageSequence {
    formats: Vec<StaticImageFormat>,
    animation: DecodedAnimation,
}

impl DecodedStaticImageSequence {
    /// Content-detected formats in the same order as the animation frames.
    pub fn formats(&self) -> &[StaticImageFormat] {
        &self.formats
    }

    /// Borrows the assembled full-canvas animation.
    pub const fn animation(&self) -> &DecodedAnimation {
        &self.animation
    }

    /// Consumes the sequence and returns only its animation.
    pub fn into_animation(self) -> DecodedAnimation {
        self.animation
    }

    /// Consumes the sequence without discarding per-frame source formats.
    pub fn into_parts(self) -> (DecodedAnimation, Vec<StaticImageFormat>) {
        (self.animation, self.formats)
    }
}

/// Failure while validating or assembling a static-image sequence.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum StaticImageSequenceError {
    /// At least one decoded image is required.
    #[error("a static-image sequence cannot be empty")]
    EmptySequence,
    /// The selected finite repeat count must be positive.
    #[error("a static-image sequence finite loop count must be positive")]
    ZeroFiniteLoopCount,
    /// The complete sequence exceeds its configured frame-count limit.
    #[error("static-image sequence has {actual} frames, above the configured limit of {limit}")]
    FrameLimitExceeded {
        /// Number of supplied static images.
        actual: usize,
        /// Maximum accepted frame count.
        limit: usize,
    },
    /// A source no longer contains exactly one static frame.
    #[error("static image {image_index} contains {actual} frames; expected exactly one")]
    InvalidSourceFrameCount {
        /// Zero-based source position.
        image_index: usize,
        /// Unexpected source frame count.
        actual: usize,
    },
    /// A source reports an empty canvas.
    #[error("static image {image_index} has empty dimensions {width}x{height}")]
    EmptyDimensions {
        /// Zero-based source position.
        image_index: usize,
        /// Reported canvas width.
        width: u16,
        /// Reported canvas height.
        height: u16,
    },
    /// A source exceeds the configured width bound.
    #[error("static image {image_index} width {actual} exceeds the configured limit of {limit}")]
    WidthLimitExceeded {
        /// Zero-based source position.
        image_index: usize,
        /// Reported source width.
        actual: u16,
        /// Maximum accepted width.
        limit: u16,
    },
    /// A source exceeds the configured height bound.
    #[error("static image {image_index} height {actual} exceeds the configured limit of {limit}")]
    HeightLimitExceeded {
        /// Zero-based source position.
        image_index: usize,
        /// Reported source height.
        actual: u16,
        /// Maximum accepted height.
        limit: u16,
    },
    /// Every source must match the first image's canvas in this initial policy.
    #[error(
        "static image {image_index} dimensions {actual_width}x{actual_height} do not match sequence canvas {expected_width}x{expected_height}"
    )]
    DimensionMismatch {
        /// Zero-based source position.
        image_index: usize,
        /// Width established by the first image.
        expected_width: u16,
        /// Height established by the first image.
        expected_height: u16,
        /// Rejected source width.
        actual_width: u16,
        /// Rejected source height.
        actual_height: u16,
    },
    /// A source frame does not contain one packed RGBA8 value per canvas pixel.
    #[error(
        "static image {image_index} RGBA buffer has {actual_bytes} bytes; expected {expected_bytes}"
    )]
    InvalidRgbaLength {
        /// Zero-based source position.
        image_index: usize,
        /// Required packed byte count.
        expected_bytes: u64,
        /// Actual source byte count.
        actual_bytes: u64,
    },
    /// One source buffer length cannot be represented by the public byte counter.
    #[error("static image {image_index} RGBA byte length exceeds u64")]
    RgbaLengthOutOfRange {
        /// Zero-based source position.
        image_index: usize,
    },
    /// Adding another frame overflowed the cumulative RGBA byte count.
    #[error("static-image sequence RGBA byte count overflowed at image {image_index}")]
    TotalRgbaBytesOverflow {
        /// Zero-based source position that overflowed the counter.
        image_index: usize,
    },
    /// The complete sequence exceeds its retained RGBA limit.
    #[error(
        "static-image sequence needs {required_bytes} RGBA bytes, above the configured limit of {limit_bytes}"
    )]
    TotalRgbaBytesLimitExceeded {
        /// Bytes required by all accepted frames.
        required_bytes: u64,
        /// Maximum retained RGBA bytes.
        limit_bytes: u64,
    },
    /// A preserved source duration is not positive.
    #[error("static image {image_index} has invalid duration {duration_us} microseconds")]
    InvalidFrameDuration {
        /// Zero-based source position.
        image_index: usize,
        /// Rejected duration.
        duration_us: u64,
    },
    /// The effective sequence duration overflowed `u64`.
    #[error("static-image sequence duration overflowed at image {image_index}")]
    DurationOverflow {
        /// Zero-based source position that overflowed the counter.
        image_index: usize,
    },
    /// The platform could not reserve the output frame list.
    #[error("could not reserve a {requested}-frame static-image sequence")]
    FrameListAllocationFailed {
        /// Number of frame slots requested.
        requested: usize,
    },
    /// The platform could not reserve the per-frame format list.
    #[error("could not reserve {requested} static-image format entries")]
    FormatListAllocationFailed {
        /// Number of format slots requested.
        requested: usize,
    },
}

/// Assembles decoded static images into one bounded, same-size animation.
///
/// Source order is playback order. Every source is revalidated as a single,
/// tightly packed RGBA8 frame. The first image establishes the canvas; this
/// initial policy rejects rather than fits differently sized images. Per-frame
/// content-detected formats remain available on the returned sequence.
///
/// # Errors
///
/// Returns [`StaticImageSequenceError`] for empty input, a zero finite loop,
/// malformed source state, dimension mismatch, configured limit violations,
/// counter overflow, invalid preserved duration, or bounded allocation failure.
pub fn assemble_static_image_sequence(
    images: Vec<DecodedStaticImage>,
    options: &StaticImageSequenceOptions,
) -> Result<DecodedStaticImageSequence, StaticImageSequenceError> {
    let (width, height) = validate_sequence(&images, options)?;
    let mut frames = Vec::new();
    frames.try_reserve_exact(images.len()).map_err(|_| {
        StaticImageSequenceError::FrameListAllocationFailed {
            requested: images.len(),
        }
    })?;
    let mut formats = Vec::new();
    formats.try_reserve_exact(images.len()).map_err(|_| {
        StaticImageSequenceError::FormatListAllocationFailed {
            requested: images.len(),
        }
    })?;

    for (image_index, image) in images.into_iter().enumerate() {
        let DecodedStaticImage { format, animation } = image;
        let mut source_frames = animation.frames;
        if source_frames.len() != 1 {
            return Err(StaticImageSequenceError::InvalidSourceFrameCount {
                image_index,
                actual: source_frames.len(),
            });
        }
        let Some(mut frame) = source_frames.pop() else {
            return Err(StaticImageSequenceError::InvalidSourceFrameCount {
                image_index,
                actual: 0,
            });
        };
        if let StaticImageSequenceDurationPolicy::Uniform(duration) = options.duration {
            frame.duration_us = duration.get();
        }
        formats.push(format);
        frames.push(frame);
    }

    Ok(DecodedStaticImageSequence {
        formats,
        animation: DecodedAnimation {
            width,
            height,
            frames,
            loop_behavior: options.loop_behavior,
        },
    })
}

fn validate_sequence(
    images: &[DecodedStaticImage],
    options: &StaticImageSequenceOptions,
) -> Result<(u16, u16), StaticImageSequenceError> {
    let first = images
        .first()
        .ok_or(StaticImageSequenceError::EmptySequence)?;
    if matches!(options.loop_behavior, LoopBehavior::Finite(0)) {
        return Err(StaticImageSequenceError::ZeroFiniteLoopCount);
    }
    if images.len() > options.limits.max_frames {
        return Err(StaticImageSequenceError::FrameLimitExceeded {
            actual: images.len(),
            limit: options.limits.max_frames,
        });
    }
    let expected_width = first.animation.width;
    let expected_height = first.animation.height;
    let mut total_rgba_bytes = 0_u64;
    let mut total_duration_us = 0_u64;
    for (image_index, image) in images.iter().enumerate() {
        validate_dimensions(
            image_index,
            image.animation.width,
            image.animation.height,
            expected_width,
            expected_height,
            options.limits,
        )?;
        let [frame] = image.animation.frames.as_slice() else {
            return Err(StaticImageSequenceError::InvalidSourceFrameCount {
                image_index,
                actual: image.animation.frames.len(),
            });
        };
        let expected_bytes = checked_frame_bytes(expected_width, expected_height, image_index)?;
        let actual_bytes = u64::try_from(frame.rgba.len())
            .map_err(|_| StaticImageSequenceError::RgbaLengthOutOfRange { image_index })?;
        if actual_bytes != expected_bytes {
            return Err(StaticImageSequenceError::InvalidRgbaLength {
                image_index,
                expected_bytes,
                actual_bytes,
            });
        }
        total_rgba_bytes = accumulate_rgba_bytes(
            total_rgba_bytes,
            actual_bytes,
            image_index,
            options.limits.max_total_rgba_bytes,
        )?;
        let duration_us = match options.duration {
            StaticImageSequenceDurationPolicy::PreserveDecoded => frame.duration_us,
            StaticImageSequenceDurationPolicy::Uniform(duration) => duration.get(),
        };
        if duration_us == 0 {
            return Err(StaticImageSequenceError::InvalidFrameDuration {
                image_index,
                duration_us,
            });
        }
        total_duration_us = total_duration_us
            .checked_add(duration_us)
            .ok_or(StaticImageSequenceError::DurationOverflow { image_index })?;
    }
    Ok((expected_width, expected_height))
}

fn validate_dimensions(
    image_index: usize,
    width: u16,
    height: u16,
    expected_width: u16,
    expected_height: u16,
    limits: DecodeLimits,
) -> Result<(), StaticImageSequenceError> {
    if width == 0 || height == 0 {
        return Err(StaticImageSequenceError::EmptyDimensions {
            image_index,
            width,
            height,
        });
    }
    if width > limits.max_width {
        return Err(StaticImageSequenceError::WidthLimitExceeded {
            image_index,
            actual: width,
            limit: limits.max_width,
        });
    }
    if height > limits.max_height {
        return Err(StaticImageSequenceError::HeightLimitExceeded {
            image_index,
            actual: height,
            limit: limits.max_height,
        });
    }
    if width != expected_width || height != expected_height {
        return Err(StaticImageSequenceError::DimensionMismatch {
            image_index,
            expected_width,
            expected_height,
            actual_width: width,
            actual_height: height,
        });
    }
    Ok(())
}

fn checked_frame_bytes(
    width: u16,
    height: u16,
    image_index: usize,
) -> Result<u64, StaticImageSequenceError> {
    u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(StaticImageSequenceError::TotalRgbaBytesOverflow { image_index })
}

fn accumulate_rgba_bytes(
    accumulated: u64,
    frame_bytes: u64,
    image_index: usize,
    limit_bytes: u64,
) -> Result<u64, StaticImageSequenceError> {
    let required_bytes = accumulated
        .checked_add(frame_bytes)
        .ok_or(StaticImageSequenceError::TotalRgbaBytesOverflow { image_index })?;
    if required_bytes > limit_bytes {
        Err(StaticImageSequenceError::TotalRgbaBytesLimitExceeded {
            required_bytes,
            limit_bytes,
        })
    } else {
        Ok(required_bytes)
    }
}

#[cfg(test)]
mod tests {
    use crate::DecodedFrame;

    use super::*;

    fn image(
        format: StaticImageFormat,
        width: u16,
        height: u16,
        rgba: Vec<u8>,
        duration_us: u64,
    ) -> DecodedStaticImage {
        DecodedStaticImage {
            format,
            animation: DecodedAnimation {
                width,
                height,
                frames: vec![DecodedFrame { rgba, duration_us }],
                loop_behavior: LoopBehavior::Once,
            },
        }
    }

    fn pixel(format: StaticImageFormat, rgba: [u8; 4], duration_us: u64) -> DecodedStaticImage {
        image(format, 1, 1, rgba.to_vec(), duration_us)
    }

    #[test]
    fn preserves_input_order_formats_pixels_and_decoded_durations() {
        let sequence = assemble_static_image_sequence(
            vec![
                pixel(StaticImageFormat::Png, [1, 2, 3, 4], 10),
                pixel(StaticImageFormat::Jpeg, [5, 6, 7, 255], 20),
                pixel(StaticImageFormat::WebP, [8, 9, 10, 11], 30),
            ],
            &StaticImageSequenceOptions::default(),
        )
        .unwrap();

        assert_eq!(
            sequence.formats(),
            [
                StaticImageFormat::Png,
                StaticImageFormat::Jpeg,
                StaticImageFormat::WebP
            ]
        );
        assert_eq!(
            (sequence.animation().width(), sequence.animation().height()),
            (1, 1)
        );
        assert_eq!(
            sequence
                .animation()
                .frames()
                .iter()
                .map(DecodedFrame::duration_us)
                .collect::<Vec<_>>(),
            [10, 20, 30]
        );
        assert_eq!(sequence.animation().frames()[0].rgba(), [1, 2, 3, 4]);
        assert_eq!(sequence.animation().frames()[1].rgba(), [5, 6, 7, 255]);
        assert_eq!(sequence.animation().frames()[2].rgba(), [8, 9, 10, 11]);
    }

    #[test]
    fn uniform_duration_and_all_valid_loop_behaviors_are_preserved() {
        for loop_behavior in [
            LoopBehavior::Once,
            LoopBehavior::Infinite,
            LoopBehavior::Finite(1),
            LoopBehavior::Finite(u16::MAX),
        ] {
            let sequence = assemble_static_image_sequence(
                vec![
                    pixel(StaticImageFormat::Png, [0; 4], 10),
                    pixel(StaticImageFormat::Bmp, [255; 4], 20),
                ],
                &StaticImageSequenceOptions {
                    duration: StaticImageSequenceDurationPolicy::Uniform(
                        NonZeroU64::new(77).unwrap(),
                    ),
                    loop_behavior,
                    ..StaticImageSequenceOptions::default()
                },
            )
            .unwrap();
            assert_eq!(sequence.animation().loop_behavior(), loop_behavior);
            assert!(
                sequence
                    .animation()
                    .frames()
                    .iter()
                    .all(|frame| frame.duration_us() == 77)
            );
            let (animation, formats) = sequence.into_parts();
            assert_eq!(animation.frames().len(), 2);
            assert_eq!(formats, [StaticImageFormat::Png, StaticImageFormat::Bmp]);
        }
    }

    #[test]
    fn rejects_empty_zero_loop_and_same_size_policy_violations() {
        assert!(matches!(
            assemble_static_image_sequence(Vec::new(), &StaticImageSequenceOptions::default()),
            Err(StaticImageSequenceError::EmptySequence)
        ));
        assert!(matches!(
            assemble_static_image_sequence(
                vec![pixel(StaticImageFormat::Png, [0; 4], 1)],
                &StaticImageSequenceOptions {
                    loop_behavior: LoopBehavior::Finite(0),
                    ..StaticImageSequenceOptions::default()
                }
            ),
            Err(StaticImageSequenceError::ZeroFiniteLoopCount)
        ));
        assert!(matches!(
            assemble_static_image_sequence(
                vec![
                    image(StaticImageFormat::Png, 2, 1, vec![0; 8], 1),
                    image(StaticImageFormat::Jpeg, 1, 2, vec![0; 8], 1),
                ],
                &StaticImageSequenceOptions::default()
            ),
            Err(StaticImageSequenceError::DimensionMismatch {
                image_index: 1,
                expected_width: 2,
                expected_height: 1,
                actual_width: 1,
                actual_height: 2,
            })
        ));
    }

    #[test]
    fn complete_sequence_reapplies_frame_dimension_and_rgba_limits() {
        let frames = vec![
            pixel(StaticImageFormat::Png, [0; 4], 1),
            pixel(StaticImageFormat::Bmp, [1; 4], 1),
        ];
        assert!(matches!(
            assemble_static_image_sequence(
                frames.clone(),
                &StaticImageSequenceOptions {
                    limits: DecodeLimits {
                        max_frames: 1,
                        ..DecodeLimits::default()
                    },
                    ..StaticImageSequenceOptions::default()
                }
            ),
            Err(StaticImageSequenceError::FrameLimitExceeded {
                actual: 2,
                limit: 1
            })
        ));
        assert!(matches!(
            assemble_static_image_sequence(
                frames,
                &StaticImageSequenceOptions {
                    limits: DecodeLimits {
                        max_total_rgba_bytes: 7,
                        ..DecodeLimits::default()
                    },
                    ..StaticImageSequenceOptions::default()
                }
            ),
            Err(StaticImageSequenceError::TotalRgbaBytesLimitExceeded {
                required_bytes: 8,
                limit_bytes: 7
            })
        ));
        assert!(matches!(
            assemble_static_image_sequence(
                vec![image(StaticImageFormat::Png, 2, 1, vec![0; 8], 1)],
                &StaticImageSequenceOptions {
                    limits: DecodeLimits {
                        max_width: 1,
                        ..DecodeLimits::default()
                    },
                    ..StaticImageSequenceOptions::default()
                }
            ),
            Err(StaticImageSequenceError::WidthLimitExceeded {
                image_index: 0,
                actual: 2,
                limit: 1
            })
        ));
        assert!(matches!(
            assemble_static_image_sequence(
                vec![image(StaticImageFormat::Png, 1, 2, vec![0; 8], 1)],
                &StaticImageSequenceOptions {
                    limits: DecodeLimits {
                        max_height: 1,
                        ..DecodeLimits::default()
                    },
                    ..StaticImageSequenceOptions::default()
                }
            ),
            Err(StaticImageSequenceError::HeightLimitExceeded {
                image_index: 0,
                actual: 2,
                limit: 1
            })
        ));
    }

    #[test]
    fn malformed_frame_duration_and_checked_overflows_are_typed() {
        assert!(matches!(
            assemble_static_image_sequence(
                vec![pixel(StaticImageFormat::Png, [0; 4], 0)],
                &StaticImageSequenceOptions::default()
            ),
            Err(StaticImageSequenceError::InvalidFrameDuration {
                image_index: 0,
                duration_us: 0
            })
        ));
        assert!(matches!(
            assemble_static_image_sequence(
                vec![
                    pixel(StaticImageFormat::Png, [0; 4], u64::MAX),
                    pixel(StaticImageFormat::Jpeg, [1; 4], 1),
                ],
                &StaticImageSequenceOptions::default()
            ),
            Err(StaticImageSequenceError::DurationOverflow { image_index: 1 })
        ));
        assert!(matches!(
            accumulate_rgba_bytes(u64::MAX, 1, 9, u64::MAX),
            Err(StaticImageSequenceError::TotalRgbaBytesOverflow { image_index: 9 })
        ));
        assert!(matches!(
            assemble_static_image_sequence(
                vec![image(StaticImageFormat::Png, 1, 1, vec![0; 3], 1)],
                &StaticImageSequenceOptions::default()
            ),
            Err(StaticImageSequenceError::InvalidRgbaLength {
                image_index: 0,
                expected_bytes: 4,
                actual_bytes: 3
            })
        ));
    }
}
