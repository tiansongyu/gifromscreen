use super::super::{Inbox, ShortcutKey, ShortcutStatus};
use super::*;
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::Instant,
};
use x11rb::{
    protocol::{
        xproto::{
            CreateWindowAux, EventMask, InputFocus, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, WindowClass,
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
    fn start() -> Self {
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
            .expect("install Xvfb for the explicit native test");
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout.take(32))
                .read_line(&mut line)
                .map(|_| line);
            let _ = sender.send(result);
        });
        let line = match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(line)) => line,
            other => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                panic!("private Xvfb startup failed: {other:?}");
            }
        };
        reader.join().unwrap();
        let number = line
            .trim()
            .parse::<u16>()
            .expect("Xvfb must report its own chosen display");
        Self {
            child,
            display: format!(":{number}"),
        }
    }
    fn client(&self) -> RustConnection {
        x11rb::connect(Some(&self.display)).unwrap().0
    }
}
impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Worker {
    context: ShortcutContext,
    result: Receiver<Result<(), String>>,
    join: Option<thread::JoinHandle<()>>,
}
impl Worker {
    fn start(display: String, bindings: Vec<ShortcutBinding>) -> Self {
        let mut requested = [false; 3];
        for binding in &bindings {
            requested[binding.action.index()] = true;
        }
        let context = ShortcutContext {
            cancellation: Arc::new(AtomicBool::new(false)),
            inbox: Arc::new(Mutex::new(Inbox {
                handler: None,
                status: ShortcutStatus::Registering,
                queue: VecDeque::new(),
                stop_pending: false,
                pressed: [false; 3],
                requested,
                dropped: 0,
            })),
        };
        let worker = ShortcutContext {
            cancellation: Arc::clone(&context.cancellation),
            inbox: Arc::clone(&context.inbox),
        };
        let (sender, result) = mpsc::sync_channel(1);
        let join = thread::spawn(move || {
            let _ = sender.send(run_display(Some(&display), &bindings, &worker));
        });
        Self {
            context,
            result,
            join: Some(join),
        }
    }
    fn active(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if matches!(
                self.context.inbox.lock().unwrap().status,
                ShortcutStatus::Active(_)
            ) {
                return;
            }
            if let Ok(result) = self.result.try_recv() {
                panic!("shortcut registration terminated: {result:?}");
            }
            assert!(Instant::now() < deadline, "shortcut registration deadline");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn drain(&self) -> Vec<ShortcutAction> {
        let mut inbox = self.context.inbox.lock().unwrap();
        if inbox.stop_pending {
            inbox.stop_pending = false;
            inbox.queue.clear();
            vec![ShortcutAction::Stop]
        } else {
            inbox.queue.drain(..).collect()
        }
    }
    fn action(&self, expected: ShortcutAction) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let actions = self.drain();
            if !actions.is_empty() {
                assert_eq!(actions, [expected]);
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing shortcut action {expected:?}; status {:?}; worker {:?}",
                self.context.inbox.lock().unwrap().status,
                self.result.try_recv()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn stop(&mut self) {
        self.context.cancellation.store(true, Ordering::Release);
        assert!(
            self.result
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_ok()
        );
        self.join.take().unwrap().join().unwrap();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.context.cancellation.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

fn code(connection: &RustConnection, symbol: u32) -> u8 {
    let setup = connection.setup();
    let count = setup.max_keycode - setup.min_keycode + 1;
    let map = connection
        .get_keyboard_mapping(setup.min_keycode, count)
        .unwrap()
        .reply()
        .unwrap();
    let columns = usize::from(map.keysyms_per_keycode);
    let index = map
        .keysyms
        .chunks(columns)
        .position(|symbols| symbols.first() == Some(&symbol))
        .unwrap();
    u8::try_from(usize::from(setup.min_keycode) + index).unwrap()
}
fn inject(connection: &RustConnection, code: u8, pressed: bool) {
    connection
        .xtest_fake_input(
            if pressed {
                KEY_PRESS_EVENT
            } else {
                KEY_RELEASE_EVENT
            },
            code,
            0,
            connection.setup().roots[0].root,
            0,
            0,
            0,
        )
        .unwrap()
        .check()
        .unwrap();
}
fn focused_window(connection: &RustConnection) -> u32 {
    let root = connection.setup().roots[0].root;
    let window = connection.generate_id().unwrap();
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            20,
            20,
            100,
            100,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
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
    assert_eq!(
        connection.get_input_focus().unwrap().reply().unwrap().focus,
        window
    );
    window
}

#[test]
#[ignore = "spawns its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_focus_held_key_release_caps_num_and_unrelated_typing() {
    let server = Xvfb::start();
    let client = server.client();
    let focus = focused_window(&client);
    // Fresh Xvfb adopts the XTEST device map on first input. Establish it
    // before registration; live mapping-change teardown is tested separately.
    let warmup = code(&client, u32::from(b'a'));
    inject(&client, warmup, true);
    inject(&client, warmup, false);
    client.sync().unwrap();
    while client.poll_for_event().unwrap().is_some() {}
    let mut bindings = super::super::default_shortcut_bindings();
    bindings[2].trigger.key = ShortcutKey::Character('R');
    let mut worker = Worker::start(server.display.clone(), bindings);
    worker.active();
    let control = code(&client, 0xffe3);
    let shift = code(&client, 0xffe1);
    let function = code(&client, 0xffc4);
    let letter = code(&client, u32::from(b'r'));
    for (caps, num) in [(false, false), (true, false), (true, true), (false, true)] {
        for (symbol, toggle) in [(0xffe5, caps), (0xff7f, num)] {
            // Each pair resets both locks after its assertions below.
            if toggle {
                let key = code(&client, symbol);
                inject(&client, key, true);
                inject(&client, key, false);
            }
        }
        inject(&client, control, true);
        inject(&client, shift, true);
        inject(&client, function, true);
        worker.action(ShortcutAction::StartPause);
        // XTEST ignores repeated downs on this server; this checks held-key
        // behavior, not a claim that hardware autorepeat was generated.
        for _ in 0..8 {
            inject(&client, function, true);
        }
        thread::sleep(Duration::from_millis(100));
        assert!(
            worker.drain().is_empty(),
            "held/repeated key must not retrigger"
        );
        inject(&client, control, false);
        inject(&client, shift, false);
        inject(&client, function, false);
        inject(&client, control, true);
        inject(&client, shift, true);
        inject(&client, letter, true);
        worker.action(ShortcutAction::Snapshot);
        inject(&client, letter, false);
        inject(&client, shift, false);
        inject(&client, control, false);
        for (symbol, toggle) in [(0xffe5, caps), (0xff7f, num)] {
            if toggle {
                let key = code(&client, symbol);
                inject(&client, key, true);
                inject(&client, key, false);
            }
        }
    }
    let stop = code(&client, 0xffc5);
    inject(&client, control, true);
    inject(&client, shift, true);
    inject(&client, stop, true);
    worker.action(ShortcutAction::Stop);
    inject(&client, stop, false);
    inject(&client, shift, false);
    inject(&client, control, false);
    inject(&client, function, true);
    inject(&client, function, false);
    thread::sleep(Duration::from_millis(30));
    assert!(
        worker.drain().is_empty(),
        "an unmodified F7 must not match CTRL+SHIFT+F7"
    );
    let ordinary = code(&client, u32::from(b'a'));
    assert_other_client_receives_typing(&client, ordinary, focus);
    assert!(worker.drain().is_empty());
    inject(&client, control, true);
    inject(&client, shift, true);
    inject(&client, function, true);
    worker.action(ShortcutAction::StartPause);
    worker.stop(); // Close while the passive grab is still actively holding F7.
    assert_other_client_receives_typing(&client, ordinary, focus);
    inject(&client, function, false);
    inject(&client, shift, false);
    inject(&client, control, false);
    client
        .grab_key(
            false,
            client.setup().roots[0].root,
            ModMask::CONTROL | ModMask::SHIFT,
            function,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .check()
        .unwrap();
}

fn assert_other_client_receives_typing(client: &RustConnection, key: u8, focus: u32) {
    inject(client, key, true);
    inject(client, key, false);
    client.sync().unwrap();
    let mut seen = false;
    while let Some(event) = client.poll_for_event().unwrap() {
        if let Event::KeyPress(event) = event {
            seen |= event.detail == key && event.event == focus;
        }
    }
    assert!(
        seen,
        "unregistered typing must reach the other focused client, including after active-grab teardown"
    );
}

#[test]
#[ignore = "spawns its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_conflict_rolls_back_partial_grabs_without_releasing_other_clients() {
    let server = Xvfb::start();
    let owner = server.client();
    let root = owner.setup().roots[0].root;
    let first = code(&owner, 0xffc4);
    let conflict = code(&owner, 0xffc5);
    let modifiers = ModMask::CONTROL | ModMask::SHIFT;
    owner
        .grab_key(
            false,
            root,
            modifiers,
            conflict,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .check()
        .unwrap();
    let mut worker = Worker::start(
        server.display.clone(),
        super::super::default_shortcut_bindings(),
    );
    let result = worker.result.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(result.unwrap_err().contains("conflict"));
    worker.join.take().unwrap().join().unwrap();
    let third = server.client();
    third
        .grab_key(
            false,
            root,
            modifiers,
            first,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .check()
        .unwrap();
    assert!(
        third
            .grab_key(
                false,
                root,
                modifiers,
                conflict,
                GrabMode::ASYNC,
                GrabMode::ASYNC
            )
            .unwrap()
            .check()
            .is_err(),
        "foreign client's conflicting grab must survive rollback"
    );
}

#[test]
#[ignore = "spawns its own private Xvfb; never uses host DISPLAY"]
fn private_xvfb_mapping_change_stops_and_releases_registration() {
    let server = Xvfb::start();
    let control = server.client();
    let mut worker = Worker::start(
        server.display.clone(),
        super::super::default_shortcut_bindings(),
    );
    worker.active();
    let key = code(&control, 0xffc4);
    let original = control
        .get_keyboard_mapping(key, 1)
        .unwrap()
        .reply()
        .unwrap();
    control
        .change_keyboard_mapping(1, key, original.keysyms_per_keycode, &original.keysyms)
        .unwrap()
        .check()
        .unwrap();
    let result = worker.result.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(result.unwrap_err().contains("mapping changed"));
    worker.join.take().unwrap().join().unwrap();
    control
        .grab_key(
            false,
            control.setup().roots[0].root,
            ModMask::CONTROL | ModMask::SHIFT,
            key,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .check()
        .unwrap();
    assert!(worker.drain().is_empty());
}

#[test]
fn no_wildcard_grab_values_are_generated_from_defaults() {
    for binding in super::super::default_shortcut_bindings() {
        assert!(binding.trigger.control);
        assert!(binding.trigger.shift);
        assert!(!binding.trigger.alt);
    }
}

#[test]
fn repeated_press_edges_require_a_release_and_never_collect_unbound_keys() {
    let grabs = [
        Grab {
            code: 73,
            modifiers: 5,
            action: ShortcutAction::StartPause,
        },
        Grab {
            code: 74,
            modifiers: 5,
            action: ShortcutAction::Stop,
        },
    ];
    let mut state = PressEdges::default();
    assert_eq!(state.press(73, 1, &grabs), None);
    assert_eq!(state.press(73, 5, &grabs), Some(ShortcutAction::StartPause));
    for _ in 0..100 {
        assert_eq!(state.press(73, 5, &grabs), None);
    }
    assert_eq!(state.press(74, 5, &grabs), Some(ShortcutAction::Stop));
    assert_eq!(state.press(38, 0, &grabs), None);
    assert_eq!(state.release(38), None);
    assert_eq!(state.release(73), Some(ShortcutAction::StartPause));
    assert_eq!(state.release(73), None);
    assert_eq!(state.press(73, 5, &grabs), Some(ShortcutAction::StartPause));
}
