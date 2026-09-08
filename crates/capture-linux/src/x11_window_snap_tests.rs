//! Explicit private-display window-snap checks, never host DISPLAY.

use super::cursor_edge_tests::PrivateXvfb;
use crate::{WindowSnapBounds, query_window_snap};
use gif_from_screen_capture::{CaptureSource, CaptureSourceId, CaptureSourceKind, PhysicalRect};
use std::sync::atomic::{AtomicBool, Ordering};
use x11rb::{
    connection::Connection,
    protocol::xproto::{
        AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, InputFocus, PropMode,
        WindowClass,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

struct Fixture {
    server: PrivateXvfb,
    connection: RustConnection,
    root: u32,
    parent: u32,
    window: u32,
    source: CaptureSource,
}

impl Fixture {
    fn new() -> Self {
        let server = PrivateXvfb::start();
        let (connection, screen) = x11rb::connect(Some(&server.display)).unwrap();
        let root = connection.setup().roots[screen].root;
        let parent = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                parent,
                root,
                20,
                30,
                100,
                100,
                2,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().override_redirect(1),
            )
            .unwrap()
            .check()
            .unwrap();
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                window,
                parent,
                4,
                20,
                80,
                60,
                1,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                b"Snap test",
            )
            .unwrap()
            .check()
            .unwrap();
        connection.map_window(window).unwrap().check().unwrap();
        connection.map_window(parent).unwrap().check().unwrap();
        connection
            .set_input_focus(InputFocus::PARENT, root, x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        let source = CaptureSource::new(
            CaptureSourceId::new(super::format_window_source_id(screen, window)).unwrap(),
            "Snap test",
            CaptureSourceKind::Window,
            Some(PhysicalRect::new(0, 0, 1, 1).unwrap()),
            2.0,
        )
        .unwrap();
        Self {
            server,
            connection,
            root,
            parent,
            window,
            source,
        }
    }

    fn snap(&self, mode: WindowSnapBounds) -> Result<PhysicalRect, String> {
        query_window_snap(
            Some(&self.server.display),
            &self.source,
            mode,
            &AtomicBool::new(false),
        )
    }

    fn atom(&self, name: &[u8]) -> u32 {
        self.connection
            .intern_atom(false, name)
            .unwrap()
            .reply()
            .unwrap()
            .atom
    }

    fn property(&self, name: &[u8], kind: AtomEnum, values: &[u32]) {
        self.connection
            .change_property32(
                PropMode::REPLACE,
                self.window,
                self.atom(name),
                kind,
                values,
            )
            .unwrap()
            .check()
            .unwrap();
    }
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_window_snap_reads_client_and_native_frame_without_using_extent_hints_or_stale_geometry()
 {
    let fixture = Fixture::new();
    fixture.property(
        b"_NET_FRAME_EXTENTS",
        AtomEnum::CARDINAL,
        &[1000, 1000, 1000, 1000],
    );
    let focus = fixture
        .connection
        .get_input_focus()
        .unwrap()
        .reply()
        .unwrap()
        .focus;
    assert_eq!(
        fixture.snap(WindowSnapBounds::Client).unwrap(),
        PhysicalRect::new(27, 53, 80, 60).unwrap()
    );
    assert_eq!(
        fixture.snap(WindowSnapBounds::Outer).unwrap(),
        PhysicalRect::new(20, 30, 104, 104).unwrap()
    );
    fixture
        .connection
        .configure_window(fixture.parent, &ConfigureWindowAux::new().x(50).y(60))
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        fixture.snap(WindowSnapBounds::Client).unwrap(),
        PhysicalRect::new(57, 83, 80, 60).unwrap()
    );
    assert_eq!(
        fixture.snap(WindowSnapBounds::Outer).unwrap(),
        PhysicalRect::new(50, 60, 104, 104).unwrap()
    );
    assert_eq!(
        fixture
            .connection
            .get_input_focus()
            .unwrap()
            .reply()
            .unwrap()
            .focus,
        focus
    );
    assert_eq!(
        fixture
            .connection
            .query_tree(fixture.window)
            .unwrap()
            .reply()
            .unwrap()
            .parent,
        fixture.parent
    );
    assert_ne!(fixture.root, fixture.window);
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_window_snap_validates_wm_frame_and_client_shadow_hints() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .snap(WindowSnapBounds::WindowFrame)
            .unwrap_err()
            .contains("No window-frame hints")
    );
    fixture.property(b"_NET_FRAME_EXTENTS", AtomEnum::CARDINAL, &[3, 3, 20, 4]);
    assert_eq!(
        fixture.snap(WindowSnapBounds::WindowFrame).unwrap(),
        PhysicalRect::new(24, 33, 86, 84).unwrap()
    );
    fixture.property(b"_NET_FRAME_EXTENTS", AtomEnum::CARDINAL, &[1000; 4]);
    assert!(
        fixture
            .snap(WindowSnapBounds::WindowFrame)
            .unwrap_err()
            .contains("disagree")
    );
    fixture.property(b"_NET_FRAME_EXTENTS", AtomEnum::CARDINAL, &[0; 4]);
    fixture.property(b"_GTK_FRAME_EXTENTS", AtomEnum::CARDINAL, &[2, 3, 4, 5]);
    assert_eq!(
        fixture.snap(WindowSnapBounds::WindowFrame).unwrap(),
        PhysicalRect::new(29, 57, 75, 51).unwrap()
    );
    fixture.property(b"_NET_FRAME_EXTENTS", AtomEnum::CARDINAL, &[1; 4]);
    assert!(
        fixture
            .snap(WindowSnapBounds::WindowFrame)
            .unwrap_err()
            .contains("ambiguous")
    );
    fixture.property(b"_NET_FRAME_EXTENTS", AtomEnum::CARDINAL, &[0; 4]);
    fixture.property(b"_GTK_FRAME_EXTENTS", AtomEnum::CARDINAL, &[40, 40, 0, 0]);
    assert!(fixture.snap(WindowSnapBounds::WindowFrame).is_err());
    fixture.property(b"_GTK_FRAME_EXTENTS", AtomEnum::CARDINAL, &[u32::MAX; 4]);
    assert!(fixture.snap(WindowSnapBounds::WindowFrame).is_err());
    fixture.property(b"_GTK_FRAME_EXTENTS", AtomEnum::CARDINAL, &[0; 5]);
    assert!(
        fixture
            .snap(WindowSnapBounds::WindowFrame)
            .unwrap_err()
            .contains("exactly four")
    );
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_window_snap_rejects_hidden_own_stale_closed_and_cancelled_targets() {
    let fixture = Fixture::new();
    let hidden = fixture.atom(b"_NET_WM_STATE_HIDDEN");
    fixture.property(b"_NET_WM_STATE", AtomEnum::ATOM, &[hidden]);
    assert!(
        fixture
            .snap(WindowSnapBounds::Client)
            .unwrap_err()
            .contains("minimized")
    );
    fixture.property(b"_NET_WM_STATE", AtomEnum::ATOM, &[]);
    fixture.property(b"_NET_WM_PID", AtomEnum::CARDINAL, &[std::process::id()]);
    assert!(
        fixture
            .snap(WindowSnapBounds::Client)
            .unwrap_err()
            .contains("own window")
    );
    fixture
        .connection
        .delete_property(fixture.window, fixture.atom(b"_NET_WM_PID"))
        .unwrap()
        .check()
        .unwrap();
    fixture
        .connection
        .change_property8(
            PropMode::REPLACE,
            fixture.window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"Different title",
        )
        .unwrap()
        .check()
        .unwrap();
    assert!(
        fixture
            .snap(WindowSnapBounds::Client)
            .unwrap_err()
            .contains("title changed")
    );
    let cancellation = AtomicBool::new(false);
    cancellation.store(true, Ordering::Release);
    assert!(
        query_window_snap(
            Some(&fixture.server.display),
            &fixture.source,
            WindowSnapBounds::Client,
            &cancellation
        )
        .unwrap_err()
        .contains("cancelled")
    );
    fixture
        .connection
        .destroy_window(fixture.window)
        .unwrap()
        .check()
        .unwrap();
    assert!(fixture.snap(WindowSnapBounds::Outer).is_err());
}
