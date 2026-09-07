//! Global input selects existing recorder actions; it owns no capture state.

use eframe::egui;
use gif_from_screen_capture_linux::{ShortcutAction, ShortcutActionHandler};
use gif_from_screen_workflow::RecordingController;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const START_WAIT_LIMIT: Duration = Duration::from_secs(5);

use crate::{
    AppView, CaptureSourceJobState, GifFromScreenApp, RecorderOverlayAction, RecorderStage,
    RecordingCadenceChoice, ShutdownState,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dispatch {
    None,
    PrepareSource,
    OpenController,
    CancelPreparation,
    Recorder(RecorderOverlayAction),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Preparation {
    Idle,
    Choosing,
    Preview,
    InitializingController,
    Controller,
}

/// The handler captures one recording controller, never an interchangeable UI slot.
#[derive(Default)]
pub(super) struct LiveShortcutState {
    terminal: Arc<AtomicBool>,
}

impl LiveShortcutState {
    fn handler(&self, controller: RecordingController, manual: bool) -> ShortcutActionHandler {
        let terminal = Arc::clone(&self.terminal);
        Arc::new(move |action| {
            if terminal.load(Ordering::Acquire) {
                return true;
            }
            match action {
                ShortcutAction::Stop => {
                    if !terminal.swap(true, Ordering::AcqRel) {
                        let _ = controller.stop();
                    }
                }
                ShortcutAction::StartPause => {
                    let _ = controller.toggle_pause();
                }
                ShortcutAction::Snapshot if manual => {
                    // A disconnected UI receipt does not cancel the accepted snapshot.
                    // The existing workflow owns its 64-request bound and frame timing.
                    drop(controller.trigger_snapshot());
                }
                ShortcutAction::Snapshot => {}
            }
            // A stopped old receiver is still consumed here, not reused as UI Start.
            true
        })
    }
}

impl Drop for LiveShortcutState {
    fn drop(&mut self) {
        self.terminal.store(true, Ordering::Release);
    }
}

struct Context {
    stage: RecorderStage,
    preparation: Preparation,
    manual: bool,
    pause_pending: bool,
}

fn dispatch(action: ShortcutAction, state: &Context) -> Dispatch {
    use RecorderOverlayAction as Recorder;
    use RecorderStage as Stage;
    match action {
        ShortcutAction::Stop => match state.stage {
            Stage::Countdown(_) => Dispatch::Recorder(Recorder::CancelCountdown),
            Stage::Recording | Stage::Paused => Dispatch::Recorder(Recorder::Stop),
            Stage::Ready
                if matches!(
                    state.preparation,
                    Preparation::Controller | Preparation::InitializingController
                ) =>
            {
                Dispatch::Recorder(Recorder::Close)
            }
            Stage::Ready
                if matches!(
                    state.preparation,
                    Preparation::Choosing | Preparation::Preview
                ) =>
            {
                Dispatch::CancelPreparation
            }
            _ => Dispatch::None,
        },
        ShortcutAction::Snapshot
            if state.stage == Stage::Recording && state.manual && !state.pause_pending =>
        {
            Dispatch::Recorder(Recorder::Snapshot)
        }
        ShortcutAction::Snapshot => Dispatch::None,
        ShortcutAction::StartPause if state.pause_pending => Dispatch::None,
        ShortcutAction::StartPause => match state.stage {
            Stage::Recording => Dispatch::Recorder(Recorder::Pause),
            Stage::Paused => Dispatch::Recorder(Recorder::Resume),
            Stage::Ready if state.preparation == Preparation::Controller => {
                Dispatch::Recorder(Recorder::Start)
            }
            Stage::Ready
                if matches!(
                    state.preparation,
                    Preparation::Choosing | Preparation::InitializingController
                ) =>
            {
                Dispatch::None
            }
            Stage::Ready if state.preparation == Preparation::Preview => Dispatch::OpenController,
            Stage::Ready => Dispatch::PrepareSource,
            _ => Dispatch::None,
        },
    }
}

impl GifFromScreenApp {
    pub(crate) fn attach_recording_shortcuts(&mut self) {
        if let Some(job) = &self.job {
            let handler = job.shortcut_state.handler(
                job.controller.clone(),
                self.settings.cadence == RecordingCadenceChoice::Manual,
            );
            self.shortcut_tool.set_recording_handler(Some(handler));
        }
    }

    pub(crate) fn poll_recorder_shortcuts(&mut self, context: &egui::Context) {
        if let Some(job) = &mut self.job {
            if job.shortcut_state.terminal.load(Ordering::Acquire) {
                job.stop_retargeting();
            }
            let (pending, acknowledged) = job.controller.pause_status();
            if pending > 0 {
                job.paused = false;
            } else {
                job.pause_requested = None;
                job.paused = acknowledged == Some(true);
            }
        }
        let scope = self.shutdown == ShutdownState::Active
            && (self.view == AppView::ScreenRecorder
                || self.recorder_overlay.is_some()
                || self.wayland_crop_controller.is_some()
                || self.job.is_some()
                || self.recording_countdown.is_active());
        let actions = self.shortcut_tool.poll(context, self.display_server, scope);
        if !scope {
            self.pending_recorder_start = None;
            return;
        }
        for action in actions {
            let state = Context {
                stage: self.recorder_stage(),
                preparation: self.shortcut_preparation(),
                manual: self.settings.cadence == RecordingCadenceChoice::Manual,
                pause_pending: self
                    .job
                    .as_ref()
                    .is_some_and(|job| job.pause_requested.is_some()),
            };
            trace(format_args!(
                "action={action:?} stage={:?} preparation={:?} dispatch={:?}",
                state.stage,
                state.preparation,
                dispatch(action, &state)
            ));
            match dispatch(action, &state) {
                Dispatch::None => {
                    if action == ShortcutAction::StartPause
                        && state.stage == RecorderStage::Ready
                        && state.preparation == Preparation::InitializingController
                    {
                        self.notice = Some("The recorder is still preparing its capture area. Wait for Start to become available, then press the shortcut again.".into());
                    }
                }
                Dispatch::PrepareSource => {
                    if self.source_catalog_job.state() == CaptureSourceJobState::Loading
                        || self.source_workers_active()
                        || self.region_picker.is_some()
                    {
                        continue;
                    }
                    if let Err(error) = self.open_recorder_overlay(context) {
                        self.notice = Some(error);
                    }
                    break; // A second queued press must not skip first presentation/geometry.
                }
                Dispatch::OpenController => {
                    if let Err(error) = self.open_wayland_crop_controller(context) {
                        self.notice = Some(error);
                    }
                    break;
                }
                Dispatch::CancelPreparation => {
                    self.shortcut_tool.reset_recording_scope();
                    if self.wayland_prepare_job.cancel() {
                        self.wayland_frozen_preview = None;
                        self.notice =
                            Some("Source preparation cancelled by the recording shortcut.".into());
                    }
                    self.region_picker = None;
                    break;
                }
                Dispatch::Recorder(action) => {
                    if action == RecorderOverlayAction::Start {
                        // Resolve this tick's geometry/input before starting capture.
                        self.pending_recorder_start = Some(Instant::now());
                        continue;
                    }
                    if self.wayland_crop_controller.is_some() {
                        self.handle_wayland_controller_action(context, action);
                    } else {
                        self.handle_recorder_overlay_action(context, action);
                    }
                }
            }
        }
    }

    fn shortcut_preparation(&self) -> Preparation {
        if let Some(overlay) = &self.recorder_overlay {
            if overlay.initialized && overlay.input.ready() && overlay.last_region_valid {
                Preparation::Controller
            } else {
                Preparation::InitializingController
            }
        } else if self.wayland_crop_controller.is_some() {
            Preparation::Controller
        } else if self.wayland_frozen_preview.is_some() {
            Preparation::Preview
        } else if self.wayland_prepare_job.is_active() || self.region_picker.is_some() {
            Preparation::Choosing
        } else {
            Preparation::Idle
        }
    }

    pub(crate) fn recorder_frame_action(
        &mut self,
        clicked: RecorderOverlayAction,
    ) -> RecorderOverlayAction {
        self.recorder_frame_action_at(clicked, Instant::now())
    }

    fn recorder_frame_action_at(
        &mut self,
        clicked: RecorderOverlayAction,
        now: Instant,
    ) -> RecorderOverlayAction {
        if clicked == RecorderOverlayAction::Start {
            self.pending_recorder_start = Some(now);
        } else if clicked != RecorderOverlayAction::None {
            self.pending_recorder_start = None;
            return clicked;
        }
        let Some(requested_at) = self.pending_recorder_start else {
            return RecorderOverlayAction::None;
        };
        if self.recorder_stage() != RecorderStage::Ready
            || (self.recorder_overlay.is_none() && self.wayland_crop_controller.is_none())
        {
            self.pending_recorder_start = None;
            return RecorderOverlayAction::None;
        }
        let error = if now.saturating_duration_since(requested_at) >= START_WAIT_LIMIT {
            Some("Start request expired while the capture area was changing. Press Start again.")
        } else if let Some(overlay) = &self.recorder_overlay {
            if overlay.input.failed() {
                Some(
                    "Start cancelled because mouse-transparent input preparation failed. Close the recorder and retry.",
                )
            } else if !overlay.last_region_valid {
                Some(
                    "Start cancelled: keep the capture rectangle inside its selected source, then press Start again.",
                )
            } else if !overlay.input.ready() {
                // The just-drawn native geometry can revoke a previously ready shape.
                // Preserve the intent only within this controller and a short deadline.
                self.notice = Some("Start pending: waiting for the current capture area to become mouse-transparent…".into());
                return RecorderOverlayAction::None;
            } else {
                None
            }
        } else {
            None
        };
        self.pending_recorder_start = None;
        if let Some(error) = error {
            self.notice = Some(error.into());
            return RecorderOverlayAction::None;
        }
        RecorderOverlayAction::Start
    }
}

/// Opt-in, bounded debug diagnostics contain recorder state only, not input text.
pub(crate) fn trace(message: std::fmt::Arguments<'_>) {
    #[cfg(debug_assertions)]
    if std::env::var_os("GFS_RECORDER_TRACE").is_some() {
        use std::sync::atomic::AtomicUsize;
        static LINES: AtomicUsize = AtomicUsize::new(0);
        if LINES.fetch_add(1, Ordering::Relaxed) < 128 {
            eprintln!("recorder: {message}");
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = message;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(stage: RecorderStage) -> Context {
        Context {
            stage,
            preparation: Preparation::Controller,
            manual: true,
            pause_pending: false,
        }
    }

    #[test]
    fn keys_use_the_same_start_pause_resume_stop_actions_as_buttons() {
        for (stage, expected) in [
            (RecorderStage::Ready, RecorderOverlayAction::Start),
            (RecorderStage::Recording, RecorderOverlayAction::Pause),
            (RecorderStage::Paused, RecorderOverlayAction::Resume),
        ] {
            assert_eq!(
                dispatch(ShortcutAction::StartPause, &context(stage)),
                Dispatch::Recorder(expected)
            );
        }
        for stage in [RecorderStage::Recording, RecorderStage::Paused] {
            assert_eq!(
                dispatch(ShortcutAction::Stop, &context(stage)),
                Dispatch::Recorder(RecorderOverlayAction::Stop)
            );
        }
        assert_eq!(
            dispatch(ShortcutAction::Stop, &context(RecorderStage::Countdown(3))),
            Dispatch::Recorder(RecorderOverlayAction::CancelCountdown)
        );
    }

    #[test]
    fn shortcuts_do_not_toggle_pending_pause_or_repeat_countdown_or_finalize() {
        for stage in [RecorderStage::Countdown(3), RecorderStage::Finalizing] {
            assert_eq!(
                dispatch(ShortcutAction::StartPause, &context(stage)),
                Dispatch::None
            );
            assert_eq!(
                dispatch(ShortcutAction::Snapshot, &context(stage)),
                Dispatch::None
            );
        }
        let mut state = context(RecorderStage::Recording);
        state.pause_pending = true;
        assert_eq!(dispatch(ShortcutAction::StartPause, &state), Dispatch::None);
        assert_eq!(dispatch(ShortcutAction::Snapshot, &state), Dispatch::None);
        assert_eq!(
            dispatch(ShortcutAction::Stop, &state),
            Dispatch::Recorder(RecorderOverlayAction::Stop)
        );
        assert_eq!(
            dispatch(ShortcutAction::Stop, &context(RecorderStage::Finalizing)),
            Dispatch::None
        );
    }

    #[test]
    fn snapshot_is_available_only_in_acknowledged_running_manual_capture() {
        let mut state = context(RecorderStage::Recording);
        assert_eq!(
            dispatch(ShortcutAction::Snapshot, &state),
            Dispatch::Recorder(RecorderOverlayAction::Snapshot)
        );
        state.manual = false;
        assert_eq!(dispatch(ShortcutAction::Snapshot, &state), Dispatch::None);
        for stage in [RecorderStage::Ready, RecorderStage::Paused] {
            assert_eq!(
                dispatch(ShortcutAction::Snapshot, &context(stage)),
                Dispatch::None
            );
        }
    }

    #[test]
    fn source_permission_and_crop_preparation_are_not_skipped() {
        let mut state = context(RecorderStage::Ready);
        state.preparation = Preparation::Idle;
        assert_eq!(
            dispatch(ShortcutAction::StartPause, &state),
            Dispatch::PrepareSource
        );
        state.preparation = Preparation::Choosing;
        assert_eq!(dispatch(ShortcutAction::StartPause, &state), Dispatch::None);
        assert_eq!(
            dispatch(ShortcutAction::Stop, &state),
            Dispatch::CancelPreparation
        );
        state.preparation = Preparation::Preview;
        assert_eq!(
            dispatch(ShortcutAction::StartPause, &state),
            Dispatch::OpenController
        );
        assert_eq!(
            dispatch(ShortcutAction::Stop, &state),
            Dispatch::CancelPreparation
        );
        state.preparation = Preparation::InitializingController;
        assert_eq!(dispatch(ShortcutAction::StartPause, &state), Dispatch::None);
        assert_eq!(
            dispatch(ShortcutAction::Stop, &state),
            Dispatch::Recorder(RecorderOverlayAction::Close)
        );
    }

    #[test]
    fn active_commands_reach_the_worker_without_any_app_update() {
        let (controller, control) = RecordingController::channel();
        let state = LiveShortcutState::default();
        let handler = state.handler(controller.clone(), true);
        assert!(handler(ShortcutAction::StartPause));
        assert_eq!(controller.pause_status().0, 1);
        assert!(handler(ShortcutAction::StartPause));
        assert_eq!(controller.pause_status().0, 1); // One in-flight toggle.
        assert!(handler(ShortcutAction::Stop));
        assert!(state.terminal.load(Ordering::Acquire));
        assert!(handler(ShortcutAction::StartPause));
        assert_eq!(controller.pause_status().0, 1);
        drop(control);
        assert_eq!(controller.pause_status(), (0, None));
        assert!(handler(ShortcutAction::StartPause)); // Never becomes next-project UI Start.
    }

    #[test]
    fn dropped_recording_target_revokes_its_handler_and_manual_queue_stays_bounded() {
        let (controller, control) = RecordingController::channel();
        let state = LiveShortcutState::default();
        let handler = state.handler(controller.clone(), true);
        for _ in 0..64 {
            assert!(handler(ShortcutAction::Snapshot));
        }
        assert!(matches!(
            controller.trigger_snapshot().status(),
            gif_from_screen_workflow::SnapshotTriggerStatus::Rejected(
                gif_from_screen_workflow::SnapshotTriggerRejection::QueueFull { .. }
            )
        ));
        drop(state);
        handler(ShortcutAction::StartPause);
        assert_eq!(controller.pause_status(), (0, None));
        drop(control);
    }

    #[test]
    fn recorder_primary_action_stays_above_scrollable_shortcut_settings() {
        fn label_rect(shape: &egui::Shape) -> Option<egui::Rect> {
            match shape {
                egui::Shape::Text(text) if text.galley.text() == "Open recorder frame" => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()))
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(label_rect),
                _ => None,
            }
        }
        for (size, font_scale) in [
            (egui::vec2(480.0, 320.0), 1.0),
            (egui::vec2(640.0, 480.0), 1.8),
        ] {
            let context = egui::Context::default();
            context.style_mut(|style| {
                for font in style.text_styles.values_mut() {
                    font.size *= font_scale;
                }
            });
            let mut app = GifFromScreenApp::default();
            app.source_catalog_attempted = true;
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        ui.add_space(36.0); // Account for the app header as well.
                        app.show_screen_recorder(ui);
                    });
                },
            );
            let (rect, clip) = output
                .shapes
                .iter()
                .find_map(|shape| label_rect(&shape.shape).map(|rect| (rect, shape.clip_rect)))
                .expect("primary recorder button rendered");
            assert!(clip.contains_rect(rect));
            assert!(screen.contains_rect(rect));
        }
    }

    #[test]
    fn x11_transparent_ui_prefers_gl_and_preserves_explicit_backend_choices() {
        fn backends(options: &eframe::NativeOptions) -> wgpu::Backends {
            match &options.wgpu_options.wgpu_setup {
                eframe::egui_wgpu::WgpuSetup::CreateNew(setup) => {
                    setup.instance_descriptor.backends
                }
                eframe::egui_wgpu::WgpuSetup::Existing(_) => panic!("test expects a new renderer"),
            }
        }
        let mut options = crate::native_options();
        assert_eq!(options.viewport.transparent, Some(true));
        assert_eq!(
            options.viewport.app_id.as_deref(),
            Some(gif_from_screen_capture_linux::APPLICATION_ID)
        );
        crate::configure_ui_backend(
            &mut options,
            Some(gif_from_screen_capture_linux::LinuxDisplayServer::X11),
            None,
        );
        assert_eq!(backends(&options), wgpu::Backends::GL);
        crate::configure_ui_backend(
            &mut options,
            Some(gif_from_screen_capture_linux::LinuxDisplayServer::X11),
            Some(wgpu::Backends::VULKAN),
        );
        assert_eq!(backends(&options), wgpu::Backends::VULKAN);
        crate::configure_ui_backend(
            &mut options,
            Some(gif_from_screen_capture_linux::LinuxDisplayServer::Wayland),
            None,
        );
        assert_eq!(backends(&options), wgpu::Backends::VULKAN);
        let app = GifFromScreenApp::default();
        assert_eq!(
            eframe::App::clear_color(&app, &egui::Visuals::dark()).map(f32::to_bits),
            [0; 4]
        );
    }

    #[test]
    fn deferred_start_is_consumed_once_and_never_overrides_a_clicked_close() {
        let mut app = GifFromScreenApp::default();
        let context = egui::Context::default();
        app.wayland_crop_controller = Some(crate::WaylandCropController {
            texture: context.load_texture(
                "start-test",
                egui::ColorImage::filled([1, 1], egui::Color32::BLACK),
                egui::TextureOptions::NEAREST,
            ),
            source_size: gif_from_screen_capture::PhysicalSize::new(640, 480).unwrap(),
            region: gif_from_screen_capture::PhysicalRect::new(0, 0, 640, 480).unwrap(),
            drag_start: None,
            drag_current: None,
            drag_initial_region: None,
        });
        app.pending_recorder_start = Some(Instant::now());
        assert_eq!(
            app.recorder_frame_action(RecorderOverlayAction::None),
            RecorderOverlayAction::Start
        );
        assert_eq!(
            app.recorder_frame_action(RecorderOverlayAction::None),
            RecorderOverlayAction::None
        );
        app.pending_recorder_start = Some(Instant::now());
        assert_eq!(
            app.recorder_frame_action(RecorderOverlayAction::Close),
            RecorderOverlayAction::Close
        );
        assert!(app.pending_recorder_start.is_none());
    }

    fn preparing_overlay() -> crate::RecorderOverlay {
        crate::RecorderOverlay {
            window_title: "synthetic-controller".into(),
            input: crate::x11_recorder_input::RecorderInput::default(),
            last_region_valid: true,
            initial_position: egui::Pos2::ZERO,
            initial_size: egui::vec2(648.0, 584.0),
            initialized: true,
            source_geometry: gif_from_screen_capture::PhysicalRect::new(0, 0, 1440, 1000).unwrap(),
        }
    }

    #[test]
    fn a_start_survives_this_frames_input_shape_change_but_expires_without_an_ack() {
        let mut app = GifFromScreenApp::default();
        app.recorder_overlay = Some(preparing_overlay());
        let now = Instant::now();
        app.pending_recorder_start = Some(now);
        assert_eq!(
            app.recorder_frame_action_at(RecorderOverlayAction::None, now),
            RecorderOverlayAction::None
        );
        assert_eq!(app.pending_recorder_start, Some(now));
        assert!(app.notice.as_deref().unwrap().contains("Start pending"));
        assert_eq!(
            app.recorder_frame_action_at(RecorderOverlayAction::None, now + START_WAIT_LIMIT),
            RecorderOverlayAction::None
        );
        assert!(app.pending_recorder_start.is_none());
        assert!(app.notice.as_deref().unwrap().contains("expired"));
    }

    #[test]
    fn clicked_start_waits_too_but_cancel_and_invalid_geometry_revoke_the_intent() {
        let mut app = GifFromScreenApp::default();
        app.recorder_overlay = Some(preparing_overlay());
        let now = Instant::now();
        assert_eq!(
            app.recorder_frame_action_at(RecorderOverlayAction::Start, now),
            RecorderOverlayAction::None
        );
        assert_eq!(app.pending_recorder_start, Some(now));
        app.recorder_overlay.as_mut().unwrap().last_region_valid = false;
        assert_eq!(
            app.recorder_frame_action_at(RecorderOverlayAction::None, now),
            RecorderOverlayAction::None
        );
        assert!(app.pending_recorder_start.is_none());
        assert!(app.notice.as_deref().unwrap().contains("inside"));
        app.pending_recorder_start = Some(now);
        assert_eq!(
            app.recorder_frame_action_at(RecorderOverlayAction::Close, now),
            RecorderOverlayAction::Close
        );
        assert!(app.pending_recorder_start.is_none());
        app.pending_recorder_start = Some(now);
        app.close_recorder_overlay();
        assert!(app.pending_recorder_start.is_none());
    }

    #[test]
    fn a_pending_start_cannot_escape_its_controller_or_replay_during_countdown() {
        let mut app = GifFromScreenApp::default();
        let now = Instant::now();
        app.pending_recorder_start = Some(now);
        assert_eq!(
            app.recorder_frame_action_at(RecorderOverlayAction::None, now),
            RecorderOverlayAction::None
        );
        assert!(app.pending_recorder_start.is_none());
        app.recorder_overlay = Some(preparing_overlay());
        app.recording_countdown.start(now, 3);
        app.pending_recorder_start = Some(now);
        assert_eq!(
            app.recorder_frame_action_at(RecorderOverlayAction::None, now),
            RecorderOverlayAction::None
        );
        assert!(app.pending_recorder_start.is_none());
    }

    #[test]
    fn closing_the_transparent_parent_stops_and_saves_instead_of_discarding() {
        let context = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let mut app = GifFromScreenApp::default();
        let (controller, _control) = RecordingController::channel();
        let (_sender, receiver) = std::sync::mpsc::channel();
        let cancellation = gif_from_screen_gif::CancellationFlag::default();
        app.job = Some(crate::RecordingJob {
            shortcut_state: LiveShortcutState::default(),
            receiver,
            controller,
            cancellation: cancellation.clone(),
            paused: false,
            pause_requested: None,
            terminal_requested: false,
            retarget: None,
            snapshot_requests: std::collections::VecDeque::default(),
        });
        let _ = context.run(input, |context| app.handle_worker_shutdown(context));
        assert_eq!(app.shutdown, ShutdownState::WaitingForWorkers);
        assert!(app.job.as_ref().unwrap().terminal_requested);
        assert!(!gif_from_screen_gif::CancellationToken::is_cancelled(
            &cancellation
        ));
    }
}
