use gif_from_screen_capture::{
    BackendDescriptor, BackendStatus, CaptureBackend, CaptureCapabilities, CaptureError,
    CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSource, CaptureTarget, CapturedFrame,
    RecoveryHint,
};

#[cfg(all(target_os = "linux", feature = "native-x11"))]
mod native {
    use std::fmt::{Debug, Formatter};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use gif_from_screen_capture::{
        CapabilityStatus, CaptureCadence, CaptureSessionState, CaptureSourceId, CaptureSourceKind,
        CaptureTimestamp, CursorCaptureMode, FramePoll, PhysicalRect, PhysicalSize, PixelFormat,
    };
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{
        ConnectionExt as _, ImageFormat, ImageOrder, Screen, VisualClass,
    };
    use x11rb::rust_connection::RustConnection;

    use super::{
        BackendDescriptor, BackendStatus, CaptureBackend, CaptureCapabilities, CaptureError,
        CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSource, CaptureTarget,
        CapturedFrame, RecoveryHint, decode_zpixmap, translate_region,
    };
    use crate::x11::{ByteOrder, PixelLayout};

    /// X11 capture backend using the core protocol's `GetImage` request.
    #[derive(Clone)]
    pub struct X11CaptureBackend {
        inner: Arc<X11Inner>,
    }

    struct X11Inner {
        connection: RustConnection,
        screen_index: usize,
        root: u32,
        sources: Vec<X11Source>,
        layout: PixelLayout,
        connected_at: Instant,
        direct_sequence: AtomicU64,
    }

    #[derive(Clone)]
    struct X11Source {
        portable: CaptureSource,
        root_region: PhysicalRect,
    }

    impl Debug for X11CaptureBackend {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("X11CaptureBackend")
                .field("screen_index", &self.inner.screen_index)
                .field("root", &self.inner.root)
                .field("source_count", &self.inner.sources.len())
                .finish_non_exhaustive()
        }
    }

    impl X11CaptureBackend {
        /// Returns whether native X11 support is compiled into this build.
        pub const fn compiled() -> bool {
            true
        }

        /// Connects to an explicit X11 display, or to `DISPLAY` when omitted.
        ///
        /// # Errors
        ///
        /// Returns [`CaptureError`] if the connection/setup fails or the root
        /// visual cannot be decoded safely by the current RGBA converter.
        pub fn connect(display: Option<&str>) -> Result<Self, CaptureError> {
            let (connection, screen_index) = x11rb::connect(display).map_err(|error| {
                CaptureError::new(
                    CaptureErrorKind::BackendUnavailable,
                    format!(
                        "failed to connect to X11 display {}: {error}",
                        display.unwrap_or("from DISPLAY")
                    ),
                    RecoveryHint::Retry,
                )
            })?;

            let (root, layout, sources) = inspect_setup(&connection, screen_index)?;
            Ok(Self {
                inner: Arc::new(X11Inner {
                    connection,
                    screen_index,
                    root,
                    sources,
                    layout,
                    connected_at: Instant::now(),
                    direct_sequence: AtomicU64::new(0),
                }),
            })
        }

        /// Captures one owned RGBA8 frame from a monitor/root or source-local
        /// rectangular target.
        ///
        /// # Errors
        ///
        /// Returns [`CaptureError`] for an unknown/out-of-bounds source, X11
        /// protocol failure, source loss, or malformed native pixel data.
        pub fn capture_once(&self, target: &CaptureTarget) -> Result<CapturedFrame, CaptureError> {
            let sequence = self.inner.direct_sequence.fetch_add(1, Ordering::Relaxed);
            let elapsed = self.inner.connected_at.elapsed().as_micros();
            let timestamp =
                CaptureTimestamp::from_micros(u64::try_from(elapsed).unwrap_or(u64::MAX));
            self.capture_target(target, sequence, timestamp)
        }

        fn capture_target(
            &self,
            target: &CaptureTarget,
            sequence: u64,
            timestamp: CaptureTimestamp,
        ) -> Result<CapturedFrame, CaptureError> {
            let region = self.resolve_target(target)?;
            let x = i16::try_from(region.origin().x).map_err(|_| {
                invalid_target("X11 capture x coordinate is outside the protocol's i16 range")
            })?;
            let y = i16::try_from(region.origin().y).map_err(|_| {
                invalid_target("X11 capture y coordinate is outside the protocol's i16 range")
            })?;
            let width = u16::try_from(region.size().width()).map_err(|_| {
                invalid_target("X11 capture width is outside the protocol's u16 range")
            })?;
            let height = u16::try_from(region.size().height()).map_err(|_| {
                invalid_target("X11 capture height is outside the protocol's u16 range")
            })?;

            let reply = self
                .inner
                .connection
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    self.inner.root,
                    x,
                    y,
                    width,
                    height,
                    u32::MAX,
                )
                .map_err(|error| source_lost("send X11 GetImage request", &error))?
                .reply()
                .map_err(|error| source_lost("receive X11 GetImage reply", &error))?;

            if reply.depth != self.inner.layout.depth {
                return Err(CaptureError::new(
                    CaptureErrorKind::Platform,
                    format!(
                        "X11 GetImage returned depth {} but root setup advertised {}",
                        reply.depth, self.inner.layout.depth
                    ),
                    RecoveryHint::Retry,
                ));
            }
            let rgba = decode_zpixmap(
                &reply.data,
                u32::from(width),
                u32::from(height),
                self.inner.layout,
            )?;
            let size = PhysicalSize::new(u32::from(width), u32::from(height))?;
            let stride = usize::from(width)
                .checked_mul(4)
                .ok_or_else(|| CaptureError::invalid_frame("RGBA stride overflow"))?;
            CapturedFrame::new(sequence, timestamp, size, stride, PixelFormat::Rgba8, rgba)
        }

        fn resolve_target(&self, target: &CaptureTarget) -> Result<PhysicalRect, CaptureError> {
            let source_id = target.source_id();
            let source = self
                .inner
                .sources
                .iter()
                .find(|candidate| candidate.portable.id() == source_id)
                .ok_or_else(|| {
                    CaptureError::new(
                        CaptureErrorKind::SourceNotFound,
                        format!("X11 capture source '{source_id}' was not found"),
                        RecoveryHint::ChooseDifferentSource,
                    )
                })?;

            match target {
                CaptureTarget::Monitor(_) => Ok(source.root_region),
                CaptureTarget::Region { region, .. } => {
                    translate_region(source.root_region, *region)
                }
                CaptureTarget::Window(_) => Err(CaptureError::new(
                    CaptureErrorKind::UnsupportedCapability,
                    "window capture is not implemented by the first X11 vertical slice",
                    RecoveryHint::ChangeRequest,
                )),
                _ => Err(invalid_target("unknown capture target kind")),
            }
        }

        fn validate_request(&self, request: &CaptureRequest) -> Result<(), CaptureError> {
            self.resolve_target(&request.target)?;
            match request.cursor {
                CursorCaptureMode::Hidden | CursorCaptureMode::Automatic => {}
                CursorCaptureMode::Embedded | CursorCaptureMode::Metadata => {
                    return Err(CaptureError::new(
                        CaptureErrorKind::UnsupportedCapability,
                        "cursor capture is not implemented by the GetImage X11 path",
                        RecoveryHint::ChangeRequest,
                    ));
                }
                _ => return Err(invalid_target("unknown cursor capture mode")),
            }
            if matches!(request.cadence, CaptureCadence::OnInteraction) {
                return Err(CaptureError::new(
                    CaptureErrorKind::UnsupportedCapability,
                    "interaction-triggered capture needs XInput support, which is not implemented yet",
                    RecoveryHint::ChangeRequest,
                ));
            }
            Ok(())
        }
    }

    impl CaptureBackend for X11CaptureBackend {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: "linux-x11-get-image",
                display_name: "X11 GetImage capture",
            }
        }

        fn status(&self) -> BackendStatus {
            BackendStatus::Ready
        }

        fn capabilities(&self) -> CaptureCapabilities {
            get_image_capabilities()
        }

        fn list_sources(&self) -> Result<Vec<CaptureSource>, CaptureError> {
            Ok(self
                .inner
                .sources
                .iter()
                .map(|source| source.portable.clone())
                .collect())
        }

        fn start_session(
            &self,
            request: CaptureRequest,
        ) -> Result<Box<dyn CaptureSession>, CaptureError> {
            self.validate_request(&request)?;
            Ok(Box::new(X11CaptureSession {
                backend: self.clone(),
                request,
                state: CaptureSessionState::Recording,
                sequence: 0,
                started_at: Instant::now(),
                next_due: Instant::now(),
            }))
        }
    }

    struct X11CaptureSession {
        backend: X11CaptureBackend,
        request: CaptureRequest,
        state: CaptureSessionState,
        sequence: u64,
        started_at: Instant,
        next_due: Instant,
    }

    impl X11CaptureSession {
        fn invalid_transition(&self, command: &str) -> CaptureError {
            CaptureError::new(
                CaptureErrorKind::InvalidStateTransition,
                format!("cannot {command} an X11 session in {:?} state", self.state),
                RecoveryHint::None,
            )
        }

        fn period(&self) -> Option<Duration> {
            match self.request.cadence {
                CaptureCadence::FixedFps(fps) => {
                    Some(Duration::from_nanos(1_000_000_000 / u64::from(fps.get())))
                }
                CaptureCadence::Interval(period) => Some(period),
                _ => None,
            }
        }
    }

    impl CaptureSession for X11CaptureSession {
        fn state(&self) -> CaptureSessionState {
            self.state
        }

        fn request(&self) -> &CaptureRequest {
            &self.request
        }

        fn pause(&mut self) -> Result<(), CaptureError> {
            if self.state != CaptureSessionState::Recording {
                return Err(self.invalid_transition("pause"));
            }
            self.state = CaptureSessionState::Paused;
            Ok(())
        }

        fn resume(&mut self) -> Result<(), CaptureError> {
            if self.state != CaptureSessionState::Paused {
                return Err(self.invalid_transition("resume"));
            }
            self.next_due = Instant::now();
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
            self.state = CaptureSessionState::Stopped;
            Ok(())
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
            self.state = CaptureSessionState::Discarded;
            Ok(())
        }

        fn poll_frame(&mut self, timeout: Duration) -> Result<FramePoll, CaptureError> {
            match self.state {
                CaptureSessionState::Recording => {}
                CaptureSessionState::Stopped
                | CaptureSessionState::Discarded
                | CaptureSessionState::Failed => return Ok(FramePoll::EndOfStream),
                _ => return Ok(FramePoll::Pending),
            }

            if let Some(period) = self.period() {
                let now = Instant::now();
                if self.next_due > now {
                    let wait = self.next_due.duration_since(now);
                    if wait > timeout {
                        if !timeout.is_zero() {
                            thread::sleep(timeout);
                        }
                        return Ok(FramePoll::Pending);
                    }
                    thread::sleep(wait);
                }
                self.next_due = Instant::now()
                    .checked_add(period)
                    .unwrap_or_else(Instant::now);
            }

            let elapsed = self.started_at.elapsed().as_micros();
            let timestamp =
                CaptureTimestamp::from_micros(u64::try_from(elapsed).unwrap_or(u64::MAX));
            match self
                .backend
                .capture_target(&self.request.target, self.sequence, timestamp)
            {
                Ok(frame) => {
                    self.sequence = self.sequence.checked_add(1).ok_or_else(|| {
                        CaptureError::new(
                            CaptureErrorKind::Platform,
                            "X11 capture sequence overflowed u64",
                            RecoveryHint::None,
                        )
                    })?;
                    Ok(FramePoll::Frame(frame))
                }
                Err(error) => {
                    self.state = CaptureSessionState::Failed;
                    Err(error)
                }
            }
        }
    }

    fn inspect_setup(
        connection: &RustConnection,
        screen_index: usize,
    ) -> Result<(u32, PixelLayout, Vec<X11Source>), CaptureError> {
        let setup = connection.setup();
        let screen = setup.roots.get(screen_index).ok_or_else(|| {
            CaptureError::new(
                CaptureErrorKind::BackendUnavailable,
                format!("X11 selected screen index {screen_index} is absent from server setup"),
                RecoveryHint::Retry,
            )
        })?;
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == screen.root_depth)
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorKind::Platform,
                    format!(
                        "X11 has no pixmap format for root depth {}",
                        screen.root_depth
                    ),
                    RecoveryHint::None,
                )
            })?;
        let visual = root_visual(screen)?;
        if visual.class != VisualClass::TRUE_COLOR {
            return Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                format!("X11 root visual {:?} is not TrueColor", visual.class),
                RecoveryHint::None,
            ));
        }
        let layout = PixelLayout {
            depth: screen.root_depth,
            bits_per_pixel: format.bits_per_pixel,
            scanline_pad: format.scanline_pad,
            byte_order: if setup.image_byte_order == ImageOrder::LSB_FIRST {
                ByteOrder::LeastSignificantFirst
            } else {
                ByteOrder::MostSignificantFirst
            },
            red_mask: visual.red_mask,
            green_mask: visual.green_mask,
            blue_mask: visual.blue_mask,
        };
        layout.validate()?;
        let sources = enumerate_sources(connection, screen_index, screen)?;
        Ok((screen.root, layout, sources))
    }

    fn root_visual(screen: &Screen) -> Result<&x11rb::protocol::xproto::Visualtype, CaptureError> {
        screen
            .allowed_depths
            .iter()
            .flat_map(|depth| depth.visuals.iter())
            .find(|visual| visual.visual_id == screen.root_visual)
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorKind::Platform,
                    "X11 root visual was absent from server setup",
                    RecoveryHint::None,
                )
            })
    }

    fn enumerate_sources(
        connection: &RustConnection,
        screen_index: usize,
        screen: &Screen,
    ) -> Result<Vec<X11Source>, CaptureError> {
        let root_region = PhysicalRect::new(
            0,
            0,
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        )?;
        let root_id = CaptureSourceId::new(format!("x11:screen:{screen_index}:root"))?;
        let root_source = CaptureSource::new(
            root_id,
            format!("X11 screen {screen_index} (root)"),
            CaptureSourceKind::Monitor,
            Some(root_region),
            1.0,
        )?;
        let mut sources = vec![X11Source {
            portable: root_source,
            root_region,
        }];

        let Ok(cookie) = connection.randr_get_monitors(screen.root, true) else {
            return Ok(sources);
        };
        let Ok(reply) = cookie.reply() else {
            return Ok(sources);
        };
        for (index, monitor) in reply.monitors.into_iter().enumerate() {
            if monitor.width == 0 || monitor.height == 0 {
                continue;
            }
            let region = PhysicalRect::new(
                i32::from(monitor.x),
                i32::from(monitor.y),
                u32::from(monitor.width),
                u32::from(monitor.height),
            )?;
            if !region.fits_within(root_region.size()) {
                continue;
            }
            let name = connection
                .get_atom_name(monitor.name)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| format!("Monitor {}", index + 1));
            let source_id =
                CaptureSourceId::new(format!("x11:screen:{screen_index}:monitor:{index}"))?;
            let portable = CaptureSource::new(
                source_id,
                name,
                CaptureSourceKind::Monitor,
                Some(region),
                1.0,
            )?;
            sources.push(X11Source {
                portable,
                root_region: region,
            });
        }
        Ok(sources)
    }

    fn get_image_capabilities() -> CaptureCapabilities {
        CaptureCapabilities {
            monitor: CapabilityStatus::Available,
            window: CapabilityStatus::Unavailable(
                "window enumeration/capture is not implemented by the GetImage slice".to_owned(),
            ),
            arbitrary_region: CapabilityStatus::Available,
            cursor_embedded: CapabilityStatus::Unavailable(
                "XFixes cursor composition is not implemented yet".to_owned(),
            ),
            cursor_metadata: CapabilityStatus::Unavailable(
                "XFixes cursor metadata is not implemented yet".to_owned(),
            ),
            passive_mouse_buttons: CapabilityStatus::Unavailable(
                "XInput mouse metadata is not implemented yet".to_owned(),
            ),
            passive_keyboard: CapabilityStatus::Unavailable(
                "XInput keyboard metadata is not implemented yet".to_owned(),
            ),
            global_shortcuts: CapabilityStatus::Unavailable(
                "X11 global shortcut registration is not implemented yet".to_owned(),
            ),
            camera: CapabilityStatus::Unavailable(
                "camera capture is provided by the separate camera backend".to_owned(),
            ),
        }
    }

    fn invalid_target(message: impl Into<String>) -> CaptureError {
        CaptureError::new(
            CaptureErrorKind::InvalidRequest,
            message,
            RecoveryHint::ChangeRequest,
        )
    }

    fn source_lost(context: &str, error: &impl std::fmt::Display) -> CaptureError {
        CaptureError::new(
            CaptureErrorKind::SourceLost,
            format!("failed to {context}: {error}"),
            RecoveryHint::ChooseDifferentSource,
        )
    }
}

#[cfg(not(all(target_os = "linux", feature = "native-x11")))]
mod native {
    use super::{
        BackendDescriptor, BackendStatus, CaptureBackend, CaptureCapabilities, CaptureError,
        CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSource, CaptureTarget,
        CapturedFrame, RecoveryHint,
    };

    /// API-compatible placeholder used when native X11 support is not linked.
    #[derive(Clone, Copy, Debug)]
    pub struct X11CaptureBackend;

    impl X11CaptureBackend {
        /// Returns whether native X11 support is compiled into this build.
        pub const fn compiled() -> bool {
            false
        }

        /// Reports that the `native-x11` feature is disabled.
        ///
        /// # Errors
        ///
        /// Always returns [`CaptureErrorKind::BackendUnavailable`].
        pub fn connect(_display: Option<&str>) -> Result<Self, CaptureError> {
            Err(not_compiled())
        }

        /// Reports that the `native-x11` feature is disabled.
        ///
        /// # Errors
        ///
        /// Always returns [`CaptureErrorKind::BackendUnavailable`].
        pub fn capture_once(&self, _target: &CaptureTarget) -> Result<CapturedFrame, CaptureError> {
            Err(not_compiled())
        }
    }

    impl CaptureBackend for X11CaptureBackend {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: "linux-x11-disabled",
                display_name: "X11 capture (not compiled)",
            }
        }

        fn status(&self) -> BackendStatus {
            BackendStatus::Unavailable(not_compiled().message().to_owned())
        }

        fn capabilities(&self) -> CaptureCapabilities {
            CaptureCapabilities::all_unavailable(not_compiled().message())
        }

        fn list_sources(&self) -> Result<Vec<CaptureSource>, CaptureError> {
            Err(not_compiled())
        }

        fn start_session(
            &self,
            _request: CaptureRequest,
        ) -> Result<Box<dyn CaptureSession>, CaptureError> {
            Err(not_compiled())
        }
    }

    fn not_compiled() -> CaptureError {
        CaptureError::new(
            CaptureErrorKind::BackendUnavailable,
            "X11 capture was not compiled; enable the 'native-x11' Cargo feature",
            RecoveryHint::InitializeBackend,
        )
    }
}

pub use native::X11CaptureBackend;

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ByteOrder {
    LeastSignificantFirst,
    MostSignificantFirst,
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelLayout {
    depth: u8,
    bits_per_pixel: u8,
    scanline_pad: u8,
    byte_order: ByteOrder,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
impl PixelLayout {
    fn validate(self) -> Result<(), CaptureError> {
        if !matches!(self.bits_per_pixel, 8 | 16 | 24 | 32) {
            return Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                format!(
                    "X11 bits-per-pixel {} is not supported by the GetImage decoder",
                    self.bits_per_pixel
                ),
                RecoveryHint::None,
            ));
        }
        if !matches!(self.scanline_pad, 8 | 16 | 32) {
            return Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                format!("X11 scanline padding {} is unsupported", self.scanline_pad),
                RecoveryHint::None,
            ));
        }
        if self.red_mask == 0 || self.green_mask == 0 || self.blue_mask == 0 {
            return Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                "X11 TrueColor visual has an empty RGB mask",
                RecoveryHint::None,
            ));
        }
        if self.red_mask & self.green_mask != 0
            || self.red_mask & self.blue_mask != 0
            || self.green_mask & self.blue_mask != 0
        {
            return Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                "X11 TrueColor visual has overlapping RGB masks",
                RecoveryHint::None,
            ));
        }
        Ok(())
    }
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn translate_region(
    source_root_region: gif_from_screen_capture::PhysicalRect,
    local_region: gif_from_screen_capture::PhysicalRect,
) -> Result<gif_from_screen_capture::PhysicalRect, CaptureError> {
    if !local_region.fits_within(source_root_region.size()) {
        return Err(CaptureError::new(
            CaptureErrorKind::InvalidRequest,
            "capture region falls outside its X11 parent source",
            RecoveryHint::ChangeRequest,
        ));
    }
    let x = source_root_region
        .origin()
        .x
        .checked_add(local_region.origin().x)
        .ok_or_else(|| CaptureError::invalid_request("X11 capture x coordinate overflow"))?;
    let y = source_root_region
        .origin()
        .y
        .checked_add(local_region.origin().y)
        .ok_or_else(|| CaptureError::invalid_request("X11 capture y coordinate overflow"))?;
    gif_from_screen_capture::PhysicalRect::new(
        x,
        y,
        local_region.size().width(),
        local_region.size().height(),
    )
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn decode_zpixmap(
    data: &[u8],
    width: u32,
    height: u32,
    layout: PixelLayout,
) -> Result<Vec<u8>, CaptureError> {
    layout.validate()?;
    if width == 0 || height == 0 {
        return Err(CaptureError::invalid_frame(
            "X11 GetImage returned an empty image",
        ));
    }
    let bits_per_row = u64::from(width)
        .checked_mul(u64::from(layout.bits_per_pixel))
        .ok_or_else(|| CaptureError::invalid_frame("X11 row bit count overflow"))?;
    let pad = u64::from(layout.scanline_pad);
    let padded_bits = bits_per_row
        .checked_add(pad - 1)
        .map(|value| value / pad * pad)
        .ok_or_else(|| CaptureError::invalid_frame("X11 padded row size overflow"))?;
    let source_stride = usize::try_from(padded_bits / 8)
        .map_err(|_| CaptureError::invalid_frame("X11 row stride exceeds usize"))?;
    let height = usize::try_from(height)
        .map_err(|_| CaptureError::invalid_frame("X11 image height exceeds usize"))?;
    let width = usize::try_from(width)
        .map_err(|_| CaptureError::invalid_frame("X11 image width exceeds usize"))?;
    let required = source_stride
        .checked_mul(height)
        .ok_or_else(|| CaptureError::invalid_frame("X11 image byte count overflow"))?;
    if data.len() < required {
        return Err(CaptureError::invalid_frame(format!(
            "X11 GetImage returned {} bytes but {required} are required",
            data.len()
        )));
    }

    let pixel_bytes = usize::from(layout.bits_per_pixel.div_ceil(8));
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| CaptureError::invalid_frame("RGBA pixel count overflow"))?;
    let mut rgba = Vec::with_capacity(
        pixel_count
            .checked_mul(4)
            .ok_or_else(|| CaptureError::invalid_frame("RGBA buffer size overflow"))?,
    );
    for y in 0..height {
        let row = &data[y * source_stride..(y + 1) * source_stride];
        for x in 0..width {
            let offset = x
                .checked_mul(pixel_bytes)
                .ok_or_else(|| CaptureError::invalid_frame("X11 pixel offset overflow"))?;
            let bytes = row.get(offset..offset + pixel_bytes).ok_or_else(|| {
                CaptureError::invalid_frame("X11 row ended inside a packed pixel")
            })?;
            let pixel = unpack_pixel(bytes, layout.byte_order);
            rgba.extend_from_slice(&[
                scale_mask(pixel, layout.red_mask),
                scale_mask(pixel, layout.green_mask),
                scale_mask(pixel, layout.blue_mask),
                255,
            ]);
        }
    }
    Ok(rgba)
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn unpack_pixel(bytes: &[u8], byte_order: ByteOrder) -> u32 {
    match byte_order {
        ByteOrder::LeastSignificantFirst => bytes
            .iter()
            .enumerate()
            .fold(0_u32, |pixel, (index, byte)| {
                pixel | (u32::from(*byte) << (index * 8))
            }),
        ByteOrder::MostSignificantFirst => bytes
            .iter()
            .fold(0_u32, |pixel, byte| (pixel << 8) | u32::from(*byte)),
    }
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn scale_mask(pixel: u32, mask: u32) -> u8 {
    let shift = mask.trailing_zeros();
    let maximum = mask >> shift;
    let value = (pixel & mask) >> shift;
    let scaled = (u64::from(value) * 255 + u64::from(maximum) / 2) / u64::from(maximum);
    u8::try_from(scaled).unwrap_or(255)
}

#[cfg(test)]
mod tests {
    use gif_from_screen_capture::PhysicalRect;
    #[cfg(all(target_os = "linux", feature = "native-x11"))]
    use gif_from_screen_capture::{CaptureBackend as _, CaptureSourceId};

    use super::*;

    const RGB888_LE: PixelLayout = PixelLayout {
        depth: 24,
        bits_per_pixel: 32,
        scanline_pad: 32,
        byte_order: ByteOrder::LeastSignificantFirst,
        red_mask: 0x00ff_0000,
        green_mask: 0x0000_ff00,
        blue_mask: 0x0000_00ff,
    };

    #[test]
    fn decodes_common_little_endian_bgrx_into_rgba() {
        let decoded = decode_zpixmap(&[0x33, 0x22, 0x11, 0], 1, 1, RGB888_LE).unwrap();
        assert_eq!(decoded, [0x11, 0x22, 0x33, 0xff]);
    }

    #[test]
    fn decodes_big_endian_and_row_padding() {
        let layout = PixelLayout {
            bits_per_pixel: 24,
            byte_order: ByteOrder::MostSignificantFirst,
            ..RGB888_LE
        };
        let decoded = decode_zpixmap(&[0x11, 0x22, 0x33, 0], 1, 1, layout).unwrap();
        assert_eq!(decoded, [0x11, 0x22, 0x33, 0xff]);
    }

    #[test]
    fn scales_rgb565_channels() {
        let layout = PixelLayout {
            depth: 16,
            bits_per_pixel: 16,
            scanline_pad: 16,
            byte_order: ByteOrder::LeastSignificantFirst,
            red_mask: 0xf800,
            green_mask: 0x07e0,
            blue_mask: 0x001f,
        };
        let decoded = decode_zpixmap(&[0xe0, 0x07], 1, 1, layout).unwrap();
        assert_eq!(decoded, [0, 255, 0, 255]);
    }

    #[test]
    fn rejects_truncated_server_frame() {
        let error = decode_zpixmap(&[0; 7], 2, 1, RGB888_LE).unwrap_err();
        assert_eq!(error.kind(), CaptureErrorKind::InvalidFrame);
    }

    #[test]
    fn translates_source_local_region_to_root_coordinates() {
        let source = PhysicalRect::new(100, 50, 800, 600).unwrap();
        let local = PhysicalRect::new(10, 20, 30, 40).unwrap();
        let translated = translate_region(source, local).unwrap();
        assert_eq!(translated, PhysicalRect::new(110, 70, 30, 40).unwrap());
        assert!(translate_region(source, PhysicalRect::new(790, 590, 20, 20).unwrap()).is_err());
    }

    #[test]
    fn disabled_build_returns_clear_error() {
        if X11CaptureBackend::compiled() {
            return;
        }
        let error = X11CaptureBackend::connect(Some(":99"))
            .expect_err("disabled feature must fail before connecting");
        assert_eq!(error.kind(), CaptureErrorKind::BackendUnavailable);
        assert!(error.message().contains("native-x11"));
    }

    #[cfg(all(target_os = "linux", feature = "native-x11"))]
    #[test]
    fn real_x11_smoke_test_when_display_is_available() {
        let Some(display) = std::env::var("DISPLAY")
            .ok()
            .filter(|display| !display.trim().is_empty())
        else {
            eprintln!("skipping real X11 smoke test because DISPLAY is unset");
            return;
        };
        let backend = X11CaptureBackend::connect(Some(&display)).unwrap();
        let root = backend
            .list_sources()
            .unwrap()
            .into_iter()
            .next()
            .expect("root source");
        let target = CaptureTarget::Region {
            source: CaptureSourceId::new(root.id().as_str()).unwrap(),
            region: PhysicalRect::new(0, 0, 2, 2).unwrap(),
        };
        let frame = backend.capture_once(&target).unwrap();
        assert_eq!(frame.size().width(), 2);
        assert_eq!(frame.size().height(), 2);
        assert_eq!(frame.pixels().len(), 16);
    }
}
