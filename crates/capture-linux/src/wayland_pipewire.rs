//! Native `PipeWire` video delivery for XDG `ScreenCast` sessions.

use std::{
    cell::RefCell,
    io::Cursor,
    os::fd::IntoRawFd,
    ptr::NonNull,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use gif_from_screen_capture::{
    BackendDescriptor, BackendStatus, CaptureBackend, CaptureCadence, CaptureCapabilities,
    CaptureError, CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSessionState,
    CaptureSource, CaptureSourceId, CaptureSourceKind, CaptureTarget, CaptureTimestamp,
    CapturedFrame, CursorCaptureMode, FramePoll, PhysicalRect, PhysicalSize, PixelFormat,
    RecoveryHint,
};
use libspa::{
    data::{ChunkFlags, DataType},
    pod::{ChoiceValue, Object, Property, PropertyFlags, Value, deserialize::PodDeserializer},
    utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle},
};
use pipewire::prelude::ListenerBuilderT as _;
use pipewire::{Context, MainLoop, properties};

use crate::wayland::{MONITOR_SOURCE_ID, WINDOW_SOURCE_ID};
use crate::{WaylandPortal, WaylandPortalCapabilities, WaylandPortalSession};

const FRAME_CHANNEL_CAPACITY: usize = 3;
const WORKER_START_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const PIPEWIRE_ITERATION: Duration = Duration::from_millis(10);
const MAX_NEGOTIATED_EDGE: u32 = 8_192;
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// Complete Wayland `CaptureBackend` backed by the portal and mapped `PipeWire` buffers.
#[derive(Clone, Debug)]
pub struct WaylandCaptureBackend {
    portal_capabilities: WaylandPortalCapabilities,
}

impl WaylandCaptureBackend {
    /// Connects to and probes the active XDG `ScreenCast` portal.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when the D-Bus session or portal frontend is unavailable.
    pub fn connect() -> Result<Self, CaptureError> {
        Ok(Self {
            portal_capabilities: WaylandPortal::connect()?.capabilities(),
        })
    }

    fn effective_cursor_mode(
        &self,
        requested: CursorCaptureMode,
    ) -> Result<CursorCaptureMode, CaptureError> {
        match requested {
            CursorCaptureMode::Automatic | CursorCaptureMode::Embedded
                if self.portal_capabilities.cursor_embedded =>
            {
                Ok(CursorCaptureMode::Embedded)
            }
            CursorCaptureMode::Automatic | CursorCaptureMode::Hidden
                if self.portal_capabilities.cursor_hidden =>
            {
                Ok(CursorCaptureMode::Hidden)
            }
            CursorCaptureMode::Automatic => Err(unsupported(
                "the portal offers neither embedded nor hidden cursor video",
            )),
            CursorCaptureMode::Metadata => Err(unsupported(
                "editable cursor metadata is not yet decoded by the PipeWire consumer",
            )),
            CursorCaptureMode::Embedded => Err(unsupported(
                "the portal does not offer embedded cursor video",
            )),
            CursorCaptureMode::Hidden => {
                Err(unsupported("the portal does not offer hidden cursor video"))
            }
            _ => Err(unsupported("unknown cursor mode")),
        }
    }
}

impl CaptureBackend for WaylandCaptureBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "wayland-portal-pipewire",
            display_name: "Wayland ScreenCast portal + PipeWire",
        }
    }

    fn status(&self) -> BackendStatus {
        BackendStatus::Ready
    }

    fn capabilities(&self) -> CaptureCapabilities {
        let mut capabilities = self.portal_capabilities.capture_capabilities();
        capabilities.cursor_metadata = gif_from_screen_capture::CapabilityStatus::Unavailable(
            "the portal may expose cursor metadata, but this consumer currently supports embedded cursor pixels only"
                .to_owned(),
        );
        capabilities
    }

    fn list_sources(&self) -> Result<Vec<CaptureSource>, CaptureError> {
        self.portal_capabilities.portal_sources()
    }

    fn start_session(
        &self,
        mut request: CaptureRequest,
    ) -> Result<Box<dyn CaptureSession>, CaptureError> {
        if matches!(request.cadence, CaptureCadence::OnInteraction) {
            return Err(unsupported(
                "interaction-triggered capture because Wayland has no passive global input feed",
            ));
        }
        let target = resolve_portal_target(&request.target, self.portal_capabilities)?;
        request.cursor = self.effective_cursor_mode(request.cursor)?;
        let portal = WaylandPortal::connect()?;
        let portal_session = portal.start_session(target.source_kind, request.cursor)?;
        WaylandCaptureSession::start(request, target, portal_session)
            .map(|session| Box::new(session) as Box<dyn CaptureSession>)
    }
}

#[derive(Clone, Debug)]
struct ResolvedPortalTarget {
    source_id: CaptureSourceId,
    source_kind: CaptureSourceKind,
    crop: Option<PhysicalRect>,
}

fn resolve_portal_target(
    target: &CaptureTarget,
    capabilities: WaylandPortalCapabilities,
) -> Result<ResolvedPortalTarget, CaptureError> {
    let (source_id, crop) = match target {
        CaptureTarget::Monitor(source) | CaptureTarget::Window(source) => (source, None),
        CaptureTarget::Region { source, region } => (source, Some(*region)),
        _ => return Err(invalid_request("unknown Wayland capture target")),
    };
    let source_kind = if source_id.as_str() == MONITOR_SOURCE_ID && capabilities.monitor {
        CaptureSourceKind::Monitor
    } else if source_id.as_str() == WINDOW_SOURCE_ID && capabilities.window {
        CaptureSourceKind::Window
    } else {
        return Err(CaptureError::new(
            CaptureErrorKind::SourceNotFound,
            format!("Wayland portal source {source_id} is not available"),
            RecoveryHint::ChooseDifferentSource,
        ));
    };
    match target {
        CaptureTarget::Monitor(_) if source_kind != CaptureSourceKind::Monitor => {
            return Err(invalid_request(
                "Wayland target kind does not match its portal source id",
            ));
        }
        CaptureTarget::Window(_) if source_kind != CaptureSourceKind::Window => {
            return Err(invalid_request(
                "Wayland target kind does not match its portal source id",
            ));
        }
        _ => {}
    }
    Ok(ResolvedPortalTarget {
        source_id: source_id.clone(),
        source_kind,
        crop,
    })
}

/// Live mapped-buffer `PipeWire` capture session.
pub struct WaylandCaptureSession {
    request: CaptureRequest,
    source_id: CaptureSourceId,
    source_kind: CaptureSourceKind,
    source_size: PhysicalSize,
    output_size: PhysicalSize,
    state: CaptureSessionState,
    commands: Sender<WorkerCommand>,
    frames: Receiver<CapturedFrame>,
    status: Receiver<WorkerStatus>,
    worker: Option<JoinHandle<Result<(), CaptureError>>>,
}

impl std::fmt::Debug for WaylandCaptureSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WaylandCaptureSession")
            .field("request", &self.request)
            .field("source_size", &self.source_size)
            .field("output_size", &self.output_size)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl WaylandCaptureSession {
    fn start(
        request: CaptureRequest,
        target: ResolvedPortalTarget,
        portal: WaylandPortalSession,
    ) -> Result<Self, CaptureError> {
        if portal
            .stream()
            .source_kind()
            .is_some_and(|selected| selected != target.source_kind)
        {
            return Err(CaptureError::new(
                CaptureErrorKind::SourceNotFound,
                "the portal returned a different source kind than was requested",
                RecoveryHint::ChooseDifferentSource,
            ));
        }
        let (command_tx, command_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(FRAME_CHANNEL_CAPACITY);
        let (status_tx, status_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::sync_channel(1);
        let cadence = CadenceGate::from_request(request.cadence)?;
        let requested_crop = target.crop;
        let worker = thread::Builder::new()
            .name("gif-from-screen-pipewire".to_owned())
            .spawn(move || {
                let result = run_pipewire_worker(
                    portal,
                    requested_crop,
                    cadence,
                    &command_rx,
                    frame_tx,
                    &status_tx,
                    &init_tx,
                );
                if let Err(error) = &result {
                    let _ = init_tx.try_send(Err(error.clone()));
                    let _ = status_tx.send(WorkerStatus::Failed(error.clone()));
                }
                result
            })
            .map_err(|error| platform_error("spawn PipeWire capture thread", &error))?;

        let initialized = match init_rx.recv_timeout(WORKER_START_TIMEOUT) {
            Ok(Ok(initialized)) => initialized,
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = command_tx.send(WorkerCommand::Shutdown { reply: None });
                // Do not defeat the explicit timeout by synchronously joining
                // a native call that may itself be stuck. The queued shutdown
                // closes the portal as soon as the worker can make progress.
                drop(worker);
                return Err(CaptureError::new(
                    CaptureErrorKind::Timeout,
                    "PipeWire did not negotiate a video format within 10 seconds",
                    RecoveryHint::Retry,
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                return Err(CaptureError::new(
                    CaptureErrorKind::Platform,
                    "PipeWire worker exited before reporting its video format",
                    RecoveryHint::Retry,
                ));
            }
        };

        Ok(Self {
            request,
            source_id: target.source_id,
            source_kind: target.source_kind,
            source_size: initialized.source_size,
            output_size: initialized.output_size,
            state: CaptureSessionState::Recording,
            commands: command_tx,
            frames: frame_rx,
            status: status_rx,
            worker: Some(worker),
        })
    }

    fn invalid_transition(&self, command: &str) -> CaptureError {
        CaptureError::new(
            CaptureErrorKind::InvalidStateTransition,
            format!(
                "cannot {command} a Wayland PipeWire session in {:?} state",
                self.state
            ),
            RecoveryHint::None,
        )
    }

    fn send_command(
        &self,
        command: impl FnOnce(SyncSender<Result<(), CaptureError>>) -> WorkerCommand,
    ) -> Result<(), CaptureError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.commands.send(command(reply_tx)).map_err(|_| {
            CaptureError::new(
                CaptureErrorKind::SourceLost,
                "PipeWire worker is no longer accepting commands",
                RecoveryHint::Retry,
            )
        })?;
        reply_rx
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => CaptureError::new(
                    CaptureErrorKind::Timeout,
                    "PipeWire worker did not acknowledge a session command",
                    RecoveryHint::Retry,
                ),
                RecvTimeoutError::Disconnected => CaptureError::new(
                    CaptureErrorKind::SourceLost,
                    "PipeWire worker exited while applying a session command",
                    RecoveryHint::Retry,
                ),
            })?
    }

    fn finish_worker(&mut self, discard: bool) -> Result<(), CaptureError> {
        self.state = CaptureSessionState::Stopping;
        let result = self.send_command(|reply| WorkerCommand::Shutdown { reply: Some(reply) });
        if result
            .as_ref()
            .is_err_and(|error| error.kind() == CaptureErrorKind::Timeout)
        {
            // A timed-out native worker must not make this API block forever.
            // Detaching is safe: the worker owns its channels, portal and fd,
            // and already has a queued shutdown request.
            self.worker.take();
            self.state = CaptureSessionState::Failed;
            return result;
        }
        if let Some(join_result) = self.worker.take().map(JoinHandle::join) {
            match join_result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.state = CaptureSessionState::Failed;
                    return Err(error);
                }
                Err(_) => {
                    self.state = CaptureSessionState::Failed;
                    return Err(CaptureError::new(
                        CaptureErrorKind::Platform,
                        "PipeWire worker panicked while shutting down",
                        RecoveryHint::Retry,
                    ));
                }
            }
        }
        if let Err(error) = result {
            let error = self.take_worker_error().unwrap_or(error);
            self.state = CaptureSessionState::Failed;
            return Err(error);
        }
        self.state = if discard {
            CaptureSessionState::Discarded
        } else {
            CaptureSessionState::Stopped
        };
        Ok(())
    }

    fn take_worker_error(&mut self) -> Option<CaptureError> {
        loop {
            match self.status.try_recv() {
                Ok(WorkerStatus::Failed(error)) => return Some(error),
                Ok(WorkerStatus::Stopped) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return None,
            }
        }
    }
}

impl CaptureSession for WaylandCaptureSession {
    fn state(&self) -> CaptureSessionState {
        self.state
    }

    fn request(&self) -> &CaptureRequest {
        &self.request
    }

    fn update_target(&mut self, target: CaptureTarget) -> Result<(), CaptureError> {
        if !matches!(
            self.state,
            CaptureSessionState::Recording | CaptureSessionState::Paused
        ) {
            return Err(self.invalid_transition("update the target of"));
        }
        if target == self.request.target {
            return Ok(());
        }
        let resolved = resolve_updated_target(&target, &self.source_id, self.source_kind)?;
        let output_size = resolved.map_or(self.source_size, PhysicalRect::size);
        if output_size != self.output_size {
            return Err(invalid_request(
                "Wayland capture target updates must preserve the output canvas size",
            ));
        }
        if resolved.is_some_and(|crop| !crop.fits_within(self.source_size)) {
            return Err(invalid_request(
                "updated Wayland crop is outside the negotiated PipeWire frame",
            ));
        }
        self.send_command(|reply| WorkerCommand::UpdateCrop {
            crop: resolved,
            reply,
        })?;
        self.request.target = target;
        Ok(())
    }

    fn pause(&mut self) -> Result<(), CaptureError> {
        if self.state != CaptureSessionState::Recording {
            return Err(self.invalid_transition("pause"));
        }
        self.send_command(|reply| WorkerCommand::SetActive {
            active: false,
            reply,
        })?;
        self.state = CaptureSessionState::Paused;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), CaptureError> {
        if self.state != CaptureSessionState::Paused {
            return Err(self.invalid_transition("resume"));
        }
        // The PipeWire worker is still inactive, so no producer can race this
        // drain. Frames queued between the preview poll and pause acknowledgement
        // belong to preparation time and must never become recording frames.
        drain_queued_frames(&self.frames);
        self.send_command(|reply| WorkerCommand::SetActive {
            active: true,
            reply,
        })?;
        self.state = CaptureSessionState::Recording;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), CaptureError> {
        if !matches!(
            self.state,
            CaptureSessionState::Recording | CaptureSessionState::Paused
        ) {
            return Err(self.invalid_transition("stop"));
        }
        self.finish_worker(false)
    }

    fn discard(&mut self) -> Result<(), CaptureError> {
        if !matches!(
            self.state,
            CaptureSessionState::Recording
                | CaptureSessionState::Paused
                | CaptureSessionState::Stopped
        ) {
            return Err(self.invalid_transition("discard"));
        }
        if self.worker.is_some() {
            self.finish_worker(true)
        } else {
            self.state = CaptureSessionState::Discarded;
            Ok(())
        }
    }

    fn poll_frame(&mut self, timeout: Duration) -> Result<FramePoll, CaptureError> {
        if let Some(error) = self.take_worker_error() {
            self.state = CaptureSessionState::Failed;
            return Err(error);
        }
        match self.state {
            CaptureSessionState::Recording => {}
            CaptureSessionState::Stopped
            | CaptureSessionState::Discarded
            | CaptureSessionState::Failed => return Ok(FramePoll::EndOfStream),
            _ => return Ok(FramePoll::Pending),
        }
        match self.frames.recv_timeout(timeout) {
            Ok(frame) => Ok(FramePoll::Frame(frame)),
            Err(RecvTimeoutError::Timeout) => Ok(FramePoll::Pending),
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(error) = self.take_worker_error() {
                    self.state = CaptureSessionState::Failed;
                    Err(error)
                } else {
                    self.state = CaptureSessionState::Failed;
                    Err(CaptureError::new(
                        CaptureErrorKind::SourceLost,
                        "PipeWire frame channel closed unexpectedly",
                        RecoveryHint::Retry,
                    ))
                }
            }
        }
    }
}

fn drain_queued_frames(frames: &Receiver<CapturedFrame>) {
    while frames.try_recv().is_ok() {}
}

impl Drop for WaylandCaptureSession {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.finish_worker(true);
        }
    }
}

fn resolve_updated_target(
    target: &CaptureTarget,
    source_id: &CaptureSourceId,
    source_kind: CaptureSourceKind,
) -> Result<Option<PhysicalRect>, CaptureError> {
    match target {
        CaptureTarget::Monitor(source)
            if source == source_id && source_kind == CaptureSourceKind::Monitor =>
        {
            Ok(None)
        }
        CaptureTarget::Window(source)
            if source == source_id && source_kind == CaptureSourceKind::Window =>
        {
            Ok(None)
        }
        CaptureTarget::Region { source, region } if source == source_id => Ok(Some(*region)),
        _ => Err(invalid_request(
            "a live Wayland session cannot switch portal-selected sources",
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawVideoFormat {
    Rgba,
    Bgra,
    Rgbx,
    Bgrx,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NegotiatedFormat {
    size: PhysicalSize,
    format: RawVideoFormat,
}

#[derive(Clone, Copy, Debug)]
struct WorkerInitialized {
    source_size: PhysicalSize,
    output_size: PhysicalSize,
}

enum WorkerCommand {
    SetActive {
        active: bool,
        reply: SyncSender<Result<(), CaptureError>>,
    },
    UpdateCrop {
        crop: Option<PhysicalRect>,
        reply: SyncSender<Result<(), CaptureError>>,
    },
    Shutdown {
        reply: Option<SyncSender<Result<(), CaptureError>>>,
    },
}

enum WorkerStatus {
    Failed(CaptureError),
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameDelivery {
    Delivered,
    Dropped,
    Disconnected,
}

#[derive(Clone, Copy, Debug)]
enum CadenceGate {
    EveryFrame,
    Periodic {
        period: Duration,
        next_due: Duration,
    },
}

impl CadenceGate {
    fn from_request(cadence: CaptureCadence) -> Result<Self, CaptureError> {
        match cadence {
            CaptureCadence::FixedFps(fps) => Ok(Self::Periodic {
                period: Duration::from_nanos((1_000_000_000 / u64::from(fps.get())).max(1)),
                next_due: Duration::ZERO,
            }),
            CaptureCadence::Interval(period) if period.is_zero() => Err(invalid_request(
                "capture interval must be greater than zero",
            )),
            CaptureCadence::Interval(period) => Ok(Self::Periodic {
                period,
                next_due: Duration::ZERO,
            }),
            CaptureCadence::Manual => Ok(Self::EveryFrame),
            CaptureCadence::OnInteraction => Err(unsupported("interaction cadence")),
            _ => Err(invalid_request("unknown capture cadence")),
        }
    }

    fn should_emit(&mut self, elapsed: Duration) -> bool {
        match self {
            Self::EveryFrame => true,
            Self::Periodic { period, next_due } if elapsed >= *next_due => {
                // Advance from the previous deadline, skipping whole missed
                // periods in one step. Anchoring to the deadline avoids the
                // cumulative drift caused by `elapsed + period` when buffers
                // consistently arrive a little late.
                let late = elapsed.saturating_sub(*next_due);
                let periods = late.as_nanos() / period.as_nanos() + 1;
                let advanced = next_due
                    .as_nanos()
                    .saturating_add(period.as_nanos().saturating_mul(periods));
                *next_due = duration_from_nanos_saturating(advanced);
                true
            }
            Self::Periodic { .. } => false,
        }
    }

    fn resume_now(&mut self, elapsed: Duration) {
        if let Self::Periodic { next_due, .. } = self {
            *next_due = elapsed;
        }
    }
}

fn duration_from_nanos_saturating(nanos: u128) -> Duration {
    const NANOS_PER_SECOND: u128 = 1_000_000_000;
    let seconds = nanos / NANOS_PER_SECOND;
    let Ok(seconds) = u64::try_from(seconds) else {
        return Duration::MAX;
    };
    let subsecond_nanos = u32::try_from(nanos % NANOS_PER_SECOND).unwrap_or(999_999_999);
    Duration::new(seconds, subsecond_nanos)
}

#[derive(Debug)]
struct ActiveClock {
    started_at: Instant,
    paused_at: Option<Instant>,
    accumulated_pause: Duration,
}

impl ActiveClock {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            paused_at: None,
            accumulated_pause: Duration::ZERO,
        }
    }

    fn pause(&mut self, now: Instant) {
        self.paused_at.get_or_insert(now);
    }

    fn resume(&mut self, now: Instant) {
        if let Some(paused_at) = self.paused_at.take() {
            self.accumulated_pause = self
                .accumulated_pause
                .saturating_add(now.saturating_duration_since(paused_at));
        }
    }

    fn elapsed(&self, now: Instant) -> Duration {
        let effective_now = self.paused_at.unwrap_or(now);
        effective_now
            .saturating_duration_since(self.started_at)
            .saturating_sub(self.accumulated_pause)
    }
}

struct WorkerData {
    negotiated: Option<NegotiatedFormat>,
    requested_crop: Option<PhysicalRect>,
    crop: Rc<RefCell<Option<PhysicalRect>>>,
    clock: Rc<RefCell<ActiveClock>>,
    cadence: Rc<RefCell<CadenceGate>>,
    sequence: u64,
    frames: SyncSender<CapturedFrame>,
    status: Sender<WorkerStatus>,
    initialized: SyncSender<Result<WorkerInitialized, CaptureError>>,
    init_sent: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
}

fn run_pipewire_worker(
    mut portal: WaylandPortalSession,
    requested_crop: Option<PhysicalRect>,
    cadence: CadenceGate,
    commands: &Receiver<WorkerCommand>,
    frames: SyncSender<CapturedFrame>,
    status: &Sender<WorkerStatus>,
    initialized: &SyncSender<Result<WorkerInitialized, CaptureError>>,
) -> Result<(), CaptureError> {
    let node_id = portal.stream().node_id();
    let remote = portal.take_pipewire_remote().ok_or_else(|| {
        platform_error(
            "take PipeWire portal remote",
            &"descriptor was already taken",
        )
    })?;
    pipewire::init();
    let main_loop = MainLoop::new().map_err(|error| pipewire_error("create main loop", &error))?;
    let context =
        Context::new(&main_loop).map_err(|error| pipewire_error("create context", &error))?;
    let core = context
        .connect_fd(remote.into_raw_fd(), None)
        .map_err(|error| pipewire_error("connect portal remote", &error))?;
    let mut stream = pipewire::stream::Stream::<()>::new(
        &core,
        "gif-from-screen-wayland",
        properties! {
            *pipewire::keys::MEDIA_TYPE => "Video",
            *pipewire::keys::MEDIA_CATEGORY => "Capture",
            *pipewire::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|error| pipewire_error("create video stream", &error))?;

    let crop = Rc::new(RefCell::new(requested_crop));
    let clock = Rc::new(RefCell::new(ActiveClock::new()));
    let cadence = Rc::new(RefCell::new(cadence));
    let init_sent = Arc::new(AtomicBool::new(false));
    let terminal = Arc::new(AtomicBool::new(false));
    // pipewire-rs 0.6 accidentally constrains its listener builder to default
    // user data even when data is supplied explicitly. Keep our real state in
    // callback-local `Rc<RefCell<_>>` storage and use the trivial `()` stream
    // data type so this remains compatible with PipeWire 0.3.48.
    let worker_data = Rc::new(RefCell::new(WorkerData {
        negotiated: None,
        requested_crop,
        crop: crop.clone(),
        clock: clock.clone(),
        cadence: cadence.clone(),
        sequence: 0,
        frames,
        status: status.clone(),
        initialized: initialized.clone(),
        init_sent: init_sent.clone(),
        terminal: terminal.clone(),
    }));
    let listener = register_stream_listener(
        &mut stream,
        worker_data,
        terminal.clone(),
        init_sent.clone(),
        initialized.clone(),
        status.clone(),
    )?;

    let negotiation = serialize_format_offer(preferred_frame_rate(*cadence.borrow()))?;
    let mut parameters = [spa_pod_pointer(&negotiation)?];
    stream
        .connect(
            libspa::Direction::Input,
            Some(node_id),
            pipewire::stream::StreamFlags::AUTOCONNECT
                | pipewire::stream::StreamFlags::MAP_BUFFERS
                | pipewire::stream::StreamFlags::DONT_RECONNECT,
            &mut parameters,
        )
        .map_err(|error| pipewire_error("connect video stream", &error))?;

    while !terminal.load(Ordering::Acquire) {
        handle_worker_commands(commands, &stream, &crop, &clock, &cadence, &terminal);
        if !terminal.load(Ordering::Acquire) {
            main_loop.iterate(PIPEWIRE_ITERATION);
        }
    }
    let _ = stream.disconnect();
    drop(listener);
    drop(stream);
    drop(core);
    drop(context);
    drop(main_loop);
    portal.close()?;
    let _ = status.send(WorkerStatus::Stopped);
    Ok(())
}

fn register_stream_listener(
    stream: &mut pipewire::stream::Stream<()>,
    worker_data: Rc<RefCell<WorkerData>>,
    terminal: Arc<AtomicBool>,
    init_sent: Arc<AtomicBool>,
    initialized: SyncSender<Result<WorkerInitialized, CaptureError>>,
    status: Sender<WorkerStatus>,
) -> Result<pipewire::stream::StreamListener<()>, CaptureError> {
    let parameter_data = worker_data.clone();
    let process_data = worker_data;
    stream
        .add_local_listener()
        .state_changed(move |_old, new| {
            handle_stream_state(new, &terminal, &init_sent, &initialized, &status);
        })
        .param_changed(move |id, _stream_data, parameter| {
            handle_format_parameter(&parameter_data, id, parameter);
        })
        .process(move |stream, _stream_data| {
            process_pipewire_frame(stream, &process_data);
        })
        .register()
        .map_err(|error| pipewire_error("register stream callbacks", &error))
}

fn handle_stream_state(
    state: pipewire::stream::StreamState,
    terminal: &AtomicBool,
    init_sent: &AtomicBool,
    initialized: &SyncSender<Result<WorkerInitialized, CaptureError>>,
    status: &Sender<WorkerStatus>,
) {
    let error = match state {
        pipewire::stream::StreamState::Error(message) => Some(CaptureError::new(
            CaptureErrorKind::SourceLost,
            format!("PipeWire stream entered error state: {message}"),
            RecoveryHint::Retry,
        )),
        pipewire::stream::StreamState::Unconnected if init_sent.load(Ordering::Acquire) => {
            Some(CaptureError::new(
                CaptureErrorKind::SourceLost,
                "PipeWire source disconnected during capture",
                RecoveryHint::Retry,
            ))
        }
        _ => None,
    };
    if let Some(error) = error {
        signal_terminal(terminal, init_sent, initialized, status, error);
    }
}

fn handle_format_parameter(
    worker_data: &RefCell<WorkerData>,
    id: u32,
    parameter: *const libspa_sys::spa_pod,
) {
    if id != libspa_sys::SPA_PARAM_Format || parameter.is_null() {
        return;
    }
    let mut data = worker_data.borrow_mut();
    let result = parse_negotiated_format(parameter).and_then(|format| {
        validate_negotiated_format(format, data.requested_crop)?;
        if let Some(previous) = data.negotiated
            && previous != format
        {
            return Err(CaptureError::new(
                CaptureErrorKind::SourceLost,
                "PipeWire changed video format during a fixed-canvas session",
                RecoveryHint::Retry,
            ));
        }
        data.negotiated = Some(format);
        let output_size = data.requested_crop.map_or(format.size, PhysicalRect::size);
        if !data.init_sent.swap(true, Ordering::AcqRel) {
            let _ = data.initialized.try_send(Ok(WorkerInitialized {
                source_size: format.size,
                output_size,
            }));
        }
        Ok(())
    });
    if let Err(error) = result {
        signal_terminal(
            &data.terminal,
            &data.init_sent,
            &data.initialized,
            &data.status,
            error,
        );
    }
}

fn process_pipewire_frame(
    stream: &pipewire::stream::Stream<()>,
    worker_data: &RefCell<WorkerData>,
) {
    let mut data = worker_data.borrow_mut();
    if data.terminal.load(Ordering::Acquire) {
        return;
    }
    let Some(format) = data.negotiated else {
        return;
    };
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(plane) = buffer.datas_mut().first_mut() else {
        signal_terminal(
            &data.terminal,
            &data.init_sent,
            &data.initialized,
            &data.status,
            CaptureError::invalid_frame("PipeWire video buffer has no data plane"),
        );
        return;
    };
    let now = Instant::now();
    let elapsed = data.clock.borrow().elapsed(now);
    if !data.cadence.borrow_mut().should_emit(elapsed) {
        return;
    }
    let crop = *data.crop.borrow();
    let result = mapped_plane_to_frame(
        plane,
        format,
        crop,
        data.sequence,
        CaptureTimestamp::from_micros(u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)),
    );
    let Some(sequence) = data.sequence.checked_add(1) else {
        signal_terminal(
            &data.terminal,
            &data.init_sent,
            &data.initialized,
            &data.status,
            CaptureError::new(
                CaptureErrorKind::Platform,
                "PipeWire frame sequence overflowed u64",
                RecoveryHint::None,
            ),
        );
        return;
    };
    data.sequence = sequence;
    match result {
        Ok(frame) => {
            if try_deliver_frame(&data.frames, frame) == FrameDelivery::Disconnected {
                data.terminal.store(true, Ordering::Release);
            }
        }
        Err(error) => signal_terminal(
            &data.terminal,
            &data.init_sent,
            &data.initialized,
            &data.status,
            error,
        ),
    }
}

fn try_deliver_frame(frames: &SyncSender<CapturedFrame>, frame: CapturedFrame) -> FrameDelivery {
    match frames.try_send(frame) {
        Ok(()) => FrameDelivery::Delivered,
        Err(TrySendError::Full(_)) => FrameDelivery::Dropped,
        Err(TrySendError::Disconnected(_)) => FrameDelivery::Disconnected,
    }
}

fn handle_worker_commands(
    commands: &Receiver<WorkerCommand>,
    stream: &pipewire::stream::Stream<()>,
    crop: &Rc<RefCell<Option<PhysicalRect>>>,
    clock: &Rc<RefCell<ActiveClock>>,
    cadence: &Rc<RefCell<CadenceGate>>,
    terminal: &AtomicBool,
) {
    loop {
        let command = match commands.try_recv() {
            Ok(command) => command,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                terminal.store(true, Ordering::Release);
                return;
            }
        };
        match command {
            WorkerCommand::SetActive { active, reply } => {
                let result = stream
                    .set_active(active)
                    .map_err(|error| pipewire_error("change stream activity", &error));
                if result.is_ok() {
                    let now = Instant::now();
                    if active {
                        clock.borrow_mut().resume(now);
                        let elapsed = clock.borrow().elapsed(now);
                        cadence.borrow_mut().resume_now(elapsed);
                    } else {
                        clock.borrow_mut().pause(now);
                    }
                }
                let _ = reply.try_send(result);
            }
            WorkerCommand::UpdateCrop {
                crop: updated,
                reply,
            } => {
                *crop.borrow_mut() = updated;
                let _ = reply.try_send(Ok(()));
            }
            WorkerCommand::Shutdown { reply } => {
                // Suppress the normal Unconnected callback before requesting
                // disconnect so an intentional stop is not reported as loss.
                terminal.store(true, Ordering::Release);
                let result = stream
                    .disconnect()
                    .map_err(|error| pipewire_error("disconnect video stream", &error));
                if let Some(reply) = reply {
                    let _ = reply.try_send(result);
                }
                return;
            }
        }
    }
}

fn signal_terminal(
    terminal: &AtomicBool,
    init_sent: &AtomicBool,
    initialized: &SyncSender<Result<WorkerInitialized, CaptureError>>,
    status: &Sender<WorkerStatus>,
    error: CaptureError,
) {
    if terminal.swap(true, Ordering::AcqRel) {
        return;
    }
    if !init_sent.swap(true, Ordering::AcqRel) {
        let _ = initialized.try_send(Err(error.clone()));
    }
    let _ = status.send(WorkerStatus::Failed(error));
}

fn mapped_plane_to_frame(
    plane: &mut libspa::data::Data,
    format: NegotiatedFormat,
    crop: Option<PhysicalRect>,
    sequence: u64,
    captured_at: CaptureTimestamp,
) -> Result<CapturedFrame, CaptureError> {
    match plane.type_() {
        DataType::MemPtr | DataType::MemFd => {}
        DataType::DmaBuf => {
            return Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                "PipeWire supplied a DMA-BUF frame; this build currently supports mapped MemPtr/MemFd buffers only",
                RecoveryHint::ChangeRequest,
            ));
        }
        other => {
            return Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                format!("PipeWire supplied unsupported buffer memory type {other:?}"),
                RecoveryHint::ChangeRequest,
            ));
        }
    }
    if plane.as_raw().chunk.is_null() {
        return Err(CaptureError::invalid_frame(
            "PipeWire video plane has no chunk metadata",
        ));
    }
    let chunk = plane.chunk();
    if chunk.flags().contains(ChunkFlags::CORRUPTED) {
        return Err(CaptureError::invalid_frame(
            "PipeWire marked the video buffer as corrupted",
        ));
    }
    let offset = usize::try_from(chunk.offset())
        .map_err(|_| CaptureError::invalid_frame("PipeWire chunk offset exceeds usize"))?;
    let size = usize::try_from(chunk.size())
        .map_err(|_| CaptureError::invalid_frame("PipeWire chunk size exceeds usize"))?;
    let stride = if chunk.stride() == 0 {
        usize::try_from(format.size.width())
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| CaptureError::invalid_frame("PipeWire row size overflowed"))?
    } else {
        usize::try_from(chunk.stride())
            .map_err(|_| CaptureError::invalid_frame("PipeWire supplied a negative video stride"))?
    };
    let mapped = plane.data().ok_or_else(|| {
        CaptureError::new(
            CaptureErrorKind::UnsupportedCapability,
            "PipeWire buffer was not CPU-mapped; DMA-BUF-only capture is not supported yet",
            RecoveryHint::ChangeRequest,
        )
    })?;
    let end = offset
        .checked_add(size)
        .ok_or_else(|| CaptureError::invalid_frame("PipeWire chunk range overflowed"))?;
    let bytes = mapped.get(offset..end).ok_or_else(|| {
        CaptureError::invalid_frame("PipeWire chunk range exceeds its mapped buffer")
    })?;
    let (pixels, output_size) = convert_raw_frame(bytes, stride, format, crop)?;
    let output_stride = usize::try_from(output_size.width())
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| CaptureError::invalid_frame("output RGBA stride overflowed"))?;
    CapturedFrame::new(
        sequence,
        captured_at,
        output_size,
        output_stride,
        PixelFormat::Rgba8,
        pixels,
    )
}

fn convert_raw_frame(
    bytes: &[u8],
    source_stride: usize,
    format: NegotiatedFormat,
    crop: Option<PhysicalRect>,
) -> Result<(Vec<u8>, PhysicalSize), CaptureError> {
    let source_width = usize::try_from(format.size.width())
        .map_err(|_| CaptureError::invalid_frame("source width exceeds usize"))?;
    let source_height = usize::try_from(format.size.height())
        .map_err(|_| CaptureError::invalid_frame("source height exceeds usize"))?;
    let minimum_stride = source_width
        .checked_mul(4)
        .ok_or_else(|| CaptureError::invalid_frame("source row size overflowed"))?;
    if source_stride < minimum_stride {
        return Err(CaptureError::invalid_frame(format!(
            "PipeWire stride {source_stride} is smaller than {minimum_stride}"
        )));
    }
    let required = source_stride
        .checked_mul(source_height)
        .ok_or_else(|| CaptureError::invalid_frame("source buffer size overflowed"))?;
    if bytes.len() < required {
        return Err(CaptureError::invalid_frame(format!(
            "PipeWire chunk has {} bytes, but {required} are required",
            bytes.len()
        )));
    }
    let crop = match crop {
        Some(crop) => crop,
        None => PhysicalRect::new(0, 0, format.size.width(), format.size.height())?,
    };
    if !crop.fits_within(format.size) {
        return Err(invalid_request(
            "Wayland crop is outside the negotiated PipeWire frame",
        ));
    }
    let output_size = crop.size();
    let output_width = usize::try_from(output_size.width())
        .map_err(|_| CaptureError::invalid_frame("crop width exceeds usize"))?;
    let output_height = usize::try_from(output_size.height())
        .map_err(|_| CaptureError::invalid_frame("crop height exceeds usize"))?;
    let output_len = output_width
        .checked_mul(output_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| CaptureError::invalid_frame("cropped RGBA size overflowed"))?;
    if output_len > MAX_FRAME_BYTES {
        return Err(CaptureError::invalid_frame(format!(
            "cropped frame requires {output_len} bytes, above the {MAX_FRAME_BYTES}-byte limit"
        )));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_len)
        .map_err(|_| CaptureError::invalid_frame("could not allocate bounded RGBA frame"))?;
    let crop_x = usize::try_from(crop.origin().x)
        .map_err(|_| invalid_request("Wayland crop x is negative"))?;
    let crop_y = usize::try_from(crop.origin().y)
        .map_err(|_| invalid_request("Wayland crop y is negative"))?;
    let crop_byte_x = crop_x
        .checked_mul(4)
        .ok_or_else(|| CaptureError::invalid_frame("crop x byte offset overflowed"))?;
    let output_row_bytes = output_width
        .checked_mul(4)
        .ok_or_else(|| CaptureError::invalid_frame("crop row length overflowed"))?;
    for y in 0..output_height {
        let row_start = (crop_y + y)
            .checked_mul(source_stride)
            .and_then(|row| row.checked_add(crop_byte_x))
            .ok_or_else(|| CaptureError::invalid_frame("crop row offset overflowed"))?;
        let row_end = row_start
            .checked_add(output_row_bytes)
            .ok_or_else(|| CaptureError::invalid_frame("crop row end overflowed"))?;
        let row = bytes
            .get(row_start..row_end)
            .ok_or_else(|| CaptureError::invalid_frame("crop row exceeds source buffer"))?;
        for pixel in row.as_chunks::<4>().0 {
            output.extend_from_slice(&format.format.to_rgba(*pixel));
        }
    }
    Ok((output, output_size))
}

impl RawVideoFormat {
    const fn to_rgba(self, pixel: [u8; 4]) -> [u8; 4] {
        match self {
            Self::Rgba => pixel,
            Self::Bgra => [pixel[2], pixel[1], pixel[0], pixel[3]],
            Self::Rgbx => [pixel[0], pixel[1], pixel[2], 255],
            Self::Bgrx => [pixel[2], pixel[1], pixel[0], 255],
        }
    }
}

fn validate_negotiated_format(
    format: NegotiatedFormat,
    crop: Option<PhysicalRect>,
) -> Result<(), CaptureError> {
    if format.size.width() > MAX_NEGOTIATED_EDGE || format.size.height() > MAX_NEGOTIATED_EDGE {
        return Err(CaptureError::invalid_frame(format!(
            "PipeWire negotiated {}x{}, above the {MAX_NEGOTIATED_EDGE}-pixel edge limit",
            format.size.width(),
            format.size.height()
        )));
    }
    let frame_bytes = usize::try_from(format.size.width())
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(format.size.height()).ok()?))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| CaptureError::invalid_frame("negotiated frame byte length overflowed"))?;
    if frame_bytes > MAX_FRAME_BYTES {
        return Err(CaptureError::invalid_frame(format!(
            "PipeWire frame requires {frame_bytes} bytes, above the {MAX_FRAME_BYTES}-byte limit"
        )));
    }
    if crop.is_some_and(|crop| !crop.fits_within(format.size)) {
        return Err(invalid_request(
            "requested Wayland crop is outside the negotiated PipeWire frame",
        ));
    }
    Ok(())
}

fn preferred_frame_rate(cadence: CadenceGate) -> u32 {
    match cadence {
        CadenceGate::EveryFrame => 60,
        CadenceGate::Periodic { period, .. } => {
            let nanos = period.as_nanos().max(1);
            u32::try_from((1_000_000_000_u128 / nanos).clamp(1, 240)).unwrap_or(60)
        }
    }
}

fn serialize_format_offer(frame_rate: u32) -> Result<Vec<u8>, CaptureError> {
    let property = |key, value| Property {
        key,
        flags: PropertyFlags::empty(),
        value,
    };
    let value = Value::Object(Object {
        type_: libspa_sys::SPA_TYPE_OBJECT_Format,
        id: libspa_sys::SPA_PARAM_EnumFormat,
        properties: vec![
            property(
                libspa_sys::SPA_FORMAT_mediaType,
                Value::Id(Id(libspa_sys::SPA_MEDIA_TYPE_video)),
            ),
            property(
                libspa_sys::SPA_FORMAT_mediaSubtype,
                Value::Id(Id(libspa_sys::SPA_MEDIA_SUBTYPE_raw)),
            ),
            property(
                libspa_sys::SPA_FORMAT_VIDEO_format,
                Value::Choice(ChoiceValue::Id(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: Id(libspa_sys::SPA_VIDEO_FORMAT_BGRx),
                        alternatives: vec![
                            Id(libspa_sys::SPA_VIDEO_FORMAT_BGRA),
                            Id(libspa_sys::SPA_VIDEO_FORMAT_RGBA),
                            Id(libspa_sys::SPA_VIDEO_FORMAT_RGBx),
                        ],
                    },
                ))),
            ),
            property(
                libspa_sys::SPA_FORMAT_VIDEO_size,
                Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Rectangle {
                            width: 1_920,
                            height: 1_080,
                        },
                        min: Rectangle {
                            width: 1,
                            height: 1,
                        },
                        max: Rectangle {
                            width: MAX_NEGOTIATED_EDGE,
                            height: MAX_NEGOTIATED_EDGE,
                        },
                    },
                ))),
            ),
            property(
                libspa_sys::SPA_FORMAT_VIDEO_framerate,
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: Fraction {
                            num: frame_rate,
                            denom: 1,
                        },
                        min: Fraction { num: 0, denom: 1 },
                        max: Fraction { num: 240, denom: 1 },
                    },
                ))),
            ),
        ],
    });
    libspa::pod::serialize::PodSerializer::serialize(Cursor::new(Vec::new()), &value)
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|error| {
            CaptureError::new(
                CaptureErrorKind::Platform,
                format!("could not serialize PipeWire format offer: {error:?}"),
                RecoveryHint::Retry,
            )
        })
}

#[allow(
    clippy::cast_ptr_alignment,
    reason = "the serializer returns bytes, so alignment is checked before exposing its pod pointer"
)]
fn spa_pod_pointer(bytes: &[u8]) -> Result<*const libspa_sys::spa_pod, CaptureError> {
    if bytes.len() < std::mem::size_of::<libspa_sys::spa_pod>() {
        return Err(CaptureError::new(
            CaptureErrorKind::Platform,
            "serialized SPA format pod is truncated",
            RecoveryHint::Retry,
        ));
    }
    let pointer = bytes.as_ptr().cast::<libspa_sys::spa_pod>();
    if !pointer.is_aligned() {
        return Err(CaptureError::new(
            CaptureErrorKind::Platform,
            "serialized SPA format pod is not naturally aligned",
            RecoveryHint::Retry,
        ));
    }
    Ok(pointer)
}

#[allow(unsafe_code)]
fn parse_negotiated_format(
    parameter: *const libspa_sys::spa_pod,
) -> Result<NegotiatedFormat, CaptureError> {
    let pointer = NonNull::new(parameter.cast_mut())
        .ok_or_else(|| CaptureError::invalid_frame("PipeWire supplied a null format pod"))?;
    if !pointer.as_ptr().is_aligned() {
        return Err(CaptureError::invalid_frame(
            "PipeWire supplied a misaligned format pod",
        ));
    }
    // SAFETY: `parameter` comes directly from PipeWire's `param_changed`
    // callback. PipeWire guarantees it points to a complete, naturally aligned
    // `spa_pod` for the duration of that callback. We deserialize immediately
    // into an owned `Value`, retain no borrowed data or pointer, and never free
    // or mutate the PipeWire-owned pod.
    let value = unsafe { PodDeserializer::deserialize_ptr::<Value>(pointer) }.map_err(|error| {
        CaptureError::invalid_frame(format!("invalid SPA format pod: {error:?}"))
    })?;
    parse_format_value(&value)
}

fn parse_format_value(value: &Value) -> Result<NegotiatedFormat, CaptureError> {
    let Value::Object(object) = value else {
        return Err(CaptureError::invalid_frame(
            "PipeWire format parameter is not an SPA object",
        ));
    };
    if object.type_ != libspa_sys::SPA_TYPE_OBJECT_Format {
        return Err(CaptureError::invalid_frame(
            "PipeWire parameter is not an SPA format object",
        ));
    }
    let id = |key| {
        object
            .properties
            .iter()
            .find(|property| property.key == key)
            .and_then(|property| match property.value {
                Value::Id(Id(value)) => Some(value),
                _ => None,
            })
    };
    if id(libspa_sys::SPA_FORMAT_mediaType) != Some(libspa_sys::SPA_MEDIA_TYPE_video)
        || id(libspa_sys::SPA_FORMAT_mediaSubtype) != Some(libspa_sys::SPA_MEDIA_SUBTYPE_raw)
    {
        return Err(unsupported("non-raw-video PipeWire format"));
    }
    let format = match id(libspa_sys::SPA_FORMAT_VIDEO_format) {
        Some(libspa_sys::SPA_VIDEO_FORMAT_RGBA) => RawVideoFormat::Rgba,
        Some(libspa_sys::SPA_VIDEO_FORMAT_BGRA) => RawVideoFormat::Bgra,
        Some(libspa_sys::SPA_VIDEO_FORMAT_RGBx) => RawVideoFormat::Rgbx,
        Some(libspa_sys::SPA_VIDEO_FORMAT_BGRx) => RawVideoFormat::Bgrx,
        Some(other) => {
            return Err(unsupported(format!(
                "negotiated PipeWire video format id {other}"
            )));
        }
        None => {
            return Err(CaptureError::invalid_frame(
                "PipeWire format omitted its pixel format",
            ));
        }
    };
    let dimensions = object
        .properties
        .iter()
        .find(|property| property.key == libspa_sys::SPA_FORMAT_VIDEO_size)
        .and_then(|property| match property.value {
            Value::Rectangle(size) => Some(size),
            _ => None,
        })
        .ok_or_else(|| CaptureError::invalid_frame("PipeWire format omitted its video size"))?;
    Ok(NegotiatedFormat {
        size: PhysicalSize::new(dimensions.width, dimensions.height)?,
        format,
    })
}

fn invalid_request(message: impl Into<String>) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::InvalidRequest,
        message,
        RecoveryHint::ChangeRequest,
    )
}

fn unsupported(message: impl Into<String>) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::UnsupportedCapability,
        message,
        RecoveryHint::ChangeRequest,
    )
}

fn pipewire_error(operation: &str, error: &impl std::fmt::Display) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::Platform,
        format!("failed to {operation} through PipeWire: {error}"),
        RecoveryHint::Retry,
    )
}

fn platform_error(operation: &str, error: &impl std::fmt::Display) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::Platform,
        format!("failed to {operation}: {error}"),
        RecoveryHint::Retry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(size: PhysicalSize, format: RawVideoFormat) -> NegotiatedFormat {
        NegotiatedFormat { size, format }
    }

    fn captured_frame(sequence: u64, micros: u64) -> CapturedFrame {
        CapturedFrame::new(
            sequence,
            CaptureTimestamp::from_micros(micros),
            PhysicalSize::new(1, 1).unwrap(),
            4,
            PixelFormat::Rgba8,
            [0, 0, 0, 255],
        )
        .unwrap()
    }

    fn fake_session(initial_region: PhysicalRect) -> WaylandCaptureSession {
        let source_id = CaptureSourceId::new(MONITOR_SOURCE_ID).unwrap();
        let request = CaptureRequest::new(
            CaptureTarget::Region {
                source: source_id.clone(),
                region: initial_region,
            },
            CaptureCadence::fixed_fps(30).unwrap(),
        );
        let (command_tx, command_rx) = mpsc::channel();
        let (_frame_tx, frame_rx) = mpsc::sync_channel(1);
        let (_status_tx, status_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(command) = command_rx.recv() {
                match command {
                    WorkerCommand::SetActive { reply, .. }
                    | WorkerCommand::UpdateCrop { reply, .. } => {
                        let _ = reply.try_send(Ok(()));
                    }
                    WorkerCommand::Shutdown { reply } => {
                        if let Some(reply) = reply {
                            let _ = reply.try_send(Ok(()));
                        }
                        break;
                    }
                }
            }
            Ok(())
        });
        WaylandCaptureSession {
            request,
            source_id,
            source_kind: CaptureSourceKind::Monitor,
            source_size: PhysicalSize::new(1_920, 1_080).unwrap(),
            output_size: initial_region.size(),
            state: CaptureSessionState::Recording,
            commands: command_tx,
            frames: frame_rx,
            status: status_rx,
            worker: Some(worker),
        }
    }

    fn capabilities() -> WaylandPortalCapabilities {
        WaylandPortalCapabilities {
            version: 5,
            monitor: true,
            window: true,
            cursor_hidden: true,
            cursor_embedded: true,
            cursor_metadata: true,
        }
    }

    #[test]
    fn portal_target_resolution_preserves_a_movable_region() {
        let source = CaptureSourceId::new(MONITOR_SOURCE_ID).unwrap();
        let first = PhysicalRect::new(10, 20, 320, 180).unwrap();
        let target = CaptureTarget::Region {
            source: source.clone(),
            region: first,
        };
        let resolved = resolve_portal_target(&target, capabilities()).unwrap();
        assert_eq!(resolved.source_kind, CaptureSourceKind::Monitor);
        assert_eq!(resolved.crop, Some(first));

        let moved = PhysicalRect::new(400, 250, 320, 180).unwrap();
        assert_eq!(
            resolve_updated_target(
                &CaptureTarget::Region {
                    source,
                    region: moved,
                },
                &resolved.source_id,
                resolved.source_kind,
            )
            .unwrap(),
            Some(moved)
        );
    }

    #[test]
    fn portal_target_kind_and_source_switches_are_rejected() {
        let monitor = CaptureSourceId::new(MONITOR_SOURCE_ID).unwrap();
        let window = CaptureSourceId::new(WINDOW_SOURCE_ID).unwrap();
        let mismatch =
            resolve_portal_target(&CaptureTarget::Window(monitor.clone()), capabilities())
                .unwrap_err();
        assert_eq!(mismatch.kind(), CaptureErrorKind::InvalidRequest);

        let switched = resolve_updated_target(
            &CaptureTarget::Window(window),
            &monitor,
            CaptureSourceKind::Monitor,
        )
        .unwrap_err();
        assert_eq!(switched.kind(), CaptureErrorKind::InvalidRequest);
    }

    #[test]
    fn portable_session_state_and_dynamic_crop_contract_are_preserved() {
        let initial = PhysicalRect::new(10, 20, 320, 180).unwrap();
        let mut session = fake_session(initial);
        session.pause().unwrap();
        assert_eq!(session.state(), CaptureSessionState::Paused);
        assert_eq!(
            session.poll_frame(Duration::ZERO).unwrap(),
            FramePoll::Pending
        );
        session.resume().unwrap();

        let moved = CaptureTarget::Region {
            source: CaptureSourceId::new(MONITOR_SOURCE_ID).unwrap(),
            region: PhysicalRect::new(400, 250, 320, 180).unwrap(),
        };
        session.update_target(moved.clone()).unwrap();
        assert_eq!(session.request().target, moved);

        let resized = CaptureTarget::Region {
            source: CaptureSourceId::new(MONITOR_SOURCE_ID).unwrap(),
            region: PhysicalRect::new(400, 250, 321, 180).unwrap(),
        };
        assert_eq!(
            session.update_target(resized).unwrap_err().kind(),
            CaptureErrorKind::InvalidRequest
        );
        assert_eq!(session.request().target, moved);

        session.stop().unwrap();
        assert_eq!(session.state(), CaptureSessionState::Stopped);
        session.discard().unwrap();
        assert_eq!(session.state(), CaptureSessionState::Discarded);
    }

    #[test]
    fn live_discard_stops_the_worker_and_marks_output_discarded() {
        let mut session = fake_session(PhysicalRect::new(0, 0, 10, 10).unwrap());
        session.discard().unwrap();
        assert_eq!(session.state(), CaptureSessionState::Discarded);
        assert_eq!(
            session.poll_frame(Duration::ZERO).unwrap(),
            FramePoll::EndOfStream
        );
    }

    #[test]
    fn cursor_selection_is_explicit_and_metadata_is_not_overclaimed() {
        let backend = WaylandCaptureBackend {
            portal_capabilities: capabilities(),
        };
        assert_eq!(
            backend
                .effective_cursor_mode(CursorCaptureMode::Automatic)
                .unwrap(),
            CursorCaptureMode::Embedded
        );
        assert_eq!(
            backend
                .effective_cursor_mode(CursorCaptureMode::Metadata)
                .unwrap_err()
                .kind(),
            CaptureErrorKind::UnsupportedCapability
        );
        assert!(matches!(
            backend.capabilities().cursor_metadata,
            gif_from_screen_capture::CapabilityStatus::Unavailable(_)
        ));
    }

    #[test]
    fn converts_all_negotiated_four_byte_formats_and_row_padding() {
        let size = PhysicalSize::new(2, 1).unwrap();
        let cases = [
            (
                RawVideoFormat::Rgba,
                [1, 2, 3, 4, 5, 6, 7, 8, 99, 99, 99, 99],
            ),
            (
                RawVideoFormat::Bgra,
                [3, 2, 1, 4, 7, 6, 5, 8, 99, 99, 99, 99],
            ),
            (
                RawVideoFormat::Rgbx,
                [1, 2, 3, 0, 5, 6, 7, 0, 99, 99, 99, 99],
            ),
            (
                RawVideoFormat::Bgrx,
                [3, 2, 1, 0, 7, 6, 5, 0, 99, 99, 99, 99],
            ),
        ];
        for (pixel_format, bytes) in cases {
            let (rgba, actual_size) =
                convert_raw_frame(&bytes, 12, format(size, pixel_format), None).unwrap();
            assert_eq!(actual_size, size);
            assert_eq!(rgba, [1, 2, 3, 4, 5, 6, 7, 8].map_with_alpha(pixel_format));
        }
    }

    #[test]
    fn crop_conversion_is_tight_and_source_local() {
        let size = PhysicalSize::new(3, 2).unwrap();
        let bytes = [
            1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255, 5, 0, 0, 255, 6, 0, 0, 255,
        ];
        let crop = PhysicalRect::new(1, 0, 2, 2).unwrap();
        let (rgba, actual_size) =
            convert_raw_frame(&bytes, 12, format(size, RawVideoFormat::Rgba), Some(crop)).unwrap();
        assert_eq!(actual_size, PhysicalSize::new(2, 2).unwrap());
        assert_eq!(
            rgba,
            [2, 0, 0, 255, 3, 0, 0, 255, 5, 0, 0, 255, 6, 0, 0, 255,]
        );
    }

    #[test]
    fn conversion_rejects_short_stride_truncated_data_and_outside_crop() {
        let size = PhysicalSize::new(2, 2).unwrap();
        let negotiated = format(size, RawVideoFormat::Rgba);
        let short_stride = convert_raw_frame(&[0; 16], 7, negotiated, None).unwrap_err();
        assert_eq!(short_stride.kind(), CaptureErrorKind::InvalidFrame);

        let truncated = convert_raw_frame(&[0; 15], 8, negotiated, None).unwrap_err();
        assert_eq!(truncated.kind(), CaptureErrorKind::InvalidFrame);

        let outside = convert_raw_frame(
            &[0; 16],
            8,
            negotiated,
            Some(PhysicalRect::new(1, 1, 2, 1).unwrap()),
        )
        .unwrap_err();
        assert_eq!(outside.kind(), CaptureErrorKind::InvalidRequest);
    }

    #[test]
    fn bounded_delivery_drops_without_rewriting_sequence_or_timestamp() {
        let (sender, receiver) = mpsc::sync_channel(1);
        assert_eq!(
            try_deliver_frame(&sender, captured_frame(0, 10)),
            FrameDelivery::Delivered
        );
        assert_eq!(
            try_deliver_frame(&sender, captured_frame(1, 20)),
            FrameDelivery::Dropped
        );
        assert_eq!(receiver.recv().unwrap().sequence(), 0);
        assert_eq!(
            try_deliver_frame(&sender, captured_frame(2, 30)),
            FrameDelivery::Delivered
        );
        let after_drop = receiver.recv().unwrap();
        assert_eq!(after_drop.sequence(), 2);
        assert_eq!(after_drop.captured_at().as_micros(), 30);
    }

    #[test]
    fn resume_boundary_discards_every_preparation_frame_before_reactivation() {
        let (sender, receiver) = mpsc::sync_channel(FRAME_CHANNEL_CAPACITY);
        for sequence in 0..u64::try_from(FRAME_CHANNEL_CAPACITY).unwrap() {
            sender
                .send(captured_frame(sequence, sequence + 10))
                .unwrap();
        }

        drain_queued_frames(&receiver);
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));

        sender.send(captured_frame(9, 90)).unwrap();
        let fresh = receiver.recv().unwrap();
        assert_eq!(fresh.sequence(), 9);
        assert_eq!(fresh.captured_at().as_micros(), 90);
    }

    #[test]
    fn serialized_and_deserialized_format_fixture_is_typed() {
        let value = Value::Object(Object {
            type_: libspa_sys::SPA_TYPE_OBJECT_Format,
            id: libspa_sys::SPA_PARAM_Format,
            properties: vec![
                Property {
                    key: libspa_sys::SPA_FORMAT_mediaType,
                    flags: PropertyFlags::empty(),
                    value: Value::Id(Id(libspa_sys::SPA_MEDIA_TYPE_video)),
                },
                Property {
                    key: libspa_sys::SPA_FORMAT_mediaSubtype,
                    flags: PropertyFlags::empty(),
                    value: Value::Id(Id(libspa_sys::SPA_MEDIA_SUBTYPE_raw)),
                },
                Property {
                    key: libspa_sys::SPA_FORMAT_VIDEO_format,
                    flags: PropertyFlags::empty(),
                    value: Value::Id(Id(libspa_sys::SPA_VIDEO_FORMAT_BGRA)),
                },
                Property {
                    key: libspa_sys::SPA_FORMAT_VIDEO_size,
                    flags: PropertyFlags::empty(),
                    value: Value::Rectangle(Rectangle {
                        width: 1_280,
                        height: 720,
                    }),
                },
            ],
        });
        let bytes =
            libspa::pod::serialize::PodSerializer::serialize(Cursor::new(Vec::new()), &value)
                .unwrap()
                .0
                .into_inner();
        let (_, decoded) = PodDeserializer::deserialize_from::<Value>(&bytes).unwrap();
        assert_eq!(
            parse_format_value(&decoded).unwrap(),
            NegotiatedFormat {
                size: PhysicalSize::new(1_280, 720).unwrap(),
                format: RawVideoFormat::Bgra,
            }
        );
    }

    #[test]
    fn cadence_gate_drops_early_buffers_but_keeps_source_time() {
        let mut cadence = CadenceGate::Periodic {
            period: Duration::from_millis(100),
            next_due: Duration::ZERO,
        };
        assert!(cadence.should_emit(Duration::ZERO));
        assert!(!cadence.should_emit(Duration::from_millis(50)));
        assert!(cadence.should_emit(Duration::from_millis(100)));
        cadence.resume_now(Duration::from_millis(125));
        assert!(cadence.should_emit(Duration::from_millis(125)));
    }

    #[test]
    fn periodic_cadence_stays_anchored_when_buffers_arrive_late() {
        let mut cadence = CadenceGate::Periodic {
            period: Duration::from_millis(100),
            next_due: Duration::ZERO,
        };
        for millis in [30, 130, 230, 330, 430, 530] {
            assert!(cadence.should_emit(Duration::from_millis(millis)));
        }
        assert!(!cadence.should_emit(Duration::from_millis(599)));
        assert!(cadence.should_emit(Duration::from_millis(600)));

        let mut skipped = CadenceGate::Periodic {
            period: Duration::from_millis(100),
            next_due: Duration::ZERO,
        };
        assert!(skipped.should_emit(Duration::from_millis(350)));
        assert!(!skipped.should_emit(Duration::from_millis(399)));
        assert!(skipped.should_emit(Duration::from_millis(400)));
    }

    #[test]
    fn active_clock_excludes_multiple_pause_intervals() {
        let started_at = Instant::now();
        let mut clock = ActiveClock {
            started_at,
            paused_at: None,
            accumulated_pause: Duration::ZERO,
        };
        clock.pause(started_at + Duration::from_millis(100));
        assert_eq!(
            clock.elapsed(started_at + Duration::from_millis(300)),
            Duration::from_millis(100)
        );
        clock.resume(started_at + Duration::from_millis(300));
        clock.pause(started_at + Duration::from_millis(450));
        clock.resume(started_at + Duration::from_millis(500));
        assert_eq!(
            clock.elapsed(started_at + Duration::from_millis(600)),
            Duration::from_millis(350)
        );
    }

    #[test]
    fn cadence_rejects_zero_interval_and_handles_extreme_fps() {
        assert_eq!(
            CadenceGate::from_request(CaptureCadence::Interval(Duration::ZERO))
                .unwrap_err()
                .kind(),
            CaptureErrorKind::InvalidRequest
        );
        let cadence =
            CadenceGate::from_request(CaptureCadence::fixed_fps(u32::MAX).unwrap()).unwrap();
        assert!(matches!(
            cadence,
            CadenceGate::Periodic { period, .. } if period == Duration::from_nanos(1)
        ));
    }

    trait ExpectedAlpha {
        fn map_with_alpha(self, format: RawVideoFormat) -> Self;
    }

    impl ExpectedAlpha for [u8; 8] {
        fn map_with_alpha(mut self, format: RawVideoFormat) -> Self {
            if matches!(format, RawVideoFormat::Rgbx | RawVideoFormat::Bgrx) {
                self[3] = 255;
                self[7] = 255;
            }
            self
        }
    }
}
