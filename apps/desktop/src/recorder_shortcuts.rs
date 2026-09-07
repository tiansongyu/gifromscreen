//! Global input selects existing recorder actions; it owns no capture state.

use eframe::egui;
use gif_from_screen_capture_linux::{ShortcutAction, ShortcutActionHandler};
use gif_from_screen_workflow::RecordingController;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

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

#[derive(Clone, Copy, Eq, PartialEq)]
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
            match dispatch(action, &state) {
                Dispatch::None => {}
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
            if overlay.initialized {
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
}
