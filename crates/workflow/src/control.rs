use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use gif_from_screen_capture::{
    CaptureError, CaptureErrorKind, CaptureSession, CaptureSessionState, CaptureTarget,
    RecoveryHint,
};

use crate::WorkflowError;

#[derive(Debug)]
enum RecordingCommand {
    Pause,
    Resume,
    UpdateTarget {
        target: CaptureTarget,
        completion: Sender<Result<(), CaptureError>>,
    },
    Stop,
    Discard,
}

/// A clonable handle used by UI or hotkey threads to control an active recording.
#[derive(Clone, Debug)]
pub struct RecordingController {
    sender: Sender<RecordingCommand>,
    dispatch: Arc<Mutex<()>>,
}

/// The worker-side command receiver for one controlled recording.
#[derive(Debug)]
pub struct RecordingControl {
    receiver: Receiver<RecordingCommand>,
}

/// Current worker-side state of one target update request.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TargetUpdateStatus {
    /// The request is queued and has not been applied by the capture worker yet.
    Pending,
    /// The worker accepted the target for subsequent frames.
    Applied,
    /// The backend rejected the target and kept the preceding target active.
    Rejected(CaptureError),
    /// The recording worker exited before it could process the request.
    WorkerExited,
}

/// A queryable acknowledgement for an asynchronous target update.
#[derive(Debug)]
pub struct TargetUpdateRequest {
    target: CaptureTarget,
    receiver: Receiver<Result<(), CaptureError>>,
    status: TargetUpdateStatus,
}

impl TargetUpdateRequest {
    /// Returns the target carried by this request.
    pub const fn target(&self) -> &CaptureTarget {
        &self.target
    }

    /// Polls and returns the latest status without blocking the caller.
    ///
    /// Terminal results are cached, so this method can be called repeatedly.
    pub fn status(&mut self) -> TargetUpdateStatus {
        if self.status != TargetUpdateStatus::Pending {
            return self.status.clone();
        }
        match self.receiver.try_recv() {
            Ok(Ok(())) => self.status = TargetUpdateStatus::Applied,
            Ok(Err(error)) => self.status = TargetUpdateStatus::Rejected(error),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.status = TargetUpdateStatus::WorkerExited,
        }
        self.status.clone()
    }
}

/// Outcome after applying all currently queued recording commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlOutcome {
    Continue,
    Stop,
    Discard,
}

impl RecordingController {
    /// Creates a controller and the corresponding worker-side receiver.
    pub fn channel() -> (Self, RecordingControl) {
        let (sender, receiver) = mpsc::channel();
        (
            Self {
                sender,
                dispatch: Arc::new(Mutex::new(())),
            },
            RecordingControl { receiver },
        )
    }

    /// Requests that the active capture session pause.
    ///
    /// Returns `false` when the recording worker has already exited.
    pub fn pause(&self) -> bool {
        self.send(RecordingCommand::Pause)
    }

    /// Requests that a paused capture session resume.
    ///
    /// Returns `false` when the recording worker has already exited.
    pub fn resume(&self) -> bool {
        self.send(RecordingCommand::Resume)
    }

    /// Requests a new capture target for subsequent frames.
    ///
    /// Sending is non-blocking. The returned acknowledgement distinguishes a
    /// queued request from an applied update, a backend rejection, and a worker
    /// that exited before processing it. A rejected request does not stop the
    /// recording and leaves the preceding target active.
    pub fn update_target(&self, target: CaptureTarget) -> TargetUpdateRequest {
        let (completion, receiver) = mpsc::channel();
        let request_target = target.clone();
        let sent = self.send(RecordingCommand::UpdateTarget { target, completion });
        TargetUpdateRequest {
            target: request_target,
            receiver,
            status: if sent {
                TargetUpdateStatus::Pending
            } else {
                TargetUpdateStatus::WorkerExited
            },
        }
    }

    /// Requests that capture stop and the frames already collected be encoded.
    ///
    /// Returns `false` when the recording worker has already exited.
    pub fn stop(&self) -> bool {
        self.send(RecordingCommand::Stop)
    }

    /// Requests that capture stop and all output from this job be discarded.
    ///
    /// Returns `false` when the recording worker has already exited.
    pub fn discard(&self) -> bool {
        self.send(RecordingCommand::Discard)
    }

    fn send(&self, command: RecordingCommand) -> bool {
        let _dispatch = self.dispatch.lock().unwrap_or_else(PoisonError::into_inner);
        self.sender.send(command).is_ok()
    }
}

impl RecordingControl {
    pub(crate) fn apply_pending(
        &mut self,
        session: &mut dyn CaptureSession,
    ) -> Result<ControlOutcome, WorkflowError> {
        let mut outcome = ControlOutcome::Continue;
        loop {
            let command = match self.receiver.try_recv() {
                Ok(command) => command,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                    return Ok(outcome);
                }
            };
            match command {
                RecordingCommand::Pause
                    if outcome == ControlOutcome::Continue
                        && session.state() == CaptureSessionState::Recording =>
                {
                    session.pause()?;
                }
                RecordingCommand::Resume
                    if outcome == ControlOutcome::Continue
                        && session.state() == CaptureSessionState::Paused =>
                {
                    session.resume()?;
                }
                RecordingCommand::Pause | RecordingCommand::Resume => {}
                RecordingCommand::UpdateTarget { target, completion } => {
                    let result = match outcome {
                        ControlOutcome::Continue => session.update_target(target),
                        ControlOutcome::Stop => Err(CaptureError::new(
                            CaptureErrorKind::InvalidStateTransition,
                            "cannot update the capture target after stop was requested",
                            RecoveryHint::None,
                        )),
                        ControlOutcome::Discard => Err(CaptureError::new(
                            CaptureErrorKind::InvalidStateTransition,
                            "cannot update the capture target after discard was requested",
                            RecoveryHint::None,
                        )),
                    };
                    let _ = completion.send(result);
                }
                RecordingCommand::Stop => {
                    if outcome == ControlOutcome::Continue && !session.state().is_terminal() {
                        session.stop()?;
                    }
                    if outcome != ControlOutcome::Discard {
                        outcome = ControlOutcome::Stop;
                    }
                }
                RecordingCommand::Discard => {
                    if !matches!(
                        session.state(),
                        CaptureSessionState::Discarded | CaptureSessionState::Failed
                    ) {
                        session.discard()?;
                    }
                    outcome = ControlOutcome::Discard;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gif_from_screen_capture::{
        CaptureBackend, CaptureCadence, CaptureRequest, CaptureSessionState, CaptureSourceId,
        CaptureTarget, SyntheticCaptureBackend,
    };

    use super::*;

    fn target(x: i32) -> CaptureTarget {
        CaptureTarget::Region {
            source: CaptureSourceId::new("synthetic:monitor:0").unwrap(),
            region: gif_from_screen_capture::PhysicalRect::new(x, 0, 1, 1).unwrap(),
        }
    }

    fn session() -> Box<dyn CaptureSession> {
        let backend = SyntheticCaptureBackend::new(Vec::new());
        backend
            .start_session(CaptureRequest::new(
                target(0),
                CaptureCadence::interval(Duration::from_millis(10)).unwrap(),
            ))
            .unwrap()
    }

    #[test]
    fn pause_resume_and_stop_are_applied_in_order() {
        let (controller, mut control) = RecordingController::channel();
        let mut session = session();
        assert!(controller.pause());
        control.apply_pending(&mut *session).unwrap();
        assert_eq!(session.state(), CaptureSessionState::Paused);
        assert!(controller.resume());
        control.apply_pending(&mut *session).unwrap();
        assert_eq!(session.state(), CaptureSessionState::Recording);
        assert!(controller.stop());
        assert_eq!(
            control.apply_pending(&mut *session).unwrap(),
            ControlOutcome::Stop
        );
        assert_eq!(session.state(), CaptureSessionState::Stopped);
    }

    #[test]
    fn discard_is_terminal_and_idempotent_at_the_controller_boundary() {
        let (controller, mut control) = RecordingController::channel();
        let mut session = session();
        assert!(controller.pause());
        assert!(controller.discard());
        assert_eq!(
            control.apply_pending(&mut *session).unwrap(),
            ControlOutcome::Discard
        );
        assert_eq!(session.state(), CaptureSessionState::Discarded);
    }

    #[test]
    fn pause_update_resume_commands_are_applied_in_send_order() {
        let (controller, mut control) = RecordingController::channel();
        let mut session = session();
        assert!(controller.pause());
        let mut update = controller.update_target(target(10));
        assert!(controller.resume());

        assert_eq!(
            control.apply_pending(&mut *session).unwrap(),
            ControlOutcome::Continue
        );
        assert_eq!(session.state(), CaptureSessionState::Recording);
        assert_eq!(session.request().target, target(10));
        assert_eq!(update.status(), TargetUpdateStatus::Applied);
    }

    #[test]
    fn consecutive_target_updates_are_acknowledged_and_leave_the_latest_active() {
        let (controller, mut control) = RecordingController::channel();
        let mut session = session();
        let mut first = controller.update_target(target(10));
        let mut second = controller.update_target(target(20));

        control.apply_pending(&mut *session).unwrap();

        assert_eq!(first.status(), TargetUpdateStatus::Applied);
        assert_eq!(second.status(), TargetUpdateStatus::Applied);
        assert_eq!(session.request().target, target(20));
    }

    #[test]
    fn rejected_target_update_is_reported_without_stopping_the_session() {
        let (controller, mut control) = RecordingController::channel();
        let mut session = session();
        let resized = CaptureTarget::Region {
            source: CaptureSourceId::new("synthetic:monitor:0").unwrap(),
            region: gif_from_screen_capture::PhysicalRect::new(10, 0, 2, 1).unwrap(),
        };
        let mut update = controller.update_target(resized);

        assert_eq!(
            control.apply_pending(&mut *session).unwrap(),
            ControlOutcome::Continue
        );
        let TargetUpdateStatus::Rejected(error) = update.status() else {
            panic!("dimension-changing update should be rejected");
        };
        assert_eq!(
            error.kind(),
            gif_from_screen_capture::CaptureErrorKind::InvalidRequest
        );
        assert_eq!(session.state(), CaptureSessionState::Recording);
        assert_eq!(session.request().target, target(0));
    }

    #[test]
    fn target_update_before_stop_is_applied_but_one_after_stop_is_rejected() {
        let (controller, mut control) = RecordingController::channel();
        let mut session = session();
        let mut before = controller.update_target(target(10));
        assert!(controller.stop());
        let mut after = controller.update_target(target(20));

        assert_eq!(
            control.apply_pending(&mut *session).unwrap(),
            ControlOutcome::Stop
        );
        assert_eq!(before.status(), TargetUpdateStatus::Applied);
        assert!(matches!(
            after.status(),
            TargetUpdateStatus::Rejected(error)
                if error.kind()
                    == gif_from_screen_capture::CaptureErrorKind::InvalidStateTransition
        ));
        assert_eq!(session.request().target, target(10));
    }

    #[test]
    fn discard_wins_a_queued_race_with_stop_in_either_order() {
        for discard_first in [false, true] {
            let (controller, mut control) = RecordingController::channel();
            let mut session = session();
            if discard_first {
                assert!(controller.discard());
                assert!(controller.stop());
            } else {
                assert!(controller.stop());
                assert!(controller.discard());
            }
            assert_eq!(
                control.apply_pending(&mut *session).unwrap(),
                ControlOutcome::Discard
            );
            assert_eq!(session.state(), CaptureSessionState::Discarded);
        }
    }

    #[test]
    fn target_update_after_discard_is_explicitly_rejected() {
        let (controller, mut control) = RecordingController::channel();
        let mut session = session();
        assert!(controller.discard());
        let mut update = controller.update_target(target(10));

        assert_eq!(
            control.apply_pending(&mut *session).unwrap(),
            ControlOutcome::Discard
        );
        assert!(matches!(
            update.status(),
            TargetUpdateStatus::Rejected(error)
                if error.kind()
                    == gif_from_screen_capture::CaptureErrorKind::InvalidStateTransition
                    && error.message().contains("after discard")
        ));
        assert_eq!(session.request().target, target(0));
    }

    #[test]
    fn update_reports_worker_exit_when_the_receiver_is_gone() {
        let (controller, control) = RecordingController::channel();
        drop(control);

        let mut update = controller.update_target(target(10));

        assert_eq!(update.target(), &target(10));
        assert_eq!(update.status(), TargetUpdateStatus::WorkerExited);
    }
}
