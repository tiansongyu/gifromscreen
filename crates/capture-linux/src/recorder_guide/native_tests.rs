//! Real X11 transport on an owned server, not a compositor-presentation proof.

use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        shape::{ConnectionExt as _, SK},
        xproto::{
            AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, CreateGCAux,
            CreateWindowAux, EventMask, GrabMode, GrabStatus, ImageFormat, ImageOrder, InputFocus,
            MOTION_NOTIFY_EVENT, MapState, Rectangle, Window, WindowClass,
        },
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

use super::*;

struct PrivateXvfb {
    child: Arc<Mutex<Child>>,
    display: String,
    stop_watchdog: Option<mpsc::SyncSender<()>>,
    watchdog: Option<thread::JoinHandle<()>>,
}

impl PrivateXvfb {
    fn start() -> Self {
        let mut child = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "400x300x24",
                "-nolisten",
                "tcp",
                "-ac",
                "-noreset",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("install Xvfb for the explicit recorder-guide test");
        let stdout = child.stdout.take().unwrap();
        let child = Arc::new(Mutex::new(child));
        let (stop, stopped) = mpsc::sync_channel(1);
        let watched = Arc::clone(&child);
        let watchdog = thread::spawn(move || {
            if stopped.recv_timeout(Duration::from_secs(45)).is_err() {
                let mut child = watched.lock().unwrap();
                let _ = child.kill();
                let _ = child.wait();
            }
        });
        let mut server = Self {
            child,
            display: String::new(),
            stop_watchdog: Some(stop),
            watchdog: Some(watchdog),
        };
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
                server.shutdown();
                reader.join().unwrap();
                panic!("private Xvfb startup: {error:?}");
            }
        };
        reader.join().unwrap();
        let display = line
            .trim()
            .parse::<u16>()
            .expect("Xvfb must allocate and report its own display");
        server.display = format!(":{display}");
        server
    }

    fn shutdown(&mut self) {
        if let Some(stop) = self.stop_watchdog.take() {
            let _ = stop.send(());
        }
        {
            let mut child = self.child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.join().unwrap();
        }
    }
}

impl Drop for PrivateXvfb {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Fixture {
    connection: RustConnection,
    root: Window,
    underlay: Window,
    original_children: BTreeSet<Window>,
    // Keep the owned server alive until all clients/guide cleanup are done.
    server: PrivateXvfb,
}

impl Fixture {
    fn new() -> Self {
        let server = PrivateXvfb::start();
        let (connection, screen) = x11rb::connect(Some(&server.display)).unwrap();
        let root = connection.setup().roots[screen].root;
        let underlay = connection.generate_id().unwrap();
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                underlay,
                root,
                0,
                0,
                400,
                300,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .background_pixel(0x0012_3456)
                    .event_mask(
                        EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE
                            | EventMask::POINTER_MOTION,
                    ),
            )
            .unwrap()
            .check()
            .unwrap();
        connection.map_window(underlay).unwrap().check().unwrap();
        connection
            .set_input_focus(InputFocus::POINTER_ROOT, underlay, x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        let gc = connection.generate_id().unwrap();
        connection
            .create_gc(gc, underlay, &CreateGCAux::new().foreground(0x0028_6743))
            .unwrap()
            .check()
            .unwrap();
        connection
            .poly_fill_rectangle(
                underlay,
                gc,
                &[Rectangle {
                    x: 120,
                    y: 80,
                    width: 110,
                    height: 130,
                }],
            )
            .unwrap()
            .check()
            .unwrap();
        connection.free_gc(gc).unwrap().check().unwrap();
        let original_children = connection
            .query_tree(root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .into_iter()
            .collect();
        Self {
            connection,
            root,
            underlay,
            original_children,
            server,
        }
    }

    fn guide(&self) -> RecorderGuide {
        RecorderGuide::start(Some(self.server.display.clone())).unwrap()
    }

    fn windows(&self) -> Vec<Window> {
        self.connection
            .query_tree(self.root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .into_iter()
            .filter(|window| !self.original_children.contains(window))
            .collect()
    }

    fn strips(&self) -> Vec<Window> {
        self.windows()
            .into_iter()
            .filter(|window| {
                self.connection
                    .get_window_attributes(*window)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .class
                    == WindowClass::INPUT_OUTPUT
            })
            .collect()
    }

    fn keeper(&self) -> Window {
        let keepers: Vec<_> = self
            .windows()
            .into_iter()
            .filter(|window| {
                self.connection
                    .get_window_attributes(*window)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .class
                    == WindowClass::INPUT_ONLY
            })
            .collect();
        assert_eq!(keepers.len(), 1);
        let keeper = keepers[0];
        assert_eq!(
            self.connection
                .get_window_attributes(keeper)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::VIEWABLE
        );
        assert!(
            self.connection
                .shape_get_rectangles(keeper, SK::INPUT)
                .unwrap()
                .reply()
                .unwrap()
                .rectangles
                .is_empty()
        );
        keeper
    }

    fn try_pointer(&self) -> GrabStatus {
        let status = self
            .connection
            .grab_pointer(
                false,
                self.underlay,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
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
        if status == GrabStatus::SUCCESS {
            self.connection
                .ungrab_pointer(x11rb::CURRENT_TIME)
                .unwrap()
                .check()
                .unwrap();
        }
        status
    }

    fn assert_pointer_free(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.try_pointer() != GrabStatus::SUCCESS {
            assert!(
                Instant::now() < deadline,
                "the guide did not release its pointer grab"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_strip_contract(&self, window: Window) {
        let attributes = self
            .connection
            .get_window_attributes(window)
            .unwrap()
            .reply()
            .unwrap();
        assert!(
            !attributes
                .all_event_masks
                .contains(EventMask::POINTER_MOTION),
            "idle strips must not select a hover motion feed"
        );
        let extents = self
            .connection
            .intern_atom(true, b"_GTK_FRAME_EXTENTS")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let extents = self
            .connection
            .get_property(false, window, extents, AtomEnum::CARDINAL, 0, 4)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(extents.value32().unwrap().collect::<Vec<_>>(), [0; 4]);
    }

    fn pixels(&self, region: PhysicalRect) -> Vec<u8> {
        self.connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                i16::try_from(region.origin().x).unwrap(),
                i16::try_from(region.origin().y).unwrap(),
                u16::try_from(region.size().width()).unwrap(),
                u16::try_from(region.size().height()).unwrap(),
                u32::MAX,
            )
            .unwrap()
            .reply()
            .unwrap()
            .data
    }

    fn motion(&self, x: i16, y: i16) {
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
    }

    fn pixel_rgb(&self, x: i32, y: i32) -> u32 {
        let pixel: [u8; 4] = self
            .pixels(PhysicalRect::new(x, y, 1, 1).unwrap())
            .try_into()
            .unwrap();
        let packed = if self.connection.setup().image_byte_order == ImageOrder::LSB_FIRST {
            u32::from_le_bytes(pixel)
        } else {
            u32::from_be_bytes(pixel)
        };
        packed & 0x00ff_ffff
    }

    fn button(&self, button: u8, pressed: bool) {
        self.connection
            .xtest_fake_input(
                if pressed {
                    BUTTON_PRESS_EVENT
                } else {
                    BUTTON_RELEASE_EVENT
                },
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

    fn drain_events(&self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(event) = self.connection.poll_for_event().unwrap() {
            events.push(event);
        }
        events
    }

    fn assert_cleanup(&self) {
        assert!(
            self.windows().is_empty(),
            "only the guide's windows must be destroyed"
        );
        assert_eq!(
            self.connection
                .get_window_attributes(self.underlay)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::VIEWABLE
        );
    }
}

fn request(
    generation: u64,
    region: Option<PhysicalRect>,
    protected_region: Option<PhysicalRect>,
) -> GuideRequest {
    GuideRequest {
        generation,
        region,
        protected_region,
        border_width: 4,
    }
}

fn region() -> PhysicalRect {
    PhysicalRect::new(80, 60, 120, 90).unwrap()
}

fn await_ack(guide: &mut RecorderGuide, generation: u64, visible: bool) -> Vec<GuidePointerEvent> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Vec::new();
    loop {
        let update = guide.poll();
        assert!(
            !matches!(update.status, GuideStatus::Failed(_)),
            "guide failed: {:?}",
            update.status
        );
        events.extend(update.events);
        if update.ack
            == Some(GuideAck {
                generation,
                visible,
            })
        {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "guide did not acknowledge generation {generation}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn await_event(
    guide: &mut RecorderGuide,
    wanted: impl Fn(&GuidePointerEvent) -> bool,
) -> Vec<GuidePointerEvent> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Vec::new();
    loop {
        let update = guide.poll();
        assert!(
            !matches!(update.status, GuideStatus::Failed(_)),
            "guide failed: {:?}",
            update.status
        );
        events.extend(update.events);
        if events.iter().any(&wanted) {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "guide input event did not arrive: {events:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn stop(guide: &mut RecorderGuide) {
    guide.stop();
    let deadline = Instant::now() + Duration::from_secs(3);
    while guide.is_running() {
        let update = guide.poll();
        assert!(update.events.is_empty() && update.ack.is_none());
        assert!(Instant::now() < deadline, "guide cleanup did not finish");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(guide.poll().status, GuideStatus::Stopped);
}

fn begin_drag(fixture: &Fixture, guide: &mut RecorderGuide, generation: u64) -> u64 {
    fixture.motion(140, 58);
    fixture.button(1, true);
    let events = await_event(guide, |event| {
        matches!(event, GuidePointerEvent::Pressed { .. })
    });
    pressed_id(&events, generation)
}

fn pressed_id(events: &[GuidePointerEvent], generation: u64) -> u64 {
    events
        .iter()
        .find_map(|event| match event {
            GuidePointerEvent::Pressed {
                generation: actual,
                gesture_id,
                ..
            } if *actual == generation => Some(*gesture_id),
            _ => None,
        })
        .expect("press must name the acknowledged initial presentation")
}

fn intersects(a: PhysicalRect, b: PhysicalRect) -> bool {
    let (ax, ay) = (i64::from(a.origin().x), i64::from(a.origin().y));
    let (bx, by) = (i64::from(b.origin().x), i64::from(b.origin().y));
    ax < bx + i64::from(b.size().width())
        && bx < ax + i64::from(a.size().width())
        && ay < by + i64::from(b.size().height())
        && by < ay + i64::from(a.size().height())
}

fn assert_shapes_outside(fixture: &Fixture, windows: &[Window], excluded: &[PhysicalRect]) {
    for &window in windows {
        let geometry = fixture
            .connection
            .get_geometry(window)
            .unwrap()
            .reply()
            .unwrap();
        let attributes = fixture
            .connection
            .get_window_attributes(window)
            .unwrap()
            .reply()
            .unwrap();
        assert!(attributes.override_redirect);
        for kind in [SK::BOUNDING, SK::INPUT] {
            let rectangles = fixture
                .connection
                .shape_get_rectangles(window, kind)
                .unwrap()
                .reply()
                .unwrap()
                .rectangles;
            for rectangle in rectangles {
                if rectangle.width == 0 || rectangle.height == 0 {
                    continue;
                }
                let physical = PhysicalRect::new(
                    i32::from(geometry.x) + i32::from(rectangle.x),
                    i32::from(geometry.y) + i32::from(rectangle.y),
                    u32::from(rectangle.width),
                    u32::from(rectangle.height),
                )
                .unwrap();
                for &protected in excluded {
                    assert!(
                        !intersects(physical, protected),
                        "native Bounding/Input shape must not cover protected pixels"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_ring_preserves_capture_pixels_shapes_clickthrough_and_owned_cleanup() {
    let fixture = Fixture::new();
    let protected = region();
    let baseline = fixture.pixels(protected);
    let mut guide = fixture.guide();
    guide.request(request(1, Some(protected), None)).unwrap();
    await_ack(&mut guide, 1, true);
    let windows = fixture.windows();
    assert_eq!(windows.len(), 5);
    fixture.keeper();
    let strips = fixture.strips();
    assert_eq!(strips.len(), 4);
    for &window in &strips {
        fixture.assert_strip_contract(window);
        let geometry = fixture
            .connection
            .get_geometry(window)
            .unwrap()
            .reply()
            .unwrap();
        assert!(
            geometry.width == 4 || geometry.height == 4,
            "each owned window must be a thin strip, not a full-canvas surface"
        );
        assert_eq!(
            fixture
                .connection
                .get_window_attributes(window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::VIEWABLE
        );
    }
    assert_eq!(
        fixture.pixel_rgb(140, 58),
        0x00f2_994a,
        "the accepted guide must paint the requested orange border on Xvfb"
    );
    assert_shapes_outside(&fixture, &windows, &[protected]);
    assert!(
        fixture.pixels(protected) == baseline,
        "the guide must not change capture pixels"
    );
    let ack = guide.poll().ack;
    assert!(guide.request(request(1, None, None)).is_err());
    assert!(guide.request(request(0, None, None)).is_err());
    assert_eq!(
        guide.poll().ack,
        ack,
        "invalid requests cannot hide the previous guide"
    );
    fixture.drain_events();
    fixture.motion(140, 100);
    fixture.button(1, true);
    fixture.button(1, false);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if fixture.drain_events().iter().any(
            |event| matches!(event, Event::ButtonPress(event) if event.event == fixture.underlay),
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "capture hole must pass input to the underlay"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(guide.poll().events.is_empty());

    let moved = PhysicalRect::new(160, 100, 80, 60).unwrap();
    guide
        .request(request(2, Some(moved), Some(protected)))
        .unwrap();
    await_ack(&mut guide, 2, true);
    assert_shapes_outside(&fixture, &windows, &[protected, moved]);
    assert!(
        fixture.pixels(protected) == baseline,
        "double exclusion must protect the old capture rectangle during retarget"
    );
    stop(&mut guide);
    fixture.assert_cleanup();
    assert!(
        fixture.pixels(protected) == baseline,
        "cleanup must leave the underlay unchanged"
    );
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_owned_gesture_survives_repeated_updates_hide_and_release() {
    let fixture = Fixture::new();
    let mut guide = fixture.guide();
    guide.request(request(1, Some(region()), None)).unwrap();
    await_ack(&mut guide, 1, true);
    fixture.motion(140, 58);
    fixture.button(3, true);
    fixture.button(3, false);
    fixture.button(1, true);
    let pressed = await_event(&mut guide, |event| {
        matches!(event, GuidePointerEvent::Pressed { .. })
    });
    assert!(pressed.iter().any(|event| matches!(
        event,
        GuidePointerEvent::Pressed {
            generation: 1,
            position: PhysicalPosition { x: 140, y: 58 },
            edge: GuideEdge::Top,
            modifiers: 0,
            ..
        }
    )));
    let first = pressed_id(&pressed, 1);
    assert!(first > 0);
    fixture.keeper();
    assert_eq!(fixture.try_pointer(), GrabStatus::ALREADY_GRABBED);
    assert_eq!(
        fixture
            .connection
            .get_input_focus()
            .unwrap()
            .reply()
            .unwrap()
            .focus,
        fixture.underlay
    );
    assert_eq!(
        pressed
            .iter()
            .filter(|event| matches!(event, GuidePointerEvent::Pressed { .. }))
            .count(),
        1,
        "secondary buttons must not start guide gestures"
    );
    for (generation, region, visible) in [
        (2, Some(PhysicalRect::new(110, 80, 100, 70).unwrap()), true),
        (3, None, false),
        (4, Some(PhysicalRect::new(160, 100, 80, 60).unwrap()), true),
    ] {
        guide.request(request(generation, region, None)).unwrap();
        let update_events = await_ack(&mut guide, generation, visible);
        assert!(update_events.iter().all(|event| !matches!(
            event,
            GuidePointerEvent::Cancelled { .. } | GuidePointerEvent::Released { .. }
        )));
        assert_eq!(
            fixture.try_pointer(),
            GrabStatus::ALREADY_GRABBED,
            "unmapping strips must not end the stable keeper grab"
        );
        fixture.motion(145, 200);
        await_event(
            &mut guide,
            |event| matches!(event, GuidePointerEvent::Moved { gesture_id, position: PhysicalPosition { x:145, y:200 } } if *gesture_id == first),
        );
        fixture.motion(140, 180);
        await_event(
            &mut guide,
            |event| matches!(event, GuidePointerEvent::Moved { gesture_id, .. } if *gesture_id == first),
        );
    }
    fixture.motion(145, 58);
    fixture.button(1, false);
    let released = await_event(&mut guide, |event| {
        matches!(event, GuidePointerEvent::Released { .. })
    });
    assert!(released.iter().any(|event| matches!(
        event,
        GuidePointerEvent::Moved {
            gesture_id,
            position: PhysicalPosition { x: 145, y: 58 }
        } if *gesture_id == first
    )));
    assert!(released.iter().any(|event| matches!(
        event,
        GuidePointerEvent::Released {
            gesture_id,
            position: PhysicalPosition { x: 145, y: 58 }
        } if *gesture_id == first
    )));
    fixture.assert_pointer_free();
    stop(&mut guide);
    fixture.assert_cleanup();
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_signed_positions_full_root_and_explicit_hide_never_cover_capture() {
    let fixture = Fixture::new();
    let mut guide = fixture.guide();
    let signed = PhysicalRect::new(-5, 20, 65, 70).unwrap();
    let visible_capture = PhysicalRect::new(0, 20, 60, 70).unwrap();
    let baseline = fixture.pixels(visible_capture);
    guide.request(request(1, Some(signed), None)).unwrap();
    await_ack(&mut guide, 1, true);
    let windows = fixture.strips();
    assert_eq!(windows.len(), 4);
    assert_shapes_outside(&fixture, &windows, &[signed]);
    assert!(
        fixture.pixels(visible_capture) == baseline,
        "signed guide coordinates must not change the visible capture pixels"
    );
    guide
        .request(request(
            2,
            Some(PhysicalRect::new(0, 0, 400, 300).unwrap()),
            None,
        ))
        .unwrap();
    await_ack(&mut guide, 2, false);
    for &window in &windows {
        assert_eq!(
            fixture
                .connection
                .get_window_attributes(window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::UNMAPPED
        );
    }
    guide.request(request(3, Some(region()), None)).unwrap();
    await_ack(&mut guide, 3, true);
    guide.request(request(4, None, None)).unwrap();
    await_ack(&mut guide, 4, false);
    for &window in &windows {
        assert_eq!(
            fixture
                .connection
                .get_window_attributes(window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::UNMAPPED
        );
    }
    stop(&mut guide);
    fixture.assert_cleanup();
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_explicit_cancel_releases_grab_and_same_presentation_gets_new_gesture_id() {
    let fixture = Fixture::new();
    let mut guide = fixture.guide();
    guide.request(request(1, Some(region()), None)).unwrap();
    await_ack(&mut guide, 1, true);
    let first = begin_drag(&fixture, &mut guide, 1);
    assert_eq!(fixture.try_pointer(), GrabStatus::ALREADY_GRABBED);
    fixture.motion(150, 200);
    guide.cancel_gesture();
    let events = await_event(
        &mut guide,
        |event| matches!(event, GuidePointerEvent::Cancelled { gesture_id } if *gesture_id == first),
    );
    assert!(
        events
            .iter()
            .all(|event| matches!(event, GuidePointerEvent::Cancelled { .. })),
        "cancel must discard queued old motion"
    );
    fixture.assert_pointer_free();
    fixture.button(1, false); // A late release of the cancelled physical press.
    let second = begin_drag(&fixture, &mut guide, 1);
    assert!(second > first);
    fixture.motion(180, 180);
    fixture.button(1, false);
    let events = await_event(
        &mut guide,
        |event| matches!(event, GuidePointerEvent::Released { gesture_id, .. } if *gesture_id == second),
    );
    assert!(events.iter().all(|event| event.gesture_id() == second));
    fixture.assert_pointer_free();
    stop(&mut guide);
    fixture.assert_cleanup();
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_stop_drop_and_keeper_failure_release_only_the_owned_pointer_grab() {
    for termination in 0..3 {
        let fixture = Fixture::new();
        let mut guide = fixture.guide();
        guide.request(request(1, Some(region()), None)).unwrap();
        await_ack(&mut guide, 1, true);
        begin_drag(&fixture, &mut guide, 1);
        assert_eq!(fixture.try_pointer(), GrabStatus::ALREADY_GRABBED);
        match termination {
            0 => stop(&mut guide),
            1 => drop(guide),
            _ => {
                fixture
                    .connection
                    .destroy_window(fixture.keeper())
                    .unwrap()
                    .check()
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(3);
                while guide.is_running() {
                    let update = guide.poll();
                    if !guide.is_running() {
                        assert!(matches!(update.status, GuideStatus::Failed(_)));
                    }
                    assert!(
                        Instant::now() < deadline,
                        "destroyed keeper must end the guide"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
        fixture.assert_pointer_free();
        fixture.button(1, false);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fixture.windows().is_empty() {
            assert!(
                Instant::now() < deadline,
                "guide Drop must destroy owned windows"
            );
            thread::sleep(Duration::from_millis(5));
        }
        fixture.assert_cleanup();
    }
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_fast_click_or_hover_cannot_leave_an_idle_pointer_grab() {
    let fixture = Fixture::new();
    let mut guide = fixture.guide();
    guide.request(request(1, Some(region()), None)).unwrap();
    await_ack(&mut guide, 1, true);
    fixture.motion(140, 58);
    fixture.assert_pointer_free();
    assert!(
        guide.poll().events.is_empty(),
        "hover is not an input stream"
    );
    for _ in 0..8 {
        fixture.button(1, true);
        fixture.button(1, false);
    }
    // Wait for a presentation barrier and native ungrab before a separate new
    // physical press; a click during an older grab's cancellation is not a new
    // independently guaranteed gesture.
    guide.request(request(2, Some(region()), None)).unwrap();
    await_ack(&mut guide, 2, true);
    fixture.assert_pointer_free();
    let id = begin_drag(&fixture, &mut guide, 2);
    fixture.button(1, false);
    await_event(
        &mut guide,
        |event| matches!(event, GuidePointerEvent::Released { gesture_id, .. } if *gesture_id == id),
    );
    fixture.assert_pointer_free();
    stop(&mut guide);
    fixture.assert_cleanup();
}

#[test]
#[ignore = "30-second watchdog on a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_gesture_lifetime_watchdog_ungrabs_without_a_release_event() {
    let fixture = Fixture::new();
    let mut guide = fixture.guide();
    guide.request(request(1, Some(region()), None)).unwrap();
    await_ack(&mut guide, 1, true);
    let id = begin_drag(&fixture, &mut guide, 1);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(35);
    loop {
        let update = guide.poll();
        assert!(
            !matches!(update.status, GuideStatus::Failed(_)),
            "watchdog is a gesture cancellation, not a guide failure"
        );
        if update.events.iter().any(|event| matches!(event, GuidePointerEvent::Cancelled { gesture_id } if *gesture_id == id)) { break; }
        assert!(
            Instant::now() < deadline,
            "gesture watchdog did not cancel the pointer grab"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(started.elapsed() >= Duration::from_secs(29));
    fixture.assert_pointer_free();
    fixture.button(1, false);
    stop(&mut guide);
    fixture.assert_cleanup();
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_stopping_an_idle_guide_does_not_ungrab_another_client() {
    let fixture = Fixture::new();
    let mut guide = fixture.guide();
    guide.request(request(1, Some(region()), None)).unwrap();
    await_ack(&mut guide, 1, true);
    assert_eq!(
        fixture
            .connection
            .grab_pointer(
                false,
                fixture.underlay,
                EventMask::POINTER_MOTION,
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
    stop(&mut guide);
    let (observer, _) = x11rb::connect(Some(&fixture.server.display)).unwrap();
    assert_eq!(
        observer
            .grab_pointer(
                false,
                fixture.underlay,
                EventMask::POINTER_MOTION,
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
    fixture.assert_pointer_free();
    fixture.assert_cleanup();
}
