//! Read-only observations of one explicit process-owned controller. Parent
//! traversal is bounded and follows real X11 ancestry, not WM extent hints.

use std::time::{Duration, Instant};

use gif_from_screen_capture::PhysicalRect;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, MapState, Window, WindowClass};

use super::{Client, Context, native_error};
use crate::recorder_guide::ControllerGeometry;

pub(super) struct ControllerObserver {
    window: Window,
    pid_atom: Atom,
    expected_pid: u32,
    sampled_at: Option<Instant>,
}

impl ControllerObserver {
    pub(super) fn new(
        connection: &Client<'_>,
        root: Window,
        window: Window,
        expected_pid: u32,
    ) -> Result<Self, String> {
        if window == 0 || window == root || expected_pid != std::process::id() {
            return Err("The recorder controller must belong to this process and cannot be the root window.".into());
        }
        let pid_atom = connection
            .intern_atom(true, b"_NET_WM_PID")
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?
            .atom;
        if pid_atom == x11rb::NONE {
            return Err("The recorder controller has no verifiable PID. Save the recording and reopen the controller.".into());
        }
        Ok(Self {
            window,
            pid_atom,
            expected_pid,
            sampled_at: None,
        })
    }

    pub(super) fn update(
        &mut self,
        connection: &Client<'_>,
        root: Window,
        context: &Context,
    ) -> Result<(), String> {
        if self
            .sampled_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(33))
        {
            return Ok(());
        }
        // The complete observation shares one five-second deadline. Idle reads
        // return to nonblocking mode even when the observation fails.
        connection.stream().begin_operation();
        let result = self.observe(connection, root);
        connection.stream().registration_complete();
        let geometry = result.map_err(|error| format!(
            "Recorder controller geometry is unavailable: {error} Save the recording and reopen the controller."
        ))?;
        self.sampled_at = Some(Instant::now());
        context.controller(geometry);
        Ok(())
    }

    fn observe(&self, connection: &Client<'_>, root: Window) -> Result<ControllerGeometry, String> {
        self.check_owner(connection)?;
        let attributes = connection
            .get_window_attributes(self.window)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        if attributes.class != WindowClass::INPUT_OUTPUT {
            return Err("The owned controller is not an input/output window.".into());
        }
        let client = rectangle(connection, self.window, root, false)?;
        let ancestor = root_child(connection, self.window, root)?;
        let outer = rectangle(connection, ancestor, root, true)?;
        // Recheck ownership after the geometry reads. This detects identity
        // changes during an observation, not an atomic server/presentation lock.
        // No controller or ancestor properties are ever written.
        self.check_owner(connection)?;
        Ok(ControllerGeometry {
            client,
            outer,
            viewable: attributes.map_state == MapState::VIEWABLE,
        })
    }

    fn check_owner(&self, connection: &Client<'_>) -> Result<(), String> {
        let property = connection
            .get_property(false, self.window, self.pid_atom, AtomEnum::CARDINAL, 0, 2)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        if property.type_ != u32::from(AtomEnum::CARDINAL)
            || property.format != 32
            || property.bytes_after != 0
            || property.value_len != 1
            || property.value32().and_then(|mut values| values.next()) != Some(self.expected_pid)
        {
            return Err("The recorder controller disappeared or its owning PID no longer matches this process.".into());
        }
        Ok(())
    }
}

fn root_child(connection: &Client<'_>, window: Window, root: Window) -> Result<Window, String> {
    let mut current = window;
    let mut visited = [x11rb::NONE; 4];
    for index in 0..visited.len() {
        if visited[..index].contains(&current) {
            return Err("The recorder controller has cyclic window ancestry.".into());
        }
        visited[index] = current;
        let tree = connection
            .query_tree(current)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        if tree.root != root || tree.parent == x11rb::NONE {
            return Err("The recorder controller is not on the selected X11 root.".into());
        }
        if tree.parent == root {
            return Ok(current);
        }
        current = tree.parent;
    }
    Err(
        "The recorder controller's window ancestry exceeds the four-query observation limit."
            .into(),
    )
}

fn rectangle(
    connection: &Client<'_>,
    window: Window,
    root: Window,
    include_border: bool,
) -> Result<PhysicalRect, String> {
    let geometry = connection
        .get_geometry(window)
        .map_err(native_error)?
        .reply()
        .map_err(native_error)?;
    let origin = connection
        .translate_coordinates(window, root, 0, 0)
        .map_err(native_error)?
        .reply()
        .map_err(native_error)?;
    if geometry.root != root || !origin.same_screen {
        return Err("The recorder controller moved to another X11 screen.".into());
    }
    let border = if include_border {
        u32::from(geometry.border_width)
    } else {
        0
    };
    PhysicalRect::new(
        i32::from(origin.dst_x) - i32::try_from(border).map_err(native_error)?,
        i32::from(origin.dst_y) - i32::try_from(border).map_err(native_error)?,
        u32::from(geometry.width) + 2 * border,
        u32::from(geometry.height) + 2 * border,
    )
    .map_err(native_error)
}
