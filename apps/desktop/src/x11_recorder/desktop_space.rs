//! One X11 root is the capture coordinate space; physical monitors place controls.

use gif_from_screen_capture::{CaptureSource, CaptureSourceKind, PhysicalRect};

use crate::recorder_geometry::RecorderGeometry;

fn screen_id(source: &CaptureSource) -> Option<&str> {
    source
        .id()
        .as_str()
        .strip_prefix("x11:screen:")?
        .split_once(':')
        .map(|(screen, _)| screen)
}

fn is_root(source: &CaptureSource) -> bool {
    source.id().as_str().ends_with(":root") && screen_id(source).is_some()
}

pub(super) fn desktop_capture_source(sources: &[CaptureSource], selected: usize) -> usize {
    let Some(source) = sources
        .get(selected)
        .filter(|s| s.kind() == CaptureSourceKind::Monitor)
    else {
        return selected;
    };
    let Some(screen) = screen_id(source) else {
        return selected;
    };
    sources
        .iter()
        .position(|candidate| {
            is_root(candidate)
                && candidate.kind() == CaptureSourceKind::Monitor
                && screen_id(candidate) == Some(screen)
                && candidate
                    .geometry()
                    .zip(source.geometry())
                    .is_some_and(|(root, monitor)| RecorderGeometry::new(root, monitor).is_ok())
        })
        .unwrap_or(selected)
}

pub(super) fn monitor_workareas(
    sources: &[CaptureSource],
    initial: PhysicalRect,
) -> Vec<PhysicalRect> {
    let mut monitors: Vec<_> = sources
        .iter()
        .filter(|source| source.kind() == CaptureSourceKind::Monitor && !is_root(source))
        .filter_map(CaptureSource::geometry)
        .take(32)
        .collect();
    if monitors.is_empty() {
        monitors.push(initial);
    }
    prefer_region_monitor(&mut monitors, initial);
    monitors
}

pub(super) fn prefer_region_monitor(monitors: &mut [PhysicalRect], region: PhysicalRect) {
    let overlap = |monitor: PhysicalRect| {
        let left = i64::from(region.origin().x).max(i64::from(monitor.origin().x));
        let top = i64::from(region.origin().y).max(i64::from(monitor.origin().y));
        let right = (i64::from(region.origin().x) + i64::from(region.size().width()))
            .min(i64::from(monitor.origin().x) + i64::from(monitor.size().width()));
        let bottom = (i64::from(region.origin().y) + i64::from(region.size().height()))
            .min(i64::from(monitor.origin().y) + i64::from(monitor.size().height()));
        (right - left).max(0) * (bottom - top).max(0)
    };
    // Stable ties keep a boundary-spanning selection from oscillating panels.
    monitors.sort_by_key(|monitor| std::cmp::Reverse(overlap(*monitor)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_capture::CaptureSourceId;

    fn source(id: &str, rect: (i32, i32, u32, u32)) -> CaptureSource {
        CaptureSource::new(
            CaptureSourceId::new(id).unwrap(),
            id,
            CaptureSourceKind::Monitor,
            Some(PhysicalRect::new(rect.0, rect.1, rect.2, rect.3).unwrap()),
            1.0,
        )
        .unwrap()
    }

    #[test]
    fn two_4k_monitors_share_root_capture_and_retain_physical_controller_areas() {
        let sources = [
            source("x11:screen:0:root", (0, 0, 7680, 2160)),
            source("x11:screen:0:monitor:0", (3840, 0, 3840, 2160)),
            source("x11:screen:0:monitor:1", (0, 0, 3840, 2160)),
        ];
        assert_eq!(desktop_capture_source(&sources, 1), 0);
        assert_eq!(desktop_capture_source(&sources, 2), 0);
        let region = PhysicalRect::new(5440, 840, 640, 480).unwrap();
        let mut geometry = RecorderGeometry::new(sources[0].geometry().unwrap(), region).unwrap();
        let mut areas = monitor_workareas(&sources, sources[1].geometry().unwrap());
        assert_eq!(areas.len(), 2);
        geometry.freeze_size();
        for x in [3800, 3520, 1600, 7040] {
            geometry.move_to(gif_from_screen_capture::PhysicalPosition { x, y: 840 });
            assert_eq!(geometry.region().origin().x, x);
            assert_eq!(geometry.source_local_region().unwrap(), geometry.region());
            assert_eq!(geometry.region().size(), region.size());
        }
        prefer_region_monitor(&mut areas, PhysicalRect::new(1600, 840, 640, 480).unwrap());
        assert_eq!(areas[0], sources[2].geometry().unwrap());
    }

    #[test]
    fn root_selection_never_crosses_x11_screens_and_windows_stay_window_relative() {
        let mut sources = vec![
            source("x11:screen:1:root", (0, 0, 8000, 4000)),
            source("x11:screen:0:monitor:0", (100, 200, 800, 600)),
        ];
        assert_eq!(desktop_capture_source(&sources, 1), 1);
        sources.push(source("x11:screen:0:root", (0, 0, 1920, 1080)));
        assert_eq!(desktop_capture_source(&sources, 1), 2);
        sources.push(
            CaptureSource::new(
                CaptureSourceId::new("x11:window:123").unwrap(),
                "Window",
                CaptureSourceKind::Window,
                Some(PhysicalRect::new(100, 200, 800, 600).unwrap()),
                1.0,
            )
            .unwrap(),
        );
        assert_eq!(desktop_capture_source(&sources, 3), 3);
    }
}
