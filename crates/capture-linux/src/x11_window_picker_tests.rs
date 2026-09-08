//! Owned Xvfb end-to-end crosshair/grab lifecycle tests; never host DISPLAY.

use super::cursor_edge_tests::PrivateXvfb;
use crate::{PickedWindow, WindowSnapBounds, window_snap::picker};
use gif_from_screen_capture::PhysicalRect;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        shape::{ConnectionExt as _, SK},
        xfixes::ConnectionExt as _,
        xproto::{
            AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, GrabStatus,
            InputFocus, PropMode, WindowClass,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

struct Fixture {
    connection: RustConnection,
    root: u32,
    window: u32,
    before: BTreeSet<u32>,
    cursor: u32,
    server: PrivateXvfb,
}

impl Fixture {
    fn new() -> Self {
        let server = PrivateXvfb::start();
        let (connection, screen) = x11rb::connect(Some(&server.display)).unwrap();
        let root = connection.setup().roots[screen].root;
        connection
            .xfixes_query_version(5, 0)
            .unwrap()
            .reply()
            .unwrap();
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                window,
                root,
                20,
                30,
                100,
                80,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(0x00ff_0000)
                    .event_mask(
                        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::KEY_PRESS,
                    ),
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
                b"Picker target",
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
            .warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 50, 60)
            .unwrap()
            .check()
            .unwrap();
        let before = connection
            .query_tree(root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .into_iter()
            .collect();
        let cursor = connection
            .xfixes_get_cursor_image()
            .unwrap()
            .reply()
            .unwrap()
            .cursor_serial;
        Self {
            connection,
            root,
            window,
            before,
            cursor,
            server,
        }
    }

    fn start(
        &self,
        lifetime: Duration,
    ) -> (
        Arc<AtomicBool>,
        mpsc::Receiver<Result<Option<PickedWindow>, String>>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::clone(&cancel);
        let display = self.server.display.clone();
        let (send, receive) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = send.send(picker::run(
                Some(&display),
                WindowSnapBounds::Client,
                &cancellation,
                lifetime,
            ));
        });
        (cancel, receive)
    }

    fn wait_for_grab(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self
            .connection
            .xfixes_get_cursor_image()
            .unwrap()
            .reply()
            .unwrap()
            .cursor_serial
            == self.cursor
        {
            assert!(Instant::now() < deadline, "crosshair did not appear");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn emit(&self, kind: u8, detail: u8) {
        self.connection
            .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, 0, 0, 0)
            .unwrap()
            .check()
            .unwrap();
    }

    fn assert_released(&self) {
        let reply = self
            .connection
            .grab_pointer(
                false,
                self.root,
                EventMask::BUTTON_PRESS,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME,
            )
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(reply.status, GrabStatus::SUCCESS);
        self.connection
            .ungrab_pointer(x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        let after: BTreeSet<_> = self
            .connection
            .query_tree(self.root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .into_iter()
            .collect();
        assert_eq!(
            after, self.before,
            "highlight windows must be destroyed before the result is published"
        );
    }
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_window_picker_selects_release_target_with_passive_highlight_and_unchanged_keyboard_focus()
 {
    let fixture = Fixture::new();
    let (_cancel, result) = fixture.start(Duration::from_secs(5));
    fixture.wait_for_grab();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let children = fixture
            .connection
            .query_tree(fixture.root)
            .unwrap()
            .reply()
            .unwrap()
            .children;
        let extra: Vec<_> = children
            .into_iter()
            .filter(|id| !fixture.before.contains(id))
            .collect();
        if extra.len() == 5
            && extra.iter().all(|window| {
                fixture
                    .connection
                    .get_window_attributes(*window)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .map_state
                    == x11rb::protocol::xproto::MapState::VIEWABLE
            })
        {
            for window in extra {
                let shape = fixture
                    .connection
                    .shape_get_rectangles(window, SK::INPUT)
                    .unwrap()
                    .reply()
                    .unwrap();
                assert!(
                    shape.rectangles.is_empty(),
                    "hover highlight must not become the pointer target"
                );
            }
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fixture
            .connection
            .get_input_focus()
            .unwrap()
            .reply()
            .unwrap()
            .focus,
        fixture.window
    );
    fixture.emit(2, 38); // Keyboard stays with its original owner during the pointer grab.
    fixture.emit(3, 38);
    fixture.emit(4, 1);
    fixture.emit(5, 1);
    fixture
        .connection
        .warp_pointer(x11rb::NONE, fixture.root, 0, 0, 0, 0, 250, 180)
        .unwrap()
        .check()
        .unwrap();
    let picked = result
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap()
        .expect("window selected");
    assert_eq!(picked.source.name(), "Picker target");
    assert_eq!(picked.region, PhysicalRect::new(20, 30, 100, 80).unwrap());
    fixture.assert_released();
    let mut keys = 0;
    while let Some(event) = fixture.connection.poll_for_event().unwrap() {
        assert!(
            !matches!(event, Event::ButtonPress(_) | Event::ButtonRelease(_)),
            "selection click leaked to target"
        );
        keys += usize::from(matches!(event, Event::KeyPress(_)));
    }
    assert_eq!(keys, 1);
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_window_picker_right_cancel_flag_and_timeout_all_release_the_pointer() {
    for mode in 0..3 {
        let fixture = Fixture::new();
        let (cancel, result) = fixture.start(if mode == 2 {
            Duration::from_millis(150)
        } else {
            Duration::from_secs(5)
        });
        fixture.wait_for_grab();
        if mode == 0 {
            fixture.emit(4, 3);
            fixture.emit(5, 3);
        }
        if mode == 1 {
            cancel.store(true, Ordering::Release);
        }
        let result = result.recv_timeout(Duration::from_secs(3)).unwrap();
        if mode == 0 {
            assert!(result.unwrap().is_none());
        } else {
            assert!(result.is_err());
        }
        fixture.assert_released();
    }
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_window_picker_closed_target_cannot_reuse_a_stale_hover() {
    let mut fixture = Fixture::new();
    let (_cancel, result) = fixture.start(Duration::from_secs(5));
    fixture.wait_for_grab();
    fixture
        .connection
        .destroy_window(fixture.window)
        .unwrap()
        .check()
        .unwrap();
    fixture.before.remove(&fixture.window);
    fixture.emit(4, 1);
    fixture.emit(5, 1);
    assert!(
        result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap()
            .is_none()
    );
    fixture.assert_released();
}

#[test]
#[ignore = "starts and supervises its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_window_picker_does_not_release_a_conflicting_foreign_grab() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .connection
            .grab_pointer(
                false,
                fixture.root,
                EventMask::BUTTON_PRESS,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME
            )
            .unwrap()
            .reply()
            .unwrap()
            .status,
        GrabStatus::SUCCESS
    );
    let (_cancel, result) = fixture.start(Duration::from_secs(5));
    assert!(
        result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap_err()
            .contains("could not acquire")
    );
    let (other, _) = x11rb::connect(Some(&fixture.server.display)).unwrap();
    assert_eq!(
        other
            .grab_pointer(
                false,
                fixture.root,
                EventMask::BUTTON_PRESS,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME
            )
            .unwrap()
            .reply()
            .unwrap()
            .status,
        GrabStatus::ALREADY_GRABBED
    );
    fixture
        .connection
        .ungrab_pointer(x11rb::CURRENT_TIME)
        .unwrap()
        .check()
        .unwrap();
    fixture.assert_released();
}
