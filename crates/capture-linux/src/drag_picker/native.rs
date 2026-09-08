//! A synchronous passive grab exists only on this owned input child. Valid
//! presses transition to the root picker on the same connection; stale layout
//! presses are replayed with the child input shape empty.

use super::{Context, DragPickerRequest, DragPickerSelection};
use crate::{
    shortcuts::x11::{Client, stream},
    window_snap::picker,
    x11_window::rectangle,
};
use gif_from_screen_capture::PhysicalRect;
use std::{
    sync::{PoisonError, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        shape::SK,
        xfixes::ConnectionExt as _,
        xproto::{
            Allow, Atom, AtomEnum, ButtonIndex, ButtonPressEvent, ChangeWindowAttributesAux,
            ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, MapState, ModMask,
            Rectangle, Window, WindowClass,
        },
    },
};

pub(super) fn run(
    display: Option<&str>,
    parent: Window,
    pid: u32,
    lifetime: Duration,
    context: &Context,
) -> Result<Option<DragPickerSelection>, String> {
    let connection = stream::connect(display, &context.cancel)?;
    let screen = usize::from(
        x11rb_protocol::parse_display::parse_display(display)
            .map_err(error)?
            .screen,
    );
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or("The selected X11 screen is unavailable")?
        .root;
    if parent == root {
        return Err("A drag handle cannot target the root window.".into());
    }
    connection
        .xfixes_query_version(5, 0)
        .map_err(error)?
        .reply()
        .map_err(error)?;
    let guard = Parent::new(&connection, parent, pid)?;
    guard.inspect(&connection, root)?;
    connection
        .change_window_attributes(
            parent,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE),
        )
        .map_err(error)?
        .check()
        .map_err(error)?;
    let mut owned = InputChild::new(&connection, parent, root, screen, guard)?;
    connection.stream().registration_complete();
    let result = owned.drive(display, lifetime, context);
    drop(owned);
    drop(connection);
    context
        .shared
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .ready = None;
    result
}

struct Parent {
    window: Window,
    pid: u32,
    pid_atom: Atom,
    state: Atom,
    hidden: Atom,
}

impl Parent {
    fn new(connection: &Client<'_>, window: Window, pid: u32) -> Result<Self, String> {
        let atom = |name: &[u8]| {
            connection
                .intern_atom(true, name)
                .map_err(error)?
                .reply()
                .map(|reply| reply.atom)
                .map_err(error)
        };
        let pid_atom = atom(b"_NET_WM_PID")?;
        if pid_atom == 0 {
            return Err("The drag handle's parent has no verifiable PID.".into());
        }
        Ok(Self {
            window,
            pid,
            pid_atom,
            state: atom(b"_NET_WM_STATE")?,
            hidden: atom(b"_NET_WM_STATE_HIDDEN")?,
        })
    }

    fn inspect(
        &self,
        connection: &Client<'_>,
        root: Window,
    ) -> Result<Option<PhysicalRect>, String> {
        self.check_pid(connection)?;
        let attributes = connection
            .get_window_attributes(self.window)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if attributes.class != WindowClass::INPUT_OUTPUT {
            return Err("The drag handle's parent must be an InputOutput GUI window.".into());
        }
        if attributes.map_state != MapState::VIEWABLE {
            return Ok(None);
        }
        if self.state != 0 && self.hidden != 0 {
            let state = connection
                .get_property(false, self.window, self.state, AtomEnum::ATOM, 0, 64)
                .map_err(error)?
                .reply()
                .map_err(error)?;
            if state.bytes_after != 0
                || (state.type_ != 0
                    && (state.type_ != u32::from(AtomEnum::ATOM) || state.format != 32))
            {
                return Err(
                    "The drag handle parent's state property is malformed or too large.".into(),
                );
            }
            if state
                .value32()
                .is_some_and(|mut values| values.any(|value| value == self.hidden))
            {
                return Ok(None);
            }
        }
        rectangle(connection, self.window, root, false).map(Some)
    }

    fn check_pid(&self, connection: &Client<'_>) -> Result<(), String> {
        let property = connection
            .get_property(false, self.window, self.pid_atom, AtomEnum::CARDINAL, 0, 2)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if property.type_ != u32::from(AtomEnum::CARDINAL)
            || property.format != 32
            || property.value_len != 1
            || property.bytes_after != 0
            || property.value32().and_then(|mut values| values.next()) != Some(self.pid)
        {
            return Err("The drag handle's parent disappeared or changed ownership.".into());
        }
        Ok(())
    }

    fn invalidates_layout(&self, event: &Event) -> bool {
        match event {
            Event::ConfigureNotify(event) => event.window == self.window,
            Event::MapNotify(event) => event.window == self.window,
            Event::UnmapNotify(event) => event.window == self.window,
            Event::PropertyNotify(event) => {
                event.window == self.window
                    && (event.atom == self.pid_atom || event.atom == self.state)
            }
            _ => false,
        }
    }
}

enum Press {
    Accept(DragPickerRequest),
    Replay,
    Cancel,
}

struct InputChild<'a, 'b> {
    connection: &'a Client<'b>,
    id: Window,
    root: Window,
    screen: usize,
    parent: Parent,
    applied: Option<DragPickerRequest>,
    event_floor: u64,
}

impl<'a, 'b> InputChild<'a, 'b> {
    fn new(
        connection: &'a Client<'b>,
        parent: Window,
        root: Window,
        screen: usize,
        guard: Parent,
    ) -> Result<Self, String> {
        let id = connection.generate_id().map_err(error)?;
        connection
            .create_window(
                0,
                id,
                parent,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
            )
            .map_err(error)?
            .check()
            .map_err(error)?;
        // RAII owns the child before any subsequent setup can fail.
        let owned = Self {
            connection,
            id,
            root,
            screen,
            parent: guard,
            applied: None,
            event_floor: 0,
        };
        owned.shape(None)?;
        connection
            .grab_button(
                false,
                id,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                x11rb::protocol::xproto::GrabMode::SYNC,
                x11rb::protocol::xproto::GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                ButtonIndex::M1,
                ModMask::ANY,
            )
            .map_err(error)?
            .check()
            .map_err(error)?;
        connection
            .map_window(id)
            .map_err(error)?
            .check()
            .map_err(error)?;
        Ok(owned)
    }

    fn drive(
        &mut self,
        display: Option<&str>,
        lifetime: Duration,
        context: &Context,
    ) -> Result<Option<DragPickerSelection>, String> {
        while !context.cancel.load(Ordering::Acquire) {
            self.connection.stream().begin_operation();
            let desired = context
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .request;
            if desired != self.applied {
                self.apply(desired, context)?;
            }
            for _ in 0..128 {
                if context.cancel.load(Ordering::Acquire) {
                    break;
                }
                let Some((event, sequence)) = self
                    .connection
                    .poll_for_event_with_sequence()
                    .map_err(error)?
                else {
                    break;
                };
                if self.parent.invalidates_layout(&event) {
                    self.disable(context)?;
                    continue;
                }
                match event {
                    Event::ButtonPress(event)
                        if event.response_type & 0x80 == 0
                            && event.event == self.id
                            && event.detail == 1 =>
                    {
                        match self.valid_press(&event, sequence, context)? {
                            Press::Accept(request) => {
                                return self
                                    .select(request, &event, display, lifetime, context)
                                    .map(Some);
                            }
                            Press::Replay => {
                                self.disable(context)?;
                                if context.cancel.load(Ordering::Acquire)
                                    || self
                                        .parent
                                        .inspect(self.connection, self.root)?
                                        .is_none_or(|parent| !parent_contains(parent, &event))
                                {
                                    return Ok(None);
                                }
                                self.replay(event.time)?;
                            }
                            Press::Cancel => return Ok(None),
                        }
                    }
                    Event::DestroyNotify(event)
                        if event.window == self.id || event.window == self.parent.window =>
                    {
                        return Err("The drag handle or its parent closed.".into());
                    }
                    _ => {}
                }
            }
            self.connection.stream().registration_complete();
            thread::sleep(Duration::from_millis(5));
        }
        Ok(None)
    }

    fn disable(&mut self, context: &Context) -> Result<(), String> {
        context
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ready = None;
        self.shape(None)?;
        self.applied = None;
        Ok(())
    }

    fn select(
        &self,
        request: DragPickerRequest,
        event: &ButtonPressEvent,
        display: Option<&str>,
        lifetime: Duration,
        context: &Context,
    ) -> Result<DragPickerSelection, String> {
        request
            .rect
            .ok_or("The claimed drag handle has no rectangle")?;
        // Capture the verified originating GUI branch before the successful
        // handoff permits the UI to hide. Do not infer it from a later pointer.
        let activation =
            crate::x11_window::root_child(self.connection, self.parent.window, self.root)?;
        self.shape(None)?; // Keep the child viewable: its synchronous grab is still active.
        let owner_check = |event: &Event| -> Result<bool, String> {
            match event {
                Event::DestroyNotify(event) => {
                    Ok(event.window == self.id || event.window == self.parent.window)
                }
                Event::PropertyNotify(event)
                    if event.window == self.parent.window && event.atom == self.parent.pid_atom =>
                {
                    self.parent.check_pid(self.connection)?;
                    Ok(false)
                }
                _ => Ok(false), // Hiding the GUI after successful handoff is permitted.
            }
        };
        let on_grab = || {
            let mut shared = context
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if context.cancel.load(Ordering::Acquire) || shared.claimed != Some(request.generation)
            {
                return Err("Drag selection cancelled during handoff.".into());
            }
            shared.active = Some(request.generation);
            Ok(())
        };
        let deadline = Instant::now()
            .checked_add(lifetime)
            .ok_or("Invalid drag selection lifetime")?;
        let picked = picker::run_on_connection(
            self.connection,
            self.screen,
            request.bounds,
            &context.cancel,
            deadline,
            display,
            event.time,
            Some(activation),
            Some(&owner_check),
            on_grab,
        )?;
        Ok(DragPickerSelection {
            generation: request.generation,
            picked,
        })
    }

    fn apply(
        &mut self,
        desired: Option<DragPickerRequest>,
        context: &Context,
    ) -> Result<(), String> {
        self.disable(context)?;
        let mut ready = None;
        if let Some(request) = desired {
            if let Some(rect) = request.rect {
                let parent = self.parent.inspect(self.connection, self.root)?;
                if parent.is_none_or(|parent| parent.size() != request.parent_size) {
                    self.applied = desired;
                    return Ok(());
                }
                self.connection
                    .configure_window(
                        self.id,
                        &ConfigureWindowAux::new()
                            .x(rect.origin().x)
                            .y(rect.origin().y)
                            .width(rect.size().width())
                            .height(rect.size().height()),
                    )
                    .map_err(error)?
                    .check()
                    .map_err(error)?;
                self.shape(Some(Rectangle {
                    x: 0,
                    y: 0,
                    width: u16::try_from(rect.size().width()).map_err(error)?,
                    height: u16::try_from(rect.size().height()).map_err(error)?,
                }))?;
            }
            ready = Some(request.generation);
        }
        let barrier = self.connection.get_geometry(self.id).map_err(error)?;
        self.event_floor = barrier.sequence_number();
        barrier.reply().map_err(error)?;
        self.applied = desired;
        let mut shared = context
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if shared.request == desired && !context.cancel.load(Ordering::Acquire) {
            shared.ready = ready;
        }
        Ok(())
    }

    fn valid_press(
        &self,
        event: &ButtonPressEvent,
        sequence: u64,
        context: &Context,
    ) -> Result<Press, String> {
        if event.root != self.root || context.cancel.load(Ordering::Acquire) {
            return Ok(Press::Cancel);
        }
        let Some(parent) = self.parent.inspect(self.connection, self.root)? else {
            return Ok(Press::Cancel);
        };
        if !parent_contains(parent, event) {
            return Ok(Press::Cancel);
        }
        let Some(request) = self.applied else {
            return Ok(Press::Replay);
        };
        let Some(rect) = request.rect else {
            return Ok(Press::Replay);
        };
        if sequence < self.event_floor || parent.size() != request.parent_size {
            return Ok(Press::Replay);
        }
        let mut shared = context
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if shared.request != Some(request) || shared.ready != Some(request.generation) {
            return Ok(Press::Replay);
        }
        let x = i32::from(event.event_x);
        let y = i32::from(event.event_y);
        if x < 0
            || y < 0
            || u32::try_from(x).map_err(error)? >= rect.size().width()
            || u32::try_from(y).map_err(error)? >= rect.size().height()
        {
            return Ok(Press::Replay);
        }
        if context.cancel.load(Ordering::Acquire) || shared.claimed.is_some() {
            return Ok(Press::Cancel);
        }
        shared.claimed = Some(request.generation);
        shared.ready = None;
        Ok(Press::Accept(request))
    }

    fn replay(&mut self, time: u32) -> Result<(), String> {
        // The caller has emptied the input shape and revalidated the parent.
        self.connection
            .allow_events(Allow::REPLAY_POINTER, time)
            .map_err(error)?
            .check()
            .map_err(error)?;
        self.applied = None; // Reinstall only the newest UI rectangle after replay.
        Ok(())
    }

    fn shape(&self, rectangle: Option<Rectangle>) -> Result<(), String> {
        let region = self.connection.generate_id().map_err(error)?;
        self.connection
            .xfixes_create_region(region, rectangle.as_slice())
            .map_err(error)?
            .check()
            .map_err(error)?;
        let result = self
            .connection
            .xfixes_set_window_shape_region(self.id, SK::INPUT, 0, 0, region)
            .map_err(error)?
            .check()
            .map_err(error);
        let _ = self.connection.xfixes_destroy_region(region);
        result
    }
}

impl Drop for InputChild<'_, '_> {
    fn drop(&mut self) {
        self.connection.stream().begin_cleanup();
        let _ = self.connection.ungrab_pointer(x11rb::CURRENT_TIME);
        let _ = self.connection.destroy_window(self.id);
        let _ = self.connection.flush();
    }
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn parent_contains(parent: PhysicalRect, event: &ButtonPressEvent) -> bool {
    let x = i64::from(event.root_x) - i64::from(parent.origin().x);
    let y = i64::from(event.root_y) - i64::from(parent.origin().y);
    x >= 0
        && y >= 0
        && x < i64::from(parent.size().width())
        && y < i64::from(parent.size().height())
}
