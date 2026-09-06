//! Linux runtime detection and capture adapter boundary.
//!
//! The default build intentionally performs no native calls. It detects the
//! active display protocol, reports the platform's honest capability limits,
//! and returns an actionable uninitialized error. Native dependency probes are
//! isolated behind the `native-wayland` and `native-x11` features.

#![deny(unsafe_code)]

use gif_from_screen_capture::{
    BackendDescriptor, BackendStatus, CapabilityStatus, CaptureBackend, CaptureCapabilities,
    CaptureError, CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSource, RecoveryHint,
};
use std::env;

#[cfg(all(target_os = "linux", feature = "wayland-portal"))]
mod wayland;
#[cfg(all(target_os = "linux", feature = "native-wayland"))]
mod wayland_pipewire;
mod x11;

#[cfg(all(target_os = "linux", feature = "wayland-portal"))]
pub use wayland::{
    PortalStreamInfo, WaylandPortal, WaylandPortalCapabilities, WaylandPortalSession,
    WaylandPortalSessionState,
};
#[cfg(all(target_os = "linux", feature = "native-wayland"))]
pub use wayland_pipewire::{WaylandCaptureBackend, WaylandCaptureSession};
pub use x11::X11CaptureBackend;

/// Display protocol selected for the Linux desktop session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinuxDisplayServer {
    /// A Wayland compositor, captured through XDG Desktop Portal and `PipeWire`.
    Wayland,
    /// An X11 server, captured through X11 protocol extensions.
    X11,
}

impl LinuxDisplayServer {
    const fn label(self) -> &'static str {
        match self {
            Self::Wayland => "Wayland",
            Self::X11 => "X11",
        }
    }

    const fn feature_name(self) -> &'static str {
        match self {
            Self::Wayland => "native-wayland",
            Self::X11 => "native-x11",
        }
    }
}

/// Evidence used to select a Linux display protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DetectionEvidence {
    /// `XDG_SESSION_TYPE` explicitly selected the protocol.
    XdgSessionType,
    /// `WAYLAND_DISPLAY` was present when the session type was inconclusive.
    WaylandDisplay,
    /// `DISPLAY` was present when the session type was inconclusive.
    X11Display,
}

/// Snapshot of environment values relevant to capture backend selection.
///
/// Keeping a value object makes detection deterministic and avoids mutating
/// process-global environment variables in tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinuxEnvironment {
    xdg_session_type: Option<String>,
    wayland_display: Option<String>,
    x11_display: Option<String>,
    has_session_bus: bool,
    has_xdg_runtime_dir: bool,
}

impl LinuxEnvironment {
    /// Reads relevant values from the current process environment.
    pub fn from_process() -> Self {
        Self {
            xdg_session_type: non_empty_env("XDG_SESSION_TYPE"),
            wayland_display: non_empty_env("WAYLAND_DISPLAY"),
            x11_display: non_empty_env("DISPLAY"),
            has_session_bus: non_empty_env("DBUS_SESSION_BUS_ADDRESS").is_some(),
            has_xdg_runtime_dir: non_empty_env("XDG_RUNTIME_DIR").is_some(),
        }
    }

    /// Creates a deterministic environment snapshot.
    pub fn from_values(
        xdg_session_type: Option<&str>,
        wayland_display: Option<&str>,
        x11_display: Option<&str>,
        has_session_bus: bool,
        has_xdg_runtime_dir: bool,
    ) -> Self {
        Self {
            xdg_session_type: normalized_value(xdg_session_type),
            wayland_display: normalized_value(wayland_display),
            x11_display: normalized_value(x11_display),
            has_session_bus,
            has_xdg_runtime_dir,
        }
    }

    /// `XDG_SESSION_TYPE`, if present.
    pub fn xdg_session_type(&self) -> Option<&str> {
        self.xdg_session_type.as_deref()
    }

    /// `WAYLAND_DISPLAY`, if present.
    pub fn wayland_display(&self) -> Option<&str> {
        self.wayland_display.as_deref()
    }

    /// `DISPLAY`, if present.
    pub fn x11_display(&self) -> Option<&str> {
        self.x11_display.as_deref()
    }

    /// Whether a D-Bus session address was present.
    pub const fn has_session_bus(&self) -> bool {
        self.has_session_bus
    }

    /// Whether `XDG_RUNTIME_DIR` was present.
    pub const fn has_xdg_runtime_dir(&self) -> bool {
        self.has_xdg_runtime_dir
    }

    /// Detects the active display protocol and records non-fatal concerns.
    pub fn detect(&self) -> LinuxEnvironmentReport {
        let explicit = self
            .xdg_session_type
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase);

        let selected = match explicit.as_deref() {
            Some("wayland") => Some((
                LinuxDisplayServer::Wayland,
                DetectionEvidence::XdgSessionType,
            )),
            Some("x11") => Some((LinuxDisplayServer::X11, DetectionEvidence::XdgSessionType)),
            _ if self.wayland_display.is_some() => Some((
                LinuxDisplayServer::Wayland,
                DetectionEvidence::WaylandDisplay,
            )),
            _ if self.x11_display.is_some() => {
                Some((LinuxDisplayServer::X11, DetectionEvidence::X11Display))
            }
            _ => None,
        };

        let mut warnings = Vec::new();
        if let Some((LinuxDisplayServer::Wayland, _)) = selected {
            if self.wayland_display.is_none() {
                warnings.push(
                    "XDG_SESSION_TYPE says Wayland but WAYLAND_DISPLAY is not set".to_owned(),
                );
            }
            if !self.has_session_bus {
                warnings.push(
                    "DBUS_SESSION_BUS_ADDRESS is not set; the ScreenCast portal may be unavailable"
                        .to_owned(),
                );
            }
            if !self.has_xdg_runtime_dir {
                warnings.push(
                    "XDG_RUNTIME_DIR is not set; Wayland/PipeWire discovery may fail".to_owned(),
                );
            }
        }
        if let Some((LinuxDisplayServer::X11, _)) = selected
            && self.x11_display.is_none()
        {
            warnings.push("XDG_SESSION_TYPE says X11 but DISPLAY is not set".to_owned());
        }
        if !matches!(explicit.as_deref(), None | Some("wayland" | "x11")) {
            warnings.push(format!(
                "unrecognized XDG_SESSION_TYPE '{}'; selected a backend from display variables",
                explicit.as_deref().unwrap_or_default()
            ));
        }

        LinuxEnvironmentReport {
            display_server: selected.map(|(server, _)| server),
            evidence: selected.map(|(_, evidence)| evidence),
            warnings,
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var_os(name)
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.trim().is_empty())
}

fn normalized_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Result of Linux desktop environment detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxEnvironmentReport {
    display_server: Option<LinuxDisplayServer>,
    evidence: Option<DetectionEvidence>,
    warnings: Vec<String>,
}

impl LinuxEnvironmentReport {
    /// Selected display protocol, or `None` for a headless/incomplete session.
    pub const fn display_server(&self) -> Option<LinuxDisplayServer> {
        self.display_server
    }

    /// Environment value that led to the selection.
    pub const fn evidence(&self) -> Option<DetectionEvidence> {
        self.evidence
    }

    /// Non-fatal configuration concerns.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// Native adapter dependencies compiled into the current binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeBuildSupport {
    /// `ashpd` `ScreenCast` portal support was compiled for Linux.
    pub wayland_portal: bool,
    /// `ashpd` and `pipewire-rs` were compiled for Linux.
    pub wayland_portal_pipewire: bool,
    /// `x11rb` was compiled for Linux.
    pub x11rb: bool,
}

impl NativeBuildSupport {
    /// Reports feature-gated native dependency availability.
    pub const fn current() -> Self {
        Self {
            wayland_portal: cfg!(all(target_os = "linux", feature = "wayland-portal")),
            wayland_portal_pipewire: cfg!(all(target_os = "linux", feature = "native-wayland")),
            x11rb: cfg!(all(target_os = "linux", feature = "native-x11")),
        }
    }

    const fn supports(self, display_server: LinuxDisplayServer) -> bool {
        match display_server {
            LinuxDisplayServer::Wayland => self.wayland_portal_pipewire,
            LinuxDisplayServer::X11 => self.x11rb,
        }
    }
}

/// Complete startup report for diagnostics and UI capability projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBackendReport {
    /// Environment detection result.
    pub environment: LinuxEnvironmentReport,
    /// Feature-gated dependencies linked into the application.
    pub native_build: NativeBuildSupport,
    /// Platform-level capabilities and documented limitations.
    pub capabilities: CaptureCapabilities,
}

/// Detected but not-yet-initialized Linux screen capture adapter.
///
/// Explicit initialization opens a complete X11 backend or the Wayland
/// `ScreenCast` portal + `PipeWire` backend. Builds without the selected native
/// feature never return fake sources or silently fall back to another display
/// protocol; callers get [`CaptureErrorKind::BackendUninitialized`].
#[derive(Debug, Clone)]
pub struct LinuxCaptureBackend {
    environment: LinuxEnvironment,
    report: LinuxBackendReport,
    status: BackendStatus,
}

impl LinuxCaptureBackend {
    /// Detects a backend using the current process environment.
    pub fn detect() -> Self {
        Self::from_environment(LinuxEnvironment::from_process())
    }

    /// Detects a backend using an explicit snapshot, primarily for tests.
    pub fn from_environment(environment: LinuxEnvironment) -> Self {
        let environment_report = environment.detect();
        let native_build = NativeBuildSupport::current();
        let capabilities = environment_report
            .display_server()
            .map_or_else(headless_capabilities, platform_capabilities);
        let status = match environment_report.display_server() {
            None => BackendStatus::Unavailable(
                "no Linux graphical session was detected; set XDG_SESSION_TYPE plus \
                 WAYLAND_DISPLAY or DISPLAY and run inside a desktop session"
                    .to_owned(),
            ),
            Some(server) if !native_build.supports(server) => {
                BackendStatus::Uninitialized(format!(
                    "{} was detected, but its native adapter is not compiled; enable the '{}' \
                     Cargo feature and initialize the adapter before capture",
                    server.label(),
                    server.feature_name()
                ))
            }
            Some(server) => BackendStatus::Uninitialized(format!(
                "{} native dependencies are compiled, but the adapter is not initialized; \
                 initialize its connection/session before capture",
                server.label()
            )),
        };
        Self {
            environment,
            report: LinuxBackendReport {
                environment: environment_report,
                native_build,
                capabilities,
            },
            status,
        }
    }

    /// Original environment snapshot.
    pub const fn environment(&self) -> &LinuxEnvironment {
        &self.environment
    }

    /// Detailed detection and capability report.
    pub const fn report(&self) -> &LinuxBackendReport {
        &self.report
    }

    /// Opens the detected native backend.
    ///
    /// Both native builds return complete [`CaptureBackend`] implementations.
    /// Wayland uses a live `ScreenCast` portal probe and consumes the selected
    /// node through `PipeWire`; it never falls back to X11.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when no graphical session was detected, the
    /// required native feature is disabled, or the selected native connection
    /// and capability probe fails.
    pub fn initialize_native(&self) -> Result<Box<dyn CaptureBackend>, CaptureError> {
        match self.report.environment.display_server() {
            Some(LinuxDisplayServer::X11) => {
                let display = self.environment.x11_display().ok_or_else(|| {
                    CaptureError::new(
                        CaptureErrorKind::BackendUnavailable,
                        "cannot initialize X11 capture: DISPLAY is not set",
                        RecoveryHint::Retry,
                    )
                })?;
                X11CaptureBackend::connect(Some(display))
                    .map(|backend| Box::new(backend) as Box<dyn CaptureBackend>)
            }
            Some(LinuxDisplayServer::Wayland) => {
                #[cfg(all(target_os = "linux", feature = "native-wayland"))]
                {
                    WaylandCaptureBackend::connect()
                        .map(|backend| Box::new(backend) as Box<dyn CaptureBackend>)
                }
                #[cfg(all(
                    target_os = "linux",
                    not(feature = "native-wayland"),
                    feature = "wayland-portal"
                ))]
                {
                    let portal = self.initialize_wayland_portal()?;
                    let capabilities = portal.capabilities();
                    Err(CaptureError::backend_uninitialized(format!(
                        "Wayland ScreenCast portal v{} is reachable (monitor={}, window={}), but \
                         the native PipeWire consumer is not compiled; enable the \
                         'native-wayland' Cargo feature",
                        capabilities.version, capabilities.monitor, capabilities.window
                    )))
                }
                #[cfg(not(all(target_os = "linux", feature = "wayland-portal")))]
                {
                    Err(self.readiness_error("initialize Wayland ScreenCast portal capture"))
                }
            }
            None => Err(self.readiness_error("initialize a Linux capture backend")),
        }
    }

    /// Opens and probes the real XDG `ScreenCast` portal for a Wayland session.
    ///
    /// This is the independently usable first half of Wayland capture. Call
    /// [`WaylandPortal::start_session`] to complete the portal lifecycle and
    /// obtain the selected `PipeWire` node/remote handoff.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when Wayland was not selected or the live
    /// session bus/portal probe fails.
    #[cfg(all(target_os = "linux", feature = "wayland-portal"))]
    pub fn initialize_wayland_portal(&self) -> Result<WaylandPortal, CaptureError> {
        if self.report.environment.display_server() != Some(LinuxDisplayServer::Wayland) {
            return Err(CaptureError::invalid_request(
                "cannot initialize a Wayland portal outside a detected Wayland session",
            ));
        }
        WaylandPortal::connect()
    }

    fn readiness_error(&self, operation: &str) -> CaptureError {
        match &self.status {
            BackendStatus::Uninitialized(reason) => {
                CaptureError::backend_uninitialized(format!("cannot {operation}: {reason}"))
            }
            BackendStatus::Unavailable(reason) => CaptureError::new(
                CaptureErrorKind::BackendUnavailable,
                format!("cannot {operation}: {reason}"),
                RecoveryHint::None,
            ),
            BackendStatus::Ready => CaptureError::new(
                CaptureErrorKind::Platform,
                format!("cannot {operation}: no native Linux driver is attached"),
                RecoveryHint::InitializeBackend,
            ),
            _ => CaptureError::new(
                CaptureErrorKind::Platform,
                format!("cannot {operation}: the backend is in an unknown future state"),
                RecoveryHint::InitializeBackend,
            ),
        }
    }
}

impl CaptureBackend for LinuxCaptureBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "linux-auto",
            display_name: "Linux screen capture",
        }
    }

    fn status(&self) -> BackendStatus {
        self.status.clone()
    }

    fn capabilities(&self) -> CaptureCapabilities {
        self.report.capabilities.clone()
    }

    fn list_sources(&self) -> Result<Vec<CaptureSource>, CaptureError> {
        Err(self.readiness_error("list Linux capture sources"))
    }

    fn start_session(
        &self,
        _request: CaptureRequest,
    ) -> Result<Box<dyn CaptureSession>, CaptureError> {
        Err(self.readiness_error("start a Linux capture session"))
    }
}

fn headless_capabilities() -> CaptureCapabilities {
    CaptureCapabilities::all_unavailable(
        "no Wayland or X11 graphical session was detected in the process environment",
    )
}

fn platform_capabilities(display_server: LinuxDisplayServer) -> CaptureCapabilities {
    match display_server {
        LinuxDisplayServer::Wayland => wayland_capabilities(),
        LinuxDisplayServer::X11 => x11_capabilities(),
    }
}

fn wayland_capabilities() -> CaptureCapabilities {
    CaptureCapabilities {
        monitor: CapabilityStatus::PermissionRequired(
            "the compositor's ScreenCast portal must ask the user to select a monitor".to_owned(),
        ),
        window: CapabilityStatus::PermissionRequired(
            "the compositor's ScreenCast portal must ask the user to select a window".to_owned(),
        ),
        arbitrary_region: CapabilityStatus::Limited(
            "Wayland has no standard rectangular source; select a monitor/window through the \
             portal, then crop it in the application"
                .to_owned(),
        ),
        cursor_embedded: CapabilityStatus::Limited(
            "embedded cursor support depends on the compositor's negotiated portal modes"
                .to_owned(),
        ),
        cursor_metadata: CapabilityStatus::Limited(
            "editable cursor metadata depends on the compositor's negotiated portal modes"
                .to_owned(),
        ),
        passive_mouse_buttons: CapabilityStatus::Unavailable(
            "Wayland does not expose passive global mouse-button observation to ordinary apps"
                .to_owned(),
        ),
        passive_keyboard: CapabilityStatus::Unavailable(
            "Wayland does not expose passive global keyboard observation to ordinary apps"
                .to_owned(),
        ),
        global_shortcuts: CapabilityStatus::PermissionRequired(
            "global shortcuts require the GlobalShortcuts portal and compositor support".to_owned(),
        ),
        camera: CapabilityStatus::Unavailable(
            "camera capture is provided by the separate camera backend".to_owned(),
        ),
    }
}

fn x11_capabilities() -> CaptureCapabilities {
    let available = CapabilityStatus::Available;
    CaptureCapabilities {
        monitor: available.clone(),
        window: available.clone(),
        arbitrary_region: available.clone(),
        cursor_embedded: available.clone(),
        cursor_metadata: available.clone(),
        passive_mouse_buttons: available.clone(),
        passive_keyboard: available.clone(),
        global_shortcuts: available,
        camera: CapabilityStatus::Unavailable(
            "camera capture is provided by the separate camera backend".to_owned(),
        ),
    }
}

/// Runs no native I/O; it only makes feature builds type-check/link their
/// selected dependency crates. The return value is suitable for diagnostics.
pub fn native_dependency_compile_probe() -> NativeBuildSupport {
    #[cfg(all(target_os = "linux", feature = "wayland-portal"))]
    native_wayland_probe::assert_dependencies_linked();
    #[cfg(all(target_os = "linux", feature = "native-x11"))]
    native_x11_probe::assert_dependency_linked();
    NativeBuildSupport::current()
}

#[cfg(all(target_os = "linux", feature = "wayland-portal"))]
mod native_wayland_probe {
    use ashpd as _;
    use tokio as _;

    #[cfg(feature = "native-wayland")]
    use pipewire as _;

    pub(super) const fn assert_dependencies_linked() {}
}

#[cfg(all(target_os = "linux", feature = "native-x11"))]
mod native_x11_probe {
    use x11rb as _;

    pub(super) const fn assert_dependency_linked() {}
}

#[cfg(test)]
mod tests {
    use gif_from_screen_capture::{
        CaptureCadence, CaptureSourceId, CaptureTarget, CursorCaptureMode,
    };

    use super::*;

    fn environment(
        session: Option<&str>,
        wayland: Option<&str>,
        x11: Option<&str>,
    ) -> LinuxEnvironment {
        LinuxEnvironment::from_values(session, wayland, x11, true, true)
    }

    #[test]
    fn explicit_session_type_has_priority() {
        let report = environment(Some("x11"), Some("wayland-0"), Some(":0")).detect();
        assert_eq!(report.display_server(), Some(LinuxDisplayServer::X11));
        assert_eq!(report.evidence(), Some(DetectionEvidence::XdgSessionType));
    }

    #[test]
    fn falls_back_to_wayland_then_x11_display_variables() {
        let both = environment(None, Some("wayland-0"), Some(":0")).detect();
        assert_eq!(both.display_server(), Some(LinuxDisplayServer::Wayland));
        assert_eq!(both.evidence(), Some(DetectionEvidence::WaylandDisplay));

        let x11 = environment(None, None, Some(":1")).detect();
        assert_eq!(x11.display_server(), Some(LinuxDisplayServer::X11));
        assert_eq!(x11.evidence(), Some(DetectionEvidence::X11Display));
    }

    #[test]
    fn headless_environment_is_unavailable() {
        let backend = LinuxCaptureBackend::from_environment(environment(None, None, None));
        assert!(matches!(backend.status(), BackendStatus::Unavailable(_)));
        assert!(!backend.capabilities().monitor.is_supported());
        let error = backend.list_sources().unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::BackendUnavailable);
    }

    #[test]
    fn wayland_report_is_honest_about_security_boundaries() {
        let backend = LinuxCaptureBackend::from_environment(environment(
            Some("wayland"),
            Some("wayland-0"),
            None,
        ));
        let capabilities = backend.capabilities();
        assert!(matches!(
            capabilities.monitor,
            CapabilityStatus::PermissionRequired(_)
        ));
        assert!(matches!(
            capabilities.arbitrary_region,
            CapabilityStatus::Limited(_)
        ));
        assert!(!capabilities.passive_keyboard.is_supported());
        assert!(!capabilities.passive_mouse_buttons.is_supported());
    }

    #[test]
    fn detected_backend_returns_actionable_uninitialized_error() {
        let backend =
            LinuxCaptureBackend::from_environment(environment(Some("x11"), None, Some(":0")));
        assert!(matches!(backend.status(), BackendStatus::Uninitialized(_)));

        let request = CaptureRequest {
            target: CaptureTarget::Monitor(CaptureSourceId::new("x11:root:0").unwrap()),
            cadence: CaptureCadence::fixed_fps(15).unwrap(),
            cursor: CursorCaptureMode::Embedded,
            prefer_damage: true,
            input_events: false,
        };
        let error = backend
            .start_session(request)
            .err()
            .expect("uninitialized backend must fail");
        assert_eq!(error.kind(), CaptureErrorKind::BackendUninitialized);
        assert_eq!(error.recovery(), RecoveryHint::InitializeBackend);
        assert!(error.message().contains("initialize"));
    }

    #[test]
    fn warns_when_wayland_prerequisites_are_missing() {
        let environment = LinuxEnvironment::from_values(Some("wayland"), None, None, false, false);
        let report = environment.detect();
        assert_eq!(report.display_server(), Some(LinuxDisplayServer::Wayland));
        assert_eq!(report.warnings().len(), 3);
    }

    #[test]
    fn compile_probe_matches_cfg_flags() {
        let support = native_dependency_compile_probe();
        assert_eq!(
            support.wayland_portal,
            cfg!(all(target_os = "linux", feature = "wayland-portal"))
        );
        assert_eq!(
            support.wayland_portal_pipewire,
            cfg!(all(target_os = "linux", feature = "native-wayland"))
        );
        assert_eq!(
            support.x11rb,
            cfg!(all(target_os = "linux", feature = "native-x11"))
        );
    }
}
