//! Physical X11 selection and a separate native border/control window.

mod desktop_space;
#[cfg(test)]
mod tests;
use desktop_space::{desktop_capture_source, monitor_workareas, prefer_region_monitor};

use std::time::{Duration, Instant};

use crate::ui_notice::Notice;
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
    failure: Option<Notice>,
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
    native_preview: Option<gif_from_screen_capture_linux::GuidePreview>,
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
            native_preview: None,
            snap: crate::window_snap::WindowSnapUi::default(),
        }
    }

    pub(super) fn ready(&self) -> bool {
        self.initialized
            && self.gesture.is_none()
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

    fn cancel_invalid_start(&mut self, context: &egui::Context, now: Instant) -> Option<Notice> {
        let started = self.pending_live_start?;
        let notice = if now.saturating_duration_since(started) >= CHANGE_TIMEOUT {
            Message::RecorderStartExpired
        } else if self
            .settle
            .as_ref()
            .is_some_and(|settle| matches!(settle.hide, HidePhase::Acknowledged))
            && !Self::is_hidden(context)
        {
            Message::RecorderStartRestored
        } else {
            return None;
        };
        self.cancel_start(context);
        Some(notice.into())
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

    fn update_guide(&mut self, protected_region: Option<PhysicalRect>, stage: RecorderStage) {
        self.update_guide_with_preview(protected_region, stage, None);
    }

    fn update_guide_with_preview(
        &mut self,
        protected_region: Option<PhysicalRect>,
        stage: RecorderStage,
        suspended_bounds: Option<PhysicalRect>,
    ) {
        if self.failure.is_some() {
            return;
        }
        let region = if self.snap.is_picking() {
            None
        } else {
            Some(self.geometry.region())
        };
        let protected_region = protected_region.filter(|protected| Some(*protected) != region);
        let handle_avoid = self.controller_geometry.map(|geometry| geometry.outer);
        let handle_scale = drag_handle_scale(self.last_scale);
        let resize_handles = stage.allows_resizing() && !self.geometry.size_is_frozen();
        let preview_bounds = region.and_then(|_| {
            if stage == RecorderStage::Ready {
                Some(self.geometry.source())
            } else {
                suspended_bounds
            }
        });
        let native_owns_motion =
            self.native_preview.is_some_and(|preview| preview.active) && self.gesture.is_some();
        if self.request.is_some_and(|request| {
            (request.region == region || native_owns_motion)
                && request.protected_region == protected_region
                && request.handle_avoid == handle_avoid
                && request.handle_scale == handle_scale
                && request.resize_handles == resize_handles
                && request.preview_bounds == preview_bounds
        }) && self.native_preview.is_none_or(|preview| preview.active)
        {
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
            border_width: 2,
            handle_scale,
            handle_avoid,
            resize_handles,
            preview_bounds,
        };
        self.acknowledged = None;
        self.settle = None;
        if let Some(guide) = &self.guide
            && let Err(error) = guide.request(request)
        {
            self.failure = Some(error.into());
            return;
        }
        self.request = Some(request);
    }

    fn update_capture_guide(&mut self, job: Option<&RecordingJob>, stage: RecorderStage) {
        let protected = job.and_then(|job| {
            job.retarget
                .as_ref()
                .and_then(|retarget| global_region(self.geometry.source(), retarget.plan.applied()))
        });
        let suspended_bounds = job
            .filter(|job| {
                self.change.is_some()
                    && self.gesture.is_some()
                    && job.controller.pause_status() == (0, Some(true))
            })
            .map(|_| self.geometry.source());
        self.update_guide_with_preview(protected, stage, suspended_bounds);
    }

    fn poll_guide(&mut self, stage: RecorderStage) {
        let Some(guide) = &mut self.guide else {
            return;
        };
        let update = guide.poll();
        self.native_preview = update.preview;
        let mut preview_geometry = self.gesture.as_ref().and_then(|gesture| {
            update
                .preview
                .filter(|preview| preview.gesture_id == gesture.id)
                .map(|preview| gesture_geometry(gesture, preview.position, stage))
        });
        self.controller_geometry = update.controller;
        match update.status {
            GuideStatus::Failed(error) => self.failure = Some(error.into()),
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
                    let native_proof = update.preview.is_some_and(|preview| {
                        preview.gesture_id == gesture_id && preview.generation == generation
                    });
                    if native_proof
                        || (self
                            .request
                            .is_some_and(|request| request.generation == generation)
                            && self.acknowledged == Some(generation))
                    {
                        self.gesture = Some(Gesture {
                            id: gesture_id,
                            start: position,
                            initial: self.geometry,
                            edge,
                        });
                        preview_geometry = update
                            .preview
                            .filter(|preview| preview.gesture_id == gesture_id)
                            .map(|preview| {
                                gesture_geometry(
                                    self.gesture.as_ref().unwrap(),
                                    preview.position,
                                    stage,
                                )
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
        if let Some(geometry) = preview_geometry {
            self.geometry = geometry;
        }
    }

    fn place(&mut self, context: &egui::Context, native: NativeWindow, now: Instant) {
        // Moving an egui toplevel on every pointer sample makes the WM/compositor
        // part of the drag loop. Keep controls stationary until release; capture
        // remains paused if a moving selection approaches the controls.
        if self.gesture.is_some()
            && self.last_scale.to_bits() == native.pixels_per_point.to_bits()
            && matches!(
                self.window_state,
                WindowState::Ready { .. } | WindowState::Hidden
            )
        {
            return;
        }
        prefer_region_monitor(&mut self.workareas, self.geometry.region());
        let scale = native.pixels_per_point;
        let area = self
            .workareas
            .first()
            .copied()
            .unwrap_or(self.geometry.source());
        let size = panel_size(
            area.size(),
            scale,
            crate::x11_controller_ui::panel_height(context),
            self.geometry.region().size().width(),
        );
        let update = self.window.advance(
            native,
            WindowRequest {
                region: self.geometry.region(),
                workareas: &self.workareas,
                panel_size: size,
                gap: 3,
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
            self.failure = Some(Message::RecorderControlsSettleFailed.into());
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
fn panel_size(area: PhysicalSize, scale: f32, height: f32, region_width: u32) -> PhysicalSize {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    PhysicalSize::new(
        (region_width as f32)
            .max(480.0 * scale)
            .round()
            .clamp(1.0, area.width() as f32) as u32,
        (height * scale).round().clamp(1.0, area.height() as f32) as u32,
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
        crate::recording_output::validate(&self.settings.output)
            .map_err(|message| localizer.text(message).to_owned())?;
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
        let capture_index = desktop_capture_source(&self.sources, self.selected_source);
        let capture_bounds = self.sources[capture_index].geometry().unwrap_or(source);
        let geometry = RecorderGeometry::new(capture_bounds, region)?;
        let workareas = monitor_workareas(&self.sources, source);
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
        if let Some(guide) = &overlay.guide {
            let context = context.clone();
            guide.set_repaint_callback(move || context.request_repaint());
        }
        if self.sources[self.selected_source].kind()
            == gif_from_screen_capture::CaptureSourceKind::Monitor
        {
            overlay.snap.set_candidates(&self.sources);
        }
        // Use one root source for the complete recording, including retargets.
        // Changing only drag bounds would still fail in the capture backend.
        self.selected_source = capture_index;
        apply_overlay_region(
            &mut self.settings,
            RecorderStage::Ready,
            geometry.source_local_region()?,
        );
        self.recorder_overlay = Some(overlay);
        self.main_window_snapshot = Some(snapshot);
        self.pending_recorder_start = None;
        self.notice = None;
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
            overlay.update_capture_guide(self.job.as_ref(), stage);
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
            self.notice = Some(notice);
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
        request_recorder_repaint(context, self.recorder_overlay.as_ref());
    }

    fn start_prepared_x11(&mut self, context: &egui::Context) {
        if let Some(overlay) = &mut self.recorder_overlay {
            overlay.pending_live_start = None;
        }
        if let Err(error) = self.start_recording() {
            self.notice = Some(error.into());
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
            let notice = self.controller_notice(overlay, stage);
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

    fn controller_notice(&self, overlay: &RecorderOverlay, stage: RecorderStage) -> String {
        let localizer = self.language_settings.localizer();
        let status = self
            .shortcut_tool
            .status_summary(localizer)
            .unwrap_or_default();
        let failure = overlay
            .failure
            .as_ref()
            .map(|notice| notice.render(localizer));
        let notice = if let Some(error) = failure.as_deref() {
            error
        } else if overlay.recovering {
            localizer.text(Message::RecorderRecoveringControls)
        } else if overlay.change.is_some() {
            localizer.text(Message::RecorderUpdatingPosition)
        } else if matches!(overlay.window_state, WindowState::Hidden) {
            localizer.text(Message::RecorderNoControlSpace)
        } else {
            ""
        };
        let current_notice = self.recorder_notice_text(stage, localizer).filter(|_| {
            !self.notice.as_ref().is_some_and(|notice| {
                matches!(
                    notice.message_id(),
                    Some(Message::RecorderStarted | Message::RecorderBorderDragHint)
                )
            })
        });
        let message = current_notice
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
                    self.notice = Some(Notice::localized(
                        self.language_settings.localizer(),
                        Message::RecorderSnapshotDuringMove,
                        &[],
                    ));
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

fn request_recorder_repaint(context: &egui::Context, overlay: Option<&RecorderOverlay>) {
    if overlay.is_some_and(|overlay| overlay.gesture.is_some()) {
        // Follow display cadence while dragging; avoid a timer after each frame.
        context.request_repaint();
    } else {
        context.request_repaint_after(Duration::from_millis(8));
    }
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
    let (pending, acknowledged) = job.controller.pause_status();
    // A healthy paused capture may wait for the user to release the grip.
    // Keep the timeout for missing pause acknowledgements and post-release work.
    if overlay.gesture.is_some() && pending == 0 && acknowledged == Some(true) {
        change.started = now;
    }
    if now.saturating_duration_since(change.started) >= CHANGE_TIMEOUT && !overlay.recovering {
        overlay.failure = Some(
            "The recording position update timed out; stopping and saving the project.".into(),
        );
        return false;
    }
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
    notice: &mut Option<Notice>,
) {
    if overlay.gesture.is_some() {
        return;
    }
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
    overlay.update_guide(None, RecorderStage::Paused);
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
        *notice = Some(Notice::new(
            Message::RecorderSnapshotsRejectedForMove,
            &[("count", &rejected.to_string())],
        ));
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite scale is rounded and clamped to the native 100–400 percent contract"
)]
fn drag_handle_scale(scale: f32) -> u16 {
    if scale.is_finite() {
        (scale.clamp(1.0, 4.0) * 100.0).round() as u16
    } else {
        100
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
