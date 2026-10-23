//! Bounded recording notifications: latest progress, ordered phase ACKs and terminal state.
//!
//! A stalled UI cannot accumulate a message or allocation for every captured frame.
//! Pause/capture transitions are kept separately from the best-effort progress slot,
//! so a later stopping snapshot cannot erase the capture owner's pause confirmation.

use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, SendError, SyncSender, TryRecvError},
};

use gif_from_screen_workflow::{WorkflowPhase, WorkflowProgress};

use crate::{JobMessage, RecordingCompletion};

/// Progress, persisting and completion are the only three possible pending messages.
/// A continuously reporting producer cannot turn one UI frame into an unbounded drain.
pub(crate) const MAX_MESSAGES_PER_FRAME: usize = 3;

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Stage {
    #[default]
    Active,
    Persisting,
    Finished,
}

#[derive(Default)]
struct Pending {
    progress: Option<WorkflowProgress>,
    phases: [Option<WorkflowPhase>; 2],
    last_phase: Option<WorkflowPhase>,
    persisting: bool,
    finished: Option<RecordingCompletion>,
    stage: Stage,
    receiver_alive: bool,
}

impl Pending {
    fn remember_phase(&mut self, phase: WorkflowPhase) {
        if self.last_phase == Some(phase) {
            return;
        }
        self.last_phase = Some(phase);
        if matches!(phase, WorkflowPhase::Paused | WorkflowPhase::Capturing)
            && self.phases[1] != Some(phase)
        {
            // Only these two phases acknowledge a UI pause/resume request. Keep
            // their most recent relative order, never an unbounded toggle history.
            self.phases = [self.phases[1], Some(phase)];
        }
    }
}

#[derive(Clone)]
pub(crate) struct RecordingMessageSender {
    pending: Arc<Mutex<Pending>>,
    wake: SyncSender<()>,
}

pub(crate) struct RecordingMessageReceiver {
    pending: Arc<Mutex<Pending>>,
    wake: Receiver<()>,
}

pub(crate) fn channel() -> (RecordingMessageSender, RecordingMessageReceiver) {
    let pending = Arc::new(Mutex::new(Pending {
        receiver_alive: true,
        ..Pending::default()
    }));
    let (wake_sender, wake_receiver) = mpsc::sync_channel(1);
    (
        RecordingMessageSender {
            pending: Arc::clone(&pending),
            wake: wake_sender,
        },
        RecordingMessageReceiver {
            pending,
            wake: wake_receiver,
        },
    )
}

impl RecordingMessageSender {
    pub(crate) fn send(&self, message: JobMessage) -> Result<(), SendError<JobMessage>> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !pending.receiver_alive {
            return Err(SendError(message));
        }
        if pending.stage == Stage::Finished {
            // Late best-effort notifications cannot overwrite a terminal result.
            // Return a second completion to its sender rather than silently
            // dropping ownership of another completed project.
            return if matches!(&message, JobMessage::Finished(_)) {
                Err(SendError(message))
            } else {
                Ok(())
            };
        }
        match message {
            JobMessage::Progress(progress) if pending.stage == Stage::Active => {
                pending.remember_phase(progress.phase);
                pending.progress = Some(progress);
            }
            JobMessage::Progress(_) => return Ok(()),
            JobMessage::Persisting => {
                pending.persisting = true;
                pending.stage = Stage::Persisting;
            }
            JobMessage::Finished(completion) => {
                pending.finished = Some(completion);
                pending.stage = Stage::Finished;
                pending.progress = None;
                pending.phases = [None; 2];
                pending.persisting = false;
            }
        }
        drop(pending);
        // One wakeup is sufficient regardless of the number of replaced snapshots.
        // Receiver shutdown racing this send has normal channel handoff semantics.
        let _ = self.wake.try_send(());
        Ok(())
    }
}

impl RecordingMessageReceiver {
    pub(crate) fn take_phase_acknowledgements(&self) -> [Option<WorkflowPhase>; 2] {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut pending.phases)
    }

    pub(crate) fn try_recv(&self) -> Result<JobMessage, TryRecvError> {
        let disconnected = matches!(self.wake.try_recv(), Err(TryRecvError::Disconnected));
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(completion) = pending.finished.take() {
            return Ok(JobMessage::Finished(completion));
        }
        if let Some(progress) = pending.progress.take() {
            return Ok(JobMessage::Progress(progress));
        }
        if std::mem::take(&mut pending.persisting) {
            return Ok(JobMessage::Persisting);
        }
        Err(if disconnected {
            TryRecvError::Disconnected
        } else {
            TryRecvError::Empty
        })
    }

    #[cfg(test)]
    pub(crate) fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<JobMessage, mpsc::RecvTimeoutError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.try_recv() {
                Ok(message) => return Ok(message),
                Err(TryRecvError::Disconnected) => {
                    return Err(mpsc::RecvTimeoutError::Disconnected);
                }
                Err(TryRecvError::Empty) => {}
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(mpsc::RecvTimeoutError::Timeout);
            }
            self.wake.recv_timeout(remaining)?;
        }
    }
}

impl Drop for RecordingMessageReceiver {
    fn drop(&mut self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.receiver_alive = false;
        pending.progress = None;
        pending.finished = None;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn progress(phase: WorkflowPhase, frames: u64) -> JobMessage {
        JobMessage::Progress(WorkflowProgress {
            phase,
            frames_captured: frames,
            capture_duration: Duration::from_millis(frames),
            playback_duration: Duration::from_millis(frames),
            encode: None,
        })
    }

    fn failed() -> JobMessage {
        JobMessage::Finished(RecordingCompletion::Failed {
            error: "retained failure".into(),
            recovery_path: None,
        })
    }

    #[test]
    fn stalled_ui_retains_one_latest_progress_and_one_wakeup() {
        let (sender, receiver) = channel();
        for frames in 0..100_000 {
            sender
                .send(progress(WorkflowPhase::Capturing, frames))
                .unwrap();
        }
        assert_eq!(receiver.wake.try_iter().count(), 1);
        assert!(
            matches!(receiver.try_recv(), Ok(JobMessage::Progress(p)) if p.frames_captured == 99_999)
        );
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn coalescing_preserves_pause_ack_before_later_non_capture_phase() {
        let (sender, receiver) = channel();
        sender.send(progress(WorkflowPhase::Capturing, 1)).unwrap();
        sender.send(progress(WorkflowPhase::Paused, 2)).unwrap();
        sender
            .send(progress(WorkflowPhase::StoppingCapture, 3))
            .unwrap();
        assert_eq!(
            receiver.take_phase_acknowledgements(),
            [Some(WorkflowPhase::Capturing), Some(WorkflowPhase::Paused)]
        );
        assert_eq!(receiver.take_phase_acknowledgements(), [None; 2]);
        assert!(
            matches!(receiver.try_recv(), Ok(JobMessage::Progress(p)) if p.phase == WorkflowPhase::StoppingCapture)
        );
    }

    #[test]
    fn repeated_toggles_keep_the_latest_two_acknowledgements_in_order() {
        let (sender, receiver) = channel();
        for _ in 0..1000 {
            sender.send(progress(WorkflowPhase::Paused, 1)).unwrap();
            sender.send(progress(WorkflowPhase::Capturing, 2)).unwrap();
        }
        assert_eq!(
            receiver.take_phase_acknowledgements(),
            [Some(WorkflowPhase::Paused), Some(WorkflowPhase::Capturing)]
        );
    }

    #[test]
    fn terminal_result_bypasses_progress_and_ignores_late_notifications() {
        let (sender, receiver) = channel();
        sender.send(progress(WorkflowPhase::Paused, 1)).unwrap();
        sender.send(JobMessage::Persisting).unwrap();
        sender.send(failed()).unwrap();
        sender
            .send(progress(WorkflowPhase::Capturing, 999))
            .unwrap();
        sender.send(JobMessage::Persisting).unwrap();
        assert_eq!(receiver.take_phase_acknowledgements(), [None; 2]);
        assert!(
            matches!(receiver.try_recv(), Ok(JobMessage::Finished(RecordingCompletion::Failed { error, .. })) if error == "retained failure")
        );
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert!(sender.send(failed()).is_err());
    }

    #[test]
    fn persisting_is_reliable_and_does_not_revert_to_late_progress() {
        let (sender, receiver) = channel();
        sender
            .send(progress(WorkflowPhase::StoppingCapture, 42))
            .unwrap();
        sender.send(JobMessage::Persisting).unwrap();
        sender
            .send(progress(WorkflowPhase::Capturing, 999))
            .unwrap();
        assert!(
            matches!(receiver.try_recv(), Ok(JobMessage::Progress(p)) if p.frames_captured == 42)
        );
        assert!(matches!(receiver.try_recv(), Ok(JobMessage::Persisting)));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn receiver_shutdown_returns_completion_ownership_to_sender() {
        let (sender, receiver) = channel();
        drop(receiver);
        assert!(matches!(
            sender.send(failed()),
            Err(SendError(JobMessage::Finished(_)))
        ));
    }

    #[test]
    fn timed_receive_drains_pending_data_before_reporting_sender_shutdown() {
        let (sender, receiver) = channel();
        sender.send(progress(WorkflowPhase::Capturing, 7)).unwrap();
        drop(sender);
        assert!(
            matches!(receiver.recv_timeout(Duration::from_millis(10)), Ok(JobMessage::Progress(p)) if p.frames_captured == 7)
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(10)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
