//! Backend interaction tests own their Xvfb and never consult host DISPLAY.

use gif_from_screen_capture::{
    CaptureCadence, CaptureSessionState, CaptureSourceKind, FramePoll, InputEvent, KeyState,
};
use x11rb::{
    connection::Connection,
    protocol::{
        xproto::{
            ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, InputFocus, WindowClass,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

use super::{cursor_edge_tests::PrivateXvfb, *};

struct Fixture {
    connection: RustConnection,
    root: u32,
    window: u32,
    backend: X11CaptureBackend,
    target: CaptureTarget,
    _server: PrivateXvfb,
}

impl Fixture {
    fn new() -> Self {
        let server = PrivateXvfb::start();
        let (connection, screen) = x11rb::connect(Some(&server.display)).unwrap();
        let root = connection.setup().roots[screen].root;
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                window,
                root,
                10,
                10,
                100,
                100,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .background_pixel(0x00ff_0000),
            )
            .unwrap()
            .check()
            .unwrap();
        connection.map_window(window).unwrap().check().unwrap();
        connection
            .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        connection
            .warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 15, 15)
            .unwrap()
            .check()
            .unwrap();
        let backend = X11CaptureBackend::connect(Some(&server.display)).unwrap();
        let source = backend
            .list_sources()
            .unwrap()
            .into_iter()
            .find(|source| source.kind() == CaptureSourceKind::Monitor)
            .unwrap();
        let target = CaptureTarget::Region {
            source: source.id().clone(),
            region: PhysicalRect::new(12, 12, 8, 8).unwrap(),
        };
        Self {
            connection,
            root,
            window,
            backend,
            target,
            _server: server,
        }
    }

    fn session(&self, metadata: bool) -> Box<dyn CaptureSession> {
        let mut request = CaptureRequest::new(self.target.clone(), CaptureCadence::OnInteraction);
        request.cursor = gif_from_screen_capture::CursorCaptureMode::Hidden;
        request.input_events = metadata;
        self.backend.start_session(request).unwrap()
    }

    fn emit(&self, kind: u8, detail: u8) {
        self.connection
            .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, 0, 0, 0)
            .unwrap()
            .check()
            .unwrap();
    }

    fn paint(&self, pixel: u32) {
        self.connection
            .change_window_attributes(
                self.window,
                &ChangeWindowAttributesAux::new().background_pixel(pixel),
            )
            .unwrap()
            .check()
            .unwrap();
        self.connection
            .clear_area(false, self.window, 0, 0, 0, 0)
            .unwrap()
            .check()
            .unwrap();
    }
}

fn sample(session: &mut dyn CaptureSession) -> CapturedFrame {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match session.poll_frame(Duration::from_millis(10)).unwrap() {
            FramePoll::Frame(frame) => return frame,
            FramePoll::Pending => assert!(
                Instant::now() < deadline,
                "interaction never yielded a frame"
            ),
            FramePoll::EndOfStream => panic!("interaction session ended"),
        }
    }
}

fn no_sample(session: &mut dyn CaptureSession) {
    assert_eq!(
        session.poll_frame(Duration::from_millis(30)).unwrap(),
        FramePoll::Pending
    );
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_interaction_samples_keys_buttons_and_scroll_without_persisting_input() {
    let fixture = Fixture::new();
    let mut session = fixture.session(false);
    no_sample(session.as_mut());
    fixture
        .connection
        .warp_pointer(x11rb::NONE, fixture.root, 0, 0, 0, 0, 20, 20)
        .unwrap()
        .check()
        .unwrap();
    no_sample(session.as_mut());
    fixture.emit(2, 38);
    let first = sample(session.as_mut());
    assert_eq!(first.sequence(), 0);
    assert_eq!(&first.pixels()[..4], &[255, 0, 0, 255]);
    assert!(first.input_events().is_empty());
    assert_eq!(first.dropped_input_events(), 0);
    fixture.emit(3, 38);
    no_sample(session.as_mut());
    fixture.paint(0x0000_ff00);
    fixture.emit(4, 1);
    let clicked = sample(session.as_mut());
    assert_eq!(clicked.sequence(), 1);
    assert_eq!(&clicked.pixels()[..4], &[0, 255, 0, 255]);
    assert!(clicked.input_events().is_empty());
    fixture.emit(5, 1);
    no_sample(session.as_mut());
    fixture.emit(4, 4);
    fixture.emit(5, 4);
    let scrolled = sample(session.as_mut());
    assert_eq!(scrolled.sequence(), 2);
    assert!(scrolled.input_events().is_empty());
    no_sample(session.as_mut());
    session.stop().unwrap();
    fixture.emit(2, 54);
    fixture.emit(3, 54);
    assert_eq!(
        session.poll_frame(Duration::ZERO).unwrap(),
        FramePoll::EndOfStream
    );
    let mut next = fixture.session(false);
    no_sample(next.as_mut());
    next.discard().unwrap();
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_interaction_pause_drops_pending_and_paused_input_then_retargets() {
    let fixture = Fixture::new();
    let mut session = fixture.session(true);
    let before_first = Instant::now();
    fixture.emit(2, 38);
    let first = sample(session.as_mut());
    assert!(first.input_events().iter().any(|event| matches!(
        event,
        InputEvent::Key {
            native_code: 38,
            state: KeyState::Pressed,
            ..
        }
    )));
    fixture.emit(3, 38);
    fixture.emit(2, 56); // A pending trigger is invalidated at the pause boundary.
    session.pause().unwrap();
    let frozen_clock = session.active_elapsed().unwrap();
    let paused_start = Instant::now();
    fixture.emit(3, 56);
    fixture.emit(2, 57);
    fixture.emit(3, 57);
    std::thread::sleep(Duration::from_millis(50));
    no_sample(session.as_mut());
    assert_eq!(session.active_elapsed(), Some(frozen_clock));
    let CaptureTarget::Region { source, .. } = &fixture.target else {
        unreachable!()
    };
    session
        .update_target(CaptureTarget::Region {
            source: source.clone(),
            region: PhysicalRect::new(20, 20, 8, 8).unwrap(),
        })
        .unwrap();
    let paused_minimum = paused_start.elapsed();
    session.resume().unwrap();
    no_sample(session.as_mut());
    fixture.emit(2, 54);
    let last = sample(session.as_mut());
    assert!(
        session.active_elapsed().unwrap() >= Duration::from_micros(last.captured_at().as_micros())
    );
    assert_eq!(last.sequence(), 1);
    assert_eq!(
        last.capture_origin(),
        Some(PhysicalPosition { x: 20, y: 20 })
    );
    assert!(last.input_events().iter().any(|event| matches!(
        event,
        InputEvent::Key {
            native_code: 54,
            state: KeyState::Pressed,
            ..
        }
    )));
    assert!(last.input_events().iter().all(|event| !matches!(
        event,
        InputEvent::Key {
            native_code: 56 | 57,
            ..
        }
    )));
    // The monotonic active clock must omit at least the known paused segment;
    // this compares measured intervals, not an arbitrary CI-speed threshold.
    let capture_gap =
        Duration::from_micros(last.captured_at().as_micros() - first.captured_at().as_micros());
    assert!(capture_gap <= before_first.elapsed().saturating_sub(paused_minimum));
    assert!(paused_minimum >= Duration::from_millis(50));
    fixture.emit(3, 54);
    session.stop().unwrap();
    assert_eq!(session.state(), CaptureSessionState::Stopped);
}
