use x11rb::{
    protocol::xproto::{ConfigureWindowAux, PropMode},
    wrapper::ConnectionExt as _,
};

use super::*;

fn property(fixture: &Fixture, name: &[u8], values: &[u32]) {
    let atom = fixture
        .connection
        .intern_atom(false, name)
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    fixture
        .connection
        .change_property32(
            PropMode::REPLACE,
            fixture.underlay,
            atom,
            AtomEnum::CARDINAL,
            values,
        )
        .unwrap()
        .check()
        .unwrap();
}

fn observer(fixture: &Fixture) -> RecorderGuide {
    RecorderGuide::start_with_controller(
        Some(fixture.server.display.clone()),
        fixture.underlay,
        std::process::id(),
    )
    .unwrap()
}

fn await_geometry(guide: &mut RecorderGuide, expected: ControllerGeometry) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let update = guide.poll();
        assert!(
            !matches!(update.status, GuideStatus::Failed(_)),
            "{:?}",
            update.status
        );
        if update.controller == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "actual geometry never matched: {:?}",
            update.controller
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn await_failure(guide: &mut RecorderGuide) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let update = guide.poll();
        if let GuideStatus::Failed(error) = update.status {
            assert!(!guide.is_running());
            assert!(
                update.controller.is_none() && update.ack.is_none() && update.events.is_empty()
            );
            assert!(error.contains("Save the recording"));
            return error;
        }
        assert!(
            Instant::now() < deadline,
            "controller failure was not reported"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn configure(fixture: &Fixture, window: Window, rect: PhysicalRect, border: u32) {
    fixture
        .connection
        .configure_window(
            window,
            &ConfigureWindowAux::new()
                .x(rect.origin().x)
                .y(rect.origin().y)
                .width(rect.size().width())
                .height(rect.size().height())
                .border_width(border),
        )
        .unwrap()
        .check()
        .unwrap();
}

fn wrapper(fixture: &Fixture, parent: Window, rect: PhysicalRect, border: u16) -> Window {
    let id = fixture.connection.generate_id().unwrap();
    fixture
        .connection
        .create_window(
            0,
            id,
            parent,
            i16::try_from(rect.origin().x).unwrap(),
            i16::try_from(rect.origin().y).unwrap(),
            u16::try_from(rect.size().width()).unwrap(),
            u16::try_from(rect.size().height()).unwrap(),
            border,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(0x0012_3456)
                .border_pixel(0),
        )
        .unwrap()
        .check()
        .unwrap();
    fixture.connection.map_window(id).unwrap().check().unwrap();
    id
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_controller_frameless_ignores_stale_extents_and_observes_hidden_movement() {
    let fixture = Fixture::new();
    property(&fixture, b"_NET_WM_PID", &[std::process::id()]);
    property(&fixture, b"_NET_FRAME_EXTENTS", &[0, 0, 37, 0]);
    let initial = PhysicalRect::new(-7, 19, 220, 163).unwrap();
    configure(&fixture, fixture.underlay, initial, 0);
    let mut guide = observer(&fixture);
    await_geometry(
        &mut guide,
        ControllerGeometry {
            client: initial,
            outer: initial,
            viewable: true,
        },
    );
    fixture
        .connection
        .unmap_window(fixture.underlay)
        .unwrap()
        .check()
        .unwrap();
    await_geometry(
        &mut guide,
        ControllerGeometry {
            client: initial,
            outer: initial,
            viewable: false,
        },
    );
    let moved = PhysicalRect::new(13, 29, 210, 153).unwrap();
    configure(&fixture, fixture.underlay, moved, 0);
    await_geometry(
        &mut guide,
        ControllerGeometry {
            client: moved,
            outer: moved,
            viewable: false,
        },
    );
    fixture
        .connection
        .map_window(fixture.underlay)
        .unwrap()
        .check()
        .unwrap();
    await_geometry(
        &mut guide,
        ControllerGeometry {
            client: moved,
            outer: moved,
            viewable: true,
        },
    );
    stop(&mut guide);
    assert!(guide.poll().controller.is_none());
    fixture.assert_cleanup();
    let extents = fixture
        .connection
        .intern_atom(true, b"_NET_FRAME_EXTENTS")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    assert_eq!(
        fixture
            .connection
            .get_property(false, fixture.underlay, extents, AtomEnum::CARDINAL, 0, 4)
            .unwrap()
            .reply()
            .unwrap()
            .value32()
            .unwrap()
            .collect::<Vec<_>>(),
        [0, 0, 37, 0]
    );
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_controller_real_wm_ancestry_includes_native_border_and_tracks_reparenting() {
    let mut fixture = Fixture::new();
    property(&fixture, b"_NET_WM_PID", &[std::process::id()]);
    property(&fixture, b"_NET_FRAME_EXTENTS", &[99; 4]);
    let outer = wrapper(
        &fixture,
        fixture.root,
        PhysicalRect::new(50, 40, 200, 180).unwrap(),
        3,
    );
    let inner = wrapper(
        &fixture,
        outer,
        PhysicalRect::new(7, 9, 170, 130).unwrap(),
        2,
    );
    fixture.original_children.insert(outer);
    fixture
        .connection
        .reparent_window(fixture.underlay, inner, 11, 13)
        .unwrap()
        .check()
        .unwrap();
    configure(
        &fixture,
        fixture.underlay,
        PhysicalRect::new(11, 13, 100, 80).unwrap(),
        1,
    );
    fixture
        .connection
        .map_window(fixture.underlay)
        .unwrap()
        .check()
        .unwrap();
    let mut guide = observer(&fixture);
    await_geometry(
        &mut guide,
        ControllerGeometry {
            client: PhysicalRect::new(74, 68, 100, 80).unwrap(),
            outer: PhysicalRect::new(50, 40, 206, 186).unwrap(),
            viewable: true,
        },
    );
    fixture
        .connection
        .unmap_window(outer)
        .unwrap()
        .check()
        .unwrap();
    await_geometry(
        &mut guide,
        ControllerGeometry {
            client: PhysicalRect::new(74, 68, 100, 80).unwrap(),
            outer: PhysicalRect::new(50, 40, 206, 186).unwrap(),
            viewable: false,
        },
    );
    fixture
        .connection
        .reparent_window(fixture.underlay, fixture.root, -5, 7)
        .unwrap()
        .check()
        .unwrap();
    fixture
        .connection
        .map_window(fixture.underlay)
        .unwrap()
        .check()
        .unwrap();
    await_geometry(
        &mut guide,
        ControllerGeometry {
            client: PhysicalRect::new(-4, 8, 100, 80).unwrap(),
            outer: PhysicalRect::new(-5, 7, 102, 82).unwrap(),
            viewable: true,
        },
    );
    stop(&mut guide);
    fixture.assert_cleanup();
    assert_eq!(
        fixture
            .connection
            .get_geometry(outer)
            .unwrap()
            .reply()
            .unwrap()
            .border_width,
        3
    );
    assert_eq!(
        fixture
            .connection
            .get_geometry(inner)
            .unwrap()
            .reply()
            .unwrap()
            .border_width,
        2
    );
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_controller_foreign_or_changed_pid_and_lost_window_fail_with_cleanup() {
    for scenario in 0..3 {
        let fixture = Fixture::new();
        property(
            &fixture,
            b"_NET_WM_PID",
            &[if scenario == 0 { 0 } else { std::process::id() }],
        );
        let mut guide = observer(&fixture);
        if scenario != 0 {
            let rect = PhysicalRect::new(0, 0, 400, 300).unwrap();
            await_geometry(
                &mut guide,
                ControllerGeometry {
                    client: rect,
                    outer: rect,
                    viewable: true,
                },
            );
            if scenario == 1 {
                property(&fixture, b"_NET_WM_PID", &[0]);
            } else {
                fixture
                    .connection
                    .destroy_window(fixture.underlay)
                    .unwrap()
                    .check()
                    .unwrap();
            }
        }
        await_failure(&mut guide);
        assert!(fixture.windows().is_empty());
        if scenario != 2 {
            fixture.assert_cleanup();
        }
    }
}

#[test]
#[ignore = "starts a supervised private Xvfb; never uses host DISPLAY"]
fn private_xvfb_controller_ancestry_is_bounded_to_four_queries() {
    let mut fixture = Fixture::new();
    property(&fixture, b"_NET_WM_PID", &[std::process::id()]);
    let mut parent = fixture.root;
    for index in 0..4 {
        let id = wrapper(
            &fixture,
            parent,
            PhysicalRect::new(0, 0, 400, 300).unwrap(),
            0,
        );
        if index == 0 {
            fixture.original_children.insert(id);
        }
        parent = id;
    }
    fixture
        .connection
        .reparent_window(fixture.underlay, parent, 0, 0)
        .unwrap()
        .check()
        .unwrap();
    fixture
        .connection
        .map_window(fixture.underlay)
        .unwrap()
        .check()
        .unwrap();
    let mut guide = observer(&fixture);
    assert!(await_failure(&mut guide).contains("four-query"));
    fixture.assert_cleanup();
}
