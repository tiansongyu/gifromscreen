//! Native handle/replay regressions on a newly allocated, supervised Xvfb only.

use std::{
    collections::BTreeSet,
    thread,
    time::{Duration, Instant},
};

use gif_from_screen_capture::{PhysicalRect, PhysicalSize};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xproto::{
            AtomEnum, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode,
            GrabStatus, KeyButMask, MapState, PropMode, StackMode, Window, WindowClass,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

use super::cursor_edge_tests::PrivateXvfb;
use crate::{DragPickerButton, DragPickerRequest, DragPickerSelection, WindowSnapBounds};

const GENERATION: u64 = 7;

struct Fixture {
    connection: RustConnection,
    root: Window,
    parent: Window,
    target: Window,
    original: BTreeSet<Window>,
    server: PrivateXvfb,
}

impl Fixture {
    fn new() -> Self {
        let server = PrivateXvfb::start();
        let (connection, screen) = x11rb::connect(Some(&server.display)).unwrap();
        let root = connection.setup().roots[screen].root;
        let parent = create_fixture_window(
            &connection,
            root,
            PhysicalRect::new(10, 10, 100, 60).unwrap(),
            b"owned drag picker parent",
        );
        let target =
            create_fixture_window(&connection, root, target_rect(), b"isolated picker target");
        let pid = connection
            .intern_atom(false, b"_NET_WM_PID")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        connection
            .change_property32(
                PropMode::REPLACE,
                parent,
                pid,
                AtomEnum::CARDINAL,
                &[std::process::id()],
            )
            .unwrap()
            .check()
            .unwrap();
        let list = connection
            .intern_atom(false, b"_NET_CLIENT_LIST")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        connection
            .change_property32(
                PropMode::REPLACE,
                root,
                list,
                AtomEnum::WINDOW,
                &[parent, target],
            )
            .unwrap()
            .check()
            .unwrap();
        let original = connection
            .query_tree(root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .into_iter()
            .collect();
        for name in [
            b"_NET_WM_STATE".as_slice(),
            b"_NET_WM_STATE_HIDDEN".as_slice(),
        ] {
            connection
                .intern_atom(false, name)
                .unwrap()
                .reply()
                .unwrap();
        }
        Self {
            connection,
            root,
            parent,
            target,
            original,
            server,
        }
    }

    fn start(&self) -> DragPickerButton {
        DragPickerButton::start(
            Some(self.server.display.clone()),
            self.parent,
            std::process::id(),
        )
        .unwrap()
    }

    fn arm(&self) -> DragPickerButton {
        let mut service = self.start();
        service.request(request(GENERATION, true)).unwrap();
        ready(&mut service, GENERATION);
        let children = self
            .connection
            .query_tree(self.parent)
            .unwrap()
            .reply()
            .unwrap()
            .children;
        assert_eq!(
            children.len(),
            1,
            "exactly one owned input child must implement the handle"
        );
        let attributes = self
            .connection
            .get_window_attributes(children[0])
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(attributes.class, WindowClass::INPUT_ONLY);
        assert_eq!(attributes.map_state, MapState::VIEWABLE);
        service
    }

    fn motion(&self, x: i16, y: i16) {
        self.connection
            .xtest_fake_input(6, 0, x11rb::CURRENT_TIME, self.root, x, y, 0)
            .unwrap()
            .check()
            .unwrap();
    }

    fn button(&self, button: u8, pressed: bool) {
        self.connection
            .xtest_fake_input(
                if pressed { 4 } else { 5 },
                button,
                x11rb::CURRENT_TIME,
                self.root,
                0,
                0,
                0,
            )
            .unwrap()
            .check()
            .unwrap();
    }

    fn click(&self, x: i16, y: i16, button: u8) {
        self.motion(x, y);
        self.button(button, true);
        self.button(button, false);
    }

    fn confirm_pointer_thawed(&self) {
        self.motion(200, 130);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let pointer = self
                .connection
                .query_pointer(self.root)
                .unwrap()
                .reply()
                .unwrap();
            if (pointer.root_x, pointer.root_y) == (200, 130) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "synchronous initiating grab never thawed"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn confirm_button_released(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let pointer = self
                .connection
                .query_pointer(self.root)
                .unwrap()
                .reply()
                .unwrap();
            if !pointer.mask.contains(KeyButMask::BUTTON1) {
                return;
            }
            assert!(Instant::now() < deadline, "queued release stayed frozen");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn parent_click(&self, button: u8) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut presses = 0;
        let mut releases = 0;
        loop {
            while let Some(event) = self.connection.poll_for_event().unwrap() {
                match event {
                    Event::ButtonPress(event)
                        if event.event == self.parent && event.detail == button =>
                    {
                        presses += 1;
                    }
                    Event::ButtonRelease(event)
                        if event.event == self.parent && event.detail == button =>
                    {
                        releases += 1;
                    }
                    _ => {}
                }
            }
            assert!(
                presses <= 1 && releases <= 1,
                "a replay must not duplicate button events"
            );
            if (presses, releases) == (1, 1) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "parent click was swallowed: {presses}/{releases}"
            );
            thread::sleep(Duration::from_millis(5));
        }
        // Server round trip + a short subsequent drain catches duplicate replay.
        self.connection.get_input_focus().unwrap().reply().unwrap();
        while let Some(event) = self.connection.poll_for_event().unwrap() {
            assert!(
                !matches!(event, Event::ButtonPress(event) | Event::ButtonRelease(event)
                if event.event == self.parent && event.detail == button),
                "extra replay after completed click"
            );
        }
    }

    fn assert_pointer_free(&self) {
        let status = self
            .connection
            .grab_pointer(
                false,
                self.target,
                EventMask::BUTTON_PRESS,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME,
            )
            .unwrap()
            .reply()
            .unwrap()
            .status;
        assert_eq!(
            status,
            GrabStatus::SUCCESS,
            "picker leaked an active pointer grab"
        );
        self.connection
            .ungrab_pointer(x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
    }

    fn assert_no_parent_button_events(&self) {
        while let Some(event) = self.connection.poll_for_event().unwrap() {
            assert!(
                !matches!(event, Event::ButtonPress(event) | Event::ButtonRelease(event)
                if event.event == self.parent),
                "duplicate replay reached the parent after cleanup"
            );
        }
    }

    fn assert_no_target_press(&self, scenario: usize) {
        while let Some(event) = self.connection.poll_for_event().unwrap() {
            assert!(
                !matches!(event, Event::ButtonPress(event) if event.event == self.parent || event.event == self.target),
                "scenario {scenario}: cancelled/hidden UI press must not be replayed to a live recipient"
            );
        }
    }

    fn assert_cleanup(&self, parent_exists: bool) {
        let mut expected = self.original.clone();
        if parent_exists {
            assert!(
                self.connection
                    .query_tree(self.parent)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .children
                    .is_empty()
            );
        } else {
            expected.remove(&self.parent);
        }
        let actual: BTreeSet<_> = self
            .connection
            .query_tree(self.root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .into_iter()
            .collect();
        assert_eq!(
            actual, expected,
            "input child and native highlights must be gone before result publication"
        );
        self.assert_pointer_free();
    }

    fn assert_selected(&self, result: Result<Option<DragPickerSelection>, String>) {
        let selection = result.unwrap().expect("accepted gesture result");
        assert_eq!(selection.generation, GENERATION);
        let picked = selection.picked.expect("fixture target must be selected");
        assert_eq!(picked.region, target_rect());
        assert_eq!(
            picked.source.id().as_str(),
            format!("x11:screen:0:window:0x{:08x}", self.target)
        );
        assert_eq!(picked.source.name(), "isolated picker target");
        self.assert_cleanup(true);
    }
}

fn create_fixture_window(
    connection: &RustConnection,
    root: Window,
    rect: PhysicalRect,
    name: &[u8],
) -> Window {
    let id = connection.generate_id().unwrap();
    connection
        .create_window(
            0,
            id,
            root,
            i16::try_from(rect.origin().x).unwrap(),
            i16::try_from(rect.origin().y).unwrap(),
            u16::try_from(rect.size().width()).unwrap(),
            u16::try_from(rect.size().height()).unwrap(),
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(0x0044_6677)
                .event_mask(EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE),
        )
        .unwrap()
        .check()
        .unwrap();
    connection
        .change_property8(
            PropMode::REPLACE,
            id,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            name,
        )
        .unwrap()
        .check()
        .unwrap();
    connection.map_window(id).unwrap().check().unwrap();
    id
}

// This is used ONLY on the fixture's new Xvfb. It forces all synthetic input
// to be queued before the helper's replacement-grab request can be processed.
struct FrozenServer<'a>(&'a RustConnection);
impl<'a> FrozenServer<'a> {
    fn new(connection: &'a RustConnection) -> Self {
        connection.grab_server().unwrap().check().unwrap();
        Self(connection)
    }
}
impl Drop for FrozenServer<'_> {
    fn drop(&mut self) {
        let _ = self.0.ungrab_server();
        let _ = self.0.flush();
    }
}

fn target_rect() -> PhysicalRect {
    PhysicalRect::new(150, 90, 120, 100).unwrap()
}

fn request(generation: u64, enabled: bool) -> DragPickerRequest {
    DragPickerRequest {
        generation,
        rect: enabled.then(|| PhysicalRect::new(10, 10, 40, 20).unwrap()),
        parent_size: PhysicalSize::new(100, 60).unwrap(),
        bounds: WindowSnapBounds::Client,
    }
}

fn ready(service: &mut DragPickerButton, generation: u64) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let update = service.poll();
        assert!(
            update.result.is_none(),
            "handle failed during setup: {:?}",
            update.result
        );
        assert!(update.running);
        if update.ready == Some(generation) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native handle did not become ready"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn active(service: &mut DragPickerButton) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let update = service.poll();
        assert!(
            update.result.is_none(),
            "selection ended before target release: {:?}",
            update.result
        );
        if update.active == Some(GENERATION) {
            assert!(service.is_picking());
            return;
        }
        assert!(Instant::now() < deadline, "press did not start selection");
        thread::sleep(Duration::from_millis(5));
    }
}

fn terminal(service: &mut DragPickerButton) -> Result<Option<DragPickerSelection>, String> {
    terminal_case(service, "selection")
}

fn terminal_case(
    service: &mut DragPickerButton,
    case: &str,
) -> Result<Option<DragPickerSelection>, String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let update = service.poll();
        if let Some(result) = update.result {
            assert!(!update.running && !service.is_picking());
            assert!(update.ready.is_none() && update.active.is_none() && !service.is_claimed());
            assert!(
                service.poll().result.is_none(),
                "terminal selection is single-use"
            );
            return result;
        }
        assert!(Instant::now() < deadline, "{case} did not clean up/finish");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_held_and_queued_fast_release_select_once_and_release_grabs() {
    for fast in [false, true] {
        let fixture = Fixture::new();
        let mut service = fixture.arm();
        if fast {
            let frozen = FrozenServer::new(&fixture.connection);
            fixture.motion(30, 30);
            fixture.button(1, true);
            fixture.motion(200, 130);
            fixture.button(1, false);
            // A later pointer position must not replace the release event's
            // target/coordinates while the frozen burst is processed.
            fixture.motion(310, 220);
            drop(frozen);
        } else {
            fixture.motion(30, 30);
            fixture.button(1, true);
            active(&mut service);
            assert!(service.request(request(GENERATION + 1, true)).is_err());
            fixture.motion(200, 130);
            fixture.button(1, false);
        }
        fixture.assert_selected(terminal(&mut service));
    }
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_stationary_click_switches_to_click_to_pick() {
    let fixture = Fixture::new();
    let mut service = fixture.arm();
    fixture.click(30, 30, 1);
    active(&mut service);
    fixture.click(200, 130, 1);
    fixture.assert_selected(terminal(&mut service));
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_queued_click_then_motion_outside_still_arms_click_to_pick() {
    let fixture = Fixture::new();
    let mut service = fixture.arm();
    let frozen = FrozenServer::new(&fixture.connection);
    fixture.click(30, 30, 1);
    fixture.motion(310, 220);
    drop(frozen);
    active(&mut service);
    fixture.confirm_button_released();
    let armed = service.poll();
    assert!(armed.active == Some(GENERATION) && armed.result.is_none());
    fixture.click(200, 130, 1);
    fixture.assert_selected(terminal(&mut service));
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_may_hide_origin_after_root_handoff_without_losing_release() {
    let fixture = Fixture::new();
    let mut service = fixture.arm();
    fixture.motion(30, 30);
    fixture.button(1, true);
    active(&mut service);
    fixture.confirm_pointer_thawed();
    fixture
        .connection
        .unmap_window(fixture.parent)
        .unwrap()
        .check()
        .unwrap();
    fixture.button(1, false);
    fixture.assert_selected(terminal(&mut service));
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_outside_and_nonprimary_clicks_are_not_intercepted() {
    let fixture = Fixture::new();
    let mut service = fixture.arm();
    fixture.click(95, 60, 1);
    fixture.parent_click(1);
    fixture.click(30, 30, 3);
    fixture.parent_click(3);
    let update = service.poll();
    assert!(update.active.is_none() && update.result.is_none() && update.running);
    service.stop();
    assert!(service.cancelled());
    assert!(terminal(&mut service).unwrap().is_none());
    fixture.assert_cleanup(true);
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_stale_size_replays_to_parent_once_without_selecting() {
    let fixture = Fixture::new();
    let mut service = fixture.arm();
    let frozen = FrozenServer::new(&fixture.connection);
    fixture
        .connection
        .configure_window(fixture.parent, &ConfigureWindowAux::new().width(110))
        .unwrap()
        .check()
        .unwrap();
    fixture.click(30, 30, 1);
    drop(frozen);
    fixture.parent_click(1);
    let update = service.poll();
    assert!(update.active.is_none() && update.result.is_none());
    service.stop();
    assert!(terminal(&mut service).unwrap().is_none());
    fixture.assert_cleanup(true);
    fixture.assert_no_parent_button_events();
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_disabled_pending_request_replays_once_without_selecting() {
    let fixture = Fixture::new();
    let mut service = fixture.arm();
    let frozen = FrozenServer::new(&fixture.connection);
    service.request(request(GENERATION + 1, false)).unwrap();
    fixture.click(30, 30, 1);
    drop(frozen);
    fixture.parent_click(1);
    let update = service.poll();
    assert!(update.active.is_none() && update.result.is_none());
    service.stop();
    assert!(terminal(&mut service).unwrap().is_none());
    fixture.assert_cleanup(true);
    fixture.assert_no_parent_button_events();
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_explicit_stop_cleans_owned_resources() {
    check_termination(0);
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_active_parent_close_cleans_owned_resources() {
    check_termination(1);
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_short_watchdog_only_expires_active_selection() {
    check_termination(2);
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_cancel_or_hidden_parent_does_not_replay_into_underlay() {
    for scenario in 0..3 {
        let fixture = Fixture::new();
        // A real, separately owned event recipient is under the GUI handle.
        fixture
            .connection
            .configure_window(
                fixture.target,
                &ConfigureWindowAux::new()
                    .x(10)
                    .y(10)
                    .width(100)
                    .height(60)
                    .sibling(fixture.parent)
                    .stack_mode(StackMode::BELOW),
            )
            .unwrap()
            .check()
            .unwrap();
        let mut service = fixture.arm();
        let frozen = FrozenServer::new(&fixture.connection);
        fixture.motion(30, 30);
        fixture.button(1, true);
        match scenario {
            0 => service.stop(),
            1 => {
                fixture
                    .connection
                    .unmap_window(fixture.parent)
                    .unwrap()
                    .check()
                    .unwrap();
            }
            _ => {
                let state = fixture
                    .connection
                    .intern_atom(true, b"_NET_WM_STATE")
                    .unwrap()
                    .reply()
                    .unwrap()
                    .atom;
                let hidden = fixture
                    .connection
                    .intern_atom(true, b"_NET_WM_STATE_HIDDEN")
                    .unwrap()
                    .reply()
                    .unwrap()
                    .atom;
                fixture
                    .connection
                    .change_property32(
                        PropMode::REPLACE,
                        fixture.parent,
                        state,
                        AtomEnum::ATOM,
                        &[hidden],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
            }
        }
        fixture.button(1, false);
        drop(frozen);
        // This is a server-state barrier, not a sleep: the synchronized original
        // press must have been released/replayed/replaced before the check ends.
        fixture.confirm_button_released();
        fixture.assert_no_target_press(scenario);
        let update = service.poll();
        assert!(
            update.active.is_none(),
            "hidden or cancelled parent cannot start a picker"
        );
        let result = if let Some(result) = update.result {
            result
        } else {
            service.stop();
            terminal(&mut service)
        };
        assert!(!matches!(result, Ok(Some(selection)) if selection.picked.is_some()));
        fixture.assert_cleanup(true);
        fixture.assert_no_target_press(scenario);
    }
}

fn check_termination(scenario: usize) {
    let fixture = Fixture::new();
    let mut service = if scenario == 2 {
        let mut service = DragPickerButton::start_with_lifetime_for_test(
            Some(fixture.server.display.clone()),
            fixture.parent,
            std::process::id(),
            Duration::from_millis(150),
        )
        .unwrap();
        service.request(request(GENERATION, true)).unwrap();
        ready(&mut service, GENERATION);
        thread::sleep(Duration::from_millis(200));
        let idle = service.poll();
        assert!(
            idle.running && idle.result.is_none(),
            "idle handles have no gesture timeout"
        );
        service
    } else {
        fixture.arm()
    };
    fixture.motion(30, 30);
    fixture.button(1, true);
    if scenario != 2 {
        active(&mut service);
        // `active` is published before the native handoff. Confirm actual
        // pointer processing resumed before destroying the parent, so this
        // tests the picker phase, not just an early setup BadWindow.
        fixture.confirm_pointer_thawed();
    }
    match scenario {
        0 => service.stop(),
        1 => {
            fixture
                .connection
                .destroy_window(fixture.parent)
                .unwrap()
                .check()
                .unwrap();
        }
        _ => {}
    }
    let result = terminal_case(
        &mut service,
        ["explicit stop", "active parent close", "gesture watchdog"][scenario],
    );
    assert!(!matches!(&result, Ok(Some(selection)) if selection.picked.is_some()));
    if scenario == 2 {
        let error = result.unwrap_err();
        assert!(
            error.contains("timed out") || error.contains("deadline"),
            "{error}"
        );
    }
    fixture.button(1, false);
    fixture.assert_cleanup(scenario != 1);
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_drag_handle_rejects_foreign_parent_and_drop_releases_active_grab() {
    let fixture = Fixture::new();
    assert!(
        DragPickerButton::start(Some(fixture.server.display.clone()), fixture.parent, 0).is_err()
    );
    assert!(
        DragPickerButton::start(Some(fixture.server.display.clone()), 0, std::process::id())
            .is_err()
    );
    let mut foreign = DragPickerButton::start(
        Some(fixture.server.display.clone()),
        fixture.target,
        std::process::id(),
    )
    .unwrap();
    assert!(terminal(&mut foreign).is_err());
    fixture.assert_cleanup(true);
    let mut service = fixture.arm();
    fixture.motion(30, 30);
    fixture.button(1, true);
    active(&mut service);
    drop(service);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !fixture
        .connection
        .query_tree(fixture.parent)
        .unwrap()
        .reply()
        .unwrap()
        .children
        .is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "Drop left its native input child alive"
        );
        thread::sleep(Duration::from_millis(5));
    }
    fixture.button(1, false);
    fixture.assert_cleanup(true);
}
