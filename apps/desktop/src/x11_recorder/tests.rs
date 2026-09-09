//! Coordinator regressions with explicit guide/WM observations and real workflow
//! acknowledgements. No native guide, window, capture device or host bus is opened.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use gif_from_screen_capture::{
    CaptureCadence, CaptureError, CaptureRequest, CaptureSession, CaptureSessionState,
    CaptureSourceId, CaptureTarget, FramePoll,
};
use gif_from_screen_gif::{CancellationFlag, RgbaFrame};
use gif_from_screen_workflow::{
    CollectOptions, CollectionLimit, RecordingController, RecordingFrameSink,
    RecordingFrameSinkError,
};

use crate::{RecordingRetarget, recorder_shortcuts::LiveShortcutState};

use super::*;

#[test]
fn large_handle_always_moves_instead_of_resizing_in_every_movable_stage() {
    let initial = RecorderGeometry::new(
        PhysicalRect::new(0, 0, 800, 600).unwrap(),
        PhysicalRect::new(100, 100, 320, 240).unwrap(),
    )
    .unwrap();
    let gesture = Gesture {
        id: 1,
        start: PhysicalPosition { x: 110, y: 75 },
        initial,
        edge: GuideEdge::Move,
    };
    for stage in [
        RecorderStage::Ready,
        RecorderStage::Countdown(2),
        RecorderStage::Recording,
        RecorderStage::Paused,
    ] {
        let moved = gesture_geometry(&gesture, PhysicalPosition { x: 180, y: 120 }, stage);
        assert_eq!(
            moved.region(),
            PhysicalRect::new(170, 145, 320, 240).unwrap()
        );
    }
}

#[test]
fn handle_scale_and_owned_controller_bounds_invalidate_the_guide_request() {
    assert_eq!(drag_handle_scale(1.77), 177);
    assert_eq!(drag_handle_scale(0.5), 100);
    assert_eq!(drag_handle_scale(8.0), 400);
    assert_eq!(drag_handle_scale(f32::NAN), 100);
    let mut overlay = overlay(region(), false);
    overlay.update_guide(None);
    let first = overlay.request.unwrap();
    overlay.last_scale = 2.0;
    overlay.update_guide(None);
    let scaled = overlay.request.unwrap();
    assert_eq!(scaled.generation, first.generation + 1);
    assert_eq!(scaled.handle_scale, 200);
    let controller = PhysicalRect::new(300, 200, 420, 300).unwrap();
    overlay.controller_geometry = Some(gif_from_screen_capture_linux::ControllerGeometry {
        client: controller,
        outer: controller,
        viewable: true,
    });
    overlay.update_guide(None);
    assert_eq!(overlay.request.unwrap().handle_avoid, Some(controller));
    assert_eq!(overlay.request.unwrap().generation, scaled.generation + 1);
    overlay.update_guide(None);
    assert_eq!(overlay.request.unwrap().generation, scaled.generation + 1);
}
fn source() -> PhysicalRect {
    PhysicalRect::new(0, 0, 1024, 768).unwrap()
}
fn region() -> PhysicalRect {
    PhysicalRect::new(100, 100, 100, 80).unwrap()
}
fn native() -> NativeWindow {
    let rect = PhysicalRect::new(400, 400, 420, 300).unwrap();
    NativeWindow {
        outer: Some(rect),
        client: Some(rect),
        maximized: Some(false),
        pixels_per_point: 1.0,
    }
}

fn overlay(region: PhysicalRect, hidden: bool) -> RecorderOverlay {
    let mut overlay = RecorderOverlay::new(
        RecorderGeometry::new(source(), region).unwrap(),
        vec![source()],
        None,
        1.0,
    );
    overlay.initialized = true;
    overlay.window_state = if hidden {
        WindowState::Hidden
    } else {
        WindowState::Ready {
            outer: native().outer.unwrap(),
            workarea_index: 0,
        }
    };
    overlay.update_guide(None);
    acknowledge(&mut overlay);
    overlay
}

fn acknowledge(overlay: &mut RecorderOverlay) {
    overlay.acknowledged = overlay.request.map(|request| request.generation);
}

fn frame<R>(
    context: &egui::Context,
    minimized: Option<bool>,
    mut draw: impl FnMut(&egui::Context) -> R,
) -> (egui::FullOutput, R) {
    frame_events(context, minimized, Vec::new(), &mut draw)
}

fn frame_events<R>(
    context: &egui::Context,
    minimized: Option<bool>,
    events: Vec<egui::Event>,
    mut draw: impl FnMut(&egui::Context) -> R,
) -> (egui::FullOutput, R) {
    let size = egui::vec2(420.0, 300.0);
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        events,
        ..Default::default()
    };
    let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    let rect = egui::Rect::from_min_size(egui::pos2(400.0, 400.0), size);
    viewport.inner_rect = Some(rect);
    viewport.outer_rect = Some(rect);
    viewport.maximized = Some(false);
    viewport.minimized = minimized;
    let mut result = None;
    let output = context.run(input, |context| {
        result = Some(draw(context));
    });
    (output, result.unwrap())
}

fn commands(output: &egui::FullOutput) -> &[egui::ViewportCommand] {
    &output.viewport_output[&egui::ViewportId::ROOT].commands
}

#[test]
fn equal_protection_normalizes_without_new_generation_or_ack_loss() {
    let mut overlay = overlay(region(), false);
    let before = overlay.request.unwrap();
    overlay.update_guide(Some(region()));
    assert_eq!(overlay.request, Some(before));
    assert!(overlay.ready());
    let extra = PhysicalRect::new(80, 100, 100, 80).unwrap();
    overlay.update_guide(Some(extra));
    assert_eq!(overlay.request.unwrap().generation, before.generation + 1);
    assert_eq!(overlay.acknowledged, None);
    acknowledge(&mut overlay);
    overlay.update_guide(Some(extra));
    assert!(overlay.ready());
    overlay.update_guide(Some(region()));
    assert_eq!(overlay.request.unwrap().protected_region, None);
    assert_eq!(overlay.request.unwrap().generation, before.generation + 2);
    assert!(!overlay.ready());
}

#[test]
fn iconic_composited_windows_can_remain_x11_viewable() {
    let context = egui::Context::default();
    let mut overlay = overlay(source(), true);
    overlay.controller_geometry = Some(gif_from_screen_capture_linux::ControllerGeometry {
        client: region(),
        outer: region(),
        viewable: false,
    });
    assert!(!frame(&context, Some(false), RecorderOverlay::is_hidden).1);
    overlay.controller_geometry.as_mut().unwrap().viewable = true;
    assert!(frame(&context, Some(true), RecorderOverlay::is_hidden).1);
}

#[test]
fn hidden_settle_needs_elapsed_time_real_app_frames_and_native_minimized_ack() {
    let context = egui::Context::default();
    let mut overlay = overlay(source(), true);
    let now = Instant::now();
    assert!(
        !frame(&context, Some(false), |context| overlay
            .settled(context, now, true))
        .1
    );
    // Enough elapsed time, but only one completed application frame.
    assert!(
        !frame(&context, Some(false), |context| overlay.settled(
            context,
            now + PRESENTATION_SETTLE,
            true
        ))
        .1
    );
    // Enough frames, but the timestamp still has not reached the settle budget.
    assert!(
        !frame(&context, Some(false), |context| overlay
            .settled(context, now, true))
        .1
    );
    let (output, settled) = frame(&context, Some(false), |context| {
        overlay.settled(context, now + PRESENTATION_SETTLE, true)
    });
    assert!(!settled);
    assert!(commands(&output).contains(&egui::ViewportCommand::Minimized(true)));
    for minimized in [None, Some(false)] {
        let (output, settled) = frame(&context, minimized, |context| {
            overlay.settled(context, now + PRESENTATION_SETTLE, true)
        });
        assert!(!settled, "sending minimize is not an acknowledgement");
        assert!(!commands(&output).contains(&egui::ViewportCommand::Minimized(true)));
    }
    for (elapsed, expected) in [
        (PRESENTATION_SETTLE, true),
        (PRESENTATION_SETTLE * 2, true),
        (PRESENTATION_SETTLE * 2, true),
    ] {
        assert_eq!(
            frame(&context, Some(true), |context| overlay.settled(
                context,
                now + elapsed,
                true
            ))
            .1,
            expected
        );
    }
}

#[test]
fn guide_change_discards_old_settle_and_stale_ack_cannot_ready_new_geometry() {
    let context = egui::Context::default();
    let mut overlay = overlay(region(), false);
    let now = Instant::now();
    frame(&context, Some(false), |context| {
        overlay.settled(context, now, false)
    });
    let old_generation = overlay.request.unwrap().generation;
    overlay.geometry.move_by(10, 0);
    overlay.update_guide(None);
    overlay.acknowledged = Some(old_generation);
    assert!(
        !frame(&context, Some(false), |context| overlay.settled(
            context,
            now + Duration::from_secs(1),
            false
        ))
        .1
    );
    assert!(overlay.settle.is_none());
    acknowledge(&mut overlay);
    assert!(
        !frame(&context, Some(false), |context| overlay.settled(
            context,
            now + Duration::from_secs(1),
            false
        ))
        .1
    );
    assert_eq!(
        overlay.settle.as_ref().unwrap().region,
        overlay.geometry.region()
    );
}

#[test]
fn pending_start_cancel_and_failed_settle_restore_interactive_ready_geometry() {
    let context = egui::Context::default();
    let mut overlay = overlay(source(), true);
    let now = Instant::now();
    overlay.pending_live_start = Some(now);
    overlay.was_minimized = true;
    frame(&context, Some(false), |context| {
        overlay.settled(context, now, true)
    });
    assert!(
        !frame(&context, Some(false), |context| overlay.settled(
            context,
            now + CHANGE_TIMEOUT,
            true
        ))
        .1
    );
    assert!(overlay.failed());
    let (output, ()) = frame(&context, Some(true), |context| {
        overlay.cancel_start(context);
    });
    assert!(overlay.pending_live_start.is_none() && overlay.settle.is_none());
    assert!(!overlay.was_minimized);
    assert!(commands(&output).contains(&egui::ViewportCommand::Minimized(false)));
    assert!(commands(&output).contains(&egui::ViewportCommand::MousePassthrough(false)));
    assert!(!overlay.geometry.size_is_frozen());
    overlay
        .geometry
        .resize(PhysicalSize::new(16, 16).unwrap())
        .unwrap();
}

#[test]
fn native_start_validation_failure_keeps_ready_selection_resizable() {
    let context = egui::Context::default();
    let mut app = GifFromScreenApp::default();
    assert!(
        app.sources.is_empty(),
        "this fixture must fail before any native backend can start"
    );
    let mut overlay = overlay(region(), false);
    let now = Instant::now();
    for _ in 0..3 {
        frame(&context, Some(false), |context| {
            overlay.place(context, native(), now);
        });
    }
    assert!(overlay.ready());
    overlay.pending_live_start = Some(now);
    overlay.settle = Some(Settling {
        region: region(),
        frame: 0,
        started: now.checked_sub(PRESENTATION_SETTLE).unwrap(),
        hide: HidePhase::Visible,
    });
    app.recorder_overlay = Some(overlay);
    let (output, ()) = frame(&context, Some(false), |context| {
        app.show_recorder_overlay(context);
    });
    assert!(app.job.is_none());
    assert!(
        app.notice
            .as_deref()
            .unwrap()
            .contains("No X11 capture source")
    );
    assert_eq!(app.recorder_stage(), RecorderStage::Ready);
    let overlay = app.recorder_overlay.as_mut().unwrap();
    assert!(overlay.pending_live_start.is_none());
    assert!(!overlay.geometry.size_is_frozen());
    overlay
        .geometry
        .resize(PhysicalSize::new(16, 16).unwrap())
        .unwrap();
    assert!(commands(&output).contains(&egui::ViewportCommand::Minimized(false)));
    assert!(commands(&output).contains(&egui::ViewportCommand::MousePassthrough(false)));
}

#[test]
fn corner_resize_is_ready_only_and_every_live_gesture_keeps_its_canvas() {
    let initial = RecorderGeometry::new(source(), region()).unwrap();
    let corner = Gesture {
        id: 1,
        start: PhysicalPosition { x: 100, y: 100 },
        initial,
        edge: GuideEdge::TopLeft,
    };
    let position = PhysicalPosition { x: 90, y: 95 };
    let resized = gesture_geometry(&corner, position, RecorderStage::Ready);
    assert_eq!(
        resized.region(),
        PhysicalRect::new(90, 95, 110, 85).unwrap()
    );
    for stage in [
        RecorderStage::Recording,
        RecorderStage::Paused,
        RecorderStage::Countdown(2),
    ] {
        let moved = gesture_geometry(&corner, position, stage);
        assert_eq!(moved.region(), PhysicalRect::new(90, 95, 100, 80).unwrap());
    }
    let mut frozen = initial;
    frozen.freeze_size();
    let frozen_gesture = Gesture {
        initial: frozen,
        ..corner
    };
    let moved = gesture_geometry(&frozen_gesture, position, RecorderStage::Ready);
    assert_eq!(moved.region().size(), initial.region().size());
    assert!(moved.size_is_frozen());
    let edge = Gesture {
        edge: GuideEdge::Top,
        initial,
        ..frozen_gesture
    };
    assert_eq!(
        gesture_geometry(&edge, position, RecorderStage::Ready)
            .region()
            .size(),
        initial.region().size()
    );
}

#[derive(Default)]
struct NativeStats {
    resumes: usize,
    targets: Vec<CaptureTarget>,
}

struct TestSession {
    request: CaptureRequest,
    state: CaptureSessionState,
    stats: Arc<Mutex<NativeStats>>,
}
impl CaptureSession for TestSession {
    fn state(&self) -> CaptureSessionState {
        self.state
    }
    fn request(&self) -> &CaptureRequest {
        &self.request
    }
    fn update_target(&mut self, target: CaptureTarget) -> Result<(), CaptureError> {
        self.stats.lock().unwrap().targets.push(target.clone());
        self.request.target = target;
        Ok(())
    }
    fn pause(&mut self) -> Result<(), CaptureError> {
        self.state = CaptureSessionState::Paused;
        Ok(())
    }
    fn resume(&mut self) -> Result<(), CaptureError> {
        self.stats.lock().unwrap().resumes += 1;
        self.state = CaptureSessionState::Recording;
        Ok(())
    }
    fn stop(&mut self) -> Result<(), CaptureError> {
        self.state = CaptureSessionState::Stopped;
        Ok(())
    }
    fn discard(&mut self) -> Result<(), CaptureError> {
        self.state = CaptureSessionState::Discarded;
        Ok(())
    }
    fn poll_frame(&mut self, timeout: Duration) -> Result<FramePoll, CaptureError> {
        thread::sleep(timeout.min(Duration::from_millis(1)));
        Ok(FramePoll::Pending)
    }
}

struct NoFrames;
impl RecordingFrameSink for NoFrames {
    fn append_provisional_frame(
        &mut self,
        _: u64,
        _: &RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        panic!("no frame-producing fixture")
    }
    fn update_frame_duration(&mut self, _: u64, _: u64) -> Result<(), RecordingFrameSinkError> {
        panic!("no frame-producing fixture")
    }
}

struct Worker {
    cancel: CancellationFlag,
    join: Option<thread::JoinHandle<()>>,
    stats: Arc<Mutex<NativeStats>>,
    _messages: mpsc::Sender<crate::JobMessage>,
}
impl Worker {
    fn start(initial: PhysicalRect) -> (RecordingJob, Self) {
        let (controller, mut control) = RecordingController::channel();
        let cancel = CancellationFlag::default();
        let cancellation = cancel.clone();
        let stats = Arc::new(Mutex::new(NativeStats::default()));
        let native_log = Arc::clone(&stats);
        let source = CaptureSourceId::new("synthetic-coordinator").unwrap();
        let target = CaptureTarget::Region {
            source: source.clone(),
            region: initial,
        };
        let join = thread::spawn(move || {
            let mut session = TestSession {
                request: CaptureRequest::new(target, CaptureCadence::Manual),
                state: CaptureSessionState::Recording,
                stats: native_log,
            };
            let options = CollectOptions {
                limit: CollectionLimit::UntilStopped,
                poll_interval: Duration::from_millis(1),
                ..Default::default()
            };
            let _ = gif_from_screen_workflow::collect_prestarted_controlled_to_sink(
                &mut session,
                &options,
                &mut control,
                &mut NoFrames,
                &cancellation,
                &mut gif_from_screen_workflow::NoopWorkflowProgress,
            );
        });
        let (messages, receiver) = mpsc::channel();
        let job = RecordingJob {
            shortcut_state: LiveShortcutState::default(),
            receiver,
            controller,
            cancellation: cancel.clone(),
            paused: false,
            pause_requested: None,
            terminal_requested: false,
            retarget: Some(RecordingRetarget::new(source, initial)),
            snapshot_requests: VecDeque::new(),
        };
        (
            job,
            Self {
                cancel,
                join: Some(join),
                stats,
                _messages: messages,
            },
        )
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "real workflow acknowledgement did not arrive"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn final_extra_exclusion_cleanup_ack_is_required_before_real_workflow_resume() {
    let context = egui::Context::default();
    let old = region();
    let (mut job, worker) = Worker::start(old);
    let mut overlay = overlay(old, false);
    overlay.geometry.freeze_size();
    overlay.geometry.move_by(10, 0);
    let moved_region = overlay.geometry.region();
    let now = Instant::now();
    begin_change(&mut overlay, &job, now);
    assert!(!prepare_change(&mut overlay, Some(&mut job), native(), now));
    until(|| job.controller.pause_status() == (0, Some(true)));
    assert!(prepare_change(&mut overlay, Some(&mut job), native(), now));
    overlay.update_guide(Some(old));
    acknowledge(&mut overlay);
    let extra_generation = overlay.request.unwrap().generation;
    let mut notice = None;
    for offset in [0, 110, 120] {
        frame(&context, Some(false), |context| {
            finish_change(
                &mut overlay,
                Some(&mut job),
                context,
                now + Duration::from_millis(offset),
                false,
                &mut notice,
            );
        });
    }
    until(|| {
        let retarget = job.retarget.as_mut().unwrap();
        retarget.plan.applied() == moved_region
            || retarget.pending.as_mut().is_some_and(|pending| {
                pending.status() == gif_from_screen_workflow::TargetUpdateStatus::Applied
            })
    });
    frame(&context, Some(false), |context| {
        finish_change(
            &mut overlay,
            Some(&mut job),
            context,
            now + Duration::from_millis(130),
            false,
            &mut notice,
        );
    });
    assert_eq!(job.retarget.as_ref().unwrap().plan.applied(), moved_region);
    assert_eq!(overlay.request.unwrap().protected_region, None);
    assert_eq!(overlay.request.unwrap().generation, extra_generation + 1);
    assert_eq!(overlay.acknowledged, None);
    for offset in [300, 400, 500] {
        frame(&context, Some(false), |context| {
            finish_change(
                &mut overlay,
                Some(&mut job),
                context,
                now + Duration::from_millis(offset),
                false,
                &mut notice,
            );
        });
        assert_eq!(job.controller.pause_status(), (0, Some(true)));
        assert_eq!(worker.stats.lock().unwrap().resumes, 0);
    }
    assert!(overlay.change.is_some());
    acknowledge(&mut overlay);
    for offset in [600, 710, 720] {
        frame(&context, Some(false), |context| {
            finish_change(
                &mut overlay,
                Some(&mut job),
                context,
                now + Duration::from_millis(offset),
                false,
                &mut notice,
            );
        });
    }
    until(|| job.controller.pause_status() == (0, Some(false)));
    assert!(overlay.change.is_none());
    assert_eq!(worker.stats.lock().unwrap().resumes, 1);
    let final_generation = overlay.request.unwrap().generation;
    overlay.update_guide(Some(moved_region));
    assert_eq!(
        overlay.request.unwrap().generation,
        final_generation,
        "first resumed polling must not reconfigure the guide"
    );
}

#[test]
fn explicit_pause_intent_survives_default_resume_sampling_after_pending_controls() {
    let context = egui::Context::default();
    let (job, _worker) = Worker::start(region());
    let mut app = GifFromScreenApp::default();
    app.job = Some(job);
    let mut overlay = overlay(region(), false);
    overlay.geometry.move_by(1, 0);
    let now = Instant::now();
    begin_change(&mut overlay, app.job.as_ref().unwrap(), now);
    app.recorder_overlay = Some(overlay);
    assert!(app.handle_geometry_action(&context, RecorderOverlayAction::Pause));
    let mut overlay = app.recorder_overlay.take().unwrap();
    assert!(!prepare_change(
        &mut overlay,
        app.job.as_mut(),
        native(),
        now
    ));
    until(|| app.job.as_ref().unwrap().controller.pause_status() == (0, Some(true)));
    assert!(prepare_change(
        &mut overlay,
        app.job.as_mut(),
        native(),
        now
    ));
    assert!(overlay.change.as_ref().unwrap().resume);
    assert!(
        !overlay.change.as_ref().unwrap().resume_intent(),
        "native default cannot replace explicit Pause"
    );
    app.recorder_overlay = Some(overlay);
    assert!(app.handle_geometry_action(&context, RecorderOverlayAction::Resume));
    assert!(
        app.recorder_overlay
            .as_ref()
            .unwrap()
            .change
            .as_ref()
            .unwrap()
            .resume_intent()
    );
}

#[test]
fn effective_geometry_progress_refreshes_deadline_but_unchanged_stall_expires() {
    let (job, _worker) = Worker::start(region());
    let mut overlay = overlay(region(), false);
    let now = Instant::now();
    begin_change(&mut overlay, &job, now);
    for index in 1..=3 {
        overlay.geometry.move_by(1, 0);
        let progress = now + Duration::from_secs(index * 3);
        begin_change(&mut overlay, &job, progress);
        assert_eq!(overlay.change.as_ref().unwrap().started, progress);
    }
    let progress = overlay.change.as_ref().unwrap().started;
    begin_change(&mut overlay, &job, progress + Duration::from_secs(1));
    assert_eq!(overlay.change.as_ref().unwrap().started, progress);
    let mut job = job;
    assert!(!prepare_change(
        &mut overlay,
        Some(&mut job),
        native(),
        progress + CHANGE_TIMEOUT
    ));
    assert!(overlay.failed());
}

#[test]
fn global_toggle_parity_and_explicit_geometry_intents_share_the_actual_live_router() {
    use gif_from_screen_capture_linux::ShortcutAction;
    let context = egui::Context::default();
    let (mut job, worker) = Worker::start(region());
    assert!(job.controller.pause());
    until(|| job.controller.pause_status() == (0, Some(true)));
    job.paused = true;
    let mut overlay = overlay(region(), false);
    begin_change(&mut overlay, &job, Instant::now());
    let handler = job.shortcut_state.handler(job.controller.clone(), true);
    let mut app = GifFromScreenApp::default();
    app.job = Some(job);
    app.recorder_overlay = Some(overlay);
    let wants_resume = |app: &GifFromScreenApp| {
        let base = app
            .recorder_overlay
            .as_ref()
            .unwrap()
            .change
            .as_ref()
            .unwrap()
            .resume_intent();
        app.job
            .as_ref()
            .unwrap()
            .shortcut_state
            .geometry_wants_resume(base)
    };
    assert!(handler(ShortcutAction::StartPause));
    assert!(wants_resume(&app));
    assert!(app.handle_geometry_action(&context, RecorderOverlayAction::Pause));
    assert!(
        !wants_resume(&app),
        "explicit Pause supersedes older queued toggle parity"
    );
    assert!(handler(ShortcutAction::StartPause));
    assert!(wants_resume(&app));
    assert!(app.handle_geometry_action(&context, RecorderOverlayAction::Resume));
    assert!(
        wants_resume(&app),
        "explicit Resume replaces older parity instead of toggling twice"
    );
    assert!(handler(ShortcutAction::StartPause));
    assert!(!wants_resume(&app));
    assert!(handler(ShortcutAction::Snapshot));
    let job = app.job.as_ref().unwrap();
    assert_eq!(
        job.controller.pause_status(),
        (0, Some(true)),
        "route-changing presses must not bypass coordinator acknowledgements"
    );
    let intent = app
        .recorder_overlay
        .as_ref()
        .unwrap()
        .change
        .as_ref()
        .unwrap()
        .resume_intent();
    assert_eq!(
        job.shortcut_state
            .finish_geometry_change(&job.controller, intent),
        1
    );
    assert_eq!(job.controller.pause_status(), (0, Some(true)));
    assert_eq!(worker.stats.lock().unwrap().resumes, 0);
}

#[test]
fn recovered_full_root_resume_button_accepts_intent_without_premature_native_resume() {
    let context = egui::Context::default();
    let (mut job, worker) = Worker::start(source());
    assert!(job.controller.pause());
    until(|| job.controller.pause_status() == (0, Some(true)));
    job.paused = true;
    let mut overlay = overlay(source(), true);
    overlay.geometry.freeze_size();
    overlay.recovering = true;
    begin_change(&mut overlay, &job, Instant::now());
    let mut app = GifFromScreenApp::default();
    app.job = Some(job);
    let draw =
        |context: &egui::Context, app: &mut GifFromScreenApp, overlay: &mut RecorderOverlay| {
            app.draw_x11_controls(context, overlay, RecorderStage::Paused, true, true)
        };
    frame(&context, Some(false), |context| {
        draw(context, &mut app, &mut overlay)
    });
    let (output, _) = frame(&context, Some(false), |context| {
        draw(context, &mut app, &mut overlay)
    });
    let resume = output
        .shapes
        .iter()
        .find_map(|shape| {
            let egui::Shape::Text(text) = &shape.shape else {
                return None;
            };
            if text.galley.text() != "Resume" {
                return None;
            }
            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            assert!(shape.clip_rect.contains_rect(rect));
            Some(rect.center())
        })
        .expect("recovery must offer a visible Resume button");
    let mut action = RecorderOverlayAction::None;
    for pressed in [true, false] {
        action = frame_events(
            &context,
            Some(false),
            vec![
                egui::Event::PointerMoved(resume),
                egui::Event::PointerButton {
                    pos: resume,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            |context| draw(context, &mut app, &mut overlay),
        )
        .1;
    }
    assert_eq!(action, RecorderOverlayAction::Resume);
    app.recorder_overlay = Some(overlay);
    assert!(app.handle_geometry_action(&context, action));
    assert!(
        app.recorder_overlay
            .as_ref()
            .unwrap()
            .change
            .as_ref()
            .unwrap()
            .resume_intent()
    );
    assert_eq!(
        app.job.as_ref().unwrap().controller.pause_status(),
        (0, Some(true))
    );
    assert_eq!(worker.stats.lock().unwrap().resumes, 0);
}
