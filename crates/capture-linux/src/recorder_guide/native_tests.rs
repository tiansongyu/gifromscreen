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
            BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, CreateGCAux,
            CreateWindowAux, EventMask, ImageFormat, ImageOrder, MOTION_NOTIFY_EVENT, MapState,
            Rectangle, Window, WindowClass,
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
            if stopped.recv_timeout(Duration::from_secs(20)).is_err() {
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
    assert_eq!(windows.len(), 4);
    for &window in &windows {
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
fn private_xvfb_owned_pointer_events_keep_generation_and_hide_cancels_old_gesture() {
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
            modifiers: 0
        }
    )));
    assert_eq!(
        pressed
            .iter()
            .filter(|event| matches!(event, GuidePointerEvent::Pressed { .. }))
            .count(),
        1,
        "secondary buttons must not start guide gestures"
    );
    fixture.motion(145, 58);
    fixture.button(1, false);
    let released = await_event(&mut guide, |event| {
        matches!(event, GuidePointerEvent::Released { .. })
    });
    assert!(released.iter().any(|event| matches!(
        event,
        GuidePointerEvent::Moved {
            generation: 1,
            position: PhysicalPosition { x: 145, y: 58 }
        }
    )));
    assert!(released.iter().any(|event| matches!(
        event,
        GuidePointerEvent::Released {
            generation: 1,
            position: PhysicalPosition { x: 145, y: 58 }
        }
    )));

    fixture.button(1, true);
    await_event(&mut guide, |event| {
        matches!(event, GuidePointerEvent::Pressed { generation: 1, .. })
    });
    guide.request(request(2, None, None)).unwrap();
    let cancelled = await_ack(&mut guide, 2, false);
    assert!(
        cancelled
            .iter()
            .any(|event| matches!(event, GuidePointerEvent::Cancelled { generation: 1 }))
    );
    fixture.button(1, false);
    guide.request(request(3, Some(region()), None)).unwrap();
    await_ack(&mut guide, 3, true);
    // A fresh primary press/release is the barrier for checking stale input.
    fixture.motion(140, 58);
    fixture.button(1, true);
    fixture.button(1, false);
    let events = await_event(&mut guide, |event| {
        matches!(event, GuidePointerEvent::Released { generation: 3, .. })
    });
    assert!(events.iter().all(|event| event.generation() == 3));
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
    let windows = fixture.windows();
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
