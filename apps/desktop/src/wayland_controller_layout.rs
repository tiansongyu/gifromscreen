//! Local-only Wayland window sizing with bounded compositor acknowledgement.
//! Targets use native logical pixels, independent of application UI zoom.

use std::time::{Duration, Instant};

use eframe::egui::{Vec2, ViewportCommand, vec2};

const GEOMETRY_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) fn compact_size(available: Vec2) -> Vec2 {
    let fit = |value: f32, preferred: f32| {
        if value.is_finite() && value > 0.0 {
            value.min(preferred)
        } else {
            preferred
        }
    };
    vec2(fit(available.x, 720.0), fit(available.y, 480.0))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Initial,
    AwaitUnmaximized,
    AwaitSize,
    AwaitMaximized,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct GeometryTransition {
    target: Vec2,
    minimum: Vec2,
    final_maximized: Option<bool>,
    started: Instant,
    phase: Phase,
}

pub(super) struct GeometryUpdate {
    pub commands: Vec<ViewportCommand>,
    pub finished: bool,
    pub timed_out: bool,
}

impl GeometryTransition {
    pub fn compact(available: Vec2, now: Instant) -> Self {
        let target = compact_size(available);
        Self::new(target, target.min(vec2(320.0, 240.0)), None, now)
    }

    pub fn restore(size: Vec2, maximized: Option<bool>, now: Instant) -> Self {
        Self::new(size, size.min(vec2(680.0, 440.0)), maximized, now)
    }

    fn new(target: Vec2, minimum: Vec2, final_maximized: Option<bool>, now: Instant) -> Self {
        Self {
            target,
            minimum,
            final_maximized,
            started: now,
            phase: Phase::Initial,
        }
    }

    pub fn advance(
        &mut self,
        size: Vec2,
        maximized: Option<bool>,
        zoom_factor: f32,
        now: Instant,
    ) -> GeometryUpdate {
        let mut commands = Vec::new();
        if self.phase == Phase::Initial {
            commands.push(ViewportCommand::MinInnerSize(self.minimum / zoom_factor));
            commands.push(ViewportCommand::Maximized(false));
            self.phase = Phase::AwaitUnmaximized;
        }
        let timed_out = now.saturating_duration_since(self.started) >= GEOMETRY_TIMEOUT;
        let mut requested_size = false;
        if self.phase == Phase::AwaitUnmaximized && maximized != Some(true) {
            // winit ignores request_inner_size while its last Wayland configure
            // is maximized. Do not issue the resize until that state has cleared.
            commands.push(ViewportCommand::InnerSize(self.target / zoom_factor));
            self.phase = Phase::AwaitSize;
            requested_size = true;
        }
        let mut confirmed = !requested_size
            && self.phase == Phase::AwaitSize
            && (size - self.target).abs().max_elem() <= 1.0;
        if confirmed
            && let Some(restore) = self.final_maximized
            && maximized != Some(restore)
        {
            commands.push(ViewportCommand::Maximized(restore));
            self.phase = Phase::AwaitMaximized;
            confirmed = false;
        }
        if self.phase == Phase::AwaitMaximized {
            confirmed = maximized == self.final_maximized;
        }
        let finished = confirmed || timed_out;
        if timed_out
            && !confirmed
            && self.phase != Phase::AwaitMaximized
            && let Some(restore) = self.final_maximized
        {
            commands.push(ViewportCommand::Maximized(restore));
        }
        GeometryUpdate {
            commands,
            finished,
            timed_out: timed_out && !confirmed,
        }
    }
}
