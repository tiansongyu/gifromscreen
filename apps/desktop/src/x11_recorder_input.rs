//! One cancellable input-shape update for the current physical recorder geometry.

use crate::background_task::BackgroundTask;
use eframe::egui;
use gif_from_screen_capture::{PhysicalRect, PhysicalSize};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Geometry {
    pub(crate) size: PhysicalSize,
    pub(crate) hole: PhysicalRect,
}

impl Geometry {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "finite physical coordinates are range checked before conversion"
    )]
    pub(crate) fn new(client: egui::Vec2, hole: egui::Rect, pixels_per_point: f32) -> Option<Self> {
        if !pixels_per_point.is_finite()
            || pixels_per_point <= 0.0
            || !client.is_finite()
            || !hole.is_finite()
        {
            return None;
        }
        let values = [
            client.x,
            client.y,
            hole.min.x,
            hole.min.y,
            hole.width(),
            hole.height(),
        ]
        .map(|value| (value * pixels_per_point).round());
        if values
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=65_535.0).contains(value))
        {
            return None;
        }
        let size = PhysicalSize::new(values[0] as u32, values[1] as u32).ok()?;
        let hole = PhysicalRect::new(
            values[2] as i32,
            values[3] as i32,
            values[4] as u32,
            values[5] as u32,
        )
        .ok()?;
        hole.fits_within(size).then_some(Self { size, hole })
    }
}

#[derive(Default)]
pub(crate) struct RecorderInput {
    task: BackgroundTask<Geometry, ()>,
    requested: Option<Geometry>,
    running: Option<Geometry>,
    ready: Option<Geometry>,
    failed: bool,
    error: Option<String>,
}

impl RecorderInput {
    pub(crate) fn ready(&self) -> bool {
        self.requested.is_some() && self.ready == self.requested && !self.failed
    }
    pub(crate) fn failed(&self) -> bool {
        self.failed
    }
    pub(crate) fn notice(&self) -> Option<&str> {
        if self.ready() {
            None
        } else {
            Some(self.error.as_deref().unwrap_or(
                "Preparing mouse-transparent capture area; Start is disabled until acknowledged.",
            ))
        }
    }

    pub(crate) fn update(
        &mut self,
        context: &egui::Context,
        title: &str,
        geometry: Option<Geometry>,
    ) {
        if geometry != self.requested {
            self.requested = geometry;
            self.ready = None;
            self.failed = false;
            self.error = None;
            self.task.cancel();
        }
        if let Some(result) = self.task.poll() {
            if self.running == self.requested && !self.task.is_cancelling() {
                match result {
                    Ok(geometry) if Some(geometry) == self.requested => self.ready = Some(geometry),
                    Ok(_) => {}
                    Err(error) => {
                        self.failed = true;
                        self.error = Some(format!("Recorder input preparation failed: {error}"));
                    }
                }
            }
            self.running = None;
        }
        if self.task.is_running() {
            context.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if self.ready() || self.failed || context.embed_viewports() {
            return;
        }
        let Some(geometry) = self.requested else {
            return;
        };
        let title = title.to_owned();
        let result = self.task.start("x11-recorder-input", move |task| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if task.cancellation().load(Ordering::Acquire) { return Err("Input preparation cancelled.".into()); }
                if Instant::now() >= deadline { return Err("The recorder window did not settle to its requested geometry. Close it and retry.".into()); }
                if gif_from_screen_capture_linux::set_recorder_input_shape(None, std::process::id(), &title, geometry.size, geometry.hole, task.cancellation())? {
                    return Ok(geometry);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        match result {
            Ok(()) => self.running = Some(geometry),
            Err(error) => {
                self.failed = true;
                self.error = Some(error);
            }
        }
        context.request_repaint_after(Duration::from_millis(16));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_uses_effective_scale_and_keeps_toolbar_outside_hole() {
        let geometry = Geometry::new(
            egui::vec2(328.0, 308.0),
            egui::Rect::from_min_size(egui::pos2(4.0, 4.0), egui::vec2(320.0, 204.0)),
            2.0,
        )
        .unwrap();
        assert_eq!(geometry.size, PhysicalSize::new(656, 616).unwrap());
        assert_eq!(geometry.hole, PhysicalRect::new(8, 8, 640, 408).unwrap());
        assert!(Geometry::new(egui::vec2(20.0, 20.0), egui::Rect::EVERYTHING, 1.0).is_none());
        assert!(
            Geometry::new(
                egui::vec2(20.0, 20.0),
                egui::Rect::from_min_max(egui::pos2(-1.0, 0.0), egui::pos2(3.0, 3.0)),
                1.0
            )
            .is_none()
        );
    }

    #[test]
    fn changed_geometry_revokes_readiness_before_a_native_update() {
        let context = egui::Context::default(); // Embedded tests never call X11.
        let geometry = Geometry::new(
            egui::vec2(100.0, 100.0),
            egui::Rect::from_min_max(egui::pos2(4.0, 4.0), egui::pos2(96.0, 50.0)),
            1.0,
        )
        .unwrap();
        let mut input = RecorderInput {
            requested: Some(geometry),
            ready: Some(geometry),
            ..Default::default()
        };
        assert!(input.ready());
        input.update(&context, "synthetic-recorder", None);
        assert!(!input.ready());
        assert!(!input.task.is_running());
        input.update(&context, "synthetic-recorder", Some(geometry));
        assert!(!input.ready());
        assert!(!input.task.is_running());
    }
}
