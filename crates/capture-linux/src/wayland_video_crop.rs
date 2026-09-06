//! Pure geometry for separating visible content from a padded video buffer.

use gif_from_screen_capture::{
    CaptureError, CaptureErrorKind, PhysicalRect, PhysicalSize, RecoveryHint,
};

/// The unvalidated visible-content rectangle attached to a video buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct VideoCrop {
    /// Horizontal content offset in the transport buffer.
    pub x: i32,
    /// Vertical content offset in the transport buffer.
    pub y: i32,
    /// Visible content width; zero means there are no usable pixels.
    pub width: u32,
    /// Visible content height; zero means there are no usable pixels.
    pub height: u32,
}

/// Validated content and output rectangles in transport-buffer coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameRegion {
    /// All visible source content, excluding transport padding.
    pub content: PhysicalRect,
    /// The requested portion of content, translated into buffer coordinates.
    pub output: PhysicalRect,
}

/// Resolves visible content and an optional content-local user crop.
///
/// An absent metadata rectangle means the complete transport buffer is content.
/// An attached rectangle with either dimension zero has no usable pixels and
/// returns `None`; it must never fall back to exposing the transport buffer.
/// When supplied, `expected_content_size` fixes the source dimensions while
/// allowing the same content to move within its transport buffer.
///
/// # Errors
///
/// Returns an invalid-frame error for malformed or out-of-bounds rectangles,
/// and a source-lost error if the fixed source dimensions have changed.
pub(super) fn resolve_frame_region(
    transport: PhysicalSize,
    metadata: Option<VideoCrop>,
    requested: Option<PhysicalRect>,
    expected_content_size: Option<PhysicalSize>,
) -> Result<Option<FrameRegion>, CaptureError> {
    let content = if let Some(crop) = metadata {
        if crop.width == 0 || crop.height == 0 {
            return Ok(None);
        }
        PhysicalRect::new(crop.x, crop.y, crop.width, crop.height)
    } else {
        PhysicalRect::new(0, 0, transport.width(), transport.height())
    }
    .map_err(|error| CaptureError::invalid_frame(error.message()))?;
    validate_rect(content, transport, "video content crop")?;

    if expected_content_size.is_some_and(|expected| expected != content.size()) {
        return Err(CaptureError::new(
            CaptureErrorKind::SourceLost,
            "window content dimensions changed during fixed-canvas capture; start a new recording",
            RecoveryHint::ChooseDifferentSource,
        ));
    }

    let output = if let Some(crop) = requested {
        validate_rect(crop, content.size(), "requested content-local crop")?;
        let translated_origin = |local: i32, offset: i32| {
            i64::from(local)
                .checked_add(i64::from(offset))
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| {
                    CaptureError::invalid_frame(
                        "requested content-local crop overflows buffer coordinates",
                    )
                })
        };
        PhysicalRect::new(
            translated_origin(crop.origin().x, content.origin().x)?,
            translated_origin(crop.origin().y, content.origin().y)?,
            crop.size().width(),
            crop.size().height(),
        )
        .map_err(|error| CaptureError::invalid_frame(error.message()))?
    } else {
        content
    };

    Ok(Some(FrameRegion { content, output }))
}

fn validate_rect(
    rect: PhysicalRect,
    bounds: PhysicalSize,
    description: &str,
) -> Result<(), CaptureError> {
    let x = i64::from(rect.origin().x);
    let y = i64::from(rect.origin().y);
    let right = x.checked_add(i64::from(rect.size().width()));
    let bottom = y.checked_add(i64::from(rect.size().height()));
    if x < 0
        || y < 0
        || right.is_none_or(|value| value > i64::from(bounds.width()))
        || bottom.is_none_or(|value| value > i64::from(bounds.height()))
    {
        return Err(CaptureError::invalid_frame(format!(
            "{description} is outside its available pixel bounds"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{VideoCrop, resolve_frame_region};
    use gif_from_screen_capture::{CaptureErrorKind, PhysicalRect, PhysicalSize};

    #[test]
    fn absent_metadata_uses_the_complete_transport_buffer() {
        let transport = PhysicalSize::new(1920, 1080).unwrap();
        let region = resolve_frame_region(transport, None, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(region.content, PhysicalRect::new(0, 0, 1920, 1080).unwrap());
        assert_eq!(region.output, region.content);
    }

    #[test]
    fn metadata_removes_padding_on_all_sides() {
        let metadata = VideoCrop {
            x: 32,
            y: 48,
            width: 800,
            height: 600,
        };
        let region = resolve_frame_region(
            PhysicalSize::new(1920, 1080).unwrap(),
            Some(metadata),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(region.content, PhysicalRect::new(32, 48, 800, 600).unwrap());
        assert_eq!(region.output, region.content);
    }

    #[test]
    fn zero_and_partial_zero_metadata_never_expose_transport_padding() {
        for metadata in [
            VideoCrop::default(),
            VideoCrop {
                x: 32,
                y: 48,
                width: 0,
                height: 600,
            },
            VideoCrop {
                x: -1,
                y: i32::MAX,
                width: u32::MAX,
                height: 0,
            },
        ] {
            assert_eq!(
                resolve_frame_region(
                    PhysicalSize::new(1920, 1080).unwrap(),
                    Some(metadata),
                    Some(PhysicalRect::new(0, 0, 100, 100).unwrap()),
                    Some(PhysicalSize::new(800, 600).unwrap()),
                )
                .unwrap(),
                None
            );
        }
    }

    #[test]
    fn negative_and_out_of_bounds_metadata_are_invalid_frames() {
        for (x, y, width, height) in [
            (-1, 0, 800, 600),
            (0, -1, 800, 600),
            (1200, 0, 800, 600),
            (0, 500, 800, 600),
            (1920, 0, 1, 1),
            (0, 1080, 1, 1),
        ] {
            let error = resolve_frame_region(
                PhysicalSize::new(1920, 1080).unwrap(),
                Some(VideoCrop {
                    x,
                    y,
                    width,
                    height,
                }),
                None,
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
        }
    }

    #[test]
    fn metadata_extents_cannot_wrap_into_the_transport_buffer() {
        for (x, y, width, height) in [(i32::MAX, 0, u32::MAX, 1), (0, i32::MAX, 1, u32::MAX)] {
            let error = resolve_frame_region(
                PhysicalSize::new(u32::MAX, u32::MAX).unwrap(),
                Some(VideoCrop {
                    x,
                    y,
                    width,
                    height,
                }),
                None,
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
        }
    }

    #[test]
    fn requested_crop_is_translated_from_content_to_buffer_coordinates() {
        let region = resolve_frame_region(
            PhysicalSize::new(1920, 1080).unwrap(),
            Some(VideoCrop {
                x: 32,
                y: 48,
                width: 800,
                height: 600,
            }),
            Some(PhysicalRect::new(100, 200, 300, 250).unwrap()),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            region.output,
            PhysicalRect::new(132, 248, 300, 250).unwrap()
        );
        assert_eq!(region.content, PhysicalRect::new(32, 48, 800, 600).unwrap());
    }

    #[test]
    fn crop_that_fits_transport_but_not_content_is_rejected() {
        for requested in [
            PhysicalRect::new(700, 0, 200, 100).unwrap(),
            PhysicalRect::new(0, 500, 100, 200).unwrap(),
            PhysicalRect::new(-1, 0, 100, 100).unwrap(),
            PhysicalRect::new(0, -1, 100, 100).unwrap(),
        ] {
            let error = resolve_frame_region(
                PhysicalSize::new(1920, 1080).unwrap(),
                Some(VideoCrop {
                    x: 32,
                    y: 48,
                    width: 800,
                    height: 600,
                }),
                Some(requested),
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
        }
    }

    #[test]
    fn same_size_content_may_move_and_rebases_the_requested_crop() {
        for (x, y) in [(0, 0), (32, 48), (1000, 400)] {
            let region = resolve_frame_region(
                PhysicalSize::new(1920, 1080).unwrap(),
                Some(VideoCrop {
                    x,
                    y,
                    width: 800,
                    height: 600,
                }),
                Some(PhysicalRect::new(100, 200, 300, 250).unwrap()),
                Some(PhysicalSize::new(800, 600).unwrap()),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                region.output,
                PhysicalRect::new(x + 100, y + 200, 300, 250).unwrap()
            );
        }
    }

    #[test]
    fn fixed_content_size_rejects_resizing_even_when_the_user_crop_still_fits() {
        for (width, height) in [(801, 600), (800, 601), (799, 600), (800, 599)] {
            let error = resolve_frame_region(
                PhysicalSize::new(1920, 1080).unwrap(),
                Some(VideoCrop {
                    x: 32,
                    y: 48,
                    width,
                    height,
                }),
                Some(PhysicalRect::new(0, 0, 100, 100).unwrap()),
                Some(PhysicalSize::new(800, 600).unwrap()),
            )
            .unwrap_err();
            assert_eq!(error.kind(), CaptureErrorKind::SourceLost);
            assert!(error.message().contains("start a new recording"));
        }
    }

    #[test]
    fn absent_metadata_also_respects_fixed_content_dimensions() {
        let error = resolve_frame_region(
            PhysicalSize::new(1920, 1080).unwrap(),
            None,
            None,
            Some(PhysicalSize::new(800, 600).unwrap()),
        )
        .unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::SourceLost);
    }

    #[test]
    fn translating_a_valid_local_crop_cannot_overflow_its_signed_origin() {
        for (x, y, local_x, local_y) in [(i32::MAX, 0, 1, 0), (0, i32::MAX, 0, 1)] {
            let error = resolve_frame_region(
                PhysicalSize::new(u32::MAX, u32::MAX).unwrap(),
                Some(VideoCrop {
                    x,
                    y,
                    width: 10,
                    height: 10,
                }),
                Some(PhysicalRect::new(local_x, local_y, 1, 1).unwrap()),
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
        }
    }

    #[test]
    fn exact_content_edges_are_valid_without_exposing_padding() {
        let region = resolve_frame_region(
            PhysicalSize::new(832, 648).unwrap(),
            Some(VideoCrop {
                x: 32,
                y: 48,
                width: 800,
                height: 600,
            }),
            Some(PhysicalRect::new(700, 500, 100, 100).unwrap()),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            region.output,
            PhysicalRect::new(732, 548, 100, 100).unwrap()
        );
    }
}
