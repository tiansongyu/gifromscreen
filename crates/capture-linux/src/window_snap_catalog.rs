//! Bounded one-shot WM discovery. No process-wide subscriptions or native writes.

use super::{
    WindowSnapCatalog,
    native::{Atoms, inspect},
};
use crate::{
    shortcuts::x11::{Client, stream},
    x11_window::rectangle,
};
use gif_from_screen_capture::{CaptureSource, CaptureSourceId, CaptureSourceKind};
use std::{
    collections::{HashSet, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt as _, Window},
};

const WINDOW_LIMIT: usize = 256;
const NODE_LIMIT: usize = 1024;

pub(super) fn query(
    display: Option<&str>,
    cancellation: &AtomicBool,
) -> Result<WindowSnapCatalog, String> {
    let started = Instant::now();
    let screen = usize::from(
        x11rb_protocol::parse_display::parse_display(display)
            .map_err(error)?
            .screen,
    );
    let connection = stream::connect(display, cancellation)?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .ok_or("The X11 screen is unavailable.")?
        .root;
    let atoms = Atoms::new(&connection)?;
    let (ids, mut truncated) = window_ids(&connection, root)?;
    let mut windows = Vec::new();
    let inspected_ids = ids.len();
    for (index, window) in ids.into_iter().enumerate() {
        if cancellation.load(Ordering::Acquire) || started.elapsed() >= Duration::from_secs(5) {
            return Err("Window discovery cancelled or exceeded five seconds.".into());
        }
        let Ok((name, _)) = inspect(&connection, window, &atoms) else {
            continue;
        };
        let Ok(bounds) = rectangle(&connection, window, root, false) else {
            continue;
        };
        let id = CaptureSourceId::new(format!("x11:screen:{screen}:window:0x{window:08x}"))
            .map_err(error)?;
        windows.push(
            CaptureSource::new(id, name, CaptureSourceKind::Window, Some(bounds), 1.0)
                .map_err(error)?,
        );
        if windows.len() == WINDOW_LIMIT {
            truncated |= index + 1 < inspected_ids;
            break;
        }
    }
    // Do not mistake a failed connection for an empty successful catalogue.
    connection
        .get_geometry(root)
        .map_err(error)?
        .reply()
        .map_err(error)?;
    Ok(WindowSnapCatalog { windows, truncated })
}

fn window_ids(connection: &Client<'_>, root: Window) -> Result<(Vec<Window>, bool), String> {
    for name in [
        b"_NET_CLIENT_LIST_STACKING".as_slice(),
        b"_NET_CLIENT_LIST".as_slice(),
    ] {
        let atom = connection
            .intern_atom(true, name)
            .map_err(error)?
            .reply()
            .map_err(error)?
            .atom;
        if atom == 0 {
            continue;
        }
        let reply = connection
            .get_property(
                false,
                root,
                atom,
                AtomEnum::WINDOW,
                0,
                u32::try_from(NODE_LIMIT).map_err(error)?,
            )
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if reply.type_ == 0 {
            continue;
        }
        if reply.type_ != u32::from(AtomEnum::WINDOW) || reply.format != 32 {
            return Err("The window manager's client list is malformed.".into());
        }
        let mut seen = HashSet::new();
        let mut windows: Vec<_> = reply
            .value32()
            .ok_or("Invalid WM client list")?
            .filter(|id| *id != 0 && *id != root && seen.insert(*id))
            .collect();
        // WM stacking is bottom-to-top; put the most recently stacked first.
        windows.reverse();
        return Ok((windows, reply.bytes_after != 0));
    }
    tree_windows(connection, root)
}

fn tree_windows(connection: &Client<'_>, root: Window) -> Result<(Vec<Window>, bool), String> {
    let children = connection
        .query_tree(root)
        .map_err(error)?
        .reply()
        .map_err(error)?
        .children;
    let mut truncated = children.len() > NODE_LIMIT;
    let mut queue: VecDeque<_> = children.into_iter().rev().take(NODE_LIMIT).collect();
    let mut seen = HashSet::new();
    let mut windows = Vec::new();
    let wm_state = connection
        .intern_atom(true, b"WM_STATE")
        .map_err(error)?
        .reply()
        .map_err(error)?
        .atom;
    let net_name = connection
        .intern_atom(true, b"_NET_WM_NAME")
        .map_err(error)?
        .reply()
        .map_err(error)?
        .atom;
    while let Some(window) = queue.pop_front() {
        if window == root || !seen.insert(window) {
            continue;
        }
        // A named client or ICCCM top-level is enough. Never enumerate every
        // child widget of an identified application window as another window.
        let named = connection
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1)
            .map_err(error)?
            .reply();
        let managed = if wm_state == 0 {
            false
        } else {
            connection
                .get_property(false, window, wm_state, AtomEnum::ANY, 0, 1)
                .map_err(error)?
                .reply()
                .is_ok_and(|reply| reply.type_ != 0)
        };
        let modern = if net_name == 0 {
            false
        } else {
            connection
                .get_property(false, window, net_name, AtomEnum::ANY, 0, 1)
                .map_err(error)?
                .reply()
                .is_ok_and(|reply| reply.type_ != 0)
        };
        if managed || modern || named.is_ok_and(|reply| reply.type_ != 0) {
            windows.push(window);
            continue;
        }
        let Ok(tree) = connection.query_tree(window).map_err(error)?.reply() else {
            continue;
        };
        let available = NODE_LIMIT.saturating_sub(seen.len() + queue.len());
        truncated |= tree.children.len() > available;
        queue.extend(tree.children.into_iter().rev().take(available));
    }
    Ok((windows, truncated))
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
