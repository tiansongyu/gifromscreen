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
pub(crate) type OwnerCheck<'a> = Option<&'a dyn Fn(&Event) -> Result<bool, String>>;

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
    run_on_connection(
        &connection,
        screen,
        bounds,
        cancel,
        deadline,
        display,
        x11rb::CURRENT_TIME,
        None,
        None,
        || Ok(()),
    )
}

/// A native drag button already owns the initiating passive grab. Replacing it
/// on this SAME connection avoids an ungrab/regrab gap and lost fast releases.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_on_connection(
    connection: &Client<'_>,
    screen: usize,
    bounds: WindowSnapBounds,
    cancel: &AtomicBool,
    deadline: Instant,
    display: Option<&str>,
    time: u32,
    activation_child: Option<Window>,
    owner_check: OwnerCheck<'_>,
    on_grab: impl FnOnce() -> Result<(), String>,
) -> Result<Option<PickedWindow>, String> {
    connection.stream().begin_operation_until(deadline);
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or("The X11 screen is unavailable")?
        .root;
    let cursor = crosshair(connection)?;
    let request = connection
        .grab_pointer(
            false,
            root,
            EventMask::POINTER_MOTION | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            cursor,
            time,
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
    let result = on_grab().and_then(|()| {
        select(
            connection,
            screen,
            root,
            event_floor,
            bounds,
            cancel,
            deadline,
            display,
            activation_child,
            owner_check,
        )
    });
    connection.stream().begin_cleanup();
    let released = connection
        .ungrab_pointer(x11rb::CURRENT_TIME)
        .map_err(error)?
        .check()
        .map_err(error);
    let _ = connection.free_cursor(cursor);
    released?;
    result
}

pub(super) fn crosshair(connection: &Client<'_>) -> Result<u32, String> {
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
    activation_child: Option<Window>,
    owner_check: OwnerCheck<'_>,
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
        pressed: activation_child.is_some(),
        activation_child,
        owner_check,
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
    activation_child: Option<Window>,
    owner_check: OwnerCheck<'a>,
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
                if let Some(check) = self.owner_check
                    && check(&event)?
                {
                    return Err(
                        "The drag handle's owner closed or changed during selection.".into(),
                    );
                }
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
                        if self.activation_child.take() == Some(event.child) {
                            // Releasing over the originating GUI branch arms
                            // click-to-pick. Frozen input replay can report later
                            // root coordinates, so this is intentionally a window
                            // identity boundary, not the old small button rectangle.
                            self.pressed = false;
                            continue;
                        }
                        // Preserve the release's routed target identity. On Xorg
                        // frozen bursts, root/event coordinates can already name
                        // a subsequent motion although child still names the
                        // release target. Freshly revalidate that window itself;
                        // never pick from a later QueryPointer location.
                        return self.target(event.child, None, true);
                    }
                    _ => {}
                }
            }
            self.update_hover(guide)?;
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn update_hover(&mut self, guide: &mut RecorderGuide) -> Result<(), String> {
        if self
            .last_hover
            .is_some_and(|at| at.elapsed() < HOVER_INTERVAL)
        {
            return Ok(());
        }
        let pointer = self
            .connection
            .query_pointer(self.root)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        let candidate = if pointer.same_screen {
            self.target(
                pointer.child,
                Some(PhysicalPosition {
                    x: i32::from(pointer.root_x),
                    y: i32::from(pointer.root_y),
                }),
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
                handle_scale: 100,
                handle_avoid: None,
            })?;
            self.shown = region;
        }
        self.last_hover = Some(Instant::now());
        Ok(())
    }

    fn target(
        &mut self,
        child: Window,
        hover_position: Option<PhysicalPosition>,
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
        if let Some(position) = hover_position {
            let outer = rectangle(self.connection, child, self.root, true)?;
            if !contains(outer, position) {
                return Ok(None);
            }
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
