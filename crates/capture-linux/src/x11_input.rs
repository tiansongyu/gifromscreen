//! Explicitly opted-in XI2 raw events. One connection/thread exists only while recording.
//! No grabs, evdev devices, periodic keymap polling, or process-wide listeners are used.
use gif_from_screen_capture::{
    ButtonState, CaptureError, CaptureErrorKind, CaptureTimestamp, InputEvent, KeyState,
    PhysicalPosition, PhysicalRect, PointerButton, RecoveryHint,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use x11rb::{
    connection::{Connection, RequestConnection},
    protocol::{
        Event,
        xinput::{self, ConnectionExt as _, EventMask, KeyEventFlags, XIEventMask},
        xproto::ConnectionExt as _,
    },
    rust_connection::RustConnection,
};

const EVENT_LIMIT: usize = 512;

pub(super) fn available(connection: &RustConnection) -> bool {
    connection
        .extension_information(xinput::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_some()
        && connection
            .xinput_xi_query_version(2, 2)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|reply| reply.major_version >= 2)
}

fn error(operation: &str, cause: impl std::fmt::Display) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::Platform,
        format!("X11 input {operation}: {cause}"),
        RecoveryHint::Retry,
    )
}

#[derive(Default)]
struct EventQueue {
    events: VecDeque<InputEvent>,
    dropped: u32,
    failure: Option<String>,
}

impl EventQueue {
    fn push(&mut self, event: InputEvent) {
        if self.events.len() == EVENT_LIMIT {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(event);
    }
}

pub(super) struct InputRecorder {
    connection: Arc<RustConnection>,
    stop: Arc<AtomicBool>,
    queue: Arc<Mutex<EventQueue>>,
    worker: Option<JoinHandle<()>>,
}

impl InputRecorder {
    pub(super) fn start(
        display: Option<&str>,
        session_started: Instant,
        paused: Duration,
    ) -> Result<Self, CaptureError> {
        let (connection, screen) =
            x11rb::connect(display).map_err(|cause| error("connect", cause))?;
        if !available(&connection) {
            return Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                "passive input recording requires the XInput 2 extension",
                RecoveryHint::ChangeRequest,
            ));
        }
        let root = connection.setup().roots[screen].root;
        let keys = KeyLabels::read(&connection)?;
        connection
            .xinput_xi_select_events(
                root,
                &[EventMask {
                    // XIAllMasterDevices prevents receiving both master and slave duplicates.
                    deviceid: 1,
                    mask: vec![
                        XIEventMask::RAW_KEY_PRESS
                            | XIEventMask::RAW_KEY_RELEASE
                            | XIEventMask::RAW_BUTTON_PRESS
                            | XIEventMask::RAW_BUTTON_RELEASE,
                    ],
                }],
            )
            .map_err(|cause| error("subscribe", cause))?
            .check()
            .map_err(|cause| error("subscribe", cause))?;
        connection.flush().map_err(|cause| error("flush", cause))?;
        let queue = Arc::new(Mutex::new(EventQueue::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_queue = Arc::clone(&queue);
        let thread_stop = Arc::clone(&stop);
        let connection = Arc::new(connection);
        let worker_connection = Arc::clone(&connection);
        let worker = thread::Builder::new()
            .name("gfs-x11-input".into())
            .spawn(move || {
                if let Err(cause) = run(
                    &worker_connection,
                    root,
                    keys,
                    session_started,
                    paused,
                    &thread_stop,
                    &thread_queue,
                ) {
                    thread_queue
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .failure = Some(cause.to_string());
                }
                // Dropping the private X connection removes every event subscription.
            })
            .map_err(|cause| error("start worker", cause))?;
        Ok(Self {
            connection,
            stop,
            queue,
            worker: Some(worker),
        })
    }

    pub(super) fn drain(
        &self,
        region: PhysicalRect,
        captured_at: CaptureTimestamp,
    ) -> Result<(Vec<InputEvent>, u32), CaptureError> {
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(failure) = &queue.failure {
            return Err(error("worker stopped", failure));
        }
        let mut events = Vec::with_capacity(queue.events.len());
        while queue
            .events
            .front()
            .is_some_and(|event| event_time(event) <= captured_at.as_micros())
        {
            let mut event = queue.events.pop_front().expect("front exists");
            if let InputEvent::PointerButton { position, .. } = &mut event {
                *position = position.and_then(|point| local_position(point, region));
            }
            events.push(event);
        }
        let dropped = std::mem::take(&mut queue.dropped);
        Ok((events, dropped))
    }
}

impl Drop for InputRecorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // A stalled X server must not keep pause/stop waiting for QueryPointer.
        // Shutting down only this private socket also removes all subscriptions.
        let _ = rustix::net::shutdown(self.connection.stream(), rustix::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn event_time(event: &InputEvent) -> u64 {
    match event {
        InputEvent::Key { at, .. } | InputEvent::PointerButton { at, .. } => at.as_micros(),
        _ => 0,
    }
}

fn local_position(point: PhysicalPosition, region: PhysicalRect) -> Option<PhysicalPosition> {
    let x = i64::from(point.x) - i64::from(region.origin().x);
    let y = i64::from(point.y) - i64::from(region.origin().y);
    (x >= 0
        && y >= 0
        && x < i64::from(region.size().width())
        && y < i64::from(region.size().height()))
    .then_some(PhysicalPosition {
        x: i32::try_from(x).ok()?,
        y: i32::try_from(y).ok()?,
    })
}

// Server milliseconds preserve inter-event spacing and wrap safely. A new anchor
// after resume deliberately excludes paused wall time. Receipt time caps future drift.
#[derive(Default)]
struct EventClock {
    anchor: Option<(u32, u64)>,
    last_us: u64,
}
impl EventClock {
    fn at(&mut self, server_ms: u32, active_us: u64) -> CaptureTimestamp {
        let (server_anchor, active_anchor) = *self.anchor.get_or_insert((server_ms, active_us));
        let at = active_anchor
            .saturating_add(u64::from(server_ms.wrapping_sub(server_anchor)) * 1000)
            .min(active_us)
            .max(self.last_us);
        self.last_us = at;
        CaptureTimestamp::from_micros(at)
    }
}

fn run(
    connection: &RustConnection,
    root: u32,
    mut keys: KeyLabels,
    started: Instant,
    paused: Duration,
    stop: &AtomicBool,
    queue: &Mutex<EventQueue>,
) -> Result<(), CaptureError> {
    let mut clock = EventClock::default();
    while !stop.load(Ordering::Acquire) {
        // Socket shutdown in Drop interrupts this wait (and a pointer reply).
        // No polling timer wakes an idle recording or adds dispatch latency.
        let event = connection
            .wait_for_event()
            .map_err(|cause| error("wait", cause))?;
        let active_us =
            u64::try_from(started.elapsed().saturating_sub(paused).as_micros()).unwrap_or(u64::MAX);
        let event = match event {
            Event::XinputRawKeyPress(event) | Event::XinputRawKeyRelease(event) => {
                let pressed = event.event_type == xinput::RAW_KEY_PRESS_EVENT;
                let repeat = event.flags.contains(KeyEventFlags::KEY_REPEAT);
                let modifiers = keys.update(event.detail, pressed);
                InputEvent::Key {
                    at: clock.at(event.time, active_us),
                    native_code: event.detail,
                    text: keys.label(event.detail, modifiers),
                    state: if pressed {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    },
                    repeat,
                    modifiers,
                }
            }
            Event::XinputRawButtonPress(event) | Event::XinputRawButtonRelease(event) => {
                // XI2 raw button events have no root coordinates. Query at dispatch (not
                // at the next screenshot) and expose this precision limit in capabilities.
                let pointer = connection
                    .query_pointer(root)
                    .map_err(|cause| error("pointer", cause))?
                    .reply()
                    .map_err(|cause| error("pointer", cause))?;
                let position = pointer.same_screen.then_some(PhysicalPosition {
                    x: i32::from(pointer.root_x),
                    y: i32::from(pointer.root_y),
                });
                let button = match event.detail {
                    1 => PointerButton::Primary,
                    2 => PointerButton::Middle,
                    3 => PointerButton::Secondary,
                    value => PointerButton::Other(u16::try_from(value).unwrap_or(u16::MAX)),
                };
                InputEvent::PointerButton {
                    at: clock.at(event.time, active_us),
                    button,
                    state: if event.event_type == xinput::RAW_BUTTON_PRESS_EVENT {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                    position,
                }
            }
            Event::MappingNotify(_) => {
                keys = KeyLabels::read(connection)?;
                continue;
            }
            _ => continue,
        };
        if !stop.load(Ordering::Acquire) {
            queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(event);
        }
    }
    Ok(())
}

struct KeyLabels {
    first: u8,
    columns: usize,
    symbols: Vec<u32>,
    held: [bool; 256],
    modifier: [u8; 256],
}
impl KeyLabels {
    fn read(connection: &RustConnection) -> Result<Self, CaptureError> {
        let setup = connection.setup();
        let first = setup.min_keycode;
        let count = setup.max_keycode.saturating_sub(first).saturating_add(1);
        let mapping = connection
            .get_keyboard_mapping(first, count)
            .map_err(|cause| error("keymap", cause))?
            .reply()
            .map_err(|cause| error("keymap", cause))?;
        let mut labels = Self {
            first,
            columns: usize::from(mapping.keysyms_per_keycode),
            symbols: mapping.keysyms,
            held: [false; 256],
            modifier: [0; 256],
        };
        // One state snapshot seeds modifiers already held at start/resume. This is
        // not a polling input recorder: every subsequent transition is an XI2 event.
        let state = connection
            .query_keymap()
            .map_err(|cause| error("initial modifiers", cause))?
            .reply()
            .map_err(|cause| error("initial modifiers", cause))?;
        for code in 0..256 {
            labels.held[code] = state.keys[code / 8] & (1 << (code % 8)) != 0;
        }
        for code in u32::from(first)..=u32::from(setup.max_keycode) {
            labels.modifier[code as usize] = match labels.symbol(code, false) {
                0xffe1 | 0xffe2 => 1,
                0xffe3 | 0xffe4 => 2,
                0xffe9 | 0xffea => 4,
                0xffeb | 0xffec => 8,
                _ => 0,
            };
        }
        Ok(labels)
    }
    fn symbol(&self, code: u32, shift: bool) -> u32 {
        let Some(index) = code
            .checked_sub(u32::from(self.first))
            .map(|code| code as usize * self.columns)
        else {
            return 0;
        };
        self.symbols
            .get(index + usize::from(shift && self.columns > 1))
            .copied()
            .filter(|value| *value != 0)
            .or_else(|| self.symbols.get(index).copied())
            .unwrap_or(0)
    }
    fn update(&mut self, code: u32, pressed: bool) -> u8 {
        if let Some(held) = self.held.get_mut(code as usize) {
            *held = pressed;
        }
        self.held
            .iter()
            .zip(self.modifier)
            .filter(|(held, _)| **held)
            .fold(0, |bits, (_, modifier)| bits | modifier)
    }
    fn label(&self, code: u32, modifiers: u8) -> Option<String> {
        let symbol = xkeysym::Keysym::new(self.symbol(code, modifiers & 1 != 0));
        let label = symbol
            .key_char()
            .filter(|character| !character.is_control())
            .map(|character| {
                if character == ' ' {
                    "Space".to_owned()
                } else {
                    character.to_string()
                }
            })
            .or_else(|| {
                symbol.name().map(|name| {
                    match name
                        .strip_prefix("XK_")
                        .or_else(|| name.strip_prefix("XF86XK_"))
                        .unwrap_or(name)
                    {
                        "Return" => "Enter",
                        "BackSpace" => "Backspace",
                        "Prior" => "PageUp",
                        "Next" => "PageDown",
                        "Shift_L" | "Shift_R" => "Shift",
                        "Control_L" | "Control_R" => "Ctrl",
                        "Alt_L" | "Alt_R" => "Alt",
                        "Super_L" | "Super_R" => "Super",
                        "ISO_Left_Tab" => "Tab",
                        other => other,
                    }
                    .to_owned()
                })
            })?;
        let mut parts = Vec::new();
        let own_modifier = self.modifier.get(code as usize).copied().unwrap_or(0);
        for (bit, label) in [(2, "Ctrl"), (4, "Alt"), (1, "Shift"), (8, "Super")] {
            if modifiers & bit != 0 && own_modifier != bit {
                parts.push(label.to_owned());
            }
        }
        parts.push(label);
        Some(parts.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queue_is_bounded_and_reports_loss() {
        let mut queue = EventQueue::default();
        for index in 0..EVENT_LIMIT + 3 {
            queue.push(InputEvent::Key {
                at: CaptureTimestamp::from_micros(index as u64),
                native_code: 38,
                text: None,
                state: KeyState::Pressed,
                repeat: false,
                modifiers: 0,
            });
        }
        assert_eq!(queue.events.len(), EVENT_LIMIT);
        assert_eq!(queue.dropped, 3);
        assert_eq!(event_time(queue.events.front().unwrap()), 3);
    }

    #[test]
    fn key_labels_keep_space_visible_and_do_not_duplicate_modifier_names() {
        let mut labels = KeyLabels {
            first: 38,
            columns: 1,
            symbols: vec![0x20, 0xffe3],
            held: [false; 256],
            modifier: [0; 256],
        };
        labels.modifier[39] = 2;
        assert_eq!(labels.label(38, 0).as_deref(), Some("Space"));
        assert_eq!(labels.label(39, 2).as_deref(), Some("Ctrl"));
        assert_eq!(labels.label(38, 2).as_deref(), Some("Ctrl+Space"));
    }
    #[test]
    fn server_time_wrap_and_resume_anchor_preserve_active_clock() {
        let mut clock = EventClock::default();
        assert_eq!(clock.at(u32::MAX - 1, 10_000).as_micros(), 10_000);
        assert_eq!(clock.at(1, 15_000).as_micros(), 13_000);
        let mut resumed = EventClock::default();
        assert_eq!(resumed.at(500_000, 20_000).as_micros(), 20_000);
    }
    #[test]
    fn retarget_translation_is_local_and_outside_clicks_are_not_clamped() {
        let point = PhysicalPosition { x: 110, y: 70 };
        assert_eq!(
            local_position(point, PhysicalRect::new(100, 50, 30, 30).unwrap()),
            Some(PhysicalPosition { x: 10, y: 20 })
        );
        assert_eq!(
            local_position(point, PhysicalRect::new(200, 50, 30, 30).unwrap()),
            None
        );
    }
}
