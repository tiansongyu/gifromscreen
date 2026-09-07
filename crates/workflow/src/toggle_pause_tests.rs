use std::{sync::Barrier, time::Duration};

use gif_from_screen_capture::{
    CaptureBackend, CaptureRequest, CaptureSourceId, FramePoll, PhysicalSize, PixelFormat,
    SyntheticCaptureBackend,
};

use super::*;

struct Session {
    inner: Box<dyn CaptureSession>,
    observed: Vec<&'static str>,
    fail: Option<&'static str>,
    during_transition: Option<RecordingController>,
    override_state: Option<CaptureSessionState>,
}

impl Session {
    fn new() -> Self {
        let frames = vec![
            CapturedFrame::new(
                1,
                CaptureTimestamp::from_micros(100),
                PhysicalSize::new(1, 1).unwrap(),
                4,
                PixelFormat::Rgba8,
                vec![1, 2, 3, 255],
            )
            .unwrap(),
        ];
        let backend = SyntheticCaptureBackend::new(frames);
        Self {
            inner: backend
                .start_session(CaptureRequest::new(
                    CaptureTarget::Monitor(CaptureSourceId::new("synthetic:monitor:0").unwrap()),
                    CaptureCadence::Manual,
                ))
                .unwrap(),
            observed: Vec::new(),
            fail: None,
            during_transition: None,
            override_state: None,
        }
    }

    fn transition(&mut self, command: &'static str) -> Result<(), CaptureError> {
        self.observed.push(command);
        if let Some(controller) = &self.during_transition {
            assert!(
                !controller.toggle_pause(),
                "slot must stay occupied during native transition"
            );
        }
        if self.fail == Some(command) {
            Err(CaptureError::new(
                CaptureErrorKind::BackendUnavailable,
                "mock native transition failure",
                RecoveryHint::None,
            ))
        } else {
            Ok(())
        }
    }
}

impl CaptureSession for Session {
    fn state(&self) -> CaptureSessionState {
        self.override_state.unwrap_or_else(|| self.inner.state())
    }
    fn request(&self) -> &CaptureRequest {
        self.inner.request()
    }
    fn update_target(&mut self, target: CaptureTarget) -> Result<(), CaptureError> {
        self.inner.update_target(target)
    }
    fn prepare_snapshot(&mut self) -> Result<(), CaptureError> {
        self.observed.push("snapshot");
        self.inner.prepare_snapshot()
    }
    fn pause(&mut self) -> Result<(), CaptureError> {
        self.transition("pause")?;
        self.inner.pause()
    }
    fn resume(&mut self) -> Result<(), CaptureError> {
        self.transition("resume")?;
        self.inner.resume()
    }
    fn stop(&mut self) -> Result<(), CaptureError> {
        self.observed.push("stop");
        self.inner.stop()
    }
    fn discard(&mut self) -> Result<(), CaptureError> {
        self.observed.push("discard");
        self.inner.discard()
    }
    fn poll_frame(&mut self, timeout: Duration) -> Result<FramePoll, CaptureError> {
        self.inner.poll_frame(timeout)
    }
}

#[test]
fn toggle_uses_actual_session_state_and_pause_does_not_consume_frames() {
    let (controller, mut control) = RecordingController::channel();
    let mut session = Session::new();
    assert!(controller.toggle_pause());
    assert_eq!(
        control.apply_pending(&mut session).unwrap(),
        ControlOutcome::Continue
    );
    assert_eq!(session.state(), CaptureSessionState::Paused);
    assert!(matches!(
        session.poll_frame(Duration::ZERO).unwrap(),
        FramePoll::Pending
    ));
    assert!(controller.toggle_pause());
    control.apply_pending(&mut session).unwrap();
    assert_eq!(session.state(), CaptureSessionState::Recording);
    let FramePoll::Frame(frame) = session.poll_frame(Duration::ZERO).unwrap() else {
        panic!("pause must preserve the next frame");
    };
    assert_eq!(frame.sequence(), 1);
    assert_eq!(session.observed, ["pause", "resume"]);
}

#[test]
fn cloned_concurrent_callers_share_exactly_one_queued_toggle_slot() {
    let (controller, mut control) = RecordingController::channel();
    let barrier = Arc::new(Barrier::new(16));
    let accepted = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let controller = controller.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    usize::from(controller.toggle_pause())
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum::<usize>()
    });
    assert_eq!(accepted, 1);
    let mut session = Session::new();
    control.apply_pending(&mut session).unwrap();
    assert_eq!(session.observed, ["pause"]);
    assert!(controller.toggle_pause());
    control.apply_pending(&mut session).unwrap();
    assert_eq!(session.observed, ["pause", "resume"]);
}

#[test]
fn inflight_toggle_is_not_released_until_native_pause_or_resume_returns() {
    let (controller, mut control) = RecordingController::channel();
    let mut session = Session::new();
    session.during_transition = Some(controller.clone());
    for expected in [CaptureSessionState::Paused, CaptureSessionState::Recording] {
        assert!(controller.toggle_pause());
        assert!(!controller.toggle_pause());
        control.apply_pending(&mut session).unwrap();
        assert_eq!(session.state(), expected);
        assert_eq!(
            controller.pause_status(),
            (0, Some(expected == CaptureSessionState::Paused))
        );
    }
}

#[test]
fn native_errors_propagate_and_release_the_toggle_slot() {
    for command in ["pause", "resume"] {
        let (controller, mut control) = RecordingController::channel();
        let mut session = Session::new();
        if command == "resume" {
            session.inner.pause().unwrap();
        }
        session.fail = Some(command);
        session.during_transition = Some(controller.clone());
        assert!(controller.toggle_pause());
        assert!(
            control
                .apply_pending(&mut session)
                .unwrap_err()
                .to_string()
                .contains("mock native transition failure")
        );
        assert_eq!(controller.pause_status(), (0, None));
        assert_eq!(session.observed, [command]);
        session.fail = None;
        assert!(controller.toggle_pause());
        control.apply_pending(&mut session).unwrap();
        assert_eq!(session.observed, [command, command]);
    }
}

#[test]
fn explicit_pause_resume_and_toggle_keep_send_order_without_ui_state_guesses() {
    let (controller, mut control) = RecordingController::channel();
    let mut session = Session::new();
    assert!(controller.pause());
    assert!(controller.toggle_pause());
    assert!(controller.pause());
    control.apply_pending(&mut session).unwrap();
    assert_eq!(session.observed, ["pause", "resume", "pause"]);
    assert_eq!(session.state(), CaptureSessionState::Paused);
    assert!(controller.resume());
    assert!(controller.toggle_pause());
    assert!(controller.resume());
    control.apply_pending(&mut session).unwrap();
    assert_eq!(
        session.observed,
        ["pause", "resume", "pause", "resume", "pause", "resume"]
    );
    assert_eq!(session.state(), CaptureSessionState::Recording);
}

#[test]
fn terminal_commands_block_later_toggles_and_release_their_slot() {
    for discard in [false, true] {
        for terminal_first in [false, true] {
            let (controller, mut control) = RecordingController::channel();
            let mut session = Session::new();
            if !terminal_first {
                assert!(controller.toggle_pause());
            }
            assert!(if discard {
                controller.discard()
            } else {
                controller.stop()
            });
            if terminal_first {
                assert!(controller.toggle_pause());
            }
            assert_eq!(
                control.apply_pending(&mut session).unwrap(),
                if discard {
                    ControlOutcome::Discard
                } else {
                    ControlOutcome::Stop
                }
            );
            assert_eq!(
                session.state(),
                if discard {
                    CaptureSessionState::Discarded
                } else {
                    CaptureSessionState::Stopped
                }
            );
            assert_eq!(controller.pause_status().0, 0);
            let calls = session.observed.clone();
            assert!(controller.toggle_pause());
            control.apply_pending(&mut session).unwrap();
            assert_eq!(
                session.observed, calls,
                "terminal session cannot be resumed by a later toggle"
            );
        }
    }
}

#[test]
fn nonrecording_states_ignore_toggle_without_invoking_native_transitions() {
    for state in [
        CaptureSessionState::Starting,
        CaptureSessionState::Stopping,
        CaptureSessionState::Stopped,
        CaptureSessionState::Discarded,
        CaptureSessionState::Failed,
    ] {
        let (controller, mut control) = RecordingController::channel();
        let mut session = Session::new();
        session.override_state = Some(state);
        assert!(controller.toggle_pause());
        control.apply_pending(&mut session).unwrap();
        assert!(session.observed.is_empty());
        assert_eq!(controller.pause_status(), (0, None));
    }
}

#[test]
fn worker_exit_send_failure_and_error_queue_drain_release_pending_slots() {
    let (controller, control) = RecordingController::channel();
    assert!(controller.toggle_pause());
    drop(control);
    assert_eq!(controller.pause_status(), (0, None));
    for _ in 0..3 {
        assert!(!controller.toggle_pause());
        assert_eq!(controller.pause_status(), (0, None));
    }
    let (controller, mut control) = RecordingController::channel();
    let mut session = Session::new();
    session.fail = Some("pause");
    assert!(controller.pause());
    assert!(controller.toggle_pause());
    assert!(control.apply_pending(&mut session).is_err());
    // Collection's existing error cleanup drains every queued command as well
    // as snapshots; it must drop a toggle left behind a failed earlier command.
    control.reject_all_snapshots(&SnapshotTriggerRejection::Cancelled);
    assert_eq!(controller.pause_status(), (0, None));
}

#[test]
fn toggle_pause_disarms_pending_manual_snapshot_and_resume_rearms_fresh_boundary() {
    let (controller, mut control) = RecordingController::channel();
    let mut session = Session::new();
    let mut snapshot = controller.trigger_snapshot();
    control.apply_pending(&mut session).unwrap();
    assert!(control.snapshot_armed);
    assert!(controller.toggle_pause());
    control.apply_pending(&mut session).unwrap();
    assert!(!control.snapshot_armed);
    assert_eq!(snapshot.status(), SnapshotTriggerStatus::Pending);
    assert!(controller.toggle_pause());
    control.apply_pending(&mut session).unwrap();
    assert!(control.snapshot_armed);
    assert_eq!(
        session.observed,
        ["snapshot", "pause", "resume", "snapshot"]
    );
    assert!(controller.stop());
    control.apply_pending(&mut session).unwrap();
    assert_eq!(
        snapshot.status(),
        SnapshotTriggerStatus::Rejected(SnapshotTriggerRejection::Stopped)
    );
}

#[test]
fn same_batch_pause_then_toggle_publishes_final_native_recording_ack_without_progress() {
    let (controller, mut control) = RecordingController::channel();
    let mut session = Session::new();
    assert_eq!(controller.pause_status(), (0, None));
    assert!(controller.pause());
    assert!(controller.toggle_pause());
    assert_eq!(controller.pause_status(), (2, None));
    control.apply_pending(&mut session).unwrap();
    assert_eq!(session.observed, ["pause", "resume"]);
    assert_eq!(controller.pause_status(), (0, Some(false)));
    assert_eq!(controller.clone().pause_status(), (0, Some(false)));

    assert!(controller.toggle_pause());
    assert!(controller.pause());
    assert_eq!(controller.pause_status(), (2, Some(false)));
    control.apply_pending(&mut session).unwrap();
    assert_eq!(session.observed, ["pause", "resume", "pause"]);
    assert_eq!(
        controller.pause_status(),
        (0, Some(true)),
        "already-paused explicit Pause is an acknowledged no-op"
    );
}

#[test]
fn native_ack_stays_known_but_not_safely_paused_while_any_resume_is_pending() {
    let (controller, mut control) = RecordingController::channel();
    let mut session = Session::new();
    assert!(controller.pause());
    control.apply_pending(&mut session).unwrap();
    assert_eq!(controller.pause_status(), (0, Some(true)));
    assert!(controller.resume());
    assert!(controller.resume());
    assert_eq!(controller.pause_status(), (2, Some(true)));
    control.apply_pending(&mut session).unwrap();
    assert_eq!(controller.pause_status(), (0, Some(false)));
    assert_eq!(session.observed, ["pause", "resume"]);
}

#[test]
fn explicit_native_failure_clears_prior_ack_and_queue_drop_never_forges_success() {
    let (controller, mut control) = RecordingController::channel();
    let mut session = Session::new();
    assert!(controller.pause());
    control.apply_pending(&mut session).unwrap();
    assert_eq!(controller.pause_status(), (0, Some(true)));
    session.fail = Some("resume");
    assert!(controller.resume());
    assert!(control.apply_pending(&mut session).is_err());
    assert_eq!(
        controller.pause_status(),
        (0, None),
        "failed resume must not publish stale privacy-safe Paused"
    );
    assert!(controller.resume());
    assert!(controller.pause());
    assert!(controller.toggle_pause());
    assert_eq!(controller.pause_status(), (3, None));
    drop(control);
    assert_eq!(controller.pause_status(), (0, None));
    assert!(!controller.pause());
    assert!(!controller.resume());
    assert!(!controller.toggle_pause());
    assert_eq!(controller.pause_status(), (0, None));
}

#[test]
fn terminal_noop_pause_commands_release_counts_and_do_not_acknowledge_paused() {
    for discard in [false, true] {
        let (controller, mut control) = RecordingController::channel();
        let mut session = Session::new();
        assert!(controller.pause());
        control.apply_pending(&mut session).unwrap();
        assert!(if discard {
            controller.discard()
        } else {
            controller.stop()
        });
        assert!(controller.pause());
        assert!(controller.resume());
        assert!(controller.toggle_pause());
        assert_eq!(controller.pause_status(), (3, Some(true)));
        control.apply_pending(&mut session).unwrap();
        assert_eq!(controller.pause_status(), (0, None));
    }
}
