//! XDG `ScreenCast` portal discovery and session-to-`PipeWire` handoff.

use std::{
    fmt,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    sync::atomic::AtomicBool,
};

use ashpd::{
    Error as PortalError, PortalError as PortalServiceError,
    desktop::{
        CreateSessionOptions, PersistMode, ResponseError, Session,
        screencast::{
            CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
            StartCastOptions, Stream as PortalStream,
        },
    },
};
use gif_from_screen_capture::{
    CapabilityStatus, CaptureCapabilities, CaptureError, CaptureErrorKind, CaptureSource,
    CaptureSourceId, CaptureSourceKind, CursorCaptureMode, RecoveryHint,
};
use tokio::runtime::{Builder, Runtime};

#[path = "wayland_cancel.rs"]
mod cancel;
pub(crate) use cancel::cancelled_error;
use cancel::{RequestObjects, cancellable};

pub(crate) const MONITOR_SOURCE_ID: &str = "wayland:portal:monitor";
pub(crate) const WINDOW_SOURCE_ID: &str = "wayland:portal:window";

/// Live capabilities advertised by `org.freedesktop.portal.ScreenCast`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these booleans mirror independent portal bit flags"
)]
pub struct WaylandPortalCapabilities {
    /// `ScreenCast` portal interface version.
    pub version: u32,
    /// Whether the portal can ask the user for a monitor.
    pub monitor: bool,
    /// Whether the portal can ask the user for a window.
    pub window: bool,
    /// Whether hidden cursor capture is advertised.
    pub cursor_hidden: bool,
    /// Whether cursor pixels can be embedded in video frames.
    pub cursor_embedded: bool,
    /// Whether separate `PipeWire` cursor metadata is advertised.
    pub cursor_metadata: bool,
}

impl WaylandPortalCapabilities {
    /// Projects live portal properties onto the portable capability model.
    pub fn capture_capabilities(self) -> CaptureCapabilities {
        let source_status = |available: bool, label: &str| {
            if available {
                CapabilityStatus::PermissionRequired(format!(
                    "the ScreenCast portal will ask the user to select a {label}"
                ))
            } else {
                CapabilityStatus::Unavailable(format!(
                    "the active ScreenCast portal does not advertise {label} sources"
                ))
            }
        };
        CaptureCapabilities {
            monitor: source_status(self.monitor, "monitor"),
            window: source_status(self.window, "window"),
            arbitrary_region: CapabilityStatus::Limited(
                "the portal selects a monitor/window; crop its PipeWire frames afterward"
                    .to_owned(),
            ),
            cursor_embedded: if self.cursor_embedded {
                CapabilityStatus::Available
            } else {
                CapabilityStatus::Unavailable(
                    "the active portal does not advertise embedded cursor capture".to_owned(),
                )
            },
            cursor_metadata: if self.cursor_metadata {
                CapabilityStatus::Available
            } else {
                CapabilityStatus::Unavailable(
                    "the active portal does not advertise cursor metadata".to_owned(),
                )
            },
            passive_mouse_buttons: CapabilityStatus::Unavailable(
                "Wayland does not expose passive global mouse-button observation".to_owned(),
            ),
            passive_keyboard: CapabilityStatus::Unavailable(
                "Wayland does not expose passive global keyboard observation".to_owned(),
            ),
            global_shortcuts: CapabilityStatus::PermissionRequired(
                "global shortcuts require a separate portal session".to_owned(),
            ),
            camera: CapabilityStatus::Unavailable(
                "camera capture is provided by the separate camera backend".to_owned(),
            ),
        }
    }

    /// Returns synthetic source choices whose final identity is selected by the compositor.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] only if construction of an invariant synthetic
    /// source descriptor fails.
    pub fn portal_sources(self) -> Result<Vec<CaptureSource>, CaptureError> {
        let mut sources = Vec::with_capacity(2);
        if self.monitor {
            sources.push(CaptureSource::new(
                CaptureSourceId::new(MONITOR_SOURCE_ID)?,
                "Choose a screen with the system portal",
                CaptureSourceKind::Monitor,
                None,
                1.0,
            )?);
        }
        if self.window {
            sources.push(CaptureSource::new(
                CaptureSourceId::new(WINDOW_SOURCE_ID)?,
                "Choose a window with the system portal",
                CaptureSourceKind::Window,
                None,
                1.0,
            )?);
        }
        Ok(sources)
    }
}

/// Synchronous owner for live `ScreenCast` portal operations.
///
/// Construction opens the D-Bus portal proxy and reads its source/cursor
/// properties. It does not display a chooser until [`Self::start_session`].
pub struct WaylandPortal {
    // A proxy must never borrow ashpd's process-global connection: that connection
    // may belong to an earlier, already-dropped capability-probe runtime.
    portal: Screencast,
    runtime: Runtime,
    capabilities: WaylandPortalCapabilities,
}

impl fmt::Debug for WaylandPortal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WaylandPortal")
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl WaylandPortal {
    /// Connects to the session bus and probes the active `ScreenCast` frontend.
    ///
    /// # Errors
    ///
    /// Returns an actionable [`CaptureError`] when the Tokio runtime, D-Bus
    /// session, portal frontend, or required properties are unavailable.
    pub fn connect() -> Result<Self, CaptureError> {
        Self::connect_cancellable(&AtomicBool::new(false))
    }

    /// Connect while observing cancellation; no permission interaction is fabricated.
    ///
    /// # Errors
    /// Returns the normal connection errors, or permission-cancellation when stopped.
    pub fn connect_cancellable(cancellation: &AtomicBool) -> Result<Self, CaptureError> {
        let runtime = portal_runtime()?;
        let (portal, capabilities) = runtime.block_on(cancellable(
            cancellation,
            portal_deadline(
                std::time::Duration::from_secs(10),
                "connect to and probe",
                async {
                    let connection = ashpd::zbus::Connection::session().await.map_err(|error| {
                        map_portal_error("connect to the session bus for", &error.into())
                    })?;
                    let portal = Screencast::with_connection(connection)
                        .await
                        .map_err(|error| map_portal_error("connect to", &error))?;
                    let capabilities = probe_proxy(&portal).await?;
                    Ok((portal, capabilities))
                },
            ),
            async {},
        ))?;
        Ok(Self {
            portal,
            runtime,
            capabilities,
        })
    }

    /// Returns the live capability snapshot obtained during connection.
    pub const fn capabilities(&self) -> WaylandPortalCapabilities {
        self.capabilities
    }

    /// Performs `CreateSession → SelectSources → Start → OpenPipeWireRemote`.
    ///
    /// The compositor presents its trusted chooser during `Start`. The returned
    /// object owns the portal session and remote file descriptor but does not
    /// pretend to be a [`gif_from_screen_capture::CaptureSession`]; a `PipeWire`
    /// video consumer must take the descriptor and keep the object alive.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureErrorKind::PermissionRequired`] when the user cancels,
    /// [`CaptureErrorKind::UnsupportedCapability`] for an unavailable source or
    /// cursor mode, and [`CaptureErrorKind::Platform`] for portal failures.
    pub fn start_session(
        self,
        source_kind: CaptureSourceKind,
        cursor: CursorCaptureMode,
    ) -> Result<WaylandPortalSession, CaptureError> {
        self.start_session_cancellable(source_kind, cursor, &AtomicBool::new(false))
    }

    /// Same trusted portal workflow, but closes pending Request/Session objects on cancellation.
    ///
    /// # Errors
    /// Returns the same errors as `start_session`, including cancellation without a capture session.
    pub fn start_session_cancellable(
        self,
        source_kind: CaptureSourceKind,
        cursor: CursorCaptureMode,
        cancellation: &AtomicBool,
    ) -> Result<WaylandPortalSession, CaptureError> {
        let source_type = select_source_type(self.capabilities, source_kind)?;
        let cursor_mode = select_cursor_mode(self.capabilities, cursor)?;
        let runtime = self.runtime;
        let portal = self.portal;
        let (portal, session, stream, remote) = runtime.block_on(async {
            let create = CreateSessionOptions::default();
            let create_objects = RequestObjects::from_options(portal.connection(), &create, None)?;
            let session = cancellable(
                cancellation,
                Box::pin(async {
                    portal
                        .create_session(create)
                        .await
                        .map_err(|error| map_portal_error("create", &error))
                }),
                create_objects.close(),
            )
            .await?;
            let outcome = async {
                let options = SelectSourcesOptions::default()
                    .set_cursor_mode(cursor_mode)
                    .set_sources(Some(source_type.into()))
                    .set_multiple(false)
                    .set_persist_mode(PersistMode::DoNot);
                let select_objects = RequestObjects::from_options(
                    portal.connection(),
                    &options,
                    create_objects.session.clone(),
                )?;
                cancellable(
                    cancellation,
                    async {
                        portal
                            .select_sources(&session, options)
                            .await
                            .and_then(|request| request.response())
                            .map_err(|error| map_portal_error("select sources for", &error))
                    },
                    select_objects.close(),
                )
                .await?;
                let options = StartCastOptions::default();
                let start_objects = RequestObjects::from_options(
                    portal.connection(),
                    &options,
                    create_objects.session.clone(),
                )?;
                let streams = cancellable(
                    cancellation,
                    async {
                        portal
                            .start(&session, None, options)
                            .await
                            .and_then(|request| request.response())
                            .map_err(|error| map_portal_error("start", &error))
                    },
                    start_objects.close(),
                )
                .await?;
                let stream = single_stream_info(streams.streams())?;
                let remote = cancellable(
                    cancellation,
                    async {
                        portal
                            .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
                            .await
                            .map_err(|error| {
                                map_portal_error("open the PipeWire remote for", &error)
                            })
                    },
                    start_objects.close(),
                )
                .await?;
                Ok((stream, remote))
            }
            .await;
            match outcome {
                Ok((stream, remote)) => Ok((portal, session, stream, remote)),
                Err(error) => {
                    if !cancellation.load(std::sync::atomic::Ordering::Acquire) {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            session.close(),
                        )
                        .await;
                    }
                    Err(error)
                }
            }
        })?;

        Ok(WaylandPortalSession {
            runtime,
            _portal: portal,
            session,
            stream,
            remote: Some(remote),
            state: WaylandPortalSessionState::Open,
        })
    }
}

fn single_stream_info(streams: &[PortalStream]) -> Result<PortalStreamInfo, CaptureError> {
    let [stream] = streams else {
        return Err(CaptureError::new(
            CaptureErrorKind::Platform,
            format!(
                "ScreenCast portal returned {} streams after requesting exactly one",
                streams.len()
            ),
            RecoveryHint::ChooseDifferentSource,
        ));
    };
    Ok(PortalStreamInfo::from_portal(stream))
}

/// Metadata for the single `PipeWire` stream chosen by the compositor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortalStreamInfo {
    node_id: u32,
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
    source_kind: Option<CaptureSourceKind>,
}

impl PortalStreamInfo {
    fn from_portal(stream: &PortalStream) -> Self {
        Self {
            node_id: stream.pipe_wire_node_id(),
            position: stream.position(),
            size: stream.size(),
            source_kind: stream.source_type().and_then(|source| match source {
                SourceType::Monitor => Some(CaptureSourceKind::Monitor),
                SourceType::Window => Some(CaptureSourceKind::Window),
                SourceType::Virtual => None,
            }),
        }
    }

    /// `PipeWire` node id to pass to the video stream consumer.
    pub const fn node_id(&self) -> u32 {
        self.node_id
    }

    /// Optional compositor-space origin reported by the portal.
    pub const fn position(&self) -> Option<(i32, i32)> {
        self.position
    }

    /// Optional compositor-space dimensions; negotiated video pixels may differ.
    pub const fn size(&self) -> Option<(i32, i32)> {
        self.size
    }

    /// Portal-reported source kind when present.
    pub const fn source_kind(&self) -> Option<CaptureSourceKind> {
        self.source_kind
    }
}

/// Lifecycle of a selected portal session and its `PipeWire` descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaylandPortalSessionState {
    /// Session is live and the descriptor remains owned here.
    Open,
    /// Descriptor was transferred to a `PipeWire` consumer; portal session remains live.
    RemoteTaken,
    /// Portal session was explicitly or implicitly closed.
    Closed,
}

impl WaylandPortalSessionState {
    /// Whether the portal session must still be kept alive.
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Open | Self::RemoteTaken)
    }

    /// Whether this object still owns the `PipeWire` remote descriptor.
    pub const fn owns_remote(self) -> bool {
        matches!(self, Self::Open)
    }
}

/// Live portal session that must outlive its `PipeWire` stream consumer.
pub struct WaylandPortalSession {
    // Retains the D-Bus connection that owns `session`.
    _portal: Screencast,
    session: Session<Screencast>,
    // Drop connection owners before their runtime. Close runs while all remain alive.
    runtime: Runtime,
    stream: PortalStreamInfo,
    remote: Option<OwnedFd>,
    state: WaylandPortalSessionState,
}

impl fmt::Debug for WaylandPortalSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WaylandPortalSession")
            .field("stream", &self.stream)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl WaylandPortalSession {
    /// Selected `PipeWire` stream metadata.
    pub const fn stream(&self) -> &PortalStreamInfo {
        &self.stream
    }

    /// Current portal/descriptor ownership state.
    pub const fn state(&self) -> WaylandPortalSessionState {
        self.state
    }

    /// Borrows the `PipeWire` remote before ownership is transferred.
    pub fn pipewire_remote(&self) -> Option<BorrowedFd<'_>> {
        self.remote.as_ref().map(AsFd::as_fd)
    }

    /// Transfers the `PipeWire` remote to a real video consumer exactly once.
    pub fn take_pipewire_remote(&mut self) -> Option<OwnedFd> {
        let remote = self.remote.take();
        if remote.is_some() {
            self.state = WaylandPortalSessionState::RemoteTaken;
        }
        remote
    }

    /// Closes the portal session and drops any untaken remote descriptor.
    ///
    /// # Errors
    ///
    /// Returns a platform [`CaptureError`] if the portal rejects session close.
    pub fn close(&mut self) -> Result<(), CaptureError> {
        if self.state == WaylandPortalSessionState::Closed {
            return Ok(());
        }
        self.remote = None;
        let result = self.runtime.block_on(portal_deadline(
            std::time::Duration::from_secs(5),
            "close",
            async {
                self.session
                    .close()
                    .await
                    .map_err(|error| map_portal_error("close", &error))
            },
        ));
        self.state = WaylandPortalSessionState::Closed;
        result
    }
}

impl Drop for WaylandPortalSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

async fn portal_deadline<T>(
    timeout: std::time::Duration,
    operation: &str,
    future: impl std::future::Future<Output = Result<T, CaptureError>>,
) -> Result<T, CaptureError> {
    tokio::time::timeout(timeout, future).await.map_err(|_| {
        CaptureError::new(
            CaptureErrorKind::BackendUnavailable,
            format!("Timed out trying to {operation} the ScreenCast portal. Check the desktop portal service and retry."),
            RecoveryHint::Retry,
        )
    })?
}

async fn probe_proxy(portal: &Screencast) -> Result<WaylandPortalCapabilities, CaptureError> {
    let source_types = portal
        .available_source_types()
        .await
        .map_err(|error| map_portal_error("query source types from", &error))?;
    let version = portal.version();
    let cursor_modes = if version >= 2 {
        portal
            .available_cursor_modes()
            .await
            .map_err(|error| map_portal_error("query cursor modes from", &error))?
    } else {
        CursorMode::Hidden.into()
    };
    Ok(WaylandPortalCapabilities {
        version,
        monitor: source_types.contains(SourceType::Monitor),
        window: source_types.contains(SourceType::Window),
        cursor_hidden: cursor_modes.contains(CursorMode::Hidden),
        cursor_embedded: cursor_modes.contains(CursorMode::Embedded),
        cursor_metadata: cursor_modes.contains(CursorMode::Metadata),
    })
}

fn portal_runtime() -> Result<Runtime, CaptureError> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            CaptureError::new(
                CaptureErrorKind::BackendUnavailable,
                format!("cannot create Wayland portal runtime: {error}"),
                RecoveryHint::Retry,
            )
        })
}

fn select_source_type(
    capabilities: WaylandPortalCapabilities,
    source_kind: CaptureSourceKind,
) -> Result<SourceType, CaptureError> {
    match source_kind {
        CaptureSourceKind::Monitor if capabilities.monitor => Ok(SourceType::Monitor),
        CaptureSourceKind::Window if capabilities.window => Ok(SourceType::Window),
        CaptureSourceKind::Monitor => Err(unsupported("monitor source")),
        CaptureSourceKind::Window => Err(unsupported("window source")),
        _ => Err(unsupported("unknown source kind")),
    }
}

fn select_cursor_mode(
    capabilities: WaylandPortalCapabilities,
    requested: CursorCaptureMode,
) -> Result<CursorMode, CaptureError> {
    match requested {
        CursorCaptureMode::Hidden => available_cursor_mode(
            capabilities.cursor_hidden,
            CursorMode::Hidden,
            "hidden cursor mode",
        ),
        CursorCaptureMode::Embedded => available_cursor_mode(
            capabilities.cursor_embedded,
            CursorMode::Embedded,
            "embedded cursor mode",
        ),
        CursorCaptureMode::Metadata => available_cursor_mode(
            capabilities.cursor_metadata,
            CursorMode::Metadata,
            "cursor metadata mode",
        ),
        CursorCaptureMode::Automatic => {
            if capabilities.cursor_metadata {
                Ok(CursorMode::Metadata)
            } else if capabilities.cursor_embedded {
                Ok(CursorMode::Embedded)
            } else {
                available_cursor_mode(
                    capabilities.cursor_hidden,
                    CursorMode::Hidden,
                    "any cursor mode",
                )
            }
        }
        _ => Err(unsupported("unknown cursor mode")),
    }
}

fn available_cursor_mode(
    available: bool,
    mode: CursorMode,
    label: &str,
) -> Result<CursorMode, CaptureError> {
    if available {
        Ok(mode)
    } else {
        Err(unsupported(label))
    }
}

fn unsupported(capability: &str) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::UnsupportedCapability,
        format!("the active ScreenCast portal does not support {capability}"),
        RecoveryHint::ChangeRequest,
    )
}

fn map_portal_error(operation: &str, error: &PortalError) -> CaptureError {
    let cancelled = matches!(
        error,
        PortalError::Response(ResponseError::Cancelled)
            | PortalError::Portal(PortalServiceError::Cancelled(_))
    );
    if cancelled {
        CaptureError::new(
            CaptureErrorKind::PermissionRequired,
            format!("the user cancelled the request to {operation} a Wayland ScreenCast session"),
            RecoveryHint::RequestPermission,
        )
    } else {
        CaptureError::new(
            CaptureErrorKind::Platform,
            format!("could not {operation} Wayland ScreenCast portal: {error}"),
            RecoveryHint::Retry,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_cancelled_connection_never_contacts_the_portal() {
        let error = WaylandPortal::connect_cancellable(&AtomicBool::new(true)).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    #[ignore = "opens and programmatically cancels the real trusted chooser; isolated desktop only"]
    fn cancelling_real_pending_chooser_does_not_require_a_permission_response() {
        use std::sync::{Arc, atomic::Ordering};
        assert_eq!(
            std::env::var("GFS_ISOLATED_WAYLAND_TEST").as_deref(),
            Ok("1")
        );
        let portal = WaylandPortal::connect().unwrap();
        let cancellation = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancellation);
        let worker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            trigger.store(true, Ordering::Release);
        });
        let started = std::time::Instant::now();
        let result = portal.start_session_cancellable(
            CaptureSourceKind::Monitor,
            CursorCaptureMode::Automatic,
            &cancellation,
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(started.elapsed() < std::time::Duration::from_secs(7));
        worker.join().unwrap();
    }

    #[test]
    fn unresponsive_portal_calls_have_a_bounded_actionable_failure() {
        let runtime = portal_runtime().unwrap();
        let error = runtime
            .block_on(portal_deadline(
                std::time::Duration::from_millis(1),
                "test an unresponsive",
                std::future::pending::<Result<(), CaptureError>>(),
            ))
            .unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::BackendUnavailable);
        assert_eq!(error.recovery(), RecoveryHint::Retry);
        assert!(error.to_string().contains("Timed out"));
        let value = runtime
            .block_on(portal_deadline(
                std::time::Duration::from_secs(1),
                "test a ready",
                async { Ok(42) },
            ))
            .unwrap();
        assert_eq!(value, 42);
    }

    #[test]
    #[ignore = "requires an isolated real ScreenCast portal and GFS_ISOLATED_WAYLAND_TEST=1"]
    fn repeated_real_portal_probes_outlive_other_probe_runtimes() {
        assert_eq!(
            std::env::var("GFS_ISOLATED_WAYLAND_TEST").as_deref(),
            Ok("1")
        );
        let mut expected = None;
        for _ in 0..3 {
            let earlier = WaylandPortal::connect().unwrap();
            let later = WaylandPortal::connect().unwrap();
            assert_eq!(earlier.capabilities(), later.capabilities());
            if let Some(expected) = expected {
                assert_eq!(later.capabilities(), expected);
            }
            expected = Some(later.capabilities());
            // Capability discovery drops its owner before recording connects again.
            drop(earlier);
            drop(later);
        }
    }

    fn capabilities() -> WaylandPortalCapabilities {
        WaylandPortalCapabilities {
            version: 5,
            monitor: true,
            window: false,
            cursor_hidden: true,
            cursor_embedded: true,
            cursor_metadata: false,
        }
    }

    #[test]
    fn live_capabilities_create_only_honest_portal_sources() {
        let capabilities = capabilities();
        let sources = capabilities.portal_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id().as_str(), MONITOR_SOURCE_ID);
        assert_eq!(sources[0].kind(), CaptureSourceKind::Monitor);
        assert!(sources[0].geometry().is_none());
        assert!(matches!(
            capabilities.capture_capabilities().window,
            CapabilityStatus::Unavailable(_)
        ));
    }

    #[test]
    fn source_and_cursor_negotiation_never_silently_fall_back() {
        let capabilities = capabilities();
        assert_eq!(
            select_source_type(capabilities, CaptureSourceKind::Monitor).unwrap(),
            SourceType::Monitor
        );
        assert_eq!(
            select_cursor_mode(capabilities, CursorCaptureMode::Automatic).unwrap(),
            CursorMode::Embedded
        );
        assert_eq!(
            select_cursor_mode(capabilities, CursorCaptureMode::Hidden).unwrap(),
            CursorMode::Hidden
        );
        assert_eq!(
            select_source_type(capabilities, CaptureSourceKind::Window)
                .unwrap_err()
                .kind(),
            CaptureErrorKind::UnsupportedCapability
        );
        assert_eq!(
            select_cursor_mode(capabilities, CursorCaptureMode::Metadata)
                .unwrap_err()
                .kind(),
            CaptureErrorKind::UnsupportedCapability
        );
    }

    #[test]
    fn portal_cancellation_is_typed_as_permission_required() {
        let error = map_portal_error("start", &PortalError::Response(ResponseError::Cancelled));
        assert_eq!(error.kind(), CaptureErrorKind::PermissionRequired);
        assert_eq!(error.recovery(), RecoveryHint::RequestPermission);
    }

    #[test]
    fn session_states_make_remote_ownership_and_lifetime_explicit() {
        assert!(WaylandPortalSessionState::Open.is_live());
        assert!(WaylandPortalSessionState::Open.owns_remote());
        assert!(WaylandPortalSessionState::RemoteTaken.is_live());
        assert!(!WaylandPortalSessionState::RemoteTaken.owns_remote());
        assert!(!WaylandPortalSessionState::Closed.is_live());
        assert!(!WaylandPortalSessionState::Closed.owns_remote());
    }
}
