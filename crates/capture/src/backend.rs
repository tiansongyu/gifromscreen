use std::fmt::{self, Display, Formatter};
use std::num::NonZeroU32;
use std::time::Duration;

use crate::{CaptureCapabilities, CaptureError, CapturedFrame, PhysicalRect};

/// Stable metadata identifying a backend implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendDescriptor {
    /// Machine-readable identifier used in logs and settings.
    pub id: &'static str,
    /// Human-readable backend name.
    pub display_name: &'static str,
}

/// Runtime lifecycle of a capture backend.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackendStatus {
    /// Environment detection succeeded, but native resources are not ready.
    Uninitialized(String),
    /// The backend is ready to enumerate/select sources and start sessions.
    Ready,
    /// The backend cannot operate in the current process environment.
    Unavailable(String),
}

/// Opaque, stable source identifier owned by one backend.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureSourceId(String);

impl CaptureSourceId {
    /// Creates a non-empty source identifier.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when the identifier is empty or only whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self, CaptureError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CaptureError::invalid_request(
                "capture source id must not be empty",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the backend-owned identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for CaptureSourceId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Kind of source exposed by a screen capture backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureSourceKind {
    /// A complete display.
    Monitor,
    /// An application window.
    Window,
}

/// One source that can be selected for capture.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureSource {
    id: CaptureSourceId,
    name: String,
    kind: CaptureSourceKind,
    geometry: Option<PhysicalRect>,
    scale_factor: f64,
}

impl CaptureSource {
    /// Creates a source descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when `name` is blank or `scale_factor` is not
    /// finite and greater than zero.
    pub fn new(
        id: CaptureSourceId,
        name: impl Into<String>,
        kind: CaptureSourceKind,
        geometry: Option<PhysicalRect>,
        scale_factor: f64,
    ) -> Result<Self, CaptureError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(CaptureError::invalid_request(
                "capture source name must not be empty",
            ));
        }
        if !scale_factor.is_finite() || scale_factor <= 0.0 {
            return Err(CaptureError::invalid_request(
                "capture source scale factor must be finite and greater than zero",
            ));
        }
        Ok(Self {
            id,
            name,
            kind,
            geometry,
            scale_factor,
        })
    }

    /// Returns the stable backend-owned id.
    pub const fn id(&self) -> &CaptureSourceId {
        &self.id
    }

    /// Returns the display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the source kind.
    pub const fn kind(&self) -> CaptureSourceKind {
        self.kind
    }

    /// Returns source geometry when the platform permits enumeration.
    pub const fn geometry(&self) -> Option<PhysicalRect> {
        self.geometry
    }

    /// Returns the logical-to-physical scale factor.
    pub const fn scale_factor(&self) -> f64 {
        self.scale_factor
    }
}

/// The requested capture target.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaptureTarget {
    /// A complete monitor.
    Monitor(CaptureSourceId),
    /// A complete application window.
    Window(CaptureSourceId),
    /// A physical-pixel crop inside a monitor/window source.
    Region {
        /// Parent source selected by the platform.
        source: CaptureSourceId,
        /// Source-local physical rectangle.
        region: PhysicalRect,
    },
}

impl CaptureTarget {
    /// Returns the parent source id.
    pub const fn source_id(&self) -> &CaptureSourceId {
        match self {
            Self::Monitor(source) | Self::Window(source) | Self::Region { source, .. } => source,
        }
    }
}

/// Policy used to turn native frames/events into project frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaptureCadence {
    /// Sample at a fixed frames-per-second target.
    FixedFps(NonZeroU32),
    /// Capture on a non-zero wall-clock interval.
    Interval(Duration),
    /// Capture only when explicitly triggered.
    Manual,
    /// Capture when supported input activity occurs.
    OnInteraction,
}

impl CaptureCadence {
    /// Creates a fixed-FPS cadence.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when `fps` is zero.
    pub fn fixed_fps(fps: u32) -> Result<Self, CaptureError> {
        NonZeroU32::new(fps)
            .map(Self::FixedFps)
            .ok_or_else(|| CaptureError::invalid_request("capture FPS must be greater than zero"))
    }

    /// Creates a non-zero interval cadence.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when `interval` is zero.
    pub fn interval(interval: Duration) -> Result<Self, CaptureError> {
        if interval.is_zero() {
            return Err(CaptureError::invalid_request(
                "capture interval must be greater than zero",
            ));
        }
        Ok(Self::Interval(interval))
    }
}

/// Requested cursor representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CursorCaptureMode {
    /// Do not capture a pointer.
    Hidden,
    /// Let the backend choose the richest supported representation.
    Automatic,
    /// Composite cursor pixels into the captured frame.
    Embedded,
    /// Return separately editable cursor metadata.
    Metadata,
}

/// Options used to create a capture session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRequest {
    /// Source or crop to capture.
    pub target: CaptureTarget,
    /// Project-frame cadence.
    pub cadence: CaptureCadence,
    /// Cursor capture behavior.
    pub cursor: CursorCaptureMode,
    /// Whether native damage information should be requested when available.
    pub prefer_damage: bool,
    /// Explicit opt-in to session-scoped passive keyboard/button recording.
    /// This may capture sensitive input from other applications. Never enabled implicitly.
    pub input_events: bool,
}

impl CaptureRequest {
    /// Creates a request with automatic cursor handling and damage enabled.
    pub const fn new(target: CaptureTarget, cadence: CaptureCadence) -> Self {
        Self {
            target,
            cadence,
            cursor: CursorCaptureMode::Automatic,
            prefer_damage: true,
            input_events: false,
        }
    }
}

/// State of a running or terminal capture session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaptureSessionState {
    /// Native resources are being started.
    Starting,
    /// Frames are being produced.
    Recording,
    /// Capture is intentionally paused.
    Paused,
    /// Native shutdown is in progress.
    Stopping,
    /// Capture completed and recorded frames should be kept.
    Stopped,
    /// Capture was discarded.
    Discarded,
    /// A terminal native failure occurred.
    Failed,
}

impl CaptureSessionState {
    /// Returns true for states that cannot produce another frame.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Discarded | Self::Failed)
    }
}

/// Result of polling a session's bounded frame channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramePoll {
    /// One frame is ready.
    Frame(CapturedFrame),
    /// No frame arrived during the supplied timeout; the session remains live.
    Pending,
    /// The session ended and no more frames will arrive.
    EndOfStream,
}

/// Platform-independent screen capture backend.
pub trait CaptureBackend: Send + Sync {
    /// Stable backend metadata.
    fn descriptor(&self) -> BackendDescriptor;

    /// Current initialization/readiness state.
    fn status(&self) -> BackendStatus;

    /// Runtime capability report. This may change after permissions change.
    fn capabilities(&self) -> CaptureCapabilities;

    /// Enumerates sources where the platform permits enumeration.
    ///
    /// Wayland implementations may instead return a synthetic portal-selection
    /// source because source choice belongs to the compositor.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if the backend is not ready or native source
    /// discovery fails.
    fn list_sources(&self) -> Result<Vec<CaptureSource>, CaptureError>;

    /// Starts a capture session.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if the backend is not ready, the request is
    /// unsupported, or the selected source cannot be opened.
    fn start_session(
        &self,
        request: CaptureRequest,
    ) -> Result<Box<dyn CaptureSession>, CaptureError>;
}

/// A single capture stream with explicit pause/stop/discard transitions.
pub trait CaptureSession: Send {
    /// Current session state.
    fn state(&self) -> CaptureSessionState;

    /// Current request used by the session.
    ///
    /// The returned request contains the most recently accepted target after
    /// a successful [`CaptureSession::update_target`] call.
    fn request(&self) -> &CaptureRequest;

    /// Changes the source or source-local rectangle used by subsequent frames.
    ///
    /// Capture sessions have a fixed output canvas. Implementations must reject
    /// a target whose resolved dimensions differ from the dimensions selected
    /// when the session started. A rejected update leaves the previous target
    /// active. Updates are also rejected after the session becomes terminal.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if the target is unknown, outside its parent
    /// source, changes the output dimensions, is unsupported by the backend, or
    /// cannot be applied in the current state.
    fn update_target(&mut self, target: CaptureTarget) -> Result<(), CaptureError>;

    /// Establishes a fresh-frame boundary before a controlled manual snapshot.
    ///
    /// Pull-based adapters may keep the default no-op because their next
    /// [`CaptureSession::poll_frame`] performs the actual capture. Buffered
    /// adapters should discard frames sampled before this call, without
    /// changing the public session state. This method does not itself consume
    /// the snapshot frame. Repeated calls replace an earlier boundary, even
    /// when its frame has not yet arrived (for example after moving a region).
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when a fresh boundary cannot be established.
    fn prepare_snapshot(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }

    /// Pauses frame production.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if pausing is invalid in the current state or
    /// the native adapter cannot pause.
    fn pause(&mut self) -> Result<(), CaptureError>;

    /// Resumes a paused session.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if resuming is invalid in the current state or
    /// the native adapter cannot resume.
    fn resume(&mut self) -> Result<(), CaptureError>;

    /// Stops the session and keeps frames already emitted.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if stopping is invalid in the current state or
    /// native shutdown fails.
    fn stop(&mut self) -> Result<(), CaptureError>;

    /// Stops the session and marks its output for discard.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if discarding is invalid in the current state
    /// or native shutdown fails.
    fn discard(&mut self) -> Result<(), CaptureError>;

    /// Waits up to `timeout` for a frame or state change.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if the source is lost or the native stream
    /// reports a failure. A normal timeout is [`FramePoll::Pending`].
    fn poll_frame(&mut self, timeout: Duration) -> Result<FramePoll, CaptureError>;
}
