//! An explicit, bounded pointer selection. No keyboard grab or persistent input feed.

use super::{
    PickedWindow, WindowSnapBounds, catalog,
    native::{Atoms, observe},
};
use crate::{
    GuideRequest, GuideStatus, RecorderGuide,
    shortcuts::x11::{Client, stream},
    x11_window::{rectangle, root_child},
};
use gif_from_screen_capture::{PhysicalPosition, PhysicalRect};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xproto::{ConnectionExt as _, EventMask, GrabMode, GrabStatus, Window},
    },
};

const HOVER_INTERVAL: Duration = Duration::from_millis(40);

pub(crate) fn run(
    display: Option<&str>,
    bounds: WindowSnapBounds,
    cancel: &AtomicBool,
    lifetime: Duration,
) -> Result<Option<PickedWindow>, String> {
    let deadline = Instant::now()
        .checked_add(lifetime)
        .ok_or("Window selection lifetime is invalid")?;
    let screen = usize::from(
        x11rb_protocol::parse_display::parse_display(display)
            .map_err(error)?
            .screen,
    );
    let connection = stream::connect(display, cancel)?;
    connection.stream().begin_operation_until(deadline);
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or("The X11 screen is unavailable")?
        .root;
    let cursor = crosshair(&connection)?;
    let request = connection
        .grab_pointer(
            false,
            root,
            EventMask::POINTER_MOTION | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            cursor,
            x11rb::CURRENT_TIME,
        )
        .map_err(error)?;
    let event_floor = request.sequence_number();
    let reply = request.reply().map_err(error)?;
    if reply.status != GrabStatus::SUCCESS {
        return Err(format!(
            "Window selection could not acquire the pointer ({:?}). Finish the other gesture and retry.",
            reply.status
        ));
    }
    let result = select(
        &connection,
        screen,
        root,
        event_floor,
        bounds,
        cancel,
        deadline,
        display,
    );
    connection.stream().begin_cleanup();
    let released = connection
        .ungrab_pointer(x11rb::CURRENT_TIME)
        .map_err(error)?
        .check()
        .map_err(error);
    drop(connection); // Also releases the cursor and any grab on every error path.
    released?;
    result
}

fn crosshair(connection: &Client<'_>) -> Result<u32, String> {
    let font = connection.generate_id().map_err(error)?;
    let cursor = connection.generate_id().map_err(error)?;
    connection
        .open_font(font, b"cursor")
        .map_err(error)?
        .check()
        .map_err(error)?;
    // X11 cursor-font XC_crosshair glyph and its adjacent mask glyph.
    connection
        .create_glyph_cursor(
            cursor,
            font,
            font,
            34,
            35,
            0,
            0,
            0,
            u16::MAX,
            u16::MAX,
            u16::MAX,
        )
        .map_err(error)?
        .check()
        .map_err(error)?;
    connection
        .close_font(font)
        .map_err(error)?
        .check()
        .map_err(error)?;
    Ok(cursor)
}

#[allow(clippy::too_many_arguments)]
fn select(
    connection: &Client<'_>,
    screen: usize,
    root: Window,
    event_floor: u64,
    bounds: WindowSnapBounds,
    cancel: &AtomicBool,
    deadline: Instant,
    display: Option<&str>,
) -> Result<Option<PickedWindow>, String> {
    let atoms = Atoms::new(connection)?;
    let mut guide = RecorderGuide::start_passive(display.map(str::to_owned))?;
    let mut state = Selection {
        connection,
        screen,
        root,
        event_floor,
        bounds,
        atoms,
        locator: Locator::default(),
        pressed: false,
        last_hover: None,
        shown: None,
        generation: 0,
    };
    let result = state.drive(&mut guide, cancel, deadline);
    guide.stop();
    let cleanup_deadline = Instant::now() + Duration::from_secs(2);
    while guide.is_running() {
        guide.poll();
        if Instant::now() >= cleanup_deadline {
            return Err(
                "Window highlight cleanup did not finish. Selection was not applied.".into(),
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    result
}

struct Selection<'a, 'b> {
    connection: &'a Client<'b>,
    screen: usize,
    root: Window,
    event_floor: u64,
    bounds: WindowSnapBounds,
    atoms: Atoms,
    locator: Locator,
    pressed: bool,
    last_hover: Option<Instant>,
    shown: Option<PhysicalRect>,
    generation: u64,
}

impl Selection<'_, '_> {
    fn drive(
        &mut self,
        guide: &mut RecorderGuide,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<Option<PickedWindow>, String> {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("Window selection cancelled.".into());
            }
            if Instant::now() >= deadline {
                return Err("Window selection timed out; original region kept.".into());
            }
            self.connection.stream().begin_operation_until(deadline);
            if let GuideStatus::Failed(error) = guide.poll().status {
                return Err(error);
            }
            for _ in 0..128 {
                if cancel.load(Ordering::Acquire) {
                    return Err("Window selection cancelled.".into());
                }
                let Some((event, sequence)) = self
                    .connection
                    .poll_for_event_with_sequence()
                    .map_err(error)?
                else {
                    break;
                };
                if sequence < self.event_floor {
                    continue;
                }
                match event {
                    Event::ButtonPress(event)
                        if event.response_type & 0x80 == 0 && event.root == self.root =>
                    {
                        if event.detail == 3 {
                            return Ok(None);
                        }
                        if event.detail == 1 {
                            self.pressed = true;
                        }
                    }
                    Event::ButtonRelease(event)
                        if event.response_type & 0x80 == 0
                            && event.root == self.root
                            && event.detail == 1
                            && self.pressed =>
                    {
                        // Use the release event's child/coordinates, never a later
                        // QueryPointer position after the user has moved again.
                        return self.target(
                            event.child,
                            PhysicalPosition {
                                x: i32::from(event.root_x),
                                y: i32::from(event.root_y),
                            },
                            true,
                        );
                    }
                    _ => {}
                }
            }
            if self
                .last_hover
                .is_none_or(|at| at.elapsed() >= HOVER_INTERVAL)
            {
                let pointer = self
                    .connection
                    .query_pointer(self.root)
                    .map_err(error)?
                    .reply()
                    .map_err(error)?;
                let candidate = if pointer.same_screen {
                    self.target(
                        pointer.child,
                        PhysicalPosition {
                            x: i32::from(pointer.root_x),
                            y: i32::from(pointer.root_y),
                        },
                        false,
                    )
                    .ok()
                    .flatten()
                } else {
                    None
                };
                let region = candidate.map(|candidate| candidate.region);
                if region != self.shown {
                    self.generation = self
                        .generation
                        .checked_add(1)
                        .ok_or("Window highlight generation overflow")?;
                    guide.request(GuideRequest {
                        generation: self.generation,
                        region,
                        protected_region: None,
                        border_width: 4,
                    })?;
                    self.shown = region;
                }
                self.last_hover = Some(Instant::now());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn target(
        &mut self,
        child: Window,
        position: PhysicalPosition,
        fresh: bool,
    ) -> Result<Option<PickedWindow>, String> {
        let Some(window) = self
            .locator
            .find(self.connection, self.root, child, fresh)?
        else {
            return Ok(None);
        };
        if root_child(self.connection, window, self.root)? != child {
            return Ok(None);
        }
        let outer = rectangle(self.connection, child, self.root, true)?;
        if !contains(outer, position) {
            return Ok(None);
        }
        observe(
            self.connection,
            self.screen,
            window,
            self.bounds,
            &self.atoms,
            None,
        )
        .map(Some)
    }
}

#[derive(Default)]
struct Locator {
    child: Window,
    client: Option<Window>,
    sampled: Option<Instant>,
}

impl Locator {
    fn find(
        &mut self,
        connection: &Client<'_>,
        root: Window,
        child: Window,
        fresh: bool,
    ) -> Result<Option<Window>, String> {
        if child == x11rb::NONE || child == root {
            return Ok(None);
        }
        if !fresh
            && self.child == child
            && (self.client.is_some()
                || self
                    .sampled
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(250)))
        {
            return Ok(self.client);
        }
        let (windows, _) = catalog::window_ids(connection, root)?;
        self.child = child;
        self.client = windows
            .into_iter()
            .find(|window| root_child(connection, *window, root).ok() == Some(child));
        self.sampled = Some(Instant::now());
        Ok(self.client)
    }
}

fn contains(rect: PhysicalRect, point: PhysicalPosition) -> bool {
    let x = i64::from(point.x) - i64::from(rect.origin().x);
    let y = i64::from(point.y) - i64::from(rect.origin().y);
    x >= 0 && y >= 0 && x < i64::from(rect.size().width()) && y < i64::from(rect.size().height())
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
