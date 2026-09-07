use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use gif_from_screen_capture::{
    CaptureCadence, CaptureError, CaptureErrorKind, CaptureSession, CaptureSessionState,
    CaptureTarget, CaptureTimestamp, CapturedFrame, RecoveryHint,
};
use thiserror::Error;

use crate::WorkflowError;

/// Maximum manual snapshot commands that may be queued or waiting for frames.
pub const MAX_PENDING_SNAPSHOTS: usize = 64;

#[derive(Debug)]
struct SnapshotCompletion {
    sender: Sender<Result<SnapshotReceipt, SnapshotTriggerRejection>>,
    outstanding: Arc<AtomicUsize>,
}

impl SnapshotCompletion {
    fn finish(self, result: Result<SnapshotReceipt, SnapshotTriggerRejection>) {
        let _ = self.sender.send(result);
    }
}

impl Drop for SnapshotCompletion {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Default)]
struct PauseState {
    pending: usize,
    acknowledged: Option<bool>,
    toggle_pending: bool,
}

/// Pending state and the native acknowledgement are published under one short
/// lock. No native operation executes while holding that lock.
#[derive(Debug)]
struct PauseCompletion {
    state: Arc<Mutex<PauseState>>,
    toggle: bool,
    acknowledged: Option<bool>,
}

impl PauseCompletion {
    fn reserve(state: &Arc<Mutex<PauseState>>, toggle: bool) -> Option<Self> {
        let mut status = state.lock().unwrap_or_else(PoisonError::into_inner);
        if toggle && status.toggle_pending {
            return None;
        }
        status.pending = status.pending.checked_add(1)?;
        status.toggle_pending |= toggle;
        Some(Self {
            state: Arc::clone(state),
            toggle,
            acknowledged: None,
        })
    }

    fn acknowledge(mut self, state: CaptureSessionState) {
        self.acknowledged = match state {
            CaptureSessionState::Recording => Some(false),
            CaptureSessionState::Paused => Some(true),
            _ => None,
        };
    }
}

impl Drop for PauseCompletion {
    fn drop(&mut self) {
        let mut status = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        status.pending -= 1;
        if self.toggle {
            status.toggle_pending = false;
        }
        // Failure, terminal/no-op states and abandoned commands are unknown,
        // never a stale "safely paused" claim inherited from an older command.
        status.acknowledged = self.acknowledged;
    }
}

#[derive(Clone, Copy, Debug)]
enum PauseCommand {
    Pause,
    Resume,
    Toggle,
}

#[derive(Debug)]
enum RecordingCommand {
    SetPause {
        request: PauseCommand,
        completion: PauseCompletion,
    },
    UpdateTarget {
        target: CaptureTarget,
        completion: Sender<Result<(), CaptureError>>,
    },
    Snapshot {
        completion: SnapshotCompletion,
    },
    Stop,
    Discard,
}

/// A clonable handle used by UI or hotkey threads to control an active recording.
#[derive(Clone, Debug)]
pub struct RecordingController {
    sender: Sender<RecordingCommand>,
    dispatch: Arc<Mutex<()>>,
    outstanding_snapshots: Arc<AtomicUsize>,
    pause_state: Arc<Mutex<PauseState>>,
}

/// The worker-side command receiver for one controlled recording.
#[derive(Debug)]
pub struct RecordingControl {
    receiver: Receiver<RecordingCommand>,
    pending_snapshots: VecDeque<SnapshotCompletion>,
    snapshot_armed: bool,
}

/// Receipt for one native frame accepted by a manual snapshot trigger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotReceipt {
    sequence: u64,
    captured_at: CaptureTimestamp,
    retained: bool,
}

impl SnapshotReceipt {
    /// Native session sequence of the accepted frame.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Actual session-relative capture timestamp, not the trigger dispatch time.
    pub const fn captured_at(self) -> CaptureTimestamp {
        self.captured_at
    }

    /// Whether frame-retention policy stored this sample in memory or the sink.
    pub const fn retained(self) -> bool {
        self.retained
    }
}

/// Why a queued manual snapshot could not accept a native frame.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum SnapshotTriggerRejection {
    /// The public burst limit was reached before this command could be queued.
    #[error("manual snapshot queue is full (limit {limit})")]
    QueueFull {
        /// Maximum queued and frame-waiting snapshot commands.
        limit: usize,
    },
    /// The worker could not reserve bounded acknowledgement queue storage.
    #[error("could not allocate bounded manual snapshot acknowledgement storage")]
    AllocationFailed,
    /// The active request does not use [`CaptureCadence::Manual`].
    #[error("snapshot trigger requires manual capture cadence")]
    NonManualCadence,
    /// The session was paused when this trigger reached the worker.
    #[error("snapshot trigger was rejected while capture was paused")]
    Paused,
    /// Stop was ordered before this trigger could accept a frame.
    #[error("snapshot trigger was cancelled by stop")]
    Stopped,
    /// Discard was ordered before this trigger could accept a frame.
    #[error("snapshot trigger was cancelled by discard")]
    Discarded,
    /// Cooperative cancellation ended collection.
    #[error("snapshot trigger was cancelled")]
    Cancelled,
    /// Native capture failed while the trigger was pending.
    #[error("native capture failed before the snapshot: {0}")]
    CaptureFailed(#[source] CaptureError),
    /// A validation, resource-limit, or sink failure ended collection.
    #[error("snapshot collection failed: {0}")]
    CollectionFailed(String),
    /// The stream or configured collection limit ended before a frame arrived.
    #[error("snapshot collection ended before a frame arrived")]
    CollectionEnded,
}

/// Current acknowledgement state of one manual snapshot trigger.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SnapshotTriggerStatus {
    /// The command is queued or waiting for the next native frame.
    Pending,
    /// One native frame was accepted for this trigger.
    Captured(SnapshotReceipt),
    /// Collection rejected the trigger before accepting a frame.
    Rejected(SnapshotTriggerRejection),
    /// The worker-side control object disappeared without an acknowledgement.
    WorkerExited,
}

/// Queryable acknowledgement for one ordered manual snapshot command.
#[derive(Debug)]
pub struct SnapshotTriggerRequest {
    receiver: Receiver<Result<SnapshotReceipt, SnapshotTriggerRejection>>,
    status: SnapshotTriggerStatus,
}

impl SnapshotTriggerRequest {
    /// Polls the acknowledgement without blocking and caches terminal results.
    pub fn status(&mut self) -> SnapshotTriggerStatus {
        if self.status != SnapshotTriggerStatus::Pending {
            return self.status.clone();
        }
        match self.receiver.try_recv() {
            Ok(Ok(receipt)) => self.status = SnapshotTriggerStatus::Captured(receipt),
            Ok(Err(reason)) => self.status = SnapshotTriggerStatus::Rejected(reason),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.status = SnapshotTriggerStatus::WorkerExited,
        }
        self.status.clone()
    }
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
        let outstanding_snapshots = Arc::new(AtomicUsize::new(0));
        (
            Self {
                sender,
                dispatch: Arc::new(Mutex::new(())),
                outstanding_snapshots,
                pause_state: Arc::default(),
            },
            RecordingControl {
                receiver,
                pending_snapshots: VecDeque::new(),
                snapshot_armed: false,
            },
        )
    }

    /// Requests that the active capture session pause.
    ///
    /// Returns `false` when the recording worker has already exited.
    pub fn pause(&self) -> bool {
        self.send_pause(PauseCommand::Pause)
    }

    /// Requests that a paused capture session resume.
    ///
    /// Returns `false` when the recording worker has already exited.
    pub fn resume(&self) -> bool {
        self.send_pause(PauseCommand::Resume)
    }

    /// Toggles pause using the capture worker's actual session state.
    ///
    /// At most one toggle may be queued or executing across all controller
    /// clones. Returns `false` when another toggle is still pending or the
    /// worker has exited. `true` means queued, not that capture is already
    /// paused: read [`Self::pause_status`] for the native acknowledgement.
    /// Terminal or not-yet-recording sessions ignore the toggle. Existing
    /// explicit pause/resume commands retain their normal ordered semantics.
    pub fn toggle_pause(&self) -> bool {
        self.send_pause(PauseCommand::Toggle)
    }

    /// Atomically reads the pending pause-command count and last native result.
    ///
    /// A nonzero count includes queued and executing explicit pause/resume and
    /// toggle requests. Do not claim capture is safely paused while it is nonzero.
    /// `Some(true)` means the last completed transition observed `Paused`;
    /// `Some(false)` means `Recording`. `None` means no acknowledgement yet,
    /// a failed/abandoned command, or a terminal/nonrecording session. Progress
    /// may skip intermediate phases; this snapshot does not rely on UI polling.
    pub fn pause_status(&self) -> (usize, Option<bool>) {
        let state = self
            .pause_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (state.pending, state.acknowledged)
    }

    fn send_pause(&self, request: PauseCommand) -> bool {
        PauseCompletion::reserve(&self.pause_state, matches!(request, PauseCommand::Toggle))
            .is_some_and(|completion| {
                self.send(RecordingCommand::SetPause {
                    request,
                    completion,
                })
            })
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

    /// Queues one snapshot without blocking later recording controls.
    ///
    /// Paused and non-manual sessions reject the command through its
    /// acknowledgement. Burst triggers are not coalesced: every accepted
    /// command consumes exactly one subsequent native frame. Pending snapshots
    /// use the current target, wait through pauses, and are cancelled by stop
    /// or discard. Moving the target or resuming establishes a fresh boundary.
    pub fn trigger_snapshot(&self) -> SnapshotTriggerRequest {
        let (completion, receiver) = mpsc::channel();
        if self
            .outstanding_snapshots
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < MAX_PENDING_SNAPSHOTS).then_some(current + 1)
            })
            .is_err()
        {
            return SnapshotTriggerRequest {
                receiver,
                status: SnapshotTriggerStatus::Rejected(SnapshotTriggerRejection::QueueFull {
                    limit: MAX_PENDING_SNAPSHOTS,
                }),
            };
        }
        let sent = self.send(RecordingCommand::Snapshot {
            completion: SnapshotCompletion {
                sender: completion,
                outstanding: Arc::clone(&self.outstanding_snapshots),
            },
        });
        SnapshotTriggerRequest {
            receiver,
            status: if sent {
                SnapshotTriggerStatus::Pending
            } else {
                SnapshotTriggerStatus::WorkerExited
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
    pub(crate) fn has_pending_snapshot(&self) -> bool {
        !self.pending_snapshots.is_empty()
    }

    pub(crate) fn complete_snapshot(&mut self, frame: &CapturedFrame, retained: bool) {
        if let Some(completion) = self.pending_snapshots.pop_front() {
            self.snapshot_armed = false;
            completion.finish(Ok(SnapshotReceipt {
                sequence: frame.sequence(),
                captured_at: frame.captured_at(),
                retained,
            }));
        }
    }

    pub(crate) fn reject_pending_snapshots(&mut self, reason: &SnapshotTriggerRejection) {
        self.snapshot_armed = false;
        for completion in self.pending_snapshots.drain(..) {
            completion.finish(Err(reason.clone()));
        }
    }

    pub(crate) fn reject_all_snapshots(&mut self, reason: &SnapshotTriggerRejection) {
        self.reject_pending_snapshots(reason);
        while let Ok(command) = self.receiver.try_recv() {
            if let RecordingCommand::Snapshot { completion } = command {
                completion.finish(Err(reason.clone()));
            }
        }
    }

    fn apply_pause_change(
        &mut self,
        session: &mut dyn CaptureSession,
        outcome: ControlOutcome,
        request: PauseCommand,
        completion: PauseCompletion,
    ) -> Result<(), CaptureError> {
        if outcome == ControlOutcome::Continue {
            match (request, session.state()) {
                (PauseCommand::Pause | PauseCommand::Toggle, CaptureSessionState::Recording) => {
                    session.pause()?;
                    self.snapshot_armed = false;
                }
                (PauseCommand::Resume | PauseCommand::Toggle, CaptureSessionState::Paused) => {
                    session.resume()?;
                }
                _ => {}
            }
        }
        // Do not release before the native transition returns; a second hotkey
        // must not queue the opposite transition while it is still executing.
        completion.acknowledge(session.state());
        Ok(())
    }

    pub(crate) fn apply_pending(
        &mut self,
        session: &mut dyn CaptureSession,
    ) -> Result<ControlOutcome, WorkflowError> {
        let mut outcome = ControlOutcome::Continue;
        loop {
            let command = match self.receiver.try_recv() {
                Ok(command) => command,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                    break;
                }
            };
            match command {
                RecordingCommand::SetPause {
                    request,
                    completion,
                } => {
                    self.apply_pause_change(session, outcome, request, completion)?;
                }
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
                    if result.is_ok() {
                        self.snapshot_armed = false;
                    }
                    let _ = completion.send(result);
                }
                RecordingCommand::Snapshot { completion } => {
                    let rejection = match outcome {
                        ControlOutcome::Stop => Some(SnapshotTriggerRejection::Stopped),
                        ControlOutcome::Discard => Some(SnapshotTriggerRejection::Discarded),
                        ControlOutcome::Continue
                            if !matches!(session.request().cadence, CaptureCadence::Manual) =>
                        {
                            Some(SnapshotTriggerRejection::NonManualCadence)
                        }
                        ControlOutcome::Continue if session.state().is_terminal() => {
                            Some(SnapshotTriggerRejection::CollectionEnded)
                        }
                        ControlOutcome::Continue
                            if session.state() == CaptureSessionState::Paused =>
                        {
                            Some(SnapshotTriggerRejection::Paused)
                        }
                        ControlOutcome::Continue => None,
                    };
                    if let Some(rejection) = rejection {
                        completion.finish(Err(rejection));
                        continue;
                    }
                    if self.pending_snapshots.try_reserve(1).is_err() {
                        completion.finish(Err(SnapshotTriggerRejection::AllocationFailed));
                        continue;
                    }
                    self.pending_snapshots.push_back(completion);
                }
                RecordingCommand::Stop => {
                    if outcome == ControlOutcome::Continue && !session.state().is_terminal() {
                        session.stop()?;
                    }
                    if outcome != ControlOutcome::Discard {
                        outcome = ControlOutcome::Stop;
                    }
                    self.reject_pending_snapshots(&SnapshotTriggerRejection::Stopped);
                }
                RecordingCommand::Discard => {
                    if !matches!(
                        session.state(),
                        CaptureSessionState::Discarded | CaptureSessionState::Failed
                    ) {
                        session.discard()?;
                    }
                    outcome = ControlOutcome::Discard;
                    self.reject_pending_snapshots(&SnapshotTriggerRejection::Discarded);
                }
            }
        }
        // Only one native request may be armed at a time, but controls must
        // keep flowing even if the source never supplies the promised frame.
        if !self.snapshot_armed
            && self.has_pending_snapshot()
            && session.state() == CaptureSessionState::Recording
        {
            session.prepare_snapshot()?;
            self.snapshot_armed = true;
        }
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "toggle_pause_tests.rs"]
mod toggle_pause_tests;

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

    fn manual_session() -> Box<dyn CaptureSession> {
        let backend = SyntheticCaptureBackend::new(Vec::new());
        backend
            .start_session(CaptureRequest::new(target(0), CaptureCadence::Manual))
            .unwrap()
    }

    fn captured(sequence: u64, timestamp_us: u64) -> CapturedFrame {
        CapturedFrame::new(
            sequence,
            CaptureTimestamp::from_micros(timestamp_us),
            gif_from_screen_capture::PhysicalSize::new(1, 1).unwrap(),
            4,
            gif_from_screen_capture::PixelFormat::Rgba8,
            vec![1, 2, 3, 255],
        )
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

    #[test]
    fn manual_snapshot_acknowledges_the_exact_accepted_native_frame() {
        let (controller, mut control) = RecordingController::channel();
        let mut session = manual_session();
        let mut request = controller.trigger_snapshot();

        assert_eq!(request.status(), SnapshotTriggerStatus::Pending);
        assert_eq!(
            control.apply_pending(&mut *session).unwrap(),
            ControlOutcome::Continue
        );
        assert!(control.has_pending_snapshot());
        control.complete_snapshot(&captured(7, 42_000), true);

        assert_eq!(
            request.status(),
            SnapshotTriggerStatus::Captured(SnapshotReceipt {
                sequence: 7,
                captured_at: CaptureTimestamp::from_micros(42_000),
                retained: true,
            })
        );
        assert!(!control.has_pending_snapshot());
    }

    #[test]
    fn nonmanual_and_paused_snapshot_requests_are_explicitly_rejected() {
        let (controller, mut control) = RecordingController::channel();
        let mut periodic = session();
        let mut nonmanual = controller.trigger_snapshot();
        control.apply_pending(&mut *periodic).unwrap();
        assert_eq!(
            nonmanual.status(),
            SnapshotTriggerStatus::Rejected(SnapshotTriggerRejection::NonManualCadence)
        );

        let (controller, mut control) = RecordingController::channel();
        let mut manual = manual_session();
        assert!(controller.pause());
        let mut paused = controller.trigger_snapshot();
        control.apply_pending(&mut *manual).unwrap();
        assert_eq!(manual.state(), CaptureSessionState::Paused);
        assert_eq!(
            paused.status(),
            SnapshotTriggerStatus::Rejected(SnapshotTriggerRejection::Paused)
        );
    }

    #[test]
    fn snapshot_burst_is_bounded_before_worker_queue_growth() {
        let (controller, control) = RecordingController::channel();
        let mut pending = (0..MAX_PENDING_SNAPSHOTS)
            .map(|_| controller.trigger_snapshot())
            .collect::<Vec<_>>();
        assert!(
            pending
                .iter_mut()
                .all(|request| request.status() == SnapshotTriggerStatus::Pending)
        );
        let mut overflow = controller.trigger_snapshot();
        assert_eq!(
            overflow.status(),
            SnapshotTriggerStatus::Rejected(SnapshotTriggerRejection::QueueFull {
                limit: MAX_PENDING_SNAPSHOTS,
            })
        );

        drop(control);
        assert!(
            pending
                .iter_mut()
                .all(|request| request.status() == SnapshotTriggerStatus::WorkerExited)
        );
    }

    #[test]
    fn terminal_commands_reject_a_snapshot_waiting_for_its_frame() {
        for discard in [false, true] {
            let (controller, mut control) = RecordingController::channel();
            let mut session = manual_session();
            let mut snapshot = controller.trigger_snapshot();
            control.apply_pending(&mut *session).unwrap();
            if discard {
                assert!(controller.discard());
            } else {
                assert!(controller.stop());
            }
            let outcome = control.apply_pending(&mut *session).unwrap();
            assert_eq!(
                outcome,
                if discard {
                    ControlOutcome::Discard
                } else {
                    ControlOutcome::Stop
                }
            );
            assert_eq!(
                snapshot.status(),
                SnapshotTriggerStatus::Rejected(if discard {
                    SnapshotTriggerRejection::Discarded
                } else {
                    SnapshotTriggerRejection::Stopped
                })
            );
        }
    }
}
