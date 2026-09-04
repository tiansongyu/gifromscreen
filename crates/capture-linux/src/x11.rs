use gif_from_screen_capture::{
    BackendDescriptor, BackendStatus, CaptureBackend, CaptureCapabilities, CaptureError,
    CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSource, CaptureTarget, CapturedFrame,
    RecoveryHint,
};

#[cfg(all(target_os = "linux", feature = "native-x11"))]
mod native {
    use std::collections::{HashSet, VecDeque};
    use std::fmt::{Debug, Formatter};
    use std::process;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use gif_from_screen_capture::{
        CapabilityStatus, CaptureCadence, CaptureSessionState, CaptureSourceId, CaptureSourceKind,
        CaptureTimestamp, CursorCaptureMode, FramePoll, PhysicalRect, PixelFormat,
    };
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{
        Atom, AtomEnum, ConnectionExt as _, ImageFormat, ImageOrder, MapState, Screen, VisualClass,
        Window, WindowClass,
    };
    use x11rb::rust_connection::RustConnection;

    use super::{
        BackendDescriptor, BackendStatus, CaptureBackend, CaptureCapabilities, CaptureError,
        CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSource, CaptureTarget,
        CapturedFrame, RecoveryHint, WindowFilterFacts, decode_text_property, decode_u32_property,
        decode_zpixmap, format_window_source_id, intersect_rect, parse_window_source_id,
        should_list_window, translate_region,
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
        root_region: PhysicalRect,
        monitor_sources: Vec<X11Source>,
        atoms: X11Atoms,
        layout: PixelLayout,
        connected_at: Instant,
        direct_sequence: AtomicU64,
    }

    #[derive(Clone)]
    struct X11Source {
        portable: CaptureSource,
        root_region: PhysicalRect,
    }

    #[derive(Clone, Copy)]
    struct X11Atoms {
        net_client_list: Atom,
        net_wm_name: Atom,
        utf8_string: Atom,
        net_wm_pid: Atom,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct ResolvedTarget {
        root_region: PhysicalRect,
        tracked_window: Option<(Window, PhysicalRect)>,
    }

    impl X11Atoms {
        fn intern(connection: &RustConnection) -> Result<Self, CaptureError> {
            Ok(Self {
                net_client_list: intern_optional_atom(connection, b"_NET_CLIENT_LIST")?,
                net_wm_name: intern_optional_atom(connection, b"_NET_WM_NAME")?,
                utf8_string: intern_optional_atom(connection, b"UTF8_STRING")?,
                net_wm_pid: intern_optional_atom(connection, b"_NET_WM_PID")?,
            })
        }
    }

    impl Debug for X11CaptureBackend {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("X11CaptureBackend")
                .field("screen_index", &self.inner.screen_index)
                .field("root", &self.inner.root)
                .field("monitor_source_count", &self.inner.monitor_sources.len())
                .finish_non_exhaustive()
        }
    }

    impl X11CaptureBackend {
        const WINDOW_CAPTURE_ATTEMPTS: usize = 3;

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

            let (root, root_region, layout, monitor_sources) =
                inspect_setup(&connection, screen_index)?;
            let atoms = X11Atoms::intern(&connection)?;
            Ok(Self {
                inner: Arc::new(X11Inner {
                    connection,
                    screen_index,
                    root,
                    root_region,
                    monitor_sources,
                    atoms,
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
            for attempt in 0..Self::WINDOW_CAPTURE_ATTEMPTS {
                let resolved = self.resolve_target(target)?;
                let rgba = self.capture_root_pixels(resolved.root_region)?;

                if let Some((window, expected_region)) = resolved.tracked_window {
                    let current_region = self.live_window_region(window)?;
                    if current_region != expected_region {
                        if attempt + 1 < Self::WINDOW_CAPTURE_ATTEMPTS {
                            continue;
                        }
                        return Err(CaptureError::new(
                            CaptureErrorKind::SourceLost,
                            format!(
                                "X11 window 0x{window:08x} kept moving or resizing while a frame was captured"
                            ),
                            RecoveryHint::Retry,
                        ));
                    }
                }

                let size = resolved.root_region.size();
                let stride = usize::try_from(size.width())
                    .ok()
                    .and_then(|width| width.checked_mul(4))
                    .ok_or_else(|| CaptureError::invalid_frame("RGBA stride overflow"))?;
                return CapturedFrame::new(
                    sequence,
                    timestamp,
                    size,
                    stride,
                    PixelFormat::Rgba8,
                    rgba,
                );
            }
            Err(CaptureError::new(
                CaptureErrorKind::SourceLost,
                "X11 window capture could not stabilize",
                RecoveryHint::Retry,
            ))
        }

        fn capture_root_pixels(&self, requested: PhysicalRect) -> Result<Vec<u8>, CaptureError> {
            // Reading the root keeps every target on the setup root visual and
            // gives defined pixels when another window occludes the client.
            // The trade-off is intentional: this records what the user sees,
            // including overlapping windows, rather than an off-screen client
            // backing store with compositor-dependent contents.
            let clipped = intersect_rect(requested, self.inner.root_region).ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorKind::SourceLost,
                    "the selected X11 window is completely outside the visible root window",
                    RecoveryHint::ChooseDifferentSource,
                )
            })?;
            let x = i16::try_from(clipped.origin().x).map_err(|_| {
                invalid_target("X11 capture x coordinate is outside the protocol's i16 range")
            })?;
            let y = i16::try_from(clipped.origin().y).map_err(|_| {
                invalid_target("X11 capture y coordinate is outside the protocol's i16 range")
            })?;
            let width = u16::try_from(clipped.size().width()).map_err(|_| {
                invalid_target("X11 capture width is outside the protocol's u16 range")
            })?;
            let height = u16::try_from(clipped.size().height()).map_err(|_| {
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
            let clipped_rgba = decode_zpixmap(
                &reply.data,
                u32::from(width),
                u32::from(height),
                self.inner.layout,
            )?;
            if clipped == requested {
                return Ok(clipped_rgba);
            }
            pad_clipped_frame(requested, clipped, &clipped_rgba)
        }

        fn resolve_target(&self, target: &CaptureTarget) -> Result<ResolvedTarget, CaptureError> {
            match target {
                CaptureTarget::Monitor(source_id) => {
                    self.find_monitor(source_id).map(|source| ResolvedTarget {
                        root_region: source.root_region,
                        tracked_window: None,
                    })
                }
                CaptureTarget::Window(source_id) => {
                    let window = self.parse_window_id(source_id)?;
                    let root_region = self.live_window_region(window)?;
                    Ok(ResolvedTarget {
                        root_region,
                        tracked_window: Some((window, root_region)),
                    })
                }
                CaptureTarget::Region { source, region } => {
                    if let Some(monitor) = self
                        .inner
                        .monitor_sources
                        .iter()
                        .find(|candidate| candidate.portable.id() == source)
                    {
                        return Ok(ResolvedTarget {
                            root_region: translate_region(monitor.root_region, *region)?,
                            tracked_window: None,
                        });
                    }
                    let window = self.parse_window_id(source)?;
                    let window_region = self.live_window_region(window)?;
                    Ok(ResolvedTarget {
                        root_region: translate_region(window_region, *region)?,
                        tracked_window: Some((window, window_region)),
                    })
                }
                _ => Err(invalid_target("unknown capture target kind")),
            }
        }

        fn find_monitor(&self, source_id: &CaptureSourceId) -> Result<&X11Source, CaptureError> {
            self.inner
                .monitor_sources
                .iter()
                .find(|candidate| candidate.portable.id() == source_id)
                .ok_or_else(|| source_not_found(source_id))
        }

        fn parse_window_id(&self, source_id: &CaptureSourceId) -> Result<Window, CaptureError> {
            parse_window_source_id(source_id.as_str(), self.inner.screen_index)
                .ok_or_else(|| source_not_found(source_id))
        }

        fn live_window_region(&self, window: Window) -> Result<PhysicalRect, CaptureError> {
            let attributes = self
                .inner
                .connection
                .get_window_attributes(window)
                .map_err(|error| source_lost("inspect the selected X11 window", &error))?
                .reply()
                .map_err(|error| source_lost("inspect the selected X11 window", &error))?;
            if attributes.map_state != MapState::VIEWABLE {
                return Err(window_unavailable(window, "is no longer viewable"));
            }
            if attributes.class != WindowClass::INPUT_OUTPUT {
                return Err(window_unavailable(window, "is not an InputOutput window"));
            }
            if attributes.override_redirect {
                return Err(window_unavailable(
                    window,
                    "became an override-redirect helper window",
                ));
            }

            let geometry = self
                .inner
                .connection
                .get_geometry(window)
                .map_err(|error| source_lost("query the selected X11 window geometry", &error))?
                .reply()
                .map_err(|error| source_lost("query the selected X11 window geometry", &error))?;
            if geometry.width == 0 || geometry.height == 0 {
                return Err(window_unavailable(window, "has an empty geometry"));
            }
            if geometry.root != self.inner.root {
                return Err(window_unavailable(window, "moved to another X11 screen"));
            }

            let translated = self
                .inner
                .connection
                .translate_coordinates(window, self.inner.root, 0, 0)
                .map_err(|error| source_lost("translate the selected X11 window", &error))?
                .reply()
                .map_err(|error| source_lost("translate the selected X11 window", &error))?;
            if !translated.same_screen {
                return Err(window_unavailable(window, "moved to another X11 screen"));
            }
            let region = PhysicalRect::new(
                i32::from(translated.dst_x),
                i32::from(translated.dst_y),
                u32::from(geometry.width),
                u32::from(geometry.height),
            )?;
            if intersect_rect(region, self.inner.root_region).is_none() {
                return Err(window_unavailable(
                    window,
                    "is completely outside the visible root window",
                ));
            }
            if window_pid(&self.inner.connection, window, self.inner.atoms.net_wm_pid)
                == Some(process::id())
            {
                return Err(window_unavailable(
                    window,
                    "belongs to this recorder and is intentionally excluded",
                ));
            }
            Ok(region)
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
            let mut sources: Vec<_> = self
                .inner
                .monitor_sources
                .iter()
                .map(|source| source.portable.clone())
                .collect();
            sources.extend(enumerate_window_sources(
                &self.inner.connection,
                self.inner.screen_index,
                self.inner.root,
                self.inner.root_region,
                self.inner.atoms,
            )?);
            Ok(sources)
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

    fn intern_optional_atom(
        connection: &RustConnection,
        name: &[u8],
    ) -> Result<Atom, CaptureError> {
        connection
            .intern_atom(true, name)
            .map_err(|error| platform_error("send X11 InternAtom request", &error))?
            .reply()
            .map(|reply| reply.atom)
            .map_err(|error| platform_error("receive X11 InternAtom reply", &error))
    }

    /// Enumerates selectable application windows on every call so newly
    /// created and destroyed windows are reflected without reconnecting.
    ///
    /// The policy intentionally lists only viewable, non-empty `InputOutput`
    /// windows that intersect the root. Override-redirect helpers (menus,
    /// tooltips, drag icons) and windows whose `_NET_WM_PID` equals this
    /// process are excluded. An absent PID is retained because remote/legacy
    /// clients commonly omit it. EWMH clients may use an XID fallback label;
    /// `QueryTree` fallback candidates need a real title to avoid exposing
    /// internal widget windows.
    fn enumerate_window_sources(
        connection: &RustConnection,
        screen_index: usize,
        root: Window,
        root_region: PhysicalRect,
        atoms: X11Atoms,
    ) -> Result<Vec<CaptureSource>, CaptureError> {
        let (windows, from_ewmh) =
            match ewmh_client_windows(connection, root, atoms.net_client_list) {
                Some(windows) => (windows, true),
                None => (query_tree_windows(connection, root)?, false),
            };
        let own_pid = process::id();
        let mut seen = HashSet::new();
        let mut sources = Vec::new();
        for window in windows {
            if window == 0 || window == root || !seen.insert(window) {
                continue;
            }
            if let Some(source) = inspect_enumerated_window(
                connection,
                screen_index,
                root,
                root_region,
                atoms,
                window,
                own_pid,
                from_ewmh,
            ) {
                sources.push(source);
            }
        }
        Ok(sources)
    }

    #[allow(clippy::too_many_arguments)]
    fn inspect_enumerated_window(
        connection: &RustConnection,
        screen_index: usize,
        root: Window,
        root_region: PhysicalRect,
        atoms: X11Atoms,
        window: Window,
        own_pid: u32,
        from_ewmh: bool,
    ) -> Option<CaptureSource> {
        let attributes = connection
            .get_window_attributes(window)
            .ok()?
            .reply()
            .ok()?;
        let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
        if geometry.root != root {
            return None;
        }
        let translated = connection
            .translate_coordinates(window, root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        if !translated.same_screen {
            return None;
        }
        let region = PhysicalRect::new(
            i32::from(translated.dst_x),
            i32::from(translated.dst_y),
            u32::from(geometry.width),
            u32::from(geometry.height),
        )
        .ok()?;
        let title = window_title(connection, window, atoms);
        let facts = WindowFilterFacts {
            viewable: attributes.map_state == MapState::VIEWABLE,
            input_output: attributes.class == WindowClass::INPUT_OUTPUT,
            override_redirect: attributes.override_redirect,
            width: u32::from(geometry.width),
            height: u32::from(geometry.height),
            intersects_root: intersect_rect(region, root_region).is_some(),
            owner_pid: window_pid(connection, window, atoms.net_wm_pid),
            has_title: title.is_some(),
            from_ewmh,
        };
        if !should_list_window(facts, own_pid) {
            return None;
        }

        let name = title.unwrap_or_else(|| format!("Window 0x{window:08x}"));
        let id = CaptureSourceId::new(format_window_source_id(screen_index, window)).ok()?;
        CaptureSource::new(id, name, CaptureSourceKind::Window, Some(region), 1.0).ok()
    }

    fn ewmh_client_windows(
        connection: &RustConnection,
        root: Window,
        net_client_list: Atom,
    ) -> Option<Vec<Window>> {
        if net_client_list == 0 {
            return None;
        }
        let reply = connection
            .get_property(false, root, net_client_list, AtomEnum::WINDOW, 0, u32::MAX)
            .ok()?
            .reply()
            .ok()?;
        if reply.type_ != u32::from(AtomEnum::WINDOW) {
            return None;
        }
        decode_u32_property(reply.format, &reply.value)
    }

    fn query_tree_windows(
        connection: &RustConnection,
        root: Window,
    ) -> Result<Vec<Window>, CaptureError> {
        const MAX_FALLBACK_WINDOWS: usize = 16_384;

        let children = connection
            .query_tree(root)
            .map_err(|error| platform_error("send X11 QueryTree fallback request", &error))?
            .reply()
            .map_err(|error| platform_error("receive X11 QueryTree fallback reply", &error))?
            .children;
        let mut queue: VecDeque<_> = children.into();
        let mut seen = HashSet::new();
        let mut windows = Vec::new();
        while let Some(window) = queue.pop_front() {
            if window == root || !seen.insert(window) {
                continue;
            }
            windows.push(window);
            if windows.len() >= MAX_FALLBACK_WINDOWS {
                break;
            }
            let Some(children) = connection
                .query_tree(window)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| reply.children)
            else {
                continue;
            };
            queue.extend(children);
        }
        Ok(windows)
    }

    fn window_title(
        connection: &RustConnection,
        window: Window,
        atoms: X11Atoms,
    ) -> Option<String> {
        if atoms.net_wm_name != 0 && atoms.utf8_string != 0 {
            let modern = connection
                .get_property(
                    false,
                    window,
                    atoms.net_wm_name,
                    atoms.utf8_string,
                    0,
                    u32::MAX,
                )
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .filter(|reply| reply.type_ == atoms.utf8_string)
                .and_then(|reply| decode_text_property(reply.format, &reply.value));
            if modern.is_some() {
                return modern;
            }
        }
        connection
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, u32::MAX)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|reply| decode_text_property(reply.format, &reply.value))
    }

    fn window_pid(connection: &RustConnection, window: Window, net_wm_pid: Atom) -> Option<u32> {
        if net_wm_pid == 0 {
            return None;
        }
        let reply = connection
            .get_property(false, window, net_wm_pid, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        if reply.type_ != u32::from(AtomEnum::CARDINAL) {
            return None;
        }
        decode_u32_property(reply.format, &reply.value)?
            .into_iter()
            .next()
    }

    fn pad_clipped_frame(
        requested: PhysicalRect,
        clipped: PhysicalRect,
        clipped_rgba: &[u8],
    ) -> Result<Vec<u8>, CaptureError> {
        let requested_width = usize::try_from(requested.size().width())
            .map_err(|_| CaptureError::invalid_frame("X11 requested width exceeds usize"))?;
        let requested_height = usize::try_from(requested.size().height())
            .map_err(|_| CaptureError::invalid_frame("X11 requested height exceeds usize"))?;
        let clipped_width = usize::try_from(clipped.size().width())
            .map_err(|_| CaptureError::invalid_frame("X11 clipped width exceeds usize"))?;
        let clipped_height = usize::try_from(clipped.size().height())
            .map_err(|_| CaptureError::invalid_frame("X11 clipped height exceeds usize"))?;
        let clipped_stride = clipped_width
            .checked_mul(4)
            .ok_or_else(|| CaptureError::invalid_frame("X11 clipped stride overflow"))?;
        let expected_clipped_len = clipped_stride
            .checked_mul(clipped_height)
            .ok_or_else(|| CaptureError::invalid_frame("X11 clipped frame size overflow"))?;
        if clipped_rgba.len() != expected_clipped_len {
            return Err(CaptureError::invalid_frame(
                "X11 clipped RGBA frame has an unexpected byte count",
            ));
        }
        let output_stride = requested_width
            .checked_mul(4)
            .ok_or_else(|| CaptureError::invalid_frame("X11 output stride overflow"))?;
        let output_len = output_stride
            .checked_mul(requested_height)
            .ok_or_else(|| CaptureError::invalid_frame("X11 output frame size overflow"))?;
        let mut output = vec![0_u8; output_len];
        for pixel in output.chunks_exact_mut(4) {
            pixel[3] = 255;
        }

        let x_offset = clipped
            .origin()
            .x
            .checked_sub(requested.origin().x)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| CaptureError::invalid_frame("X11 clipped x offset is invalid"))?;
        let y_offset = clipped
            .origin()
            .y
            .checked_sub(requested.origin().y)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| CaptureError::invalid_frame("X11 clipped y offset is invalid"))?;
        let destination_x = x_offset
            .checked_mul(4)
            .ok_or_else(|| CaptureError::invalid_frame("X11 destination x offset overflow"))?;
        for row in 0..clipped_height {
            let source_start = row
                .checked_mul(clipped_stride)
                .ok_or_else(|| CaptureError::invalid_frame("X11 source row offset overflow"))?;
            let destination_start = y_offset
                .checked_add(row)
                .and_then(|row| row.checked_mul(output_stride))
                .and_then(|offset| offset.checked_add(destination_x))
                .ok_or_else(|| {
                    CaptureError::invalid_frame("X11 destination row offset overflow")
                })?;
            output[destination_start..destination_start + clipped_stride]
                .copy_from_slice(&clipped_rgba[source_start..source_start + clipped_stride]);
        }
        Ok(output)
    }

    fn inspect_setup(
        connection: &RustConnection,
        screen_index: usize,
    ) -> Result<(u32, PhysicalRect, PixelLayout, Vec<X11Source>), CaptureError> {
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
        let root_region = PhysicalRect::new(
            0,
            0,
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        )?;
        let sources = enumerate_monitor_sources(connection, screen_index, screen, root_region)?;
        Ok((screen.root, root_region, layout, sources))
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

    fn enumerate_monitor_sources(
        connection: &RustConnection,
        screen_index: usize,
        screen: &Screen,
        root_region: PhysicalRect,
    ) -> Result<Vec<X11Source>, CaptureError> {
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
            window: CapabilityStatus::Limited(
                "captures the window's current root pixels; overlapping windows are included"
                    .to_owned(),
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

    fn source_not_found(source_id: &CaptureSourceId) -> CaptureError {
        CaptureError::new(
            CaptureErrorKind::SourceNotFound,
            format!("X11 capture source '{source_id}' was not found"),
            RecoveryHint::ChooseDifferentSource,
        )
    }

    fn window_unavailable(window: Window, reason: &str) -> CaptureError {
        CaptureError::new(
            CaptureErrorKind::SourceLost,
            format!("X11 window 0x{window:08x} {reason}"),
            RecoveryHint::ChooseDifferentSource,
        )
    }

    fn platform_error(context: &str, error: &impl std::fmt::Display) -> CaptureError {
        CaptureError::new(
            CaptureErrorKind::Platform,
            format!("failed to {context}: {error}"),
            RecoveryHint::Retry,
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
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowFilterFacts {
    viewable: bool,
    input_output: bool,
    override_redirect: bool,
    width: u32,
    height: u32,
    intersects_root: bool,
    owner_pid: Option<u32>,
    has_title: bool,
    from_ewmh: bool,
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn should_list_window(facts: WindowFilterFacts, own_pid: u32) -> bool {
    facts.viewable
        && facts.input_output
        && !facts.override_redirect
        && facts.width > 0
        && facts.height > 0
        && facts.intersects_root
        && facts.owner_pid != Some(own_pid)
        && (facts.from_ewmh || facts.has_title)
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn format_window_source_id(screen_index: usize, window: u32) -> String {
    format!("x11:screen:{screen_index}:window:0x{window:08x}")
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn parse_window_source_id(source_id: &str, screen_index: usize) -> Option<u32> {
    let prefix = format!("x11:screen:{screen_index}:window:0x");
    let xid = source_id.strip_prefix(&prefix)?;
    if xid.len() != 8 || !xid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(xid, 16)
        .ok()
        .filter(|window| *window != 0)
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn decode_u32_property(format: u8, value: &[u8]) -> Option<Vec<u32>> {
    if format != 32 || !value.len().is_multiple_of(4) {
        return None;
    }
    Some(
        value
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect(),
    )
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn decode_text_property(format: u8, value: &[u8]) -> Option<String> {
    if format != 8 {
        return None;
    }
    let text = value.split(|byte| *byte == 0).next().unwrap_or_default();
    let normalized = String::from_utf8_lossy(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn intersect_rect(
    left: gif_from_screen_capture::PhysicalRect,
    right: gif_from_screen_capture::PhysicalRect,
) -> Option<gif_from_screen_capture::PhysicalRect> {
    let x = i64::from(left.origin().x).max(i64::from(right.origin().x));
    let y = i64::from(left.origin().y).max(i64::from(right.origin().y));
    let left_edge = i64::from(left.origin().x) + i64::from(left.size().width());
    let right_edge = i64::from(right.origin().x) + i64::from(right.size().width());
    let bottom_left = i64::from(left.origin().y) + i64::from(left.size().height());
    let bottom_right = i64::from(right.origin().y) + i64::from(right.size().height());
    let width = left_edge.min(right_edge).checked_sub(x)?;
    let height = bottom_left.min(bottom_right).checked_sub(y)?;
    if width <= 0 || height <= 0 {
        return None;
    }
    gif_from_screen_capture::PhysicalRect::new(
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
    )
    .ok()
}

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
    use gif_from_screen_capture::{CaptureSourceId, CaptureSourceKind};

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
    fn parses_native_u32_properties_and_rejects_wrong_formats() {
        let expected = [0x0123_4567_u32, 0x89ab_cdef];
        let bytes: Vec<_> = expected
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        assert_eq!(decode_u32_property(32, &bytes).unwrap(), expected);
        assert!(decode_u32_property(8, &bytes).is_none());
        assert!(decode_u32_property(32, &bytes[..7]).is_none());
    }

    #[test]
    fn parses_ewmh_text_without_nul_tail_or_layout_whitespace() {
        assert_eq!(
            decode_text_property(8, b"  GIF\n recorder  \0ignored"),
            Some("GIF recorder".to_owned())
        );
        assert_eq!(decode_text_property(8, b" \t\n"), None);
        assert_eq!(decode_text_property(32, b"title"), None);
    }

    #[test]
    fn window_source_ids_round_trip_the_xid_and_screen() {
        let id = format_window_source_id(2, 0x01ab_cdef);
        assert_eq!(id, "x11:screen:2:window:0x01abcdef");
        assert_eq!(parse_window_source_id(&id, 2), Some(0x01ab_cdef));
        assert_eq!(parse_window_source_id(&id, 1), None);
        assert_eq!(
            parse_window_source_id("x11:screen:2:window:0x00000000", 2),
            None
        );
        assert_eq!(parse_window_source_id("x11:screen:2:window:1234", 2), None);
    }

    #[test]
    fn window_filter_excludes_invisible_empty_helpers_and_own_process() {
        let own_pid = 42;
        let eligible = WindowFilterFacts {
            viewable: true,
            input_output: true,
            override_redirect: false,
            width: 640,
            height: 480,
            intersects_root: true,
            owner_pid: Some(7),
            has_title: true,
            from_ewmh: true,
        };
        assert!(should_list_window(eligible, own_pid));
        assert!(!should_list_window(
            WindowFilterFacts {
                viewable: false,
                ..eligible
            },
            own_pid
        ));
        assert!(!should_list_window(
            WindowFilterFacts {
                width: 0,
                ..eligible
            },
            own_pid
        ));
        assert!(!should_list_window(
            WindowFilterFacts {
                override_redirect: true,
                ..eligible
            },
            own_pid
        ));
        assert!(!should_list_window(
            WindowFilterFacts {
                owner_pid: Some(own_pid),
                ..eligible
            },
            own_pid
        ));
        assert!(should_list_window(
            WindowFilterFacts {
                owner_pid: None,
                has_title: false,
                ..eligible
            },
            own_pid
        ));
        assert!(!should_list_window(
            WindowFilterFacts {
                has_title: false,
                from_ewmh: false,
                ..eligible
            },
            own_pid
        ));
    }

    #[test]
    fn rectangle_intersection_handles_partially_offscreen_windows() {
        let root = PhysicalRect::new(0, 0, 1920, 1080).unwrap();
        let window = PhysicalRect::new(-20, 100, 100, 50).unwrap();
        assert_eq!(
            intersect_rect(root, window),
            Some(PhysicalRect::new(0, 100, 80, 50).unwrap())
        );
        assert!(intersect_rect(root, PhysicalRect::new(-100, -100, 50, 50).unwrap()).is_none());
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
        let Ok(backend) = X11CaptureBackend::connect(Some(&display)) else {
            eprintln!("skipping real X11 smoke test because DISPLAY is not reachable");
            return;
        };
        let sources = backend.list_sources().unwrap();
        let root = sources
            .iter()
            .find(|source| source.kind() == CaptureSourceKind::Monitor)
            .expect("root source");
        let target = CaptureTarget::Region {
            source: CaptureSourceId::new(root.id().as_str()).unwrap(),
            region: PhysicalRect::new(0, 0, 2, 2).unwrap(),
        };
        let frame = backend.capture_once(&target).unwrap();
        assert_eq!(frame.size().width(), 2);
        assert_eq!(frame.size().height(), 2);
        assert_eq!(frame.pixels().len(), 16);

        let Some(window) = sources
            .into_iter()
            .find(|source| source.kind() == CaptureSourceKind::Window)
        else {
            eprintln!("skipping X11 window capture smoke check because no window is selectable");
            return;
        };
        assert!(window.id().as_str().contains(":window:0x"));
        match backend.capture_once(&CaptureTarget::Window(window.id().clone())) {
            Ok(frame) => {
                assert!(frame.size().width() > 0);
                assert!(frame.size().height() > 0);
            }
            Err(error) if error.kind() == CaptureErrorKind::SourceLost => {
                eprintln!("window vanished during optional smoke capture: {error}");
            }
            Err(error) => panic!("X11 window smoke capture failed: {error}"),
        }
    }
}
