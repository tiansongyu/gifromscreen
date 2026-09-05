//! Background Wayland portal selection and frozen first-frame preparation.
//!
//! Native backend/session objects never cross the worker boundary. After the
//! first complete frame is copied into an owned preview, the worker pauses and
//! retains the same session until cancellation (or a future recording commit).

use std::{
    io,
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread,
    time::Duration,
};

use gif_from_screen_capture::{
    CaptureBackend, CaptureCadence, CaptureError, CaptureRequest, CaptureSession, CaptureSource,
    CaptureSourceKind, CaptureTarget, CapturedFrame, CursorCaptureMode, FramePoll, PhysicalSize,
    PixelFormat,
};
use gif_from_screen_capture_linux::WaylandCaptureBackend;
use gif_from_screen_gif::CancellationFlag;
use gif_from_screen_workflow::{RecordingControl, RecordingController};
use thiserror::Error;

use crate::fixed_crop_session::FixedCropSession;
use crate::{
    JobMessage, RecordingCompletion, RecordingJob, RecordingRetarget, RecordingWorkerRequest,
    run_incremental_prestarted_recording,
};

const PREPARE_THREAD_NAME: &str = "gfs-wayland-prepare";
const FIRST_FRAME_POLL: Duration = Duration::from_millis(50);
const HELD_SESSION_POLL: Duration = Duration::from_millis(50);
const MAX_PREVIEW_BYTES: usize = 256 * 1024 * 1024;

/// UI-visible lifecycle for one portal selection and first-frame request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum WaylandPrepareJobState {
    /// No worker owns a portal session.
    #[default]
    Idle,
    /// The worker is connecting to the portal backend.
    Connecting,
    /// The system chooser and `PipeWire` negotiation are in progress.
    Choosing,
    /// A session exists and the worker is waiting for its first frame.
    WaitingForFrame,
    /// A frozen preview was delivered and the same paused session is retained.
    Prepared,
    /// Cancellation was requested and native teardown is pending.
    Cancelling,
    /// The worker exited and a terminal result is available.
    Finished,
}

/// Owned, tightly packed RGBA pixels used only for the frozen setup preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrozenSourcePreview {
    size: PhysicalSize,
    rgba: Arc<[u8]>,
}

impl FrozenSourcePreview {
    pub(crate) const fn size(&self) -> PhysicalSize {
        self.size
    }

    pub(crate) fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    fn from_frame(frame: &CapturedFrame) -> Result<Self, WaylandPrepareJobError> {
        let width = usize::try_from(frame.size().width())
            .map_err(|_| WaylandPrepareJobError::InvalidPreview("width exceeds usize"))?;
        let height = usize::try_from(frame.size().height())
            .map_err(|_| WaylandPrepareJobError::InvalidPreview("height exceeds usize"))?;
        let row_bytes = width
            .checked_mul(4)
            .ok_or(WaylandPrepareJobError::InvalidPreview(
                "row byte length overflowed",
            ))?;
        let byte_len =
            row_bytes
                .checked_mul(height)
                .ok_or(WaylandPrepareJobError::InvalidPreview(
                    "preview byte length overflowed",
                ))?;
        if byte_len > MAX_PREVIEW_BYTES {
            return Err(WaylandPrepareJobError::PreviewTooLarge {
                required_bytes: byte_len,
                limit_bytes: MAX_PREVIEW_BYTES,
            });
        }
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(byte_len)
            .map_err(|_| WaylandPrepareJobError::PreviewAllocation { byte_len })?;
        for row in frame.pixels().chunks(frame.stride()).take(height) {
            let row = row
                .get(..row_bytes)
                .ok_or(WaylandPrepareJobError::InvalidPreview(
                    "captured row is truncated",
                ))?;
            match frame.format() {
                PixelFormat::Rgba8 => rgba.extend_from_slice(row),
                PixelFormat::Bgra8 => {
                    for pixel in row.as_chunks::<4>().0 {
                        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                    }
                }
                _ => {
                    return Err(WaylandPrepareJobError::InvalidPreview(
                        "unsupported future pixel format",
                    ));
                }
            }
        }
        if rgba.len() != byte_len {
            return Err(WaylandPrepareJobError::InvalidPreview(
                "captured frame has too few rows",
            ));
        }
        Ok(Self {
            size: frame.size(),
            rgba: rgba.into(),
        })
    }
}

/// Terminal outcome after explicitly closing a prepared session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaylandPrepareOutcome {
    Cancelled,
}

/// Typed preparation failures shown by the recorder UI.
#[derive(Debug, Error)]
pub(crate) enum WaylandPrepareJobError {
    #[error("could not initialize Wayland portal capture: {0}")]
    Initialize(#[source] CaptureError),
    #[error("the selected source kind cannot be prepared through the Wayland portal")]
    UnsupportedSource,
    #[error("Wayland system selection or PipeWire startup failed: {0}")]
    StartSession(#[source] CaptureError),
    #[error("could not poll the selected Wayland source: {0}")]
    Poll(#[source] CaptureError),
    #[error("the selected Wayland source ended before producing a preview frame")]
    EndedBeforePreview,
    #[error("the selected Wayland source ended while its frozen preview was prepared")]
    SourceEndedWhilePrepared,
    #[error("could not pause the selected Wayland source after its preview: {0}")]
    Pause(#[source] CaptureError),
    #[error("could not discard the prepared Wayland source: {0}")]
    Discard(#[source] CaptureError),
    #[error("invalid captured preview: {0}")]
    InvalidPreview(&'static str),
    #[error("captured preview needs {required_bytes} bytes, above the {limit_bytes}-byte limit")]
    PreviewTooLarge {
        required_bytes: usize,
        limit_bytes: usize,
    },
    #[error("could not reserve {byte_len} bytes for the captured preview")]
    PreviewAllocation { byte_len: usize },
    #[error("Wayland preparation worker exited without reporting a result")]
    WorkerExited,
}

/// Failure to start a preparation worker.
#[derive(Debug, Error)]
pub(crate) enum WaylandPrepareJobStartError {
    #[error("Wayland preparation is already {state:?}")]
    AlreadyStarted { state: WaylandPrepareJobState },
    #[error("could not spawn Wayland preparation worker: {0}")]
    Spawn(#[source] io::Error),
}

/// Failure to transfer the prepared session into recording.
#[derive(Debug, Error)]
pub(crate) enum WaylandCommitError {
    #[error("Wayland source is not prepared (current state: {state:?})")]
    NotPrepared { state: WaylandPrepareJobState },
    #[error("Wayland preparation worker exited before accepting the selected crop")]
    WorkerExited,
}

/// Non-blocking notifications returned from [`WaylandPrepareJob::drain`].
#[derive(Debug)]
pub(crate) enum WaylandPrepareJobEvent {
    StateChanged(WaylandPrepareJobState),
    PreviewReady(FrozenSourcePreview),
    Finished,
}

enum WorkerCommand {
    Cancel,
    Commit {
        crop: gif_from_screen_capture::PhysicalRect,
        worker: Box<RecordingWorkerRequest>,
        control: RecordingControl,
        cancellation: CancellationFlag,
        messages: Sender<JobMessage>,
    },
}

enum WorkerMessage {
    Stage(WaylandPrepareJobState),
    Preview(FrozenSourcePreview),
    Finished(Result<WaylandPrepareOutcome, WaylandPrepareJobError>),
}

/// UI-owned handle for a worker-held portal and `PipeWire` session.
#[derive(Default)]
pub(crate) struct WaylandPrepareJob {
    state: WaylandPrepareJobState,
    commands: Option<Sender<WorkerCommand>>,
    receiver: Option<Receiver<WorkerMessage>>,
    result: Option<Result<WaylandPrepareOutcome, WaylandPrepareJobError>>,
}

impl WaylandPrepareJob {
    pub(crate) fn start(
        &mut self,
        source: CaptureSource,
        cadence: CaptureCadence,
    ) -> Result<(), WaylandPrepareJobStartError> {
        self.start_with(source, cadence, || {
            WaylandCaptureBackend::connect()
                .map(|backend| Box::new(backend) as Box<dyn CaptureBackend>)
                .map_err(WaylandPrepareJobError::Initialize)
        })
    }

    fn start_with<F>(
        &mut self,
        source: CaptureSource,
        cadence: CaptureCadence,
        backend_factory: F,
    ) -> Result<(), WaylandPrepareJobStartError>
    where
        F: FnOnce() -> Result<Box<dyn CaptureBackend>, WaylandPrepareJobError> + Send + 'static,
    {
        if self.state != WaylandPrepareJobState::Idle {
            return Err(WaylandPrepareJobStartError::AlreadyStarted { state: self.state });
        }
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        self.state = WaylandPrepareJobState::Connecting;
        self.result = None;
        let spawn = thread::Builder::new()
            .name(PREPARE_THREAD_NAME.to_owned())
            .spawn(move || {
                prepare_worker(
                    &source,
                    cadence,
                    backend_factory,
                    &command_receiver,
                    &event_sender,
                );
            });
        if let Err(error) = spawn {
            self.state = WaylandPrepareJobState::Idle;
            return Err(WaylandPrepareJobStartError::Spawn(error));
        }
        self.commands = Some(command_sender);
        self.receiver = Some(event_receiver);
        Ok(())
    }

    pub(crate) const fn state(&self) -> WaylandPrepareJobState {
        self.state
    }

    pub(crate) fn is_active(&self) -> bool {
        !matches!(
            self.state,
            WaylandPrepareJobState::Idle | WaylandPrepareJobState::Finished
        )
    }

    pub(crate) fn cancel(&mut self) -> bool {
        if !self.is_active() || self.state == WaylandPrepareJobState::Cancelling {
            return false;
        }
        let sent = self
            .commands
            .as_ref()
            .is_some_and(|commands| commands.send(WorkerCommand::Cancel).is_ok());
        if sent {
            self.state = WaylandPrepareJobState::Cancelling;
        }
        sent
    }

    pub(crate) fn commit_crop(
        &mut self,
        crop: gif_from_screen_capture::PhysicalRect,
        worker: RecordingWorkerRequest,
    ) -> Result<RecordingJob, WaylandCommitError> {
        if self.state != WaylandPrepareJobState::Prepared {
            return Err(WaylandCommitError::NotPrepared { state: self.state });
        }
        let source = worker.source_id.clone();
        let cancellation = CancellationFlag::default();
        let worker_cancellation = cancellation.clone();
        let (controller, control) = RecordingController::channel();
        let (messages, receiver) = mpsc::channel();
        self.commands
            .as_ref()
            .ok_or(WaylandCommitError::WorkerExited)?
            .send(WorkerCommand::Commit {
                crop,
                worker: Box::new(worker),
                control,
                cancellation: worker_cancellation,
                messages,
            })
            .map_err(|_| WaylandCommitError::WorkerExited)?;

        self.state = WaylandPrepareJobState::Idle;
        self.commands = None;
        self.receiver = None;
        self.result = None;
        Ok(RecordingJob {
            receiver,
            cancellation,
            controller,
            paused: false,
            terminal_requested: false,
            retarget: Some(RecordingRetarget::new(source, crop)),
            snapshot_requests: std::collections::VecDeque::new(),
        })
    }

    /// Drains all currently available events without waiting for native work.
    pub(crate) fn drain(&mut self) -> Vec<WaylandPrepareJobEvent> {
        if !self.is_active() {
            return Vec::new();
        }
        let mut events = Vec::new();
        loop {
            let message = match self.receiver.as_ref().map(Receiver::try_recv) {
                Some(Ok(message)) => message,
                Some(Err(TryRecvError::Empty)) => break,
                Some(Err(TryRecvError::Disconnected)) | None => {
                    self.finish(Err(WaylandPrepareJobError::WorkerExited));
                    events.push(WaylandPrepareJobEvent::Finished);
                    break;
                }
            };
            match message {
                WorkerMessage::Stage(state) => {
                    if self.state != WaylandPrepareJobState::Cancelling {
                        self.state = state;
                    }
                    events.push(WaylandPrepareJobEvent::StateChanged(self.state));
                }
                WorkerMessage::Preview(preview) => {
                    if self.state != WaylandPrepareJobState::Cancelling {
                        self.state = WaylandPrepareJobState::Prepared;
                        events.push(WaylandPrepareJobEvent::PreviewReady(preview));
                    }
                }
                WorkerMessage::Finished(result) => {
                    self.finish(result);
                    events.push(WaylandPrepareJobEvent::Finished);
                    break;
                }
            }
        }
        events
    }

    pub(crate) fn take_result(
        &mut self,
    ) -> Option<Result<WaylandPrepareOutcome, WaylandPrepareJobError>> {
        let result = self.result.take()?;
        self.state = WaylandPrepareJobState::Idle;
        Some(result)
    }

    fn finish(&mut self, result: Result<WaylandPrepareOutcome, WaylandPrepareJobError>) {
        self.commands = None;
        self.receiver = None;
        self.result = Some(result);
        self.state = WaylandPrepareJobState::Finished;
    }
}

impl Drop for WaylandPrepareJob {
    fn drop(&mut self) {
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(WorkerCommand::Cancel);
        }
        self.receiver = None;
    }
}

fn prepare_worker<F>(
    source: &CaptureSource,
    cadence: CaptureCadence,
    backend_factory: F,
    commands: &Receiver<WorkerCommand>,
    events: &Sender<WorkerMessage>,
) where
    F: FnOnce() -> Result<Box<dyn CaptureBackend>, WaylandPrepareJobError>,
{
    let result = prepare_worker_inner(source, cadence, backend_factory, commands, events);
    if let Some(result) = result {
        let _ = events.send(WorkerMessage::Finished(result));
    }
}

/// `None` means the UI receiver disappeared, so there is nobody to notify.
fn prepare_worker_inner<F>(
    source: &CaptureSource,
    cadence: CaptureCadence,
    backend_factory: F,
    commands: &Receiver<WorkerCommand>,
    events: &Sender<WorkerMessage>,
) -> Option<Result<WaylandPrepareOutcome, WaylandPrepareJobError>>
where
    F: FnOnce() -> Result<Box<dyn CaptureBackend>, WaylandPrepareJobError>,
{
    if cancellation_requested(commands) {
        return Some(Ok(WaylandPrepareOutcome::Cancelled));
    }
    if events
        .send(WorkerMessage::Stage(WaylandPrepareJobState::Connecting))
        .is_err()
    {
        return None;
    }
    let backend = match backend_factory() {
        Ok(backend) => backend,
        Err(error) => return Some(Err(error)),
    };
    if cancellation_requested(commands) {
        return Some(Ok(WaylandPrepareOutcome::Cancelled));
    }
    if events
        .send(WorkerMessage::Stage(WaylandPrepareJobState::Choosing))
        .is_err()
    {
        return None;
    }
    let mut session = match start_full_source_session(&*backend, source, cadence) {
        Ok(session) => session,
        Err(WaylandPrepareJobError::StartSession(error))
            if error.kind() == gif_from_screen_capture::CaptureErrorKind::PermissionRequired =>
        {
            return Some(Ok(WaylandPrepareOutcome::Cancelled));
        }
        Err(error) => return Some(Err(error)),
    };
    if cancellation_requested(commands) {
        return Some(finish_cancel(&mut *session));
    }
    if events
        .send(WorkerMessage::Stage(
            WaylandPrepareJobState::WaitingForFrame,
        ))
        .is_err()
    {
        let _ = session.discard();
        return None;
    }

    let preview = match wait_for_first_preview(&mut *session, commands) {
        Ok(Some(preview)) => preview,
        Ok(None) => return Some(finish_cancel(&mut *session)),
        Err(error) => {
            let _ = session.discard();
            return Some(Err(error));
        }
    };

    if let Err(error) = session.pause() {
        let _ = session.discard();
        return Some(Err(WaylandPrepareJobError::Pause(error)));
    }
    let preview_size = preview.size();
    if events.send(WorkerMessage::Preview(preview)).is_err() {
        let _ = session.discard();
        return None;
    }

    loop {
        match commands.recv_timeout(HELD_SESSION_POLL) {
            Ok(WorkerCommand::Cancel) | Err(RecvTimeoutError::Disconnected) => {
                return Some(finish_cancel(&mut *session));
            }
            Ok(WorkerCommand::Commit {
                crop,
                worker,
                control,
                cancellation,
                messages,
            }) => {
                run_committed_recording(
                    session,
                    preview_size,
                    crop,
                    &worker,
                    control,
                    &cancellation,
                    &messages,
                );
                return None;
            }
            Err(RecvTimeoutError::Timeout) => match session.poll_frame(Duration::ZERO) {
                Ok(FramePoll::Pending) => {}
                Ok(FramePoll::EndOfStream) => {
                    return Some(Err(WaylandPrepareJobError::SourceEndedWhilePrepared));
                }
                Ok(FramePoll::Frame(_)) => {
                    return Some(Err(WaylandPrepareJobError::Poll(
                        CaptureError::invalid_frame(
                            "paused Wayland session unexpectedly produced a frame",
                        ),
                    )));
                }
                Err(error) => return Some(Err(WaylandPrepareJobError::Poll(error))),
            },
        }
    }
}

fn wait_for_first_preview(
    session: &mut dyn CaptureSession,
    commands: &Receiver<WorkerCommand>,
) -> Result<Option<FrozenSourcePreview>, WaylandPrepareJobError> {
    loop {
        if cancellation_requested(commands) {
            return Ok(None);
        }
        match session
            .poll_frame(FIRST_FRAME_POLL)
            .map_err(WaylandPrepareJobError::Poll)?
        {
            FramePoll::Frame(frame) => {
                return FrozenSourcePreview::from_frame(&frame).map(Some);
            }
            FramePoll::Pending => {}
            FramePoll::EndOfStream => return Err(WaylandPrepareJobError::EndedBeforePreview),
        }
    }
}

fn run_committed_recording(
    session: Box<dyn CaptureSession>,
    source_size: PhysicalSize,
    crop: gif_from_screen_capture::PhysicalRect,
    worker: &RecordingWorkerRequest,
    mut control: RecordingControl,
    cancellation: &CancellationFlag,
    messages: &Sender<JobMessage>,
) {
    let source_id = worker.source_id.clone();
    let mut session = match FixedCropSession::new(session, source_id, source_size, crop) {
        Ok(session) => session,
        Err(error) => {
            let _ = messages.send(JobMessage::Finished(RecordingCompletion::Failed {
                error: format!(
                    "{error}; the prepared Wayland session was discarded before recording"
                ),
                recovery_path: None,
            }));
            return;
        }
    };
    let progress_messages = messages.clone();
    let mut progress = move |snapshot| {
        let _ = progress_messages.send(JobMessage::Progress(snapshot));
    };
    let completion = run_incremental_prestarted_recording(
        worker,
        &mut session,
        &mut control,
        cancellation,
        &mut progress,
        || {
            let _ = messages.send(JobMessage::Persisting);
        },
    );
    let _ = messages.send(JobMessage::Finished(completion));
}

fn start_full_source_session(
    backend: &dyn CaptureBackend,
    source: &CaptureSource,
    cadence: CaptureCadence,
) -> Result<Box<dyn CaptureSession>, WaylandPrepareJobError> {
    let target = match source.kind() {
        CaptureSourceKind::Monitor => CaptureTarget::Monitor(source.id().clone()),
        CaptureSourceKind::Window => CaptureTarget::Window(source.id().clone()),
        _ => return Err(WaylandPrepareJobError::UnsupportedSource),
    };
    let mut request = CaptureRequest::new(target, cadence);
    request.cursor = CursorCaptureMode::Automatic;
    backend
        .start_session(request)
        .map_err(WaylandPrepareJobError::StartSession)
}

fn cancellation_requested(commands: &Receiver<WorkerCommand>) -> bool {
    matches!(
        commands.try_recv(),
        Ok(WorkerCommand::Cancel) | Err(TryRecvError::Disconnected)
    )
}

fn finish_cancel(
    session: &mut dyn CaptureSession,
) -> Result<WaylandPrepareOutcome, WaylandPrepareJobError> {
    match session.discard() {
        Ok(()) => Ok(WaylandPrepareOutcome::Cancelled),
        Err(error) => Err(WaylandPrepareJobError::Discard(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex, mpsc},
        thread::ThreadId,
        time::Duration,
    };

    use gif_from_screen_capture::{
        BackendDescriptor, BackendStatus, CaptureCapabilities, CaptureErrorKind,
        CaptureSessionState, CaptureSourceId, CaptureTimestamp, FramePoll, RecoveryHint,
    };

    use super::*;

    fn source_without_geometry() -> CaptureSource {
        CaptureSource::new(
            CaptureSourceId::new("wayland:portal:monitor").unwrap(),
            "Choose a screen",
            CaptureSourceKind::Monitor,
            None,
            1.0,
        )
        .unwrap()
    }

    fn frame(format: PixelFormat, stride: usize, pixels: Vec<u8>) -> CapturedFrame {
        frame_at(0, 10, format, stride, pixels)
    }

    fn frame_at(
        sequence: u64,
        micros: u64,
        format: PixelFormat,
        stride: usize,
        pixels: Vec<u8>,
    ) -> CapturedFrame {
        CapturedFrame::new(
            sequence,
            CaptureTimestamp::from_micros(micros),
            PhysicalSize::new(2, 1).unwrap(),
            stride,
            format,
            pixels,
        )
        .unwrap()
    }

    #[derive(Default)]
    struct FakeSignals {
        paused: bool,
        discarded: bool,
        start_thread: Option<ThreadId>,
        pause_thread: Option<ThreadId>,
        discard_thread: Option<ThreadId>,
        start_calls: usize,
        started_cadence: Option<CaptureCadence>,
    }

    struct FakeSession {
        request: CaptureRequest,
        state: CaptureSessionState,
        frames: VecDeque<CapturedFrame>,
        signals: Arc<Mutex<FakeSignals>>,
        discarded: Option<Sender<()>>,
    }

    impl CaptureSession for FakeSession {
        fn state(&self) -> CaptureSessionState {
            self.state
        }

        fn request(&self) -> &CaptureRequest {
            &self.request
        }

        fn update_target(&mut self, target: CaptureTarget) -> Result<(), CaptureError> {
            self.request.target = target;
            Ok(())
        }

        fn pause(&mut self) -> Result<(), CaptureError> {
            self.state = CaptureSessionState::Paused;
            let mut signals = self.signals.lock().unwrap();
            signals.paused = true;
            signals.pause_thread = Some(thread::current().id());
            Ok(())
        }

        fn resume(&mut self) -> Result<(), CaptureError> {
            self.state = CaptureSessionState::Recording;
            Ok(())
        }

        fn stop(&mut self) -> Result<(), CaptureError> {
            self.state = CaptureSessionState::Stopped;
            Ok(())
        }

        fn discard(&mut self) -> Result<(), CaptureError> {
            self.state = CaptureSessionState::Discarded;
            let mut signals = self.signals.lock().unwrap();
            signals.discarded = true;
            signals.discard_thread = Some(thread::current().id());
            drop(signals);
            if let Some(discarded) = self.discarded.take() {
                let _ = discarded.send(());
            }
            Ok(())
        }

        fn poll_frame(&mut self, _timeout: Duration) -> Result<FramePoll, CaptureError> {
            if self.state == CaptureSessionState::Paused {
                return Ok(FramePoll::Pending);
            }
            Ok(self
                .frames
                .pop_front()
                .map_or(FramePoll::Pending, FramePoll::Frame))
        }
    }

    struct FakeBackend {
        frames: Mutex<Option<VecDeque<CapturedFrame>>>,
        signals: Arc<Mutex<FakeSignals>>,
        discarded: Mutex<Option<Sender<()>>>,
        start_gate: Mutex<Option<Receiver<()>>>,
    }

    impl CaptureBackend for FakeBackend {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: "fake-wayland",
                display_name: "Fake Wayland",
            }
        }

        fn status(&self) -> BackendStatus {
            BackendStatus::Ready
        }

        fn capabilities(&self) -> CaptureCapabilities {
            CaptureCapabilities::all_unavailable("test")
        }

        fn list_sources(&self) -> Result<Vec<CaptureSource>, CaptureError> {
            Ok(vec![source_without_geometry()])
        }

        fn start_session(
            &self,
            request: CaptureRequest,
        ) -> Result<Box<dyn CaptureSession>, CaptureError> {
            let mut signals = self.signals.lock().unwrap();
            signals.start_thread = Some(thread::current().id());
            signals.start_calls += 1;
            signals.started_cadence = Some(request.cadence);
            drop(signals);
            if let Some(gate) = self.start_gate.lock().unwrap().take() {
                let _ = gate.recv();
            }
            let frames = self.frames.lock().unwrap().take().ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorKind::SourceLost,
                    "test frame already taken",
                    RecoveryHint::None,
                )
            })?;
            Ok(Box::new(FakeSession {
                request,
                state: CaptureSessionState::Recording,
                frames,
                signals: self.signals.clone(),
                discarded: self.discarded.lock().unwrap().take(),
            }))
        }
    }

    fn backend(
        frames: impl IntoIterator<Item = CapturedFrame>,
        signals: Arc<Mutex<FakeSignals>>,
        discarded: Sender<()>,
        start_gate: Option<Receiver<()>>,
    ) -> Box<dyn CaptureBackend> {
        Box::new(FakeBackend {
            frames: Mutex::new(Some(frames.into_iter().collect())),
            signals,
            discarded: Mutex::new(Some(discarded)),
            start_gate: Mutex::new(start_gate),
        })
    }

    fn wait_for_preview(job: &mut WaylandPrepareJob) -> FrozenSourcePreview {
        for _ in 0..500 {
            for event in job.drain() {
                if let WaylandPrepareJobEvent::PreviewReady(preview) = event {
                    return preview;
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("preparation worker did not return a preview in time");
    }

    fn wait_for_finish(job: &mut WaylandPrepareJob) {
        for _ in 0..500 {
            if job
                .drain()
                .into_iter()
                .any(|event| matches!(event, WaylandPrepareJobEvent::Finished))
            {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("preparation worker did not finish in time");
    }

    #[test]
    fn frozen_preview_is_tightly_packed_and_converts_bgra() {
        let preview = FrozenSourcePreview::from_frame(&frame(
            PixelFormat::Bgra8,
            12,
            vec![3, 2, 1, 4, 7, 6, 5, 8, 99, 99, 99, 99],
        ))
        .unwrap();
        assert_eq!(preview.size(), PhysicalSize::new(2, 1).unwrap());
        assert_eq!(preview.rgba(), &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn chooser_wait_never_blocks_start_or_ui_drain() {
        let signals = Arc::new(Mutex::new(FakeSignals::default()));
        let (discarded_tx, discarded_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut job = WaylandPrepareJob::default();
        let backend = backend(
            [frame(PixelFormat::Rgba8, 8, vec![1, 2, 3, 4, 5, 6, 7, 8])],
            signals,
            discarded_tx,
            Some(release_rx),
        );
        job.start_with(
            source_without_geometry(),
            CaptureCadence::fixed_fps(30).unwrap(),
            move || Ok(backend),
        )
        .unwrap();
        let _ = job.drain();

        for _ in 0..500 {
            let _ = job.drain();
            if job.state() == WaylandPrepareJobState::Choosing {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(job.state(), WaylandPrepareJobState::Choosing);

        assert!(job.cancel());
        release_tx.send(()).unwrap();
        discarded_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        wait_for_finish(&mut job);
        assert_eq!(
            job.take_result().unwrap().unwrap(),
            WaylandPrepareOutcome::Cancelled
        );
    }

    #[test]
    fn geometryless_source_yields_preview_and_worker_holds_paused_session() {
        let signals = Arc::new(Mutex::new(FakeSignals::default()));
        let (discarded_tx, discarded_rx) = mpsc::channel();
        let mut job = WaylandPrepareJob::default();
        let backend = backend(
            [frame(PixelFormat::Rgba8, 8, vec![1, 2, 3, 4, 5, 6, 7, 8])],
            signals.clone(),
            discarded_tx,
            None,
        );
        job.start_with(
            source_without_geometry(),
            CaptureCadence::Manual,
            move || Ok(backend),
        )
        .unwrap();

        let preview = wait_for_preview(&mut job);
        assert_eq!(preview.size(), PhysicalSize::new(2, 1).unwrap());
        assert_eq!(job.state(), WaylandPrepareJobState::Prepared);
        let worker_threads = signals.lock().unwrap();
        assert!(worker_threads.paused);
        assert_eq!(worker_threads.started_cadence, Some(CaptureCadence::Manual));
        assert_eq!(worker_threads.start_thread, worker_threads.pause_thread);
        assert_ne!(worker_threads.start_thread, Some(thread::current().id()));
        drop(worker_threads);
        assert!(discarded_rx.try_recv().is_err());

        assert!(job.cancel());
        discarded_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        wait_for_finish(&mut job);
        let worker_threads = signals.lock().unwrap();
        assert_eq!(worker_threads.start_thread, worker_threads.discard_thread);
    }

    #[test]
    fn dropping_ui_receiver_discards_the_worker_owned_session() {
        let signals = Arc::new(Mutex::new(FakeSignals::default()));
        let (discarded_tx, discarded_rx) = mpsc::channel();
        let mut job = WaylandPrepareJob::default();
        let backend = backend(
            [frame(PixelFormat::Rgba8, 8, vec![1, 2, 3, 4, 5, 6, 7, 8])],
            signals.clone(),
            discarded_tx,
            None,
        );
        job.start_with(
            source_without_geometry(),
            CaptureCadence::fixed_fps(30).unwrap(),
            move || Ok(backend),
        )
        .unwrap();
        let _ = wait_for_preview(&mut job);

        drop(job);
        discarded_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(signals.lock().unwrap().discarded);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the test keeps countdown, prepared-session handoff, manual triggers, crop persistence, and acknowledgements in one lifecycle"
    )]
    fn nonzero_countdown_commits_the_same_session_and_persists_cropped_frames() {
        use crate::RecordingSettings;
        use crate::countdown::{CountdownStart, CountdownTick, RecordingCountdown};
        use gif_from_screen_domain::PhysicalSize as ProjectSize;
        use tempfile::tempdir;

        let signals = Arc::new(Mutex::new(FakeSignals::default()));
        let (discarded_tx, _discarded_rx) = mpsc::channel();
        let frames = [
            frame_at(
                0,
                10,
                PixelFormat::Rgba8,
                8,
                vec![1, 2, 3, 255, 4, 5, 6, 255],
            ),
            frame_at(
                1,
                100,
                PixelFormat::Rgba8,
                8,
                vec![10, 11, 12, 255, 20, 21, 22, 255],
            ),
            frame_at(
                2,
                100_100,
                PixelFormat::Rgba8,
                8,
                vec![30, 31, 32, 255, 40, 41, 42, 255],
            ),
        ];
        let backend = backend(frames, signals.clone(), discarded_tx, None);
        let mut preparation = WaylandPrepareJob::default();
        preparation
            .start_with(
                source_without_geometry(),
                CaptureCadence::Manual,
                move || Ok(backend),
            )
            .unwrap();
        let _ = wait_for_preview(&mut preparation);

        let started_at = std::time::Instant::now();
        let mut countdown = RecordingCountdown::default();
        assert_eq!(countdown.start(started_at, 2), CountdownStart::Started);
        assert_eq!(
            countdown.tick(started_at + Duration::from_secs(2)),
            CountdownTick::Finished
        );

        let directory = tempdir().unwrap();
        let output = directory.path().join("wayland.gif");
        let project_path = directory.path().join("wayland.gfsproj");
        let crop = gif_from_screen_capture::PhysicalRect::new(1, 0, 1, 1).unwrap();
        let worker = RecordingWorkerRequest {
            settings: RecordingSettings {
                output: output.to_string_lossy().into_owned(),
                duration_ms: 100,
                cadence: crate::RecordingCadenceChoice::Manual,
                fps: 30,
                interval_count: 1,
                interval_unit: crate::RecordingIntervalUnit::Seconds,
                manual_frame_duration_ms: 100,
                countdown_seconds: 2,
                changes_only: false,
                region_enabled: true,
                region_x: 1,
                region_y: 0,
                region_width: 1,
                region_height: 1,
            },
            source_id: CaptureSourceId::new("wayland:portal:monitor").unwrap(),
            source_kind: CaptureSourceKind::Monitor,
            source_label: "Portal monitor".to_owned(),
            project_path: project_path.clone(),
            canvas: ProjectSize::new(1, 1).unwrap(),
        };
        let recording = preparation.commit_crop(crop, worker).unwrap();
        let mut first_snapshot = recording.controller.trigger_snapshot();
        let mut boundary_snapshot = recording.controller.trigger_snapshot();
        let completion = loop {
            match recording
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            {
                JobMessage::Finished(completion) => break completion,
                JobMessage::Progress(_) | JobMessage::Persisting => {}
            }
        };
        let RecordingCompletion::Completed(project) = completion else {
            panic!("prepared recording did not complete successfully");
        };
        assert_eq!(signals.lock().unwrap().start_calls, 1);
        assert!(matches!(
            first_snapshot.status(),
            gif_from_screen_workflow::SnapshotTriggerStatus::Captured(receipt)
                if receipt.sequence() == 1 && receipt.captured_at().as_micros() == 100
        ));
        assert_eq!(
            boundary_snapshot.status(),
            gif_from_screen_workflow::SnapshotTriggerStatus::Rejected(
                gif_from_screen_workflow::SnapshotTriggerRejection::CollectionEnded
            )
        );
        assert_eq!(
            project.manifest().canvas.size,
            ProjectSize::new(1, 1).unwrap()
        );
        assert_eq!(project.manifest().timeline.frames.len(), 1);
        let clip = &project.manifest().timeline.frames[0];
        assert_eq!(
            project.assets().read(clip.asset_id).unwrap(),
            [20, 21, 22, 255]
        );
        assert_eq!(project.layout().root, project_path);
    }
}
