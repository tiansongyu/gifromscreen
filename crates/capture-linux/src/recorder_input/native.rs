use std::{collections::BTreeSet, sync::atomic::AtomicBool};

use gif_from_screen_capture::{PhysicalRect, PhysicalSize};
use x11rb::{
    connection::Connection,
    errors::ReplyError,
    protocol::{
        ErrorKind,
        shape::{ConnectionExt as _, SK},
        xfixes::ConnectionExt as _,
        xproto::{Atom, AtomEnum, ConnectionExt as _, Rectangle, Window},
    },
    wrapper::ConnectionExt as _,
};

use super::check_cancel;
use crate::shortcuts::x11::{Client, stream};

const MAX_WINDOWS: usize = 1024;
const MAX_SCREENS: usize = 16;

struct Atoms {
    clients: Atom,
    pid: Atom,
    name: Atom,
    utf8: Atom,
}

impl Atoms {
    fn read(connection: &Client<'_>) -> Result<Self, String> {
        let atom = |name: &[u8]| {
            connection
                .intern_atom(false, name)
                .map_err(protocol)?
                .reply()
                .map(|reply| reply.atom)
                .map_err(protocol)
        };
        Ok(Self {
            clients: atom(b"_NET_CLIENT_LIST")?,
            pid: atom(b"_NET_WM_PID")?,
            name: atom(b"_NET_WM_NAME")?,
            utf8: atom(b"UTF8_STRING")?,
        })
    }
}

struct Target<'a> {
    pid: u32,
    title: &'a str,
    size: PhysicalSize,
}

pub(super) fn set(
    display: Option<&str>,
    pid: u32,
    title: &str,
    size: PhysicalSize,
    hole: PhysicalRect,
    cancel: &AtomicBool,
) -> Result<bool, String> {
    let connection = stream::connect(display, cancel)?;
    let version = connection
        .xfixes_query_version(5, 0)
        .map_err(protocol)?
        .reply()
        .map_err(protocol)?;
    if version.major_version < 3 {
        return Err("The X11 server does not support XFixes 3 input regions.".into());
    }
    let shape = connection
        .shape_query_version()
        .map_err(protocol)?
        .reply()
        .map_err(protocol)?;
    if (shape.major_version, shape.minor_version) < (1, 1) {
        return Err("The X11 server does not support SHAPE 1.1 input regions.".into());
    }
    let atoms = Atoms::read(&connection)?;
    let target = Target { pid, title, size };
    let Some(window) = find_recorder(&connection, &atoms, &target, cancel)? else {
        return Ok(false);
    };
    apply(&connection, &atoms, &target, window, hole, cancel)
}

fn find_recorder(
    connection: &Client<'_>,
    atoms: &Atoms,
    target: &Target<'_>,
    cancel: &AtomicBool,
) -> Result<Option<Window>, String> {
    let roots = &connection.setup().roots;
    if roots.is_empty() || roots.len() > MAX_SCREENS {
        return Err("Recorder discovery requires 1–16 X11 screens.".into());
    }
    let root_ids = roots
        .iter()
        .map(|screen| screen.root)
        .collect::<BTreeSet<_>>();
    let mut windows = BTreeSet::new();
    for screen in roots {
        check_cancel(cancel)?;
        let reply = connection
            .get_property(
                false,
                screen.root,
                atoms.clients,
                AtomEnum::WINDOW,
                0,
                u32::try_from(MAX_WINDOWS).map_err(protocol)?,
            )
            .map_err(protocol)?
            .reply()
            .map_err(protocol)?;
        if reply.bytes_after != 0 {
            return Err("Recorder window list exceeds the 1,024-window discovery limit.".into());
        }
        if reply.type_ == AtomEnum::WINDOW.into()
            && let Some(values) = reply.value32()
        {
            for window in values {
                add_candidate(&mut windows, &root_ids, window)?;
            }
        }
    }
    // Scan both sources: a new recorder may not have reached _NET_CLIENT_LIST yet,
    // and a second same-identity window must never be hidden by an early match.
    for root in &root_ids {
        let root_children = children(connection, *root, cancel)?;
        for child in &root_children {
            add_candidate(&mut windows, &root_ids, *child)?;
        }
        for child in root_children {
            for grandchild in children(connection, child, cancel)? {
                add_candidate(&mut windows, &root_ids, grandchild)?;
            }
        }
    }
    let mut found = None;
    for window in windows {
        check_cancel(cancel)?;
        if matches_identity(connection, atoms, target, window)? && found.replace(window).is_some() {
            return Err(
                "Multiple windows match this recorder identity; no input shape was changed.".into(),
            );
        }
    }
    Ok(found)
}

fn children(
    connection: &Client<'_>,
    window: Window,
    cancel: &AtomicBool,
) -> Result<Vec<Window>, String> {
    check_cancel(cancel)?;
    let Some(reply) = optional(connection.query_tree(window).map_err(protocol)?.reply())? else {
        return Ok(Vec::new());
    };
    if reply.children.len() > MAX_WINDOWS {
        return Err("Recorder window tree exceeds the 1,024-window discovery limit.".into());
    }
    Ok(reply.children)
}

fn add_candidate(
    windows: &mut BTreeSet<Window>,
    roots: &BTreeSet<Window>,
    window: Window,
) -> Result<(), String> {
    if window == 0 || roots.contains(&window) {
        return Ok(());
    }
    windows.insert(window);
    if windows.len() > MAX_WINDOWS {
        return Err("Recorder discovery exceeds 1,024 distinct windows.".into());
    }
    Ok(())
}

fn matches_identity(
    connection: &Client<'_>,
    atoms: &Atoms,
    target: &Target<'_>,
    window: Window,
) -> Result<bool, String> {
    let Some(pid) = optional(
        connection
            .get_property(false, window, atoms.pid, AtomEnum::CARDINAL, 0, 1)
            .map_err(protocol)?
            .reply(),
    )?
    else {
        return Ok(false);
    };
    if pid.type_ != AtomEnum::CARDINAL.into()
        || pid.format != 32
        || pid.bytes_after != 0
        || pid
            .value32()
            .is_none_or(|mut values| values.next() != Some(target.pid) || values.next().is_some())
    {
        return Ok(false);
    }
    let Some(title) = optional(
        connection
            .get_property(false, window, atoms.name, atoms.utf8, 0, 65)
            .map_err(protocol)?
            .reply(),
    )?
    else {
        return Ok(false);
    };
    Ok(title.type_ == atoms.utf8
        && title.format == 8
        && title.bytes_after == 0
        && title.value == target.title.as_bytes())
}

fn matches_size(
    connection: &Client<'_>,
    window: Window,
    size: PhysicalSize,
) -> Result<bool, String> {
    let Some(geometry) = optional(connection.get_geometry(window).map_err(protocol)?.reply())?
    else {
        return Ok(false);
    };
    Ok(u32::from(geometry.width) == size.width() && u32::from(geometry.height) == size.height())
}

fn apply(
    connection: &Client<'_>,
    atoms: &Atoms,
    target: &Target<'_>,
    window: Window,
    hole: PhysicalRect,
    cancel: &AtomicBool,
) -> Result<bool, String> {
    if !matches_size(connection, window, target.size)? {
        return Ok(false);
    }
    let outer = Rectangle {
        x: 0,
        y: 0,
        width: u16::try_from(target.size.width()).map_err(protocol)?,
        height: u16::try_from(target.size.height()).map_err(protocol)?,
    };
    let hole = Rectangle {
        x: i16::try_from(hole.origin().x).map_err(protocol)?,
        y: i16::try_from(hole.origin().y).map_err(protocol)?,
        width: u16::try_from(hole.size().width()).map_err(protocol)?,
        height: u16::try_from(hole.size().height()).map_err(protocol)?,
    };
    let mut regions = Regions {
        connection,
        ids: Vec::with_capacity(2),
    };
    let outer = regions.create(outer)?;
    let hole = regions.create(hole)?;
    connection
        .xfixes_subtract_region(outer, hole, outer)
        .map_err(protocol)?
        .check()
        .map_err(protocol)?;
    check_cancel(cancel)?;
    if !matches_identity(connection, atoms, target, window)?
        || !matches_size(connection, window, target.size)?
    {
        return Ok(false);
    }
    check_cancel(cancel)?;
    // One atomic assignment of an already-complete region. Never install an
    // empty/partial shape while calculating the border and toolbar regions.
    Ok(optional(
        connection
            .xfixes_set_window_shape_region(window, SK::INPUT, 0, 0, outer)
            .map_err(protocol)?
            .check(),
    )?
    .is_some())
}

struct Regions<'a, 'c> {
    connection: &'a Client<'c>,
    ids: Vec<u32>,
}

impl Regions<'_, '_> {
    fn create(&mut self, rectangle: Rectangle) -> Result<u32, String> {
        let id = self.connection.generate_id().map_err(protocol)?;
        self.connection
            .xfixes_create_region(id, &[rectangle])
            .map_err(protocol)?
            .check()
            .map_err(protocol)?;
        self.ids.push(id);
        Ok(id)
    }
}

impl Drop for Regions<'_, '_> {
    fn drop(&mut self) {
        self.connection.stream().begin_cleanup();
        for id in &self.ids {
            let _ = self.connection.xfixes_destroy_region(*id);
        }
        let _ = self.connection.flush();
        let _ = self.connection.sync();
    }
}

fn optional<T>(result: Result<T, ReplyError>) -> Result<Option<T>, String> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ReplyError::X11Error(error))
            if matches!(error.error_kind, ErrorKind::Window | ErrorKind::Drawable) =>
        {
            Ok(None)
        }
        Err(error) => Err(protocol(error)),
    }
}

fn protocol(error: impl std::fmt::Display) -> String {
    format!("X11 recorder input-shape operation failed: {error}")
}

#[cfg(test)]
mod tests;
