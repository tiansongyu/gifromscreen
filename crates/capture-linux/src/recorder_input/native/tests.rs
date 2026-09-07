use super::*;
use crate::set_recorder_input_shape;
use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    protocol::{
        Event,
        xproto::{
            BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, CreateWindowAux, EventMask,
            MOTION_NOTIFY_EVENT, PropMode, WindowClass,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    fn new() -> Self {
        let mut child = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "800x600x24",
                "-nolisten",
                "tcp",
                "-ac",
                "-noreset",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("install Xvfb for this explicit private native test");
        let stdout = child.stdout.take().unwrap();
        let (send, receive) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout.take(32))
                .read_line(&mut line)
                .map(|_| line);
            let _ = send.send(result);
        });
        let line = match receive.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(line)) => line,
            error => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                panic!("private Xvfb failed to start: {error:?}");
            }
        };
        reader.join().unwrap();
        let number = line.trim().parse::<u16>().unwrap();
        Self {
            child,
            display: format!(":{number}"),
        }
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    connection: RustConnection,
    root: Window,
    underlay: Window,
    overlay: Window,
    title: String,
    pid_atom: Atom,
    name_atom: Atom,
    utf8: Atom,
}

impl Fixture {
    fn new(server: &Xvfb) -> Self {
        let (connection, screen) = x11rb::connect(Some(&server.display)).unwrap();
        let root = connection.setup().roots[screen].root;
        let atom = |name: &[u8]| {
            connection
                .intern_atom(false, name)
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let pid_atom = atom(b"_NET_WM_PID");
        let name_atom = atom(b"_NET_WM_NAME");
        let utf8 = atom(b"UTF8_STRING");
        let title = format!("GifFromScreen recorder-input owned {}", std::process::id());
        let mut fixture = Self {
            connection,
            root,
            underlay: 0,
            overlay: 0,
            title,
            pid_atom,
            name_atom,
            utf8,
        };
        fixture.underlay = fixture.window(root, 100, 100, "Owned underlying fixture");
        fixture.overlay = fixture.window(root, 100, 100, &fixture.title);
        fixture.connection.sync().unwrap();
        fixture
    }

    fn window(&self, parent: Window, x: i16, y: i16, title: &str) -> Window {
        let id = self.connection.generate_id().unwrap();
        self.connection
            .create_window(
                0,
                id,
                parent,
                x,
                y,
                300,
                220,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(0x0020_2020)
                    .override_redirect(1)
                    .event_mask(EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE),
            )
            .unwrap()
            .check()
            .unwrap();
        self.connection
            .change_property32(
                PropMode::REPLACE,
                id,
                self.pid_atom,
                AtomEnum::CARDINAL,
                &[std::process::id()],
            )
            .unwrap()
            .check()
            .unwrap();
        self.title(id, title);
        self.connection.map_window(id).unwrap().check().unwrap();
        id
    }

    fn title(&self, window: Window, title: &str) {
        self.connection
            .change_property8(
                PropMode::REPLACE,
                window,
                self.name_atom,
                self.utf8,
                title.as_bytes(),
            )
            .unwrap()
            .check()
            .unwrap();
    }

    fn shape(&self, window: Window, kind: SK) -> Vec<(i16, i16, u16, u16)> {
        self.connection
            .shape_get_rectangles(window, kind)
            .unwrap()
            .reply()
            .unwrap()
            .rectangles
            .iter()
            .map(|rect| (rect.x, rect.y, rect.width, rect.height))
            .collect()
    }

    fn click(&self, x: i16, y: i16) -> Window {
        while self.connection.poll_for_event().unwrap().is_some() {}
        self.connection
            .xtest_fake_input(
                MOTION_NOTIFY_EVENT,
                0,
                x11rb::CURRENT_TIME,
                self.root,
                x,
                y,
                0,
            )
            .unwrap()
            .check()
            .unwrap();
        for kind in [BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT] {
            self.connection
                .xtest_fake_input(kind, 1, x11rb::CURRENT_TIME, self.root, x, y, 0)
                .unwrap()
                .check()
                .unwrap();
        }
        self.connection.sync().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(Event::ButtonPress(event)) = self.connection.poll_for_event().unwrap() {
                return event.event;
            }
            assert!(
                Instant::now() < deadline,
                "the private pointer click did not reach either owned window"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
}

fn size() -> PhysicalSize {
    PhysicalSize::new(300, 220).unwrap()
}
fn hole() -> PhysicalRect {
    PhysicalRect::new(20, 20, 260, 150).unwrap()
}

#[test]
#[ignore = "spawns its own private Xvfb and test windows; never uses host DISPLAY"]
fn private_xvfb_recorder_input_hole_passes_clicks_and_preserves_border_toolbar_after_disconnect() {
    let server = Xvfb::new();
    let fixture = Fixture::new(&server);
    let bounding = fixture.shape(fixture.overlay, SK::BOUNDING);
    assert_eq!(fixture.click(150, 150), fixture.overlay);
    assert!(
        set_recorder_input_shape(
            Some(&server.display),
            std::process::id(),
            &fixture.title,
            size(),
            hole(),
            &AtomicBool::new(false)
        )
        .unwrap()
    );
    // The helper and its temporary region connection have already been dropped.
    assert_eq!(fixture.click(150, 150), fixture.underlay);
    assert_eq!(fixture.click(105, 130), fixture.overlay);
    assert_eq!(fixture.click(150, 300), fixture.overlay);
    assert_eq!(fixture.shape(fixture.overlay, SK::BOUNDING), bounding);
    assert_eq!(
        fixture.shape(fixture.overlay, SK::INPUT),
        [
            (0, 0, 300, 20),
            (0, 20, 20, 150),
            (280, 20, 20, 150),
            (0, 170, 300, 50)
        ]
    );
}

#[test]
#[ignore = "spawns its own private Xvfb and test windows; never uses host DISPLAY"]
fn private_xvfb_recorder_input_mismatch_duplicate_cancel_and_closed_window_never_change_other_shapes()
 {
    let server = Xvfb::new();
    let fixture = Fixture::new(&server);
    let before = fixture.shape(fixture.overlay, SK::INPUT);
    let underlying = fixture.shape(fixture.underlay, SK::INPUT);
    let cancel = AtomicBool::new(false);
    assert!(
        !set_recorder_input_shape(
            Some(&server.display),
            std::process::id(),
            "Not the requested title",
            size(),
            hole(),
            &cancel
        )
        .unwrap()
    );
    assert!(
        !set_recorder_input_shape(
            Some(&server.display),
            std::process::id(),
            &fixture.title,
            PhysicalSize::new(301, 220).unwrap(),
            hole(),
            &cancel
        )
        .unwrap()
    );
    assert!(
        set_recorder_input_shape(
            Some(&server.display),
            std::process::id(),
            &fixture.title,
            size(),
            hole(),
            &AtomicBool::new(true)
        )
        .is_err()
    );
    let duplicate = fixture.window(fixture.root, 450, 100, &fixture.title);
    assert!(
        set_recorder_input_shape(
            Some(&server.display),
            std::process::id(),
            &fixture.title,
            size(),
            hole(),
            &cancel
        )
        .unwrap_err()
        .contains("Multiple windows")
    );
    fixture
        .connection
        .destroy_window(duplicate)
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(fixture.shape(fixture.overlay, SK::INPUT), before);
    fixture
        .connection
        .destroy_window(fixture.overlay)
        .unwrap()
        .check()
        .unwrap();
    assert!(
        !set_recorder_input_shape(
            Some(&server.display),
            std::process::id(),
            &fixture.title,
            size(),
            hole(),
            &cancel
        )
        .unwrap()
    );
    assert_eq!(fixture.shape(fixture.underlay, SK::INPUT), underlying);
    assert_eq!(fixture.click(150, 150), fixture.underlay);
}

#[test]
#[ignore = "spawns its own private Xvfb and test windows; never uses host DISPLAY"]
fn private_xvfb_recorder_input_rechecks_identity_and_bounds_two_level_discovery() {
    let server = Xvfb::new();
    let fixture = Fixture::new(&server);
    let cancel = AtomicBool::new(false);
    let connection = stream::connect(Some(&server.display), &cancel).unwrap();
    connection
        .xfixes_query_version(5, 0)
        .unwrap()
        .reply()
        .unwrap();
    let atoms = Atoms::read(&connection).unwrap();
    let target = Target {
        pid: std::process::id(),
        title: &fixture.title,
        size: size(),
    };
    let candidate = find_recorder(&connection, &atoms, &target, &cancel)
        .unwrap()
        .unwrap();
    fixture.title(fixture.overlay, "Identity changed after discovery");
    assert!(!apply(&connection, &atoms, &target, candidate, hole(), &cancel).unwrap());
    assert_eq!(
        fixture.shape(fixture.overlay, SK::INPUT),
        [(0, 0, 300, 220)]
    );
    fixture.title(fixture.overlay, &fixture.title);
    let frame = fixture.window(fixture.root, 400, 300, "Owned reparenting frame");
    fixture
        .connection
        .reparent_window(fixture.overlay, frame, 0, 0)
        .unwrap()
        .check()
        .unwrap();
    assert!(
        set_recorder_input_shape(
            Some(&server.display),
            std::process::id(),
            &fixture.title,
            size(),
            hole(),
            &cancel
        )
        .unwrap()
    );
    let clients = fixture
        .connection
        .intern_atom(false, b"_NET_CLIENT_LIST")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    fixture
        .connection
        .change_property32(
            PropMode::REPLACE,
            fixture.root,
            clients,
            AtomEnum::WINDOW,
            &vec![fixture.underlay; MAX_WINDOWS + 1],
        )
        .unwrap()
        .check()
        .unwrap();
    assert!(
        set_recorder_input_shape(
            Some(&server.display),
            std::process::id(),
            &fixture.title,
            size(),
            hole(),
            &cancel
        )
        .unwrap_err()
        .contains("1,024")
    );
}
