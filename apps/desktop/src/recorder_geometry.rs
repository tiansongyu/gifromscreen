//! Recorder geometry in physical desktop pixels, independent of UI scale.
//!
//! The selection is authoritative: window-manager placement, font size and UI
//! zoom must never be fed back as a new capture rectangle. This module does not
//! position native windows or acknowledge asynchronous capture retargets.

use gif_from_screen_capture::{PhysicalPosition, PhysicalRect, PhysicalSize};

/// A validated source and selection in the same global physical coordinate space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RecorderGeometry {
    source: PhysicalRect,
    region: PhysicalRect,
    size_frozen: bool,
}

impl RecorderGeometry {
    /// Rejects an invalid initial selection instead of silently resizing it.
    pub(crate) fn new(source: PhysicalRect, region: PhysicalRect) -> Result<Self, String> {
        if !Bounds::from(source).contains(Bounds::from(region)) {
            return Err("The recording region must be fully inside its capture source".into());
        }
        Ok(Self {
            source,
            region,
            size_frozen: false,
        })
    }

    pub(crate) const fn source(self) -> PhysicalRect {
        self.source
    }

    pub(crate) const fn region(self) -> PhysicalRect {
        self.region
    }

    /// Converts only the origin; the canvas dimensions remain unchanged.
    pub(crate) fn source_local_region(self) -> Result<PhysicalRect, String> {
        let x = i64::from(self.region.origin().x) - i64::from(self.source.origin().x);
        let y = i64::from(self.region.origin().y) - i64::from(self.source.origin().y);
        rect_at(x, y, self.region.size())
            .ok_or_else(|| "Source-local recording coordinates exceed the capture API range".into())
    }

    pub(crate) const fn size_is_frozen(self) -> bool {
        self.size_frozen
    }

    /// Call when native recording successfully starts. Countdown disallows
    /// resizing through its UI stage; a failed start must not freeze the canvas.
    /// Moving remains possible while recording or paused.
    pub(crate) fn freeze_size(&mut self) -> PhysicalSize {
        self.size_frozen = true;
        self.region.size()
    }

    /// Returns the actual clamped region, never a requested-but-unapplied size.
    pub(crate) fn move_to(&mut self, origin: PhysicalPosition) -> PhysicalRect {
        self.move_to_wide(i64::from(origin.x), i64::from(origin.y))
    }

    /// Deltas are physical pixels. Saturation handles arbitrary input deltas
    /// before source clamping without overflowing the signed desktop origin.
    pub(crate) fn move_by(&mut self, dx: i64, dy: i64) -> PhysicalRect {
        self.move_to_wide(
            i64::from(self.region.origin().x).saturating_add(dx),
            i64::from(self.region.origin().y).saturating_add(dy),
        )
    }

    /// Ready-only resize. Oversized input is rejected atomically, not shrunk.
    /// A valid larger size may move the top-left corner back inside the source.
    pub(crate) fn resize(&mut self, size: PhysicalSize) -> Result<PhysicalRect, String> {
        if self.size_frozen {
            return Err("Recording canvas size is frozen; only its position may change".into());
        }
        if size.width() > self.source.size().width() || size.height() > self.source.size().height()
        {
            return Err("Recording dimensions exceed the capture source".into());
        }
        let origin = self.region.origin();
        self.region = PhysicalRect::new(origin.x, origin.y, size.width(), size.height())
            .expect("PhysicalSize guarantees non-empty dimensions");
        Ok(self.move_to(origin))
    }

    /// An explicit snap is all-or-nothing. Never clip or shift a chosen window
    /// to make it fit, and never replace an active recording's canvas.
    pub(crate) fn snap_to(&mut self, region: PhysicalRect) -> Result<(), String> {
        if self.size_frozen {
            return Err("Window snapping is only available before recording.".into());
        }
        *self = Self::new(self.source, region)?;
        Ok(())
    }

    fn move_to_wide(&mut self, x: i64, y: i64) -> PhysicalRect {
        let source = Bounds::from(self.source);
        let max_x = (source.right - i64::from(self.region.size().width())).min(i64::from(i32::MAX));
        let max_y =
            (source.bottom - i64::from(self.region.size().height())).min(i64::from(i32::MAX));
        self.region = rect_at(
            x.clamp(source.left, max_x),
            y.clamp(source.top, max_y),
            self.region.size(),
        )
        .expect("a contained size and i32 source origin have a representable clamped position");
        self.region
    }
}

/// A whole native control panel fits outside the recording, or must be hidden.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlPlacement {
    Visible {
        rect: PhysicalRect,
        workarea_index: usize,
    },
    Hidden,
}

/// Work areas are ordered by preference (normally the source monitor first).
/// For each, try below, above, left and right of the recording, centering on
/// the other axis and clamping to that work area. All rectangles are half-open.
///
/// `panel_size` must include native decorations/shadows; `gap` is physical
/// separation. The panel is never resized, clipped, or placed over the capture.
/// An empty list, oversized panel, or no free space returns `Hidden`. A native
/// adapter must still verify the actual window placement before capture starts.
pub(crate) fn place_controls(
    region: PhysicalRect,
    panel_size: PhysicalSize,
    workareas: &[PhysicalRect],
    gap: u32,
) -> ControlPlacement {
    for (workarea_index, workarea) in workareas.iter().copied().enumerate() {
        if let Some(rect) = place_in_workarea(region, panel_size, workarea, gap) {
            return ControlPlacement::Visible {
                rect,
                workarea_index,
            };
        }
    }
    ControlPlacement::Hidden
}

fn place_in_workarea(
    region: PhysicalRect,
    panel_size: PhysicalSize,
    workarea: PhysicalRect,
    gap: u32,
) -> Option<PhysicalRect> {
    let area = Bounds::from(workarea);
    let capture = Bounds::from(region);
    let width = i64::from(panel_size.width());
    let height = i64::from(panel_size.height());
    let gap = i64::from(gap);
    let max_x = (area.right - width).min(i64::from(i32::MAX));
    let max_y = (area.bottom - height).min(i64::from(i32::MAX));
    if max_x < area.left || max_y < area.top {
        return None;
    }

    // Divide toward negative infinity for deterministic left/top bias when the
    // ideal center lies between physical pixels, including negative monitors.
    let center_x = (capture.left + capture.right - width).div_euclid(2);
    let center_y = (capture.top + capture.bottom - height).div_euclid(2);
    let x = center_x.clamp(area.left, max_x);
    let y = center_y.clamp(area.top, max_y);
    let below = (capture.bottom + gap).max(area.top);
    let above = (capture.top - gap - height).min(max_y);
    let left = (capture.left - gap - width).min(max_x);
    let right = (capture.right + gap).max(area.left);
    let candidates = [
        (below <= max_y, x, below),
        (above >= area.top, x, above),
        (left >= area.left, left, y),
        (right <= max_x, right, y),
    ];
    candidates
        .into_iter()
        .find_map(|(fits, x, y)| fits.then(|| rect_at(x, y, panel_size)).flatten())
}

fn rect_at(x: i64, y: i64, size: PhysicalSize) -> Option<PhysicalRect> {
    PhysicalRect::new(
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        size.width(),
        size.height(),
    )
    .ok()
}

/// i32 origins plus u32 sizes fit i64 even when the far edge exceeds i32.
#[derive(Clone, Copy)]
struct Bounds {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl From<PhysicalRect> for Bounds {
    fn from(rect: PhysicalRect) -> Self {
        let left = i64::from(rect.origin().x);
        let top = i64::from(rect.origin().y);
        Self {
            left,
            top,
            right: left + i64::from(rect.size().width()),
            bottom: top + i64::from(rect.size().height()),
        }
    }
}

impl Bounds {
    fn contains(self, other: Self) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }
}

#[cfg(test)]
mod tests {
    use super::{Bounds, ControlPlacement, RecorderGeometry, place_controls};
    use gif_from_screen_capture::{PhysicalPosition, PhysicalRect, PhysicalSize};

    fn rect(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect {
        PhysicalRect::new(x, y, width, height).unwrap()
    }

    fn size(width: u32, height: u32) -> PhysicalSize {
        PhysicalSize::new(width, height).unwrap()
    }

    fn visible(rect: PhysicalRect, workarea_index: usize) -> ControlPlacement {
        ControlPlacement::Visible {
            rect,
            workarea_index,
        }
    }

    #[test]
    fn source_and_region_are_global_and_initial_invalid_selection_is_rejected() {
        let source = rect(-1920, -100, 1920, 1080);
        let region = rect(-1200, 20, 640, 420);
        let geometry = RecorderGeometry::new(source, region).unwrap();
        assert_eq!(geometry.source(), source);
        assert_eq!(geometry.region(), region);
        assert_eq!(
            geometry.source_local_region().unwrap(),
            rect(720, 120, 640, 420)
        );
        for invalid in [
            rect(-1921, 0, 1, 1),
            rect(-1, 0, 2, 1),
            rect(-100, -101, 1, 1),
            rect(-100, 979, 1, 2),
        ] {
            assert!(RecorderGeometry::new(source, invalid).is_err());
        }
    }

    #[test]
    fn movement_clamps_only_position_and_returns_actual_rectangle() {
        let mut geometry =
            RecorderGeometry::new(rect(-100, -200, 300, 400), rect(0, 0, 10, 20)).unwrap();
        assert_eq!(
            geometry.move_by(i64::MIN, i64::MAX),
            rect(-100, 180, 10, 20)
        );
        assert_eq!(
            geometry.move_by(i64::MAX, i64::MIN),
            rect(190, -200, 10, 20)
        );
        assert_eq!(
            geometry.move_to(PhysicalPosition { x: -20, y: 30 }),
            rect(-20, 30, 10, 20)
        );
        assert_eq!(
            geometry.source_local_region().unwrap(),
            rect(80, 230, 10, 20)
        );
    }

    #[test]
    fn ready_resize_rejects_oversized_dimensions_atomically() {
        let mut geometry =
            RecorderGeometry::new(rect(0, 0, 100, 80), rect(90, 70, 10, 10)).unwrap();
        let before = geometry;
        assert!(geometry.resize(size(101, 10)).is_err());
        assert!(geometry.resize(size(10, 81)).is_err());
        assert_eq!(geometry, before);
        assert_eq!(geometry.resize(size(40, 30)).unwrap(), rect(60, 50, 40, 30));
        assert_eq!(geometry.resize(size(1, 1)).unwrap(), rect(60, 50, 1, 1));
    }

    #[test]
    fn recording_size_is_frozen_through_moves_and_resize_attempts() {
        let mut geometry =
            RecorderGeometry::new(rect(0, 0, 1440, 1000), rect(5, 7, 640, 420)).unwrap();
        assert!(!geometry.size_is_frozen());
        assert_eq!(geometry.freeze_size(), size(640, 420));
        assert!(geometry.size_is_frozen());
        let before = geometry;
        for candidate in [size(638, 394), size(640, 420), size(1, 1)] {
            assert!(geometry.resize(candidate).is_err());
            assert_eq!(geometry, before);
        }
        assert_eq!(
            geometry.move_by(i64::MAX, i64::MAX),
            rect(800, 580, 640, 420)
        );
        assert_eq!(geometry.freeze_size(), size(640, 420));
    }

    #[test]
    fn full_monitor_and_one_pixel_selections_have_no_ui_minimum() {
        let source = rect(-1440, -1000, 1440, 1000);
        let mut full = RecorderGeometry::new(source, source).unwrap();
        assert_eq!(full.move_by(9999, -9999), source);
        assert_eq!(full.source_local_region().unwrap(), rect(0, 0, 1440, 1000));
        assert_eq!(full.resize(size(1, 1)).unwrap(), rect(-1440, -1000, 1, 1));
        assert_eq!(full.move_by(9999, 9999), rect(-1, -1, 1, 1));
    }

    #[test]
    fn extreme_origins_sizes_and_deltas_never_wrap() {
        let source = rect(i32::MIN, i32::MIN, u32::MAX, u32::MAX);
        let mut geometry = RecorderGeometry::new(source, rect(i32::MIN, i32::MIN, 1, 1)).unwrap();
        assert_eq!(
            geometry.move_by(i64::MAX, i64::MAX),
            rect(i32::MAX - 1, i32::MAX - 1, 1, 1)
        );
        assert!(geometry.source_local_region().is_err());
        assert_eq!(
            geometry.move_by(i64::MIN, i64::MIN),
            rect(i32::MIN, i32::MIN, 1, 1)
        );
        assert_eq!(geometry.resize(size(u32::MAX, u32::MAX)).unwrap(), source);

        let beyond_i32_edge = rect(i32::MAX, i32::MAX, u32::MAX, u32::MAX);
        let mut positive =
            RecorderGeometry::new(beyond_i32_edge, rect(i32::MAX, i32::MAX, 1, 1)).unwrap();
        assert_eq!(
            positive.move_by(i64::MAX, i64::MAX),
            rect(i32::MAX, i32::MAX, 1, 1)
        );
    }

    #[test]
    fn panel_tries_below_above_left_right_in_order() {
        let area = rect(0, 0, 100, 100);
        let panel = size(20, 10);
        for (capture, expected) in [
            (rect(40, 40, 20, 20), rect(40, 62, 20, 10)),
            (rect(40, 80, 20, 20), rect(40, 68, 20, 10)),
            (rect(40, 0, 20, 100), rect(18, 45, 20, 10)),
            (rect(0, 0, 20, 100), rect(22, 45, 20, 10)),
        ] {
            assert_eq!(
                place_controls(capture, panel, &[area], 2),
                visible(expected, 0)
            );
        }
    }

    #[test]
    fn panel_clamps_center_without_clipping_and_preserves_physical_gap() {
        let area = rect(-200, -100, 200, 100);
        assert_eq!(
            place_controls(rect(-199, -99, 1, 1), size(120, 30), &[area], 4),
            visible(rect(-200, -94, 120, 30), 0)
        );
        assert_eq!(
            place_controls(rect(-1, -99, 1, 1), size(120, 30), &[area], 4),
            visible(rect(-120, -94, 120, 30), 0)
        );
        assert_eq!(
            place_controls(rect(-5, -90, 1, 1), size(2, 1), &[area], 0),
            visible(rect(-6, -89, 2, 1), 0)
        );
    }

    #[test]
    fn other_workarea_can_host_controls_for_full_monitor_capture() {
        let capture = rect(0, 0, 1440, 1000);
        let other = rect(-800, 32, 800, 568);
        assert_eq!(
            place_controls(capture, size(280, 100), &[capture, other], 8),
            visible(rect(-288, 450, 280, 100), 1)
        );
        // Only a whole panel contained within one work area is acceptable.
        assert_eq!(
            place_controls(capture, size(801, 100), &[capture, other], 8),
            ControlPlacement::Hidden
        );
    }

    #[test]
    fn hidden_is_explicit_when_fullscreen_or_no_whole_panel_fits() {
        let area = rect(0, 0, 1440, 1000);
        assert_eq!(
            place_controls(area, size(280, 100), &[area], 0),
            ControlPlacement::Hidden
        );
        assert_eq!(
            place_controls(rect(1, 1, 1, 1), size(10, 10), &[], 0),
            ControlPlacement::Hidden
        );
        assert_eq!(
            place_controls(rect(1, 1, 1, 1), size(1441, 10), &[area], 0),
            ControlPlacement::Hidden
        );
        assert_eq!(
            place_controls(rect(1, 1, 1, 1), size(10, 1001), &[area], 0),
            ControlPlacement::Hidden
        );
    }

    #[test]
    fn edge_touching_is_allowed_only_when_gap_is_zero() {
        let capture = rect(0, 0, 100, 90);
        let area = rect(0, 0, 100, 100);
        assert_eq!(
            place_controls(capture, size(100, 10), &[area], 0),
            visible(rect(0, 90, 100, 10), 0)
        );
        assert_eq!(
            place_controls(capture, size(100, 10), &[area], 1),
            ControlPlacement::Hidden
        );
        let large_area = rect(i32::MAX, i32::MAX, u32::MAX, u32::MAX);
        assert_eq!(
            place_controls(large_area, size(1, 1), &[large_area], u32::MAX),
            ControlPlacement::Hidden
        );
    }

    fn separated(capture: PhysicalRect, panel: PhysicalRect, gap: u32) -> bool {
        let capture = Bounds::from(capture);
        let panel = Bounds::from(panel);
        let gap = i64::from(gap);
        panel.left >= capture.right + gap
            || panel.right <= capture.left - gap
            || panel.top >= capture.bottom + gap
            || panel.bottom <= capture.top - gap
    }

    #[test]
    fn exhaustive_small_layouts_find_space_if_and_only_if_a_whole_panel_fits() {
        let areas = [rect(-3, -2, 5, 5), rect(2, -2, 5, 5)];
        for x in -4..=4 {
            for y in -3..=3 {
                for width in 1..=3 {
                    for height in 1..=3 {
                        for gap in 0..=2 {
                            let capture = rect(x, y, 2, 2);
                            let panel_size = size(width, height);
                            let mut first_fit = None;
                            for (index, area) in areas.into_iter().enumerate() {
                                for px in -3..=6 {
                                    for py in -2..=2 {
                                        let panel = rect(px, py, width, height);
                                        if Bounds::from(area).contains(Bounds::from(panel))
                                            && separated(capture, panel, gap)
                                        {
                                            first_fit.get_or_insert(index);
                                        }
                                    }
                                }
                            }
                            match place_controls(capture, panel_size, &areas, gap) {
                                ControlPlacement::Hidden => assert_eq!(first_fit, None),
                                ControlPlacement::Visible {
                                    rect: panel,
                                    workarea_index,
                                } => {
                                    assert_eq!(first_fit, Some(workarea_index));
                                    assert_eq!(panel.size(), panel_size);
                                    assert!(
                                        Bounds::from(areas[workarea_index])
                                            .contains(Bounds::from(panel))
                                    );
                                    assert!(separated(capture, panel, gap));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
