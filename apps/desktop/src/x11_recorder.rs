//! Physical X11 selection and a separate native border/control window.

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use eframe::egui;
use gif_from_screen_capture::{PhysicalPosition, PhysicalRect, PhysicalSize};
use gif_from_screen_capture_linux::{
    GuideEdge, GuidePointerEvent, GuideRequest, GuideStatus, RecorderGuide,
};
use gif_from_screen_localization::Message;

use crate::{
    GifFromScreenApp, MainWindowRestore, MainWindowSnapshot, RecorderOverlayAction, RecorderStage,
    RecordingJob, apply_overlay_region,
    recorder_geometry::RecorderGeometry,
    x11_controller_window::{ControllerWindow, NativeWindow, WindowRequest, WindowState},
};

const CHANGE_TIMEOUT: Duration = Duration::from_secs(5);
const PRESENTATION_SETTLE: Duration = Duration::from_millis(100);

pub(super) struct RecorderOverlay {
    pub(super) geometry: RecorderGeometry,
    guide: Option<RecorderGuide>,
    controller_geometry: Option<gif_from_screen_capture_linux::ControllerGeometry>,
    request: Option<GuideRequest>,
    acknowledged: Option<u64>,
    failure: Option<String>,
    window: ControllerWindow,
    window_state: WindowState,
    workareas: Vec<PhysicalRect>,
    initialized: bool,
    pub(super) pending_live_start: Option<Instant>,
    settle: Option<Settling>,
    change: Option<GeometryChange>,
    recovering: bool,
    was_minimized: bool,
    last_scale: f32,
    gesture: Option<Gesture>,
    snap: crate::window_snap::WindowSnapUi,
}

struct Gesture {
    id: u64,
    start: PhysicalPosition,
    initial: RecorderGeometry,
    edge: GuideEdge,
}

struct Settling {
    region: PhysicalRect,
    frame: u64,
    started: Instant,
    hide: HidePhase,
}

enum HidePhase {
    Visible,
    Requested,
    Acknowledged,
}

struct GeometryChange {
    started: Instant,
    pause_sent: bool,
    resume: bool,
    resume_override: Option<bool>,
    desired: PhysicalRect,
}

impl GeometryChange {
    fn resume_intent(&self) -> bool {
        self.resume_override.unwrap_or(self.resume)
    }
}

impl RecorderOverlay {
    pub(super) fn new(
        geometry: RecorderGeometry,
        workareas: Vec<PhysicalRect>,
        guide: Option<RecorderGuide>,
        scale: f32,
    ) -> Self {
        Self {
            geometry,
            guide,
            controller_geometry: None,
            request: None,
            acknowledged: None,
            failure: None,
            window: ControllerWindow::default(),
            window_state: WindowState::Positioning,
            workareas,
            initialized: false,
            pending_live_start: None,
            settle: None,
            change: None,
            recovering: false,
            was_minimized: false,
            last_scale: scale,
            gesture: None,
            snap: crate::window_snap::WindowSnapUi::default(),
        }
    }

    pub(super) fn ready(&self) -> bool {
        self.initialized
            && !self.snap.is_pending()
            && !self.snap.is_closing()
            && (self.guide.is_none() || self.controller_geometry.is_some())
            && self.guide_ready()
            && matches!(
                self.window_state,
                WindowState::Ready { .. } | WindowState::Hidden
            )
    }

    pub(super) fn failed(&self) -> bool {
        self.failure.is_some()
    }

    fn cancel_invalid_start(
        &mut self,
        context: &egui::Context,
        now: Instant,
    ) -> Option<&'static str> {
        let started = self.pending_live_start?;
        let notice = if now.saturating_duration_since(started) >= CHANGE_TIMEOUT {
            "Start expired while preparing the controls. Retry after the recorder is ready."
        } else if self
            .settle
            .as_ref()
            .is_some_and(|settle| matches!(settle.hide, HidePhase::Acknowledged))
            && !Self::is_hidden(context)
        {
            "Start cancelled because the controls were restored before capture began."
        } else {
            return None;
        };
        self.cancel_start(context);
        Some(notice)
    }

    fn is_hidden(context: &egui::Context) -> bool {
        // A compositing WM may keep an Iconic client X11-viewable to maintain
        // its redirected pixmap. Geometry/map-state observations are not the
        // WM's visibility contract; use its acknowledged minimized state here.
        context.input(|input| input.viewport().minimized == Some(true))
    }

    #[cfg(test)]
    pub(super) fn fail_for_test(&mut self) {
        self.failure = Some("injected native failure".into());
    }

    fn guide_ready(&self) -> bool {
        self.failure.is_none()
            && self.request.is_some_and(|request| {
                self.acknowledged == Some(request.generation)
                    && request.region == Some(self.geometry.region())
            })
    }

    fn update_guide(&mut self, protected_region: Option<PhysicalRect>) {
        if self.failure.is_some() {
            return;
        }
        let region = if self.snap.is_picking() {
            None
        } else {
            Some(self.geometry.region())
        };
        let protected_region = protected_region.filter(|protected| Some(*protected) != region);
        if self.request.is_some_and(|request| {
            request.region == region && request.protected_region == protected_region
        }) {
            return;
        }
        let Some(generation) = self
            .request
            .map_or(Some(1), |request| request.generation.checked_add(1))
        else {
            self.failure = Some("Recorder guide generation exhausted; reopen the recorder.".into());
            return;
        };
        let request = GuideRequest {
            generation,
            region,
            protected_region,
            border_width: 4,
        };
        self.acknowledged = None;
        self.settle = None;
        if let Some(guide) = &self.guide
            && let Err(error) = guide.request(request)
        {
            self.failure = Some(error);
            return;
        }
        self.request = Some(request);
    }

    fn poll_guide(&mut self, stage: RecorderStage) {
        let Some(guide) = &mut self.guide else {
            return;
        };
        let update = guide.poll();
        self.controller_geometry = update.controller;
        match update.status {
            GuideStatus::Failed(error) => self.failure = Some(error),
            GuideStatus::Stopped => {
                self.failure = Some("The recording guide stopped. Reopen the recorder.".into());
            }
            _ => {}
        }
        self.acknowledged = update.ack.map(|ack| ack.generation);
        if !stage.allows_moving() {
            if self.gesture.take().is_some() {
                guide.cancel_gesture();
            }
            return;
        }
        for event in update.events {
            match event {
                GuidePointerEvent::Pressed {
                    generation,
                    gesture_id,
                    position,
                    edge,
                    ..
                } => {
                    if self
                        .request
                        .is_some_and(|request| request.generation == generation)
                        && self.acknowledged == Some(generation)
                    {
                        self.gesture = Some(Gesture {
                            id: gesture_id,
                            start: position,
                            initial: self.geometry,
                            edge,
                        });
                    }
                }
                GuidePointerEvent::Moved {
                    gesture_id,
                    position,
                }
                | GuidePointerEvent::Released {
                    gesture_id,
                    position,
                } => {
                    if let Some(gesture) = &self.gesture
                        && gesture.id == gesture_id
                    {
                        self.geometry = gesture_geometry(gesture, position, stage);
                    }
                    if matches!(event, GuidePointerEvent::Released { .. })
                        && self
                            .gesture
                            .as_ref()
                            .is_some_and(|gesture| gesture.id == gesture_id)
                    {
                        self.gesture = None;
                    }
                }
                GuidePointerEvent::Cancelled { gesture_id } => {
                    if self
                        .gesture
                        .as_ref()
                        .is_some_and(|gesture| gesture.id == gesture_id)
                    {
                        self.gesture = None;
                    }
                }
            }
        }
    }

    fn place(&mut self, context: &egui::Context, native: NativeWindow, now: Instant) {
        let scale = native.pixels_per_point;
        let area = self
            .workareas
            .first()
            .copied()
            .unwrap_or(self.geometry.source());
        let size = panel_size(area.size(), scale);
        let update = self.window.advance(
            native,
            WindowRequest {
                region: self.geometry.region(),
                workareas: &self.workareas,
                panel_size: size,
                gap: 8,
            },
            now,
        );
        for command in update.commands {
            context.send_viewport_cmd(command);
        }
        self.window_state = update.state;
        self.initialized = true;
        self.last_scale = scale;
    }

    /// Native placement and two completed app frames are a settling heuristic,
    /// not a compositor presentation fence. Native pixel acceptance remains required.
    fn settled(&mut self, context: &egui::Context, now: Instant, hidden: bool) -> bool {
        let minimized = Self::is_hidden(context);
        if !self.ready() {
            self.settle = None;
            return false;
        }
        let region = self.geometry.region();
        if self
            .settle
            .as_ref()
            .is_none_or(|settle| settle.region != region)
        {
            self.settle = Some(Settling {
                region,
                frame: context.cumulative_frame_nr(),
                started: now,
                hide: HidePhase::Visible,
            });
        }
        let settle = self.settle.as_mut().unwrap();
        if now.saturating_duration_since(settle.started) >= CHANGE_TIMEOUT {
            self.failure = Some(
                "The recorder could not finish hiding or updating its controls; stopping safely."
                    .into(),
            );
            return false;
        }
        if context.cumulative_frame_nr().saturating_sub(settle.frame) < 2
            || now.saturating_duration_since(settle.started) < PRESENTATION_SETTLE
        {
            return false;
        }
        if !hidden {
            return true;
        }
        match settle.hide {
            HidePhase::Visible => {
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                settle.hide = HidePhase::Requested;
                false
            }
            HidePhase::Requested => {
                if minimized {
                    settle.hide = HidePhase::Acknowledged;
                }
                minimized
            }
            HidePhase::Acknowledged => minimized,
        }
    }

    pub(super) fn cancel_start(&mut self, context: &egui::Context) {
        self.pending_live_start = None;
        self.settle = None;
        self.was_minimized = false;
        context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        context.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(false));
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "finite physical panel budgets are clamped to nonempty u32 source dimensions"
)]
fn panel_size(area: PhysicalSize, scale: f32) -> PhysicalSize {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    PhysicalSize::new(
        (420.0 * scale).round().clamp(1.0, area.width() as f32) as u32,
        (300.0 * scale).round().clamp(1.0, area.height() as f32) as u32,
    )
    .unwrap()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "physical values are finite and range checked before conversion"
)]
fn physical_rect(rect: egui::Rect, scale: f32) -> Option<PhysicalRect> {
    let [x, y, width, height] = [rect.left(), rect.top(), rect.width(), rect.height()]
        .map(|v| (f64::from(v) * f64::from(scale)).round());
    if ![x, y, width, height].iter().all(|v| v.is_finite())
        || !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&x)
        || !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&y)
        || !(1.0..=f64::from(u32::MAX)).contains(&width)
        || !(1.0..=f64::from(u32::MAX)).contains(&height)
    {
        return None;
    }
    PhysicalRect::new(x as i32, y as i32, width as u32, height as u32).ok()
}

pub(super) fn window_id(frame: &eframe::Frame) -> Option<u32> {
    use wgpu::rwh::{HasWindowHandle as _, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok().filter(|id| *id != 0),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        _ => None,
    }
}

fn observed_window(context: &egui::Context, overlay: &RecorderOverlay) -> NativeWindow {
    let mut native = native_window(context);
    if let Some(geometry) = overlay.controller_geometry {
        native.outer = Some(geometry.outer);
        native.client = Some(geometry.client);
    } else if overlay.guide.is_some() {
        native.outer = None;
        native.client = None;
    }
    native
}

fn native_window(context: &egui::Context) -> NativeWindow {
    let scale = context.pixels_per_point();
    context.input(|input| NativeWindow {
        outer: input
            .viewport()
            .outer_rect
            .and_then(|rect| physical_rect(rect, scale)),
        client: input
            .viewport()
            .inner_rect
            .and_then(|rect| physical_rect(rect, scale)),
        maximized: input.viewport().maximized,
        pixels_per_point: scale,
    })
}

fn outside(a: PhysicalRect, b: PhysicalRect) -> bool {
    let ax = i64::from(a.origin().x);
    let ay = i64::from(a.origin().y);
    let bx = i64::from(b.origin().x);
    let by = i64::from(b.origin().y);
    ax >= bx + i64::from(b.size().width())
        || bx >= ax + i64::from(a.size().width())
        || ay >= by + i64::from(b.size().height())
        || by >= ay + i64::from(a.size().height())
}

fn finish_snap_frame(
    context: &egui::Context,
    overlay: &mut RecorderOverlay,
    stage: RecorderStage,
    visible: bool,
    mut action: RecorderOverlayAction,
) -> RecorderOverlayAction {
    if action == RecorderOverlayAction::Close && !overlay.snap.request_close() {
        context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        action = RecorderOverlayAction::None;
    }
    if overlay.snap.close_ready() {
        action = RecorderOverlayAction::Close;
    }
    overlay.snap.publish_drag_button(
        overlay.geometry,
        stage,
        visible
            && !matches!(
                action,
                RecorderOverlayAction::Start
                    | RecorderOverlayAction::Resume
                    | RecorderOverlayAction::Close
            ),
        context,
    );
    action
}

impl GifFromScreenApp {
    pub(super) fn open_recorder_overlay(&mut self, context: &egui::Context) -> Result<(), String> {
        let localizer = self.language_settings.localizer();
        crate::validate_settings(&self.settings)?;
        if self.display_server == Some(gif_from_screen_capture_linux::LinuxDisplayServer::Wayland) {
            return self.begin_wayland_preparation();
        }
        let scale = context.pixels_per_point();
        let snapshot = context
            .input(|input| {
                let viewport = input.viewport();
                Some(MainWindowSnapshot {
                    position: Some(viewport.outer_rect?.min * scale),
                    size: viewport.inner_rect?.size() * scale,
                    maximized: viewport.maximized,
                    restore: MainWindowRestore::X11Geometry,
                })
            })
            .ok_or("Could not read the main window geometry.")?;
        let source = self
            .sources
            .get(self.selected_source)
            .ok_or("No X11 source is selected.")?;
        let source = source
            .geometry()
            .ok_or("The selected source has no usable geometry.")?;
        let (x, y, width, height) = if self.settings.region_enabled {
            (
                self.settings.region_x,
                self.settings.region_y,
                self.settings.region_width,
                self.settings.region_height,
            )
        } else {
            (0, 0, source.size().width(), source.size().height())
        };
        let region = PhysicalRect::new(
            source
                .origin()
                .x
                .checked_add(x)
                .ok_or("Recorder X coordinate overflowed.")?,
            source
                .origin()
                .y
                .checked_add(y)
                .ok_or("Recorder Y coordinate overflowed.")?,
            width,
            height,
        )
        .map_err(|error| error.to_string())?;
        let geometry = RecorderGeometry::new(source, region)?;
        let mut workareas = self
            .sources
            .iter()
            .filter(|source| source.kind() == gif_from_screen_capture::CaptureSourceKind::Monitor)
            .filter_map(gif_from_screen_capture::CaptureSource::geometry)
            .take(32)
            .collect::<Vec<_>>();
        if workareas.is_empty() {
            workareas.push(source);
        }
        if let Some(index) = workareas.iter().position(|area| *area == source) {
            workareas.swap(0, index);
        }
        let guide = if context.embed_viewports() {
            None
        } else {
            Some(RecorderGuide::start_with_controller(
                None,
                self.x11_window_id
                    .ok_or("The native X11 control window is unavailable.")?,
                std::process::id(),
            )?)
        };
        let mut overlay = RecorderOverlay::new(geometry, workareas, guide, scale);
        if self.sources[self.selected_source].kind()
            == gif_from_screen_capture::CaptureSourceKind::Monitor
        {
            overlay.snap.set_candidates(&self.sources);
        }
        self.recorder_overlay = Some(overlay);
        self.main_window_snapshot = Some(snapshot);
        self.pending_recorder_start = None;
        self.notice = Some(localizer.text(Message::RecorderBorderDragHint).into());
        context.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        context.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::AlwaysOnTop,
        ));
        context.send_viewport_cmd(egui::ViewportCommand::Title(
            localizer.text(Message::RecorderX11WindowTitle).into(),
        ));
        context.request_repaint();
        Ok(())
    }

    pub(super) fn close_recorder_overlay(&mut self) {
        self.pending_recorder_start = None;
        self.shortcut_tool.reset_recording_scope();
        self.recording_countdown.cancel();
        if let Some(overlay) = &mut self.recorder_overlay
            && !overlay.snap.request_close()
        {
            // Even an idle armed native handle owns an input child. Restore the
            // ordinary main window only after its terminal cleanup receipt.
            overlay.pending_live_start = None;
            self.restore_main_window = false;
            return;
        }
        self.recorder_overlay = None;
        self.restore_main_window = true;
    }

    pub(super) fn show_recorder_overlay(&mut self, context: &egui::Context) {
        let stage = self.recorder_stage();
        let Some(mut overlay) = self.recorder_overlay.take() else {
            return;
        };
        let now = Instant::now();
        let before = overlay.geometry.region();
        overlay.poll_guide(stage);
        let parent =
            self.x11_window_id
                .zip(overlay.controller_geometry)
                .map(|(window_id, geometry)| crate::window_snap::DragParent {
                    window_id,
                    client: geometry.client,
                });
        overlay.snap.begin_frame(parent, context.pixels_per_point());
        overlay.snap.keyboard_control(context);
        overlay
            .snap
            .poll(&mut overlay.geometry, stage, overlay.gesture.is_some());
        let native = observed_window(context, &overlay);
        let may_place = prepare_change(&mut overlay, self.job.as_mut(), native, now);
        if may_place {
            overlay.place(context, native, now);
            overlay.update_guide(self.job.as_ref().and_then(|job| {
                job.retarget.as_ref().and_then(|retarget| {
                    global_region(overlay.geometry.source(), retarget.plan.applied())
                })
            }));
        }
        let hidden = matches!(overlay.window_state, WindowState::Hidden);
        let wants_resume = overlay.change.as_ref().is_some_and(|change| {
            self.job.as_ref().is_some_and(|job| {
                job.shortcut_state
                    .geometry_wants_resume(change.resume_intent())
            })
        });
        let paused = self
            .job
            .as_ref()
            .is_some_and(|job| job.controller.pause_status() == (0, Some(true)));
        let unsafe_to_paint = must_hide_for_capture(
            self.job.as_ref(),
            overlay.geometry.source(),
            native.outer,
            paused,
        );
        let hide_pixels = overlay.snap.is_picking()
            || overlay.snap.is_closing()
            || unsafe_to_paint
            || (hidden
                && (overlay.pending_live_start.is_some() || self.job.is_some())
                && !(overlay.recovering && paused && !wants_resume));
        context.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(hide_pixels));
        let action = self.draw_x11_controls(context, &mut overlay, stage, paused, !hide_pixels);
        let action = finish_snap_frame(context, &mut overlay, stage, !hide_pixels, action);
        self.stop_for_snap_close(&mut overlay);
        if overlay.geometry.region() != before {
            // A manual edit supersedes any still-pending snap, including a
            // move away and back while the native worker is finishing.
            overlay.snap.cancel();
            overlay.settle = None;
            if let Some(job) = &self.job {
                begin_change(&mut overlay, job, now);
            }
        }
        if let Ok(region) = overlay.geometry.source_local_region() {
            apply_overlay_region(&mut self.settings, stage, region);
        }
        if let Some(error) = &overlay.failure {
            overlay.snap.cancel();
            self.notice = Some(error.clone());
            if let Some(job) = &mut self.job {
                job.stop_retargeting();
                let _ = job.controller.stop();
            }
            overlay.pending_live_start = None;
        }
        if let Some(notice) = overlay.cancel_invalid_start(context, now) {
            self.notice = Some(notice.into());
        }
        let start = overlay.pending_live_start.is_some() && overlay.settled(context, now, hidden);
        finish_change(
            &mut overlay,
            self.job.as_mut(),
            context,
            now,
            hidden,
            &mut self.notice,
        );
        if RecorderOverlay::is_hidden(context) {
            overlay.was_minimized = true;
        }
        self.recorder_overlay = Some(overlay);
        if start {
            self.start_prepared_x11(context);
        }
        let action = self.recorder_frame_action(action);
        if !self.handle_geometry_action(context, action) {
            self.handle_recorder_overlay_action(context, action);
        }
        context.request_repaint_after(Duration::from_millis(16));
    }

    fn start_prepared_x11(&mut self, context: &egui::Context) {
        if let Some(overlay) = &mut self.recorder_overlay {
            overlay.pending_live_start = None;
        }
        if let Err(error) = self.start_recording() {
            self.notice = Some(error);
            if let Some(overlay) = &mut self.recorder_overlay {
                overlay.cancel_start(context);
            }
        } else if let Some(overlay) = &mut self.recorder_overlay {
            overlay.geometry.freeze_size();
        }
    }

    fn stop_for_snap_close(&mut self, overlay: &mut RecorderOverlay) {
        if !overlay.snap.is_closing() {
            return;
        }
        self.pending_recorder_start = None;
        self.recording_countdown.cancel();
        overlay.pending_live_start = None;
        if let Some(job) = &mut self.job {
            job.stop_retargeting();
            let _ = job.controller.stop();
        }
    }

    fn draw_x11_controls(
        &mut self,
        context: &egui::Context,
        overlay: &mut RecorderOverlay,
        stage: RecorderStage,
        paused: bool,
        visible: bool,
    ) -> RecorderOverlayAction {
        let localizer = self.language_settings.localizer();
        let mut action = RecorderOverlayAction::None;
        if visible {
            let notice = self.controller_notice(overlay);
            let input_ready =
                overlay.ready() && (overlay.change.is_none() || (overlay.recovering && paused));
            // The independent window can be recovered while capture is paused.
            // Never paint it over a live full-monitor recording.
            action = crate::x11_controller_ui::draw(
                context,
                &mut overlay.geometry,
                stage,
                self.progress,
                &mut self.settings,
                Some(&notice),
                input_ready,
                &mut overlay.snap,
                self.language_settings.localizer(),
            );
            if matches!(
                overlay.window_state,
                WindowState::TimedOut | WindowState::Invalid(_)
            ) {
                egui::Window::new(localizer.text(Message::RecorderPlacement))
                    .id(egui::Id::new("recorder-placement"))
                    .collapsible(false)
                    .show(context, |ui| {
                        ui.label(localizer.text(Message::RecorderPlacementUnsafe));
                        if ui
                            .button(localizer.text(Message::RecorderRetryPlacement))
                            .clicked()
                        {
                            overlay.window.retry();
                        }
                        if ui.button(localizer.text(Message::RecorderClose)).clicked() {
                            action = RecorderOverlayAction::Close;
                        }
                    });
            }
        }
        if context.input(|input| input.viewport().close_requested()) {
            action = RecorderOverlayAction::Close;
        }
        action
    }

    fn controller_notice(&self, overlay: &RecorderOverlay) -> String {
        let localizer = self.language_settings.localizer();
        let status = self.shortcut_tool.status_summary().unwrap_or_default();
        let notice = if let Some(error) = &overlay.failure {
            error.as_str()
        } else if overlay.recovering {
            localizer.text(Message::RecorderRecoveringControls)
        } else if overlay.change.is_some() {
            localizer.text(Message::RecorderUpdatingPosition)
        } else if matches!(overlay.window_state, WindowState::Hidden) {
            localizer.text(Message::RecorderNoControlSpace)
        } else {
            ""
        };
        let message = self
            .notice
            .as_deref()
            .filter(|message| *message != notice)
            .unwrap_or_default();
        format!("{notice} {message} {status}")
    }

    fn handle_geometry_action(
        &mut self,
        context: &egui::Context,
        action: RecorderOverlayAction,
    ) -> bool {
        let Some(overlay) = &mut self.recorder_overlay else {
            return false;
        };
        if action == RecorderOverlayAction::CancelCountdown && overlay.pending_live_start.is_some()
        {
            overlay.cancel_start(context);
            return true;
        }
        if let Some(change) = &mut overlay.change {
            match action {
                RecorderOverlayAction::Pause | RecorderOverlayAction::Resume => {
                    change.resume_override = Some(action == RecorderOverlayAction::Resume);
                    if let Some(job) = &self.job {
                        job.shortcut_state.reset_geometry_toggle();
                    }
                    overlay.settle = None;
                    return true;
                }
                RecorderOverlayAction::Snapshot => {
                    self.notice = Some(
                        "Snapshot not captured while the recording position is changing.".into(),
                    );
                    return true;
                }
                _ => {}
            }
        }
        false
    }
}

fn must_hide_for_capture(
    job: Option<&RecordingJob>,
    source: PhysicalRect,
    outer: Option<PhysicalRect>,
    paused: bool,
) -> bool {
    job.is_some_and(|job| {
        !paused
            && job.retarget.as_ref().is_none_or(|retarget| {
                global_region(source, retarget.plan.applied())
                    .is_none_or(|applied| outer.is_none_or(|outer| !outside(outer, applied)))
            })
    })
}

fn global_region(source: PhysicalRect, local: PhysicalRect) -> Option<PhysicalRect> {
    PhysicalRect::new(
        source.origin().x.checked_add(local.origin().x)?,
        source.origin().y.checked_add(local.origin().y)?,
        local.size().width(),
        local.size().height(),
    )
    .ok()
}

fn begin_change(overlay: &mut RecorderOverlay, job: &RecordingJob, now: Instant) {
    if overlay.change.is_none() && !job.terminal_requested {
        job.shortcut_state.begin_geometry_change();
        overlay.change = Some(GeometryChange {
            started: now,
            pause_sent: false,
            resume: !job.paused,
            resume_override: None,
            desired: overlay.geometry.region(),
        });
        overlay.settle = None;
    } else if let Some(change) = &mut overlay.change
        && change.desired != overlay.geometry.region()
    {
        change.desired = overlay.geometry.region();
        change.started = now;
        overlay.settle = None;
    }
}

fn prepare_change(
    overlay: &mut RecorderOverlay,
    job: Option<&mut RecordingJob>,
    native: NativeWindow,
    now: Instant,
) -> bool {
    let Some(job) = job else {
        return true;
    };
    if job.terminal_requested {
        return false;
    }
    let target_changed = overlay.geometry.source_local_region().is_ok_and(|region| {
        job.retarget
            .as_ref()
            .is_some_and(|retarget| retarget.plan.applied() != region)
    });
    let unsafe_window = !matches!(overlay.window_state, WindowState::Hidden)
        && native
            .outer
            .is_some_and(|rect| !outside(rect, overlay.geometry.region()));
    if target_changed
        || unsafe_window
        || overlay.last_scale.to_bits() != native.pixels_per_point.to_bits()
    {
        begin_change(overlay, job, now);
    }
    let Some(change) = &mut overlay.change else {
        return true;
    };
    if now.saturating_duration_since(change.started) >= CHANGE_TIMEOUT && !overlay.recovering {
        overlay.failure = Some(
            "The recording position update timed out; stopping and saving the project.".into(),
        );
        return false;
    }
    let (pending, acknowledged) = job.controller.pause_status();
    if pending != 0 {
        return false;
    }
    if !change.pause_sent {
        change.resume = acknowledged != Some(true) && !overlay.recovering;
        change.pause_sent = job.controller.pause();
        return false;
    }
    acknowledged == Some(true)
}

fn finish_change(
    overlay: &mut RecorderOverlay,
    job: Option<&mut RecordingJob>,
    context: &egui::Context,
    now: Instant,
    hidden: bool,
    notice: &mut Option<String>,
) {
    let Some(job) = job else {
        return;
    };
    if job.terminal_requested {
        return;
    }
    let restored = overlay.was_minimized && !RecorderOverlay::is_hidden(context);
    if restored && hidden && !overlay.recovering {
        overlay.recovering = true;
        begin_change(overlay, job, now);
        overlay.was_minimized = false;
    }
    let Some(change) = &overlay.change else {
        return;
    };
    if job.controller.pause_status() != (0, Some(true)) {
        return;
    }
    if hidden
        && overlay.recovering
        && !job
            .shortcut_state
            .geometry_wants_resume(change.resume_intent())
    {
        return;
    }
    if !overlay.settled(context, now, hidden) {
        return;
    }
    let Ok(region) = overlay.geometry.source_local_region() else {
        return;
    };
    job.observe_target(region);
    if let Some(error) = job.poll_retarget(true) {
        overlay.failure = Some(error);
        return;
    }
    let applied = job
        .retarget
        .as_ref()
        .is_some_and(|retarget| retarget.pending.is_none() && retarget.plan.applied() == region);
    if !applied {
        return;
    }
    // Retire the old extra exclusion inside the paused transaction, not on the
    // first resumed sample. The final guide generation must also be acknowledged.
    overlay.update_guide(None);
    if !overlay.guide_ready() {
        return;
    }
    let resume = overlay.change.take().unwrap().resume_intent();
    let rejected = job
        .shortcut_state
        .finish_geometry_change(&job.controller, resume);
    overlay.recovering = false;
    overlay.settle = None;
    if rejected > 0 {
        *notice = Some(format!(
            "{rejected} snapshot requests were not captured while the recording region changed."
        ));
    }
}

fn gesture_geometry(
    gesture: &Gesture,
    position: PhysicalPosition,
    stage: RecorderStage,
) -> RecorderGeometry {
    let dx = i64::from(position.x) - i64::from(gesture.start.x);
    let dy = i64::from(position.y) - i64::from(gesture.start.y);
    let mut geometry = gesture.initial;
    let corner = matches!(
        gesture.edge,
        GuideEdge::TopLeft | GuideEdge::TopRight | GuideEdge::BottomLeft | GuideEdge::BottomRight
    );
    if !stage.allows_resizing() || geometry.size_is_frozen() || !corner {
        geometry.move_by(dx, dy);
        return geometry;
    }
    let source = geometry.source();
    let region = geometry.region();
    let (mut left, mut top) = (i64::from(region.origin().x), i64::from(region.origin().y));
    let (mut right, mut bottom) = (
        left + i64::from(region.size().width()),
        top + i64::from(region.size().height()),
    );
    if matches!(gesture.edge, GuideEdge::TopLeft | GuideEdge::BottomLeft) {
        left = left
            .saturating_add(dx)
            .clamp(i64::from(source.origin().x), right - 1);
    } else {
        right = right.saturating_add(dx).clamp(
            left + 1,
            i64::from(source.origin().x) + i64::from(source.size().width()),
        );
    }
    if matches!(gesture.edge, GuideEdge::TopLeft | GuideEdge::TopRight) {
        top = top
            .saturating_add(dy)
            .clamp(i64::from(source.origin().y), bottom - 1);
    } else {
        bottom = bottom.saturating_add(dy).clamp(
            top + 1,
            i64::from(source.origin().y) + i64::from(source.size().height()),
        );
    }
    let resized = (|| {
        let region = PhysicalRect::new(
            i32::try_from(left).ok()?,
            i32::try_from(top).ok()?,
            u32::try_from(right - left).ok()?,
            u32::try_from(bottom - top).ok()?,
        )
        .ok()?;
        RecorderGeometry::new(source, region).ok()
    })();
    resized.unwrap_or(geometry)
}
