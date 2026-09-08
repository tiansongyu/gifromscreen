//! Read-only native geometry shared by recorder placement and window snapping.

use gif_from_screen_capture::PhysicalRect;
use x11rb::{
    connection::Connection,
    protocol::xproto::{ConnectionExt as _, Window},
};

pub(crate) fn root_child(
    connection: &impl Connection,
    window: Window,
    root: Window,
) -> Result<Window, String> {
    let mut current = window;
    let mut visited = [x11rb::NONE; 4];
    for index in 0..visited.len() {
        if visited[..index].contains(&current) {
            return Err("The X11 window has cyclic ancestry.".into());
        }
        visited[index] = current;
        let tree = connection
            .query_tree(current)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if tree.root != root || tree.parent == x11rb::NONE {
            return Err("The window is not on the selected X11 root.".into());
        }
        if tree.parent == root {
            return Ok(current);
        }
        current = tree.parent;
    }
    Err("The X11 window's ancestry exceeds the four-query observation limit.".into())
}

pub(crate) fn rectangle(
    connection: &impl Connection,
    window: Window,
    root: Window,
    include_border: bool,
) -> Result<PhysicalRect, String> {
    let geometry = connection
        .get_geometry(window)
        .map_err(error)?
        .reply()
        .map_err(error)?;
    let origin = connection
        .translate_coordinates(window, root, 0, 0)
        .map_err(error)?
        .reply()
        .map_err(error)?;
    if geometry.root != root || !origin.same_screen {
        return Err("The window moved to another X11 screen.".into());
    }
    let border = if include_border {
        u32::from(geometry.border_width)
    } else {
        0
    };
    PhysicalRect::new(
        i32::from(origin.dst_x) - i32::try_from(border).map_err(error)?,
        i32::from(origin.dst_y) - i32::try_from(border).map_err(error)?,
        u32::from(geometry.width) + 2 * border,
        u32::from(geometry.height) + 2 * border,
    )
    .map_err(error)
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
