use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use gif_from_screen_capture::{CaptureSession, CaptureSessionState};

use crate::WorkflowError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordingCommand {
    Pause,
    Resume,
    Stop,
    Discard,
}

/// A clonable handle used by UI or hotkey threads to control an active recording.
#[derive(Clone, Debug)]
pub struct RecordingController {
    sender: Sender<RecordingCommand>,
}

/// The worker-side command receiver for one controlled recording.
#[derive(Debug)]
pub struct RecordingControl {
    receiver: Receiver<RecordingCommand>,
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
        (Self { sender }, RecordingControl { receiver })
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
        self.sender.send(command).is_ok()
    }
}

impl RecordingControl {
    pub(crate) fn apply_pending(
        &mut self,
        session: &mut dyn CaptureSession,
    ) -> Result<ControlOutcome, WorkflowError> {
        loop {
            let command = match self.receiver.try_recv() {
                Ok(command) => command,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                    return Ok(ControlOutcome::Continue);
                }
            };
            match command {
                RecordingCommand::Pause if session.state() == CaptureSessionState::Recording => {
                    session.pause()?;
                }
                RecordingCommand::Resume if session.state() == CaptureSessionState::Paused => {
                    session.resume()?;
                }
                RecordingCommand::Pause | RecordingCommand::Resume => {}
                RecordingCommand::Stop => {
                    if !session.state().is_terminal() {
                        session.stop()?;
                    }
                    return Ok(ControlOutcome::Stop);
                }
                RecordingCommand::Discard => {
                    if !matches!(
                        session.state(),
                        CaptureSessionState::Discarded | CaptureSessionState::Failed
                    ) {
                        session.discard()?;
                    }
                    return Ok(ControlOutcome::Discard);
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

    fn session() -> Box<dyn CaptureSession> {
        let backend = SyntheticCaptureBackend::new(Vec::new());
        backend
            .start_session(CaptureRequest::new(
                CaptureTarget::Monitor(CaptureSourceId::new("synthetic:monitor:0").unwrap()),
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
}
