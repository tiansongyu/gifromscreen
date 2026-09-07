//! Bounded X11 control-window placement, separate from capture geometry.
//!
//! These are native geometry acknowledgements, not proof that a compositor has
//! presented/hidden pixels. The caller owns transparency, visibility, pausing,
//! capture acknowledgements and any pending Start/Resume intent.

use std::time::{Duration, Instant};

use eframe::egui::{ViewportCommand, pos2, vec2};
use gif_from_screen_capture::{PhysicalRect, PhysicalSize};

use crate::recorder_geometry::{ControlPlacement, place_controls};

const PLACEMENT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_WORKAREAS: usize = 32;

/// All rectangles are actual global physical pixels, not egui logical points.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeWindow {
    pub outer: Option<PhysicalRect>,
    pub client: Option<PhysicalRect>,
    pub maximized: Option<bool>,
    /// Effective egui pixels per point, including application zoom, used only
    /// to convert outgoing commands back to egui's coordinate convention.
    pub pixels_per_point: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowRequest<'a> {
    pub region: PhysicalRect,
    /// Ordered by preference, normally source monitor followed by other monitors.
    pub workareas: &'a [PhysicalRect],
    /// Whole outer-window budget; the caller chooses it for its current UI/font
    /// scale. This is never inferred from, or applied to, the recording size.
    pub panel_size: PhysicalSize,
    /// Preferred placement margin. An adjusted WM position is accepted if its
    /// complete actual outer rectangle remains contained and outside capture.
    pub gap: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WindowState {
    Positioning,
    Ready {
        outer: PhysicalRect,
        workarea_index: usize,
    },
    /// No complete panel fits: the caller must hide/blank it and provide safe
    /// recovery controls. This state does not itself permit recording to start.
    Hidden,
    /// No automatic retries until the request changes or `retry()` is called.
    TimedOut,
    Invalid(String),
}

pub(crate) struct WindowUpdate {
    pub commands: Vec<ViewportCommand>,
    pub state: WindowState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Phase {
    #[default]
    Initial,
    AwaitUnmaximized,
    AwaitSize,
    AwaitPosition,
    Ready,
    Hidden,
    TimedOut,
    Invalid,
}

#[derive(Debug)]
struct Configuration {
    region: PhysicalRect,
    workareas: Vec<PhysicalRect>,
    panel_size: PhysicalSize,
    gap: u32,
    pixels_per_point: f32,
}

impl Configuration {
    fn matches(&self, request: WindowRequest<'_>, pixels_per_point: f32) -> bool {
        self.region == request.region
            && self.workareas == request.workareas
            && self.panel_size == request.panel_size
            && self.gap == request.gap
            && self.pixels_per_point.to_bits() == pixels_per_point.to_bits()
    }
}

#[derive(Debug, Default)]
pub(crate) struct ControllerWindow {
    configuration: Option<Configuration>,
    phase: Phase,
    started: Option<Instant>,
    target: Option<PhysicalRect>,
    target_client: Option<PhysicalSize>,
    decoration_correction_used: bool,
    error: Option<String>,
}

impl ControllerWindow {
    /// Explicit retry after a user restores/moves the window or corrects its
    /// native setup. Repeated polling alone never restarts a timed-out attempt.
    pub(crate) fn retry(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn advance(
        &mut self,
        native: NativeWindow,
        request: WindowRequest<'_>,
        now: Instant,
    ) -> WindowUpdate {
        if !native.pixels_per_point.is_finite() || native.pixels_per_point <= 0.0 {
            self.retry();
            return update(WindowState::Invalid(
                "UI pixels per point must be finite and positive".into(),
            ));
        }
        if request.workareas.len() > MAX_WORKAREAS {
            self.retry();
            return update(WindowState::Invalid(format!(
                "At most {MAX_WORKAREAS} controller work areas are supported"
            )));
        }
        if !self
            .configuration
            .as_ref()
            .is_some_and(|configuration| configuration.matches(request, native.pixels_per_point))
        {
            self.configure(request, native.pixels_per_point, now);
        }
        match self.phase {
            Phase::Hidden => return update(WindowState::Hidden),
            Phase::TimedOut => return update(WindowState::TimedOut),
            Phase::Invalid => {
                return update(WindowState::Invalid(
                    self.error
                        .clone()
                        .expect("invalid phase retains its failure reason"),
                ));
            }
            Phase::Ready => {
                if let Some(state) = self.ready_state(native, request) {
                    return update(state);
                }
                // A user/WM change invalidates Ready immediately. Start one new
                // bounded attempt, not a fresh timeout on every subsequent poll.
                self.phase = Phase::Initial;
                self.started = Some(now);
                self.target_client = None;
                self.decoration_correction_used = false;
            }
            _ => {}
        }
        if now.saturating_duration_since(self.started.expect("configured transition has a start"))
            >= PLACEMENT_TIMEOUT
        {
            self.phase = Phase::TimedOut;
            return update(WindowState::TimedOut);
        }
        self.advance_phase(native, request)
    }

    fn configure(&mut self, request: WindowRequest<'_>, pixels_per_point: f32, now: Instant) {
        self.configuration = Some(Configuration {
            region: request.region,
            workareas: request.workareas.to_vec(),
            panel_size: request.panel_size,
            gap: request.gap,
            pixels_per_point,
        });
        self.started = Some(now);
        self.target_client = None;
        self.decoration_correction_used = false;
        self.error = None;
        match place_controls(
            request.region,
            request.panel_size,
            request.workareas,
            request.gap,
        ) {
            ControlPlacement::Visible { rect, .. } => {
                self.target = Some(rect);
                self.phase = Phase::Initial;
            }
            ControlPlacement::Hidden => {
                self.target = None;
                self.phase = Phase::Hidden;
            }
        }
    }

    fn advance_phase(&mut self, native: NativeWindow, request: WindowRequest<'_>) -> WindowUpdate {
        if self.phase == Phase::Initial {
            self.phase = Phase::AwaitUnmaximized;
            if native.maximized != Some(false) {
                return WindowUpdate {
                    commands: vec![ViewportCommand::Maximized(false)],
                    state: WindowState::Positioning,
                };
            }
        }
        if self.phase == Phase::AwaitUnmaximized && native.maximized == Some(false) {
            return self.request_resize(native);
        }
        if self.phase == Phase::AwaitSize && self.matches_size(native, request.panel_size) {
            self.phase = Phase::AwaitPosition;
            let target = self.target.expect("visible placement has a target");
            let position = pos2(
                logical_pixel(f64::from(target.origin().x), native.pixels_per_point),
                logical_pixel(f64::from(target.origin().y), native.pixels_per_point),
            );
            if !position.is_finite() {
                return self.fail("Native position cannot be represented in UI coordinates");
            }
            return WindowUpdate {
                commands: vec![ViewportCommand::OuterPosition(position)],
                state: WindowState::Positioning,
            };
        }
        if self.phase == Phase::AwaitSize && self.needs_decoration_correction(native) {
            // Decoration removal can arrive after the client resize was ACKed.
            // Correct the client target once, keeping the original two-second
            // deadline. Including unmaximize/position, an attempt emits at most
            // six commands even if the native frame extents keep oscillating.
            self.decoration_correction_used = true;
            return self.request_resize(native);
        }
        if self.phase == Phase::AwaitPosition
            && let Some(state) = self.ready_state(native, request)
        {
            self.phase = Phase::Ready;
            return update(state);
        }
        update(WindowState::Positioning)
    }

    fn needs_decoration_correction(&self, native: NativeWindow) -> bool {
        if self.decoration_correction_used || native.maximized != Some(false) {
            return false;
        }
        let (Some(outer), Some(client), Some(target), Some(target_client)) =
            (native.outer, native.client, self.target, self.target_client)
        else {
            return false;
        };
        if !contains(outer, client) || !fits_size_budget(client.size(), target_client) {
            return false;
        }
        let observed_frame = (
            outer.size().width() - client.size().width(),
            outer.size().height() - client.size().height(),
        );
        let requested_frame = (
            target.size().width() - target_client.width(),
            target.size().height() - target_client.height(),
        );
        observed_frame != requested_frame
    }

    fn request_resize(&mut self, native: NativeWindow) -> WindowUpdate {
        let (Some(outer), Some(client)) = (native.outer, native.client) else {
            return update(WindowState::Positioning);
        };
        if !contains(outer, client) {
            // Configure events can deliver one new rectangle before the other.
            // Wait for a coherent observation within the same overall deadline.
            return update(WindowState::Positioning);
        }
        let target = self.target.expect("visible placement has a target");
        let frame_width = outer.size().width() - client.size().width();
        let frame_height = outer.size().height() - client.size().height();
        let Some(width) = target.size().width().checked_sub(frame_width) else {
            return self.fail("Native decorations exceed the controller width budget");
        };
        let Some(height) = target.size().height().checked_sub(frame_height) else {
            return self.fail("Native decorations exceed the controller height budget");
        };
        let Ok(size) = PhysicalSize::new(width, height) else {
            return self.fail("Native decorations leave no usable controller client area");
        };
        let logical = vec2(
            logical_pixel(f64::from(width), native.pixels_per_point),
            logical_pixel(f64::from(height), native.pixels_per_point),
        );
        if !logical.is_finite() || logical.min_elem() <= 0.0 {
            return self.fail("Controller dimensions cannot be represented in UI coordinates");
        }
        self.target_client = Some(size);
        self.phase = Phase::AwaitSize;
        WindowUpdate {
            // Replace the larger main-window minimum before requesting compact
            // dimensions; the caller chose this budget for its complete controls.
            commands: vec![
                ViewportCommand::MinInnerSize(logical),
                ViewportCommand::InnerSize(logical),
            ],
            state: WindowState::Positioning,
        }
    }

    fn matches_size(&self, native: NativeWindow, outer_budget: PhysicalSize) -> bool {
        let (Some(outer), Some(client), Some(target_client)) =
            (native.outer, native.client, self.target_client)
        else {
            return false;
        };
        native.maximized == Some(false)
            && contains(outer, client)
            && fits_size_budget(outer.size(), outer_budget)
            && fits_size_budget(client.size(), target_client)
    }

    fn ready_state(&self, native: NativeWindow, request: WindowRequest<'_>) -> Option<WindowState> {
        if !self.matches_size(native, request.panel_size) {
            return None;
        }
        let outer = native.outer?;
        if intersects(outer, request.region) {
            return None;
        }
        let workarea_index = request
            .workareas
            .iter()
            .position(|area| contains(*area, outer))?;
        Some(WindowState::Ready {
            outer,
            workarea_index,
        })
    }

    fn fail(&mut self, message: &str) -> WindowUpdate {
        self.phase = Phase::Invalid;
        self.error = Some(message.to_owned());
        update(WindowState::Invalid(message.to_owned()))
    }
}

fn update(state: WindowState) -> WindowUpdate {
    WindowUpdate {
        commands: Vec::new(),
        state,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn logical_pixel(value: f64, pixels_per_point: f32) -> f32 {
    // Egui's API is f32; incoming native geometry remains integer-authoritative.
    // Finite outgoing commands are checked and actual physical placement must
    // subsequently pass the size/containment checks, never a float approximation.
    (value / f64::from(pixels_per_point)) as f32
}

fn fits_size_budget(actual: PhysicalSize, budget: PhysicalSize) -> bool {
    let matches = |actual: u32, target: u32| actual <= target && target - actual <= 1;
    matches(actual.width(), budget.width()) && matches(actual.height(), budget.height())
}

fn edges(rect: PhysicalRect) -> (i64, i64, i64, i64) {
    let left = i64::from(rect.origin().x);
    let top = i64::from(rect.origin().y);
    (
        left,
        top,
        left + i64::from(rect.size().width()),
        top + i64::from(rect.size().height()),
    )
}

fn contains(outer: PhysicalRect, inner: PhysicalRect) -> bool {
    let (left, top, right, bottom) = edges(outer);
    let (other_left, other_top, other_right, other_bottom) = edges(inner);
    left <= other_left && top <= other_top && right >= other_right && bottom >= other_bottom
}

fn intersects(first: PhysicalRect, second: PhysicalRect) -> bool {
    let (left, top, right, bottom) = edges(first);
    let (other_left, other_top, other_right, other_bottom) = edges(second);
    left < other_right && right > other_left && top < other_bottom && bottom > other_top
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect {
        PhysicalRect::new(x, y, width, height).unwrap()
    }

    fn size(width: u32, height: u32) -> PhysicalSize {
        PhysicalSize::new(width, height).unwrap()
    }

    fn native(outer: PhysicalRect) -> NativeWindow {
        NativeWindow {
            outer: Some(outer),
            client: Some(outer),
            maximized: Some(false),
            pixels_per_point: 1.0,
        }
    }

    fn request(areas: &[PhysicalRect]) -> WindowRequest<'_> {
        WindowRequest {
            region: rect(100, 100, 640, 420),
            workareas: areas,
            panel_size: size(420, 300),
            gap: 8,
        }
    }

    fn ready(
        window: &mut ControllerWindow,
        request: WindowRequest<'_>,
        now: Instant,
    ) -> NativeWindow {
        let first = window.advance(native(rect(0, 0, 800, 600)), request, now);
        assert_eq!(first.state, WindowState::Positioning);
        assert!(matches!(
            &first.commands[..],
            [
                ViewportCommand::MinInnerSize(_),
                ViewportCommand::InnerSize(_)
            ]
        ));
        let resized = native(rect(
            0,
            0,
            request.panel_size.width(),
            request.panel_size.height(),
        ));
        let moved = window.advance(resized, request, now);
        let [ViewportCommand::OuterPosition(_)] = &moved.commands[..] else {
            panic!("expected position command, got {:?}", moved.commands);
        };
        let outer = window.target.unwrap();
        let actual = native(outer);
        assert!(matches!(
            window.advance(actual, request, now).state,
            WindowState::Ready { .. }
        ));
        actual
    }

    #[test]
    fn actual_unmaximize_resize_and_position_acknowledgements_are_separate_stages() {
        let areas = [rect(0, 0, 1440, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        let mut window = ControllerWindow::default();
        let mut actual = native(areas[0]);
        actual.maximized = Some(true);
        let first = window.advance(actual, request, now);
        assert_eq!(first.commands, [ViewportCommand::Maximized(false)]);
        assert_eq!(first.state, WindowState::Positioning);
        assert!(window.advance(actual, request, now).commands.is_empty());
        actual.maximized = None;
        assert!(window.advance(actual, request, now).commands.is_empty());
        actual.maximized = Some(false);
        let resize = window.advance(actual, request, now);
        assert_eq!(
            resize.commands,
            [
                ViewportCommand::MinInnerSize(vec2(420.0, 300.0)),
                ViewportCommand::InnerSize(vec2(420.0, 300.0))
            ]
        );
        assert_eq!(resize.state, WindowState::Positioning);
        assert!(window.advance(actual, request, now).commands.is_empty());
        let resized = native(rect(100, 100, 420, 300));
        let movement = window.advance(resized, request, now);
        assert_eq!(
            movement.commands,
            [ViewportCommand::OuterPosition(pos2(210.0, 528.0))]
        );
        assert_eq!(movement.state, WindowState::Positioning);
        assert_eq!(
            window.advance(resized, request, now).state,
            WindowState::Positioning
        );
        let actual = native(rect(210, 528, 420, 300));
        assert_eq!(
            window.advance(actual, request, now).state,
            WindowState::Ready {
                outer: actual.outer.unwrap(),
                workarea_index: 0
            }
        );
        assert!(
            window
                .advance(actual, request, now + Duration::from_secs(100))
                .commands
                .is_empty()
        );
    }

    #[test]
    fn measured_decorations_are_removed_from_outer_budget_before_requesting_client_size() {
        let areas = [rect(-1000, -800, 2440, 1800)];
        let mut request = request(&areas);
        request.region = rect(-800, -600, 1, 1);
        let mut actual = native(rect(0, 0, 800, 600));
        actual.client = Some(rect(4, 24, 792, 572));
        actual.pixels_per_point = 1.25;
        let mut window = ControllerWindow::default();
        let now = Instant::now();
        assert_eq!(
            window.advance(actual, request, now).commands,
            [
                ViewportCommand::MinInnerSize(vec2(329.6, 217.6)),
                ViewportCommand::InnerSize(vec2(329.6, 217.6))
            ]
        );
        actual.outer = Some(rect(0, 0, 420, 300));
        actual.client = Some(rect(4, 24, 412, 272));
        let movement = window.advance(actual, request, now);
        assert!(
            matches!(&movement.commands[..], [ViewportCommand::OuterPosition(position)] if position.x < 0.0 && position.y < 0.0)
        );
        actual.outer = Some(rect(-1000, -500, 420, 300));
        actual.client = Some(rect(-996, -476, 412, 272));
        assert!(matches!(
            window.advance(actual, request, now).state,
            WindowState::Ready { .. }
        ));
        assert_eq!(request.region, rect(-800, -600, 1, 1));
    }

    #[test]
    fn full_monitor_without_space_reports_hidden_without_window_commands() {
        let areas = [rect(0, 0, 1440, 1000)];
        let mut request = request(&areas);
        request.region = areas[0];
        let mut window = ControllerWindow::default();
        for time in [Instant::now(), Instant::now() + Duration::from_secs(100)] {
            let result = window.advance(native(areas[0]), request, time);
            assert_eq!(result.state, WindowState::Hidden);
            assert!(result.commands.is_empty());
        }
        assert_eq!(request.region, areas[0]);
    }

    #[test]
    fn wm_adjusted_safe_position_on_another_monitor_is_accepted_but_overlap_is_not() {
        let areas = [rect(0, 0, 1440, 1000), rect(-800, 0, 800, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        let mut window = ControllerWindow::default();
        let actual = ready(&mut window, request, now);
        let alternate = native(rect(-600, 70, 420, 300));
        assert_eq!(
            window.advance(alternate, request, now).state,
            WindowState::Ready {
                outer: alternate.outer.unwrap(),
                workarea_index: 1
            }
        );
        assert!(window.advance(actual, request, now).commands.is_empty());
        let overlapping = native(rect(400, 300, 420, 300));
        let result = window.advance(overlapping, request, now);
        assert_eq!(result.state, WindowState::Positioning);
        assert!(matches!(
            &result.commands[..],
            [
                ViewportCommand::MinInnerSize(_),
                ViewportCommand::InnerSize(_)
            ]
        ));
    }

    #[test]
    fn timeouts_are_terminal_until_explicit_retry_or_a_changed_request() {
        let areas = [rect(0, 0, 1440, 1000)];
        let mut request = request(&areas);
        let now = Instant::now();
        let mut window = ControllerWindow::default();
        let actual = native(rect(0, 0, 800, 600));
        assert!(!window.advance(actual, request, now).commands.is_empty());
        for elapsed in [2, 10, 100] {
            let result = window.advance(actual, request, now + Duration::from_secs(elapsed));
            assert_eq!(result.state, WindowState::TimedOut);
            assert!(result.commands.is_empty());
        }
        window.retry();
        assert!(
            !window
                .advance(actual, request, now + Duration::from_secs(100))
                .commands
                .is_empty()
        );
        assert_eq!(
            window
                .advance(actual, request, now + Duration::from_secs(103))
                .state,
            WindowState::TimedOut
        );
        request.region = rect(150, 100, 640, 420);
        assert!(
            !window
                .advance(actual, request, now + Duration::from_secs(104))
                .commands
                .is_empty()
        );
    }

    #[test]
    fn dpi_zoom_and_region_changes_revoke_old_ready_without_mutating_capture() {
        let areas = [rect(0, 0, 1440, 1000)];
        let mut request = request(&areas);
        let now = Instant::now();
        let mut window = ControllerWindow::default();
        let mut actual = ready(&mut window, request, now);
        actual.pixels_per_point = 2.0;
        let result = window.advance(actual, request, now);
        assert_eq!(result.state, WindowState::Positioning);
        assert_eq!(
            result.commands,
            [
                ViewportCommand::MinInnerSize(vec2(210.0, 150.0)),
                ViewportCommand::InnerSize(vec2(210.0, 150.0))
            ]
        );
        request.region = areas[0];
        assert_eq!(
            window.advance(actual, request, now).state,
            WindowState::Hidden
        );
        assert_eq!(request.region, areas[0]);
    }

    #[test]
    fn missing_incoherent_and_maximized_metrics_never_become_ready() {
        let areas = [rect(0, 0, 1440, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        for actual in [
            NativeWindow {
                outer: None,
                client: None,
                maximized: Some(false),
                pixels_per_point: 1.0,
            },
            NativeWindow {
                outer: Some(rect(0, 0, 800, 600)),
                client: Some(rect(-1, 0, 800, 600)),
                maximized: Some(false),
                pixels_per_point: 1.0,
            },
            NativeWindow {
                maximized: None,
                ..native(rect(0, 0, 420, 300))
            },
        ] {
            let mut window = ControllerWindow::default();
            assert_eq!(
                window.advance(actual, request, now).state,
                WindowState::Positioning
            );
            assert_eq!(
                window
                    .advance(actual, request, now + PLACEMENT_TIMEOUT)
                    .state,
                WindowState::TimedOut
            );
        }
    }

    #[test]
    fn invalid_scale_extreme_commands_and_excess_workareas_are_explicit_errors() {
        let areas = [rect(0, 0, 1440, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        for scale in [
            0.0,
            -1.0,
            f32::NAN,
            f32::INFINITY,
            f32::MIN_POSITIVE / 100.0,
        ] {
            let mut window = ControllerWindow::default();
            let actual = NativeWindow {
                pixels_per_point: scale,
                ..native(rect(0, 0, 800, 600))
            };
            let result = window.advance(actual, request, now);
            assert!(matches!(result.state, WindowState::Invalid(_)));
            assert!(result.commands.is_empty());
        }
        let many = vec![areas[0]; MAX_WORKAREAS + 1];
        let mut window = ControllerWindow::default();
        let result = window.advance(
            native(areas[0]),
            WindowRequest {
                workareas: &many,
                ..request
            },
            now,
        );
        assert!(matches!(result.state, WindowState::Invalid(_)));
        assert!(result.commands.is_empty());
    }

    #[test]
    fn no_budget_overrun_or_cross_workarea_panel_can_become_ready() {
        let areas = [rect(0, 0, 1440, 1000), rect(-800, 0, 800, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        for actual in [
            native(rect(210, 528, 421, 300)),
            native(rect(-100, 650, 420, 300)),
            native(rect(210, 528, 418, 300)),
        ] {
            let mut window = ControllerWindow::default();
            ready(&mut window, request, now);
            assert_eq!(
                window.advance(actual, request, now).state,
                WindowState::Positioning
            );
        }
        let mut window = ControllerWindow::default();
        ready(&mut window, request, now);
        let smaller = native(rect(210, 528, 419, 299));
        assert!(matches!(
            window.advance(smaller, request, now).state,
            WindowState::Ready { .. }
        ));
    }

    #[test]
    fn decoration_budget_underflow_is_a_recoverable_failure() {
        let areas = [rect(0, 0, 1440, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        let mut window = ControllerWindow::default();
        let actual = NativeWindow {
            client: Some(rect(1, 1, 100, 100)),
            ..native(rect(0, 0, 800, 600))
        };
        assert!(matches!(
            window.advance(actual, request, now).state,
            WindowState::Invalid(_)
        ));
        assert!(
            window
                .advance(native(rect(0, 0, 800, 600)), request, now)
                .commands
                .is_empty()
        );
        window.retry();
        assert!(
            !window
                .advance(native(rect(0, 0, 800, 600)), request, now)
                .commands
                .is_empty()
        );
    }

    #[test]
    fn monitor_removal_revokes_ready_and_hidden_can_recover_when_a_monitor_returns() {
        let areas = [rect(0, 0, 1440, 1000), rect(-800, 0, 800, 1000)];
        let mut request = request(&areas);
        request.region = areas[0];
        let now = Instant::now();
        let mut window = ControllerWindow::default();
        let actual = ready(&mut window, request, now);
        let removed = WindowRequest {
            workareas: &areas[..1],
            ..request
        };
        let hidden = window.advance(actual, removed, now);
        assert_eq!(hidden.state, WindowState::Hidden);
        assert!(hidden.commands.is_empty());
        let returning = window.advance(actual, request, now);
        assert_eq!(returning.state, WindowState::Positioning);
        assert!(matches!(
            &returning.commands[..],
            [
                ViewportCommand::MinInnerSize(_),
                ViewportCommand::InnerSize(_)
            ]
        ));
        assert_eq!(request.region, areas[0]);
    }

    fn with_decorations(client_size: PhysicalSize, frame: (u32, u32)) -> NativeWindow {
        NativeWindow {
            outer: Some(rect(
                0,
                0,
                client_size.width() + frame.0,
                client_size.height() + frame.1,
            )),
            client: Some(rect(
                i32::try_from(frame.0).unwrap(),
                i32::try_from(frame.1).unwrap(),
                client_size.width(),
                client_size.height(),
            )),
            maximized: Some(false),
            pixels_per_point: 1.0,
        }
    }

    #[test]
    fn acknowledged_client_is_corrected_once_when_decorations_appear_or_disappear() {
        let areas = [rect(0, 0, 1440, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        for (old_frame, new_frame) in [((0, 37), (0, 0)), ((0, 0), (8, 37))] {
            let mut window = ControllerWindow::default();
            let mut initial = with_decorations(size(800, 600), old_frame);
            initial.maximized = Some(true);
            let unmaximize = window.advance(initial, request, now);
            assert_eq!(unmaximize.commands, [ViewportCommand::Maximized(false)]);
            initial.maximized = Some(false);
            let resize = window.advance(initial, request, now);
            assert_eq!(resize.commands.len(), 2);
            let first_target = window.target_client.unwrap();
            assert_eq!(first_target, size(420 - old_frame.0, 300 - old_frame.1));

            let changed = with_decorations(first_target, new_frame);
            let correction = window.advance(changed, request, now + Duration::from_millis(100));
            let corrected_target = size(420 - new_frame.0, 300 - new_frame.1);
            assert_eq!(window.target_client, Some(corrected_target));
            let logical = vec2(
                logical_pixel(f64::from(corrected_target.width()), 1.0),
                logical_pixel(f64::from(corrected_target.height()), 1.0),
            );
            assert_eq!(
                correction.commands,
                [
                    ViewportCommand::MinInnerSize(logical),
                    ViewportCommand::InnerSize(logical)
                ]
            );
            assert_eq!(correction.state, WindowState::Positioning);
            assert!(window.decoration_correction_used);
            assert!(
                window
                    .advance(changed, request, now + Duration::from_millis(200))
                    .commands
                    .is_empty()
            );

            let corrected = with_decorations(corrected_target, new_frame);
            let positioned = window.advance(corrected, request, now + Duration::from_millis(300));
            assert!(matches!(
                &positioned.commands[..],
                [ViewportCommand::OuterPosition(_)]
            ));
            assert_eq!(
                unmaximize.commands.len()
                    + resize.commands.len()
                    + correction.commands.len()
                    + positioned.commands.len(),
                6
            );
            let outer = window.target.unwrap();
            let actual = NativeWindow {
                outer: Some(outer),
                client: Some(rect(
                    outer.origin().x + i32::try_from(new_frame.0).unwrap(),
                    outer.origin().y + i32::try_from(new_frame.1).unwrap(),
                    corrected_target.width(),
                    corrected_target.height(),
                )),
                ..corrected
            };
            let final_update = window.advance(actual, request, now + Duration::from_millis(400));
            assert!(matches!(final_update.state, WindowState::Ready { .. }));
            assert!(final_update.commands.is_empty());
        }
    }

    #[test]
    fn decoration_correction_waits_for_the_old_client_ack_and_does_not_resend_on_stable_metrics() {
        let areas = [rect(0, 0, 1440, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        let mut window = ControllerWindow::default();
        window.advance(with_decorations(size(800, 600), (0, 37)), request, now);
        let not_acknowledged = with_decorations(size(420, 260), (0, 0));
        for elapsed in 1..=20 {
            let waiting = window.advance(
                not_acknowledged,
                request,
                now + Duration::from_millis(elapsed),
            );
            assert_eq!(waiting.state, WindowState::Positioning);
            assert!(waiting.commands.is_empty());
            assert!(!window.decoration_correction_used);
        }
        let acknowledged = with_decorations(size(420, 263), (0, 0));
        assert_eq!(
            window
                .advance(acknowledged, request, now + Duration::from_millis(25))
                .commands
                .len(),
            2
        );
        for elapsed in 26..=50 {
            let waiting =
                window.advance(acknowledged, request, now + Duration::from_millis(elapsed));
            assert_eq!(waiting.state, WindowState::Positioning);
            assert!(waiting.commands.is_empty());
        }
    }

    #[test]
    fn oscillating_decorations_have_one_correction_and_keep_the_original_deadline() {
        let areas = [rect(0, 0, 1440, 1000)];
        let request = request(&areas);
        let now = Instant::now();
        let mut window = ControllerWindow::default();
        let initial = with_decorations(size(800, 600), (0, 37));
        assert_eq!(window.advance(initial, request, now).commands.len(), 2);
        let removed = with_decorations(size(420, 263), (0, 0));
        let correction = window.advance(removed, request, now + Duration::from_millis(1900));
        assert_eq!(correction.commands.len(), 2);
        for elapsed in 1901..=1999 {
            let oscillating =
                with_decorations(size(420, 300), (0, if elapsed % 2 == 0 { 10 } else { 37 }));
            let waiting =
                window.advance(oscillating, request, now + Duration::from_millis(elapsed));
            assert_eq!(waiting.state, WindowState::Positioning);
            assert!(waiting.commands.is_empty());
        }
        for elapsed in [2000, 2100, 3900, 9000] {
            let timed_out = window.advance(removed, request, now + Duration::from_millis(elapsed));
            assert_eq!(timed_out.state, WindowState::TimedOut);
            assert!(timed_out.commands.is_empty());
        }
        window.retry();
        assert!(
            !window
                .advance(removed, request, now + Duration::from_secs(10))
                .commands
                .is_empty()
        );
        assert!(!window.decoration_correction_used);
    }
}
