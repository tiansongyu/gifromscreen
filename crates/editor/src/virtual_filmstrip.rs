use std::ops::Range;

use thiserror::Error;

/// Maximum frame count accepted by [`VirtualFilmstripLayout`].
///
/// The explicit one-billion-frame limit is far beyond interactive project sizes while ensuring
/// every internal integer-to-`f64` conversion is exact on all supported platforms.
pub const MAX_VIRTUAL_FILMSTRIP_FRAMES: usize = 1_000_000_000;

/// Allocation-free horizontal layout for a virtualized frame filmstrip.
///
/// Frame `i` occupies the half-open interval
/// `[i * (item_width + gap), i * (item_width + gap) + item_width)`. There is no trailing gap after
/// the final frame. All geometry remains `f64`; a UI should convert only the small, viewport-local
/// coordinates it submits to its renderer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualFilmstripLayout {
    frame_count: usize,
    item_width: f64,
    gap: f64,
    stride: f64,
    total_content_width: f64,
}

impl VirtualFilmstripLayout {
    /// Creates a validated virtual filmstrip layout.
    ///
    /// `item_width` must be finite and strictly positive. `gap` must be finite and non-negative;
    /// zero is supported for a contiguous strip. An empty timeline is valid and has zero content
    /// width.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualFilmstripError`] for invalid dimensions, a frame count above
    /// [`MAX_VIRTUAL_FILMSTRIP_FRAMES`], arithmetic overflow, or geometry whose distinct item
    /// positions cannot be represented by `f64`.
    pub fn new(
        frame_count: usize,
        item_width: f64,
        gap: f64,
    ) -> Result<Self, VirtualFilmstripError> {
        if frame_count > MAX_VIRTUAL_FILMSTRIP_FRAMES {
            return Err(VirtualFilmstripError::FrameCountExceedsLimit {
                frame_count,
                maximum: MAX_VIRTUAL_FILMSTRIP_FRAMES,
            });
        }
        if !item_width.is_finite() || item_width <= 0.0 {
            return Err(VirtualFilmstripError::InvalidItemWidth(item_width));
        }
        if !gap.is_finite() || gap < 0.0 {
            return Err(VirtualFilmstripError::InvalidGap(gap));
        }

        let stride = item_width + gap;
        if !stride.is_finite() {
            return Err(VirtualFilmstripError::GeometryOverflow);
        }
        if frame_count > 1 && gap > 0.0 && stride <= item_width {
            return Err(VirtualFilmstripError::GeometryPrecisionLoss);
        }

        let total_content_width = if frame_count == 0 {
            0.0
        } else {
            let last_index = frame_count - 1;
            let last_start = index_scalar(last_index) * stride;
            if !last_start.is_finite() {
                return Err(VirtualFilmstripError::GeometryOverflow);
            }
            if last_index > 0 {
                let previous_start = index_scalar(last_index - 1) * stride;
                if last_start <= previous_start {
                    return Err(VirtualFilmstripError::GeometryPrecisionLoss);
                }
            }
            let total = last_start + item_width;
            if !total.is_finite() {
                return Err(VirtualFilmstripError::GeometryOverflow);
            }
            if total <= last_start {
                return Err(VirtualFilmstripError::GeometryPrecisionLoss);
            }
            total
        };

        Ok(Self {
            frame_count,
            item_width,
            gap,
            stride,
            total_content_width,
        })
    }

    /// Returns the number of frames represented by this layout.
    pub const fn frame_count(self) -> usize {
        self.frame_count
    }

    /// Returns `true` when the timeline contains no frames.
    pub const fn is_empty(self) -> bool {
        self.frame_count == 0
    }

    /// Returns each frame item's width.
    pub const fn item_width(self) -> f64 {
        self.item_width
    }

    /// Returns the space between adjacent frame items.
    pub const fn gap(self) -> f64 {
        self.gap
    }

    /// Returns the distance between adjacent item origins.
    pub const fn stride(self) -> f64 {
        self.stride
    }

    /// Returns the full scrollable content width without a trailing gap.
    pub const fn total_content_width(self) -> f64 {
        self.total_content_width
    }

    /// Returns the strictly bounded range of frames intersecting a viewport plus overscan.
    ///
    /// `viewport_offset` is the non-negative content-space scroll position and `viewport_width`
    /// must be positive. Items and the viewport use half-open intervals, so an item beginning
    /// exactly at the viewport end is excluded. `overscan` is measured in frame items and expands
    /// both ends with saturating, frame-count-clamped arithmetic. A viewport entirely within a gap
    /// can therefore return an empty range when `overscan` is zero. Offsets beyond the content are
    /// accepted and return an empty base range (or trailing overscan frames).
    ///
    /// # Errors
    ///
    /// Returns an error for a negative/non-finite offset, a non-positive/non-finite viewport width,
    /// or overflow while computing the viewport end.
    pub fn visible_range(
        self,
        viewport_offset: f64,
        viewport_width: f64,
        overscan: usize,
    ) -> Result<Range<usize>, VirtualFilmstripError> {
        validate_viewport(viewport_offset, viewport_width)?;
        let viewport_end = viewport_offset + viewport_width;
        if !viewport_end.is_finite() {
            return Err(VirtualFilmstripError::CoordinateOverflow);
        }

        let first = self.first_item_ending_after(viewport_offset);
        let end = self.first_item_starting_at_or_after(viewport_end);
        let base_end = end.max(first);
        Ok(first.saturating_sub(overscan)..base_end.saturating_add(overscan).min(self.frame_count))
    }

    /// Returns the content-space X coordinate of a frame's leading edge.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualFilmstripError::FrameIndexOutOfBounds`] when `index` is not present.
    pub fn x_for_index(self, index: usize) -> Result<f64, VirtualFilmstripError> {
        self.ensure_index(index)?;
        Ok(self.x_for_index_unchecked(index))
    }

    /// Returns the frame whose half-open item interval contains content-space `x`.
    ///
    /// Finite coordinates before/after the strip and coordinates in a gap return `None`.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualFilmstripError::InvalidCoordinate`] for NaN or infinite input.
    pub fn index_at_x(self, x: f64) -> Result<Option<usize>, VirtualFilmstripError> {
        if !x.is_finite() {
            return Err(VirtualFilmstripError::InvalidCoordinate(x));
        }
        let index = self.first_item_ending_after(x);
        if index == self.frame_count || x < self.x_for_index_unchecked(index) {
            Ok(None)
        } else {
            Ok(Some(index))
        }
    }

    /// Returns a clamped scroll offset that makes `index` visible with the smallest movement.
    ///
    /// When an item fits in the viewport, the returned offset makes the complete item visible. If
    /// the item is wider than the viewport, its leading edge is preferred while still respecting
    /// the maximum scroll offset. A viewport wider than the content always returns zero.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid frame index, offset, or viewport width.
    pub fn scroll_offset_to_reveal(
        self,
        index: usize,
        current_offset: f64,
        viewport_width: f64,
    ) -> Result<f64, VirtualFilmstripError> {
        self.ensure_index(index)?;
        validate_viewport(current_offset, viewport_width)?;

        let maximum_offset = (self.total_content_width - viewport_width).max(0.0);
        let current = current_offset.clamp(0.0, maximum_offset);
        let item_start = self.x_for_index_unchecked(index);
        let item_end = item_start + self.item_width;
        let viewport_end = current + viewport_width;

        let desired = if self.item_width > viewport_width || item_start < current {
            item_start
        } else if item_end > viewport_end {
            item_end - viewport_width
        } else {
            current
        };
        Ok(desired.clamp(0.0, maximum_offset))
    }

    fn ensure_index(self, index: usize) -> Result<(), VirtualFilmstripError> {
        if index < self.frame_count {
            Ok(())
        } else {
            Err(VirtualFilmstripError::FrameIndexOutOfBounds {
                index,
                frame_count: self.frame_count,
            })
        }
    }

    fn x_for_index_unchecked(self, index: usize) -> f64 {
        index_scalar(index) * self.stride
    }

    fn item_end(self, index: usize) -> f64 {
        self.x_for_index_unchecked(index) + self.item_width
    }

    fn first_item_ending_after(self, x: f64) -> usize {
        partition_point(self.frame_count, |index| self.item_end(index) <= x)
    }

    fn first_item_starting_at_or_after(self, x: f64) -> usize {
        partition_point(self.frame_count, |index| {
            self.x_for_index_unchecked(index) < x
        })
    }
}

fn index_scalar(index: usize) -> f64 {
    // The public frame-count limit makes this conversion infallible and exactly representable.
    f64::from(u32::try_from(index).expect("validated filmstrip index fits in u32"))
}

fn partition_point(length: usize, mut predicate: impl FnMut(usize) -> bool) -> usize {
    let mut left = 0;
    let mut right = length;
    while left < right {
        let middle = left + (right - left) / 2;
        if predicate(middle) {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    left
}

fn validate_viewport(offset: f64, width: f64) -> Result<(), VirtualFilmstripError> {
    if !offset.is_finite() || offset < 0.0 {
        return Err(VirtualFilmstripError::InvalidViewportOffset(offset));
    }
    if !width.is_finite() || width <= 0.0 {
        return Err(VirtualFilmstripError::InvalidViewportWidth(width));
    }
    Ok(())
}

/// Errors produced by virtual filmstrip geometry and queries.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum VirtualFilmstripError {
    /// Item width must be finite and strictly positive.
    #[error("filmstrip item width must be finite and positive, got {0:?}")]
    InvalidItemWidth(f64),
    /// Gap must be finite and non-negative.
    #[error("filmstrip gap must be finite and non-negative, got {0:?}")]
    InvalidGap(f64),
    /// Frame count exceeds the documented exact-layout limit.
    #[error("filmstrip frame count {frame_count} exceeds the limit of {maximum}")]
    FrameCountExceedsLimit {
        /// Requested frame count.
        frame_count: usize,
        /// Maximum accepted frame count.
        maximum: usize,
    },
    /// Geometry arithmetic produced a value outside finite `f64` space.
    #[error("filmstrip geometry overflowed finite f64 space")]
    GeometryOverflow,
    /// Positive dimensions collapsed onto the same `f64` coordinate.
    #[error("filmstrip geometry cannot represent distinct item coordinates in f64")]
    GeometryPrecisionLoss,
    /// Viewport offset must be finite and non-negative.
    #[error("viewport offset must be finite and non-negative, got {0:?}")]
    InvalidViewportOffset(f64),
    /// Viewport width must be finite and strictly positive.
    #[error("viewport width must be finite and positive, got {0:?}")]
    InvalidViewportWidth(f64),
    /// Adding viewport offset and width exceeded finite `f64` space.
    #[error("viewport coordinates overflowed finite f64 space")]
    CoordinateOverflow,
    /// Hit-test coordinate must be finite.
    #[error("filmstrip X coordinate must be finite, got {0:?}")]
    InvalidCoordinate(f64),
    /// Frame index does not exist in the layout.
    #[error("frame index {index} is outside a filmstrip with {frame_count} frames")]
    FrameIndexOutOfBounds {
        /// Requested zero-based frame index.
        index: usize,
        /// Number of frames in the layout.
        frame_count: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::{MAX_VIRTUAL_FILMSTRIP_FRAMES, VirtualFilmstripError, VirtualFilmstripLayout};

    fn assert_close(actual: f64, expected: f64) {
        let tolerance = expected.abs().max(1.0) * 1e-12;
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected:?}, got {actual:?}"
        );
    }

    #[test]
    fn fifty_thousand_frames_have_exact_bounded_visible_ranges() {
        let layout = VirtualFilmstripLayout::new(50_000, 10.0, 2.0).unwrap();
        assert_eq!(layout.frame_count(), 50_000);
        assert!(!layout.is_empty());
        assert_close(layout.item_width(), 10.0);
        assert_close(layout.gap(), 2.0);
        assert_close(layout.stride(), 12.0);
        assert_close(layout.total_content_width(), 599_998.0);

        let offset = layout.x_for_index(25_000).unwrap();
        assert_eq!(
            layout.visible_range(offset, 120.0, 3).unwrap(),
            24_997..25_013
        );
        assert_eq!(
            layout
                .visible_range(layout.x_for_index(49_999).unwrap(), 10.0, 8)
                .unwrap(),
            49_991..50_000
        );
    }

    #[test]
    fn empty_timeline_is_explicit_and_queries_stay_bounded() {
        let layout = VirtualFilmstripLayout::new(0, 10.0, 2.0).unwrap();
        assert!(layout.is_empty());
        assert_close(layout.total_content_width(), 0.0);
        assert_eq!(layout.visible_range(0.0, 100.0, usize::MAX).unwrap(), 0..0);
        assert_eq!(layout.index_at_x(0.0).unwrap(), None);
        assert_eq!(
            layout.x_for_index(0),
            Err(VirtualFilmstripError::FrameIndexOutOfBounds {
                index: 0,
                frame_count: 0,
            })
        );
        assert!(matches!(
            layout.scroll_offset_to_reveal(0, 0.0, 100.0),
            Err(VirtualFilmstripError::FrameIndexOutOfBounds { .. })
        ));
    }

    #[test]
    fn subpixel_items_and_gap_only_viewports_respect_half_open_edges() {
        let layout = VirtualFilmstripLayout::new(4, 0.25, 0.125).unwrap();
        assert_close(layout.total_content_width(), 1.375);
        assert_eq!(layout.visible_range(0.25, 0.125, 0).unwrap(), 1..1);
        assert_eq!(layout.visible_range(0.374, 0.002, 0).unwrap(), 1..2);
        assert_eq!(layout.visible_range(0.25, 0.125, 1).unwrap(), 0..2);
        assert_eq!(layout.index_at_x(0.25).unwrap(), None);
        assert_eq!(layout.index_at_x(0.375).unwrap(), Some(1));
    }

    #[test]
    fn index_coordinate_roundtrip_holds_across_a_large_layout() {
        let layout = VirtualFilmstripLayout::new(50_001, 7.25, 0.5).unwrap();
        for index in 0..layout.frame_count() {
            let start = layout.x_for_index(index).unwrap();
            assert_eq!(layout.index_at_x(start).unwrap(), Some(index));
            assert_eq!(
                layout
                    .index_at_x(start + layout.item_width() / 2.0)
                    .unwrap(),
                Some(index)
            );
            if index + 1 < layout.frame_count() {
                assert_eq!(
                    layout
                        .index_at_x(start + layout.item_width() + layout.gap() / 2.0)
                        .unwrap(),
                    None
                );
            }
        }
        assert_eq!(layout.index_at_x(-1.0).unwrap(), None);
        assert_eq!(
            layout.index_at_x(layout.total_content_width()).unwrap(),
            None
        );
    }

    #[test]
    fn overscan_saturates_at_both_timeline_edges() {
        let layout = VirtualFilmstripLayout::new(100, 10.0, 2.0).unwrap();
        assert_eq!(layout.visible_range(0.0, 1.0, 5).unwrap(), 0..6);
        assert_eq!(
            layout
                .visible_range(layout.x_for_index(99).unwrap(), 1.0, 5)
                .unwrap(),
            94..100
        );
        assert_eq!(
            layout.visible_range(600.0, 1.0, usize::MAX).unwrap(),
            0..100
        );
        assert_eq!(layout.visible_range(2_000.0, 1.0, 0).unwrap(), 100..100);
    }

    #[test]
    fn reveal_uses_minimum_clamped_scroll_movement() {
        let layout = VirtualFilmstripLayout::new(100, 10.0, 2.0).unwrap();
        assert_close(layout.scroll_offset_to_reveal(0, 0.0, 34.0).unwrap(), 0.0);
        assert_close(layout.scroll_offset_to_reveal(3, 0.0, 34.0).unwrap(), 12.0);
        assert_close(layout.scroll_offset_to_reveal(3, 50.0, 34.0).unwrap(), 36.0);
        assert_close(
            layout.scroll_offset_to_reveal(99, 0.0, 34.0).unwrap(),
            layout.total_content_width() - 34.0,
        );
        assert_close(
            layout.scroll_offset_to_reveal(50, 9.0, 2_000.0).unwrap(),
            0.0,
        );

        let wide_item = VirtualFilmstripLayout::new(2, 100.0, 10.0).unwrap();
        assert_close(
            wide_item.scroll_offset_to_reveal(1, 0.0, 20.0).unwrap(),
            110.0,
        );
    }

    #[test]
    fn invalid_dimensions_coordinates_and_arithmetic_are_typed() {
        for width in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                VirtualFilmstripLayout::new(1, width, 1.0),
                Err(VirtualFilmstripError::InvalidItemWidth(_))
            ));
        }
        for gap in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                VirtualFilmstripLayout::new(1, 10.0, gap),
                Err(VirtualFilmstripError::InvalidGap(_))
            ));
        }
        let layout = VirtualFilmstripLayout::new(1, 10.0, 0.0).unwrap();
        for offset in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                layout.visible_range(offset, 10.0, 0),
                Err(VirtualFilmstripError::InvalidViewportOffset(_))
            ));
        }
        for width in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                layout.visible_range(0.0, width, 0),
                Err(VirtualFilmstripError::InvalidViewportWidth(_))
            ));
        }
        assert_eq!(
            layout.visible_range(f64::MAX, f64::MAX, 0),
            Err(VirtualFilmstripError::CoordinateOverflow)
        );
        assert!(matches!(
            layout.index_at_x(f64::NAN),
            Err(VirtualFilmstripError::InvalidCoordinate(_))
        ));
        assert_eq!(
            VirtualFilmstripLayout::new(2, f64::MAX, f64::MAX),
            Err(VirtualFilmstripError::GeometryOverflow)
        );
        assert_eq!(
            VirtualFilmstripLayout::new(2, 1e308, 1.0),
            Err(VirtualFilmstripError::GeometryPrecisionLoss)
        );
    }

    #[test]
    fn explicit_frame_limit_rejects_usize_extremes_before_float_conversion() {
        assert_eq!(
            VirtualFilmstripLayout::new(usize::MAX, 1.0, 0.0),
            Err(VirtualFilmstripError::FrameCountExceedsLimit {
                frame_count: usize::MAX,
                maximum: MAX_VIRTUAL_FILMSTRIP_FRAMES,
            })
        );
        let layout = VirtualFilmstripLayout::new(MAX_VIRTUAL_FILMSTRIP_FRAMES, 0.25, 0.0).unwrap();
        assert_close(
            layout
                .x_for_index(MAX_VIRTUAL_FILMSTRIP_FRAMES - 1)
                .unwrap(),
            249_999_999.75,
        );
        assert_eq!(
            layout
                .visible_range(layout.total_content_width() - 0.25, 0.25, usize::MAX)
                .unwrap(),
            0..MAX_VIRTUAL_FILMSTRIP_FRAMES
        );
    }

    #[test]
    fn visible_ranges_match_a_linear_reference_and_stay_bounded() {
        let layout = VirtualFilmstripLayout::new(31, 1.25, 0.75).unwrap();
        let widths = [0.01, 0.5, 1.25, 2.0, 9.75, 100.0];
        let overscans = [0, 1, 7, usize::MAX];
        for step in 0_u32..=700 {
            let offset = f64::from(step) / 10.0;
            for width in widths {
                for overscan in overscans {
                    let visible = layout.visible_range(offset, width, overscan).unwrap();
                    let first = (0..layout.frame_count())
                        .find(|index| {
                            layout.x_for_index(*index).unwrap() + layout.item_width() > offset
                        })
                        .unwrap_or(layout.frame_count());
                    let end = (0..layout.frame_count())
                        .find(|index| layout.x_for_index(*index).unwrap() >= offset + width)
                        .unwrap_or(layout.frame_count())
                        .max(first);
                    let expected = first.saturating_sub(overscan)
                        ..end.saturating_add(overscan).min(layout.frame_count());
                    assert_eq!(visible, expected);
                    assert!(visible.start <= visible.end);
                    assert!(visible.end <= layout.frame_count());
                }
            }
        }
    }
}
