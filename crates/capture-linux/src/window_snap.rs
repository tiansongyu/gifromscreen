//! One-shot, read-only window bounds for explicit pre-record region snapping.

use gif_from_screen_capture::{CaptureSource, PhysicalRect};
use std::sync::atomic::AtomicBool;

/// Which actual native rectangle to use for an explicit window snap.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WindowSnapBounds {
    /// Client contents, without the native X11 border or ancestor decorations.
    Client,
    /// Window-manager frame extents, excluding advertised client-side shadows.
    /// Hints must be well-formed, stable and contained by actual native bounds.
    #[default]
    WindowFrame,
    /// Root-child ancestor and its X11 border, including native decorations.
    /// Client-side invisible margins remain part of the client; compositor
    /// shadows outside native bounds are not included.
    Outer,
}

/// Re-reads an enumerated window's current physical bounds on a dedicated,
/// cancellable connection. Never moves, focuses, raises or grabs any window.
///
/// The caller must run this off the UI thread, retain the current source/canvas
/// contract, and reject late results after the user changes the selection.
/// This is a one-shot observation, not ongoing tracking or a presentation fence.
///
/// # Errors
///
/// Rejects stale titles, hidden/closed/own/helper windows, wrong X11 screens,
/// malformed/oversized properties, unstable geometry and cancelled/timed-out
/// transport. Builds without native X11 return an unsupported error.
pub fn query_window_snap(
    display: Option<&str>,
    source: &CaptureSource,
    bounds: WindowSnapBounds,
    cancellation: &AtomicBool,
) -> Result<PhysicalRect, String> {
    #[cfg(all(target_os = "linux", feature = "native-x11"))]
    {
        native::query(display, source, bounds, cancellation)
    }
    #[cfg(not(all(target_os = "linux", feature = "native-x11")))]
    {
        let _ = (display, source, bounds, cancellation);
        Err("Window snapping requires a native X11 build.".into())
    }
}

#[cfg(all(target_os = "linux", feature = "native-x11"))]
mod native {
    use super::{AtomicBool, CaptureSource, PhysicalRect, WindowSnapBounds};
    use crate::{
        shortcuts::x11::{Client, stream},
        x11_window::{rectangle, root_child},
    };
    use gif_from_screen_capture::CaptureSourceKind;
    use x11rb::{
        connection::Connection,
        protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, MapState, Window, WindowClass},
    };

    struct Atoms {
        name: Atom,
        utf8: Atom,
        pid: Atom,
        state: Atom,
        hidden: Atom,
        frame: Atom,
        gtk_frame: Atom,
    }

    pub(super) fn query(
        display: Option<&str>,
        source: &CaptureSource,
        bounds: WindowSnapBounds,
        cancellation: &AtomicBool,
    ) -> Result<PhysicalRect, String> {
        if source.kind() != CaptureSourceKind::Window {
            return Err("Choose an X11 window to snap to.".into());
        }
        let screen = usize::from(
            x11rb_protocol::parse_display::parse_display(display)
                .map_err(error)?
                .screen,
        );
        let window = crate::x11::parse_window_source_id(source.id().as_str(), screen).ok_or(
            "The selected window belongs to a different X11 screen or has an invalid identity.",
        )?;
        let connection = stream::connect(display, cancellation)?;
        let root = connection
            .setup()
            .roots
            .get(screen)
            .ok_or("The X11 screen is unavailable.")?
            .root;
        if window == root {
            return Err("Cannot snap to the root as a window.".into());
        }
        let atom = |name: &[u8]| {
            connection
                .intern_atom(true, name)
                .map_err(error)?
                .reply()
                .map(|reply| reply.atom)
                .map_err(error)
        };
        let atoms = Atoms {
            name: atom(b"_NET_WM_NAME")?,
            utf8: atom(b"UTF8_STRING")?,
            pid: atom(b"_NET_WM_PID")?,
            state: atom(b"_NET_WM_STATE")?,
            hidden: atom(b"_NET_WM_STATE_HIDDEN")?,
            frame: atom(b"_NET_FRAME_EXTENTS")?,
            gtk_frame: atom(b"_GTK_FRAME_EXTENTS")?,
        };
        let identity = inspect(&connection, window, &atoms)?;
        if identity.0 != source.name() {
            return Err(
                "The window title changed since discovery. Refresh sources and choose it again."
                    .into(),
            );
        }
        let mut previous = None;
        for _ in 0..3 {
            let client = rectangle(&connection, window, root, false)?;
            let ancestor = root_child(&connection, window, root)?;
            let outer = rectangle(&connection, ancestor, root, true)?;
            let requested = match bounds {
                WindowSnapBounds::Client => client,
                WindowSnapBounds::Outer => outer,
                WindowSnapBounds::WindowFrame => {
                    frame_bounds(&connection, window, &atoms, client, outer)?
                }
            };
            if inspect(&connection, window, &atoms)? != identity {
                return Err(
                    "The window changed identity while snapping. Refresh sources and retry.".into(),
                );
            }
            let observed = (client, ancestor, outer, requested);
            if previous == Some(observed) {
                return Ok(requested);
            }
            previous = Some(observed);
        }
        Err("The window is moving or resizing. Let it settle and retry snapping.".into())
    }

    fn frame_bounds(
        connection: &Client<'_>,
        window: Window,
        atoms: &Atoms,
        client: PhysicalRect,
        outer: PhysicalRect,
    ) -> Result<PhysicalRect, String> {
        let wm = extents(connection, window, atoms.frame)?;
        let gtk = extents(connection, window, atoms.gtk_frame)?;
        if wm.is_none() && gtk.is_none() {
            return Err(
                "No window-frame hints are available. Choose Client area or Native bounds.".into(),
            );
        }
        let wm = wm.unwrap_or([0; 4]);
        let gtk = gtk.unwrap_or([0; 4]);
        if wm != [0; 4] && gtk != [0; 4] {
            return Err("Combined client-side shadows and window-manager borders are ambiguous. Choose Native bounds.".into());
        }
        let sides =
            std::array::from_fn::<_, 4, _>(|index| i64::from(wm[index]) - i64::from(gtk[index]));
        let x = i64::from(client.origin().x) - sides[0];
        let y = i64::from(client.origin().y) - sides[2];
        let width = i64::from(client.size().width()) + sides[0] + sides[1];
        let height = i64::from(client.size().height()) + sides[2] + sides[3];
        let left = i64::from(outer.origin().x);
        let top = i64::from(outer.origin().y);
        if width <= 0
            || height <= 0
            || x < left
            || y < top
            || x + width > left + i64::from(outer.size().width())
            || y + height > top + i64::from(outer.size().height())
        {
            return Err("Window-frame hints disagree with actual native bounds. Retry or choose Native bounds.".into());
        }
        PhysicalRect::new(
            i32::try_from(x).map_err(error)?,
            i32::try_from(y).map_err(error)?,
            u32::try_from(width).map_err(error)?,
            u32::try_from(height).map_err(error)?,
        )
        .map_err(error)
    }

    fn extents(
        connection: &Client<'_>,
        window: Window,
        atom: Atom,
    ) -> Result<Option<[u32; 4]>, String> {
        if atom == 0 {
            return Ok(None);
        }
        let reply = connection
            .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 5)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if reply.type_ == 0 {
            return Ok(None);
        }
        if reply.type_ != u32::from(AtomEnum::CARDINAL)
            || reply.format != 32
            || reply.bytes_after != 0
            || reply.value_len != 4
        {
            return Err(
                "Window-frame extents must contain exactly four 32-bit cardinal values.".into(),
            );
        }
        let mut values = reply.value32().ok_or("Invalid window-frame extents")?;
        Ok(Some(std::array::from_fn(|_| {
            values.next().expect("validated four extents")
        })))
    }

    fn inspect(
        connection: &Client<'_>,
        window: Window,
        atoms: &Atoms,
    ) -> Result<(String, Option<u32>), String> {
        let attributes = connection
            .get_window_attributes(window)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if attributes.map_state != MapState::VIEWABLE
            || attributes.class != WindowClass::INPUT_OUTPUT
            || attributes.override_redirect
        {
            return Err("The selected window is hidden or is a helper window.".into());
        }
        let pid = cardinal(connection, window, atoms.pid)?;
        if pid == Some(std::process::id()) {
            return Err("Cannot snap to this recorder's own window.".into());
        }
        if atoms.state != 0 && atoms.hidden != 0 {
            let state = connection
                .get_property(false, window, atoms.state, AtomEnum::ATOM, 0, 64)
                .map_err(error)?
                .reply()
                .map_err(error)?;
            if state.bytes_after != 0
                || (state.type_ != 0
                    && (state.type_ != u32::from(AtomEnum::ATOM) || state.format != 32))
            {
                return Err("The window state property is malformed or too large.".into());
            }
            if state
                .value32()
                .is_some_and(|mut values| values.any(|value| value == atoms.hidden))
            {
                return Err("The selected window is minimized. Restore it before snapping.".into());
            }
        }
        let modern = if atoms.name != 0 && atoms.utf8 != 0 {
            title(connection, window, atoms.name, atoms.utf8)?
        } else {
            None
        };
        let title = match modern {
            Some(title) => title,
            None => title(
                connection,
                window,
                AtomEnum::WM_NAME.into(),
                AtomEnum::ANY.into(),
            )?
            .unwrap_or_else(|| format!("Window 0x{window:08x}")),
        };
        Ok((title, pid))
    }

    fn cardinal(
        connection: &Client<'_>,
        window: Window,
        atom: Atom,
    ) -> Result<Option<u32>, String> {
        if atom == 0 {
            return Ok(None);
        }
        let reply = connection
            .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 2)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if reply.type_ == 0 {
            return Ok(None);
        }
        if reply.type_ != u32::from(AtomEnum::CARDINAL)
            || reply.format != 32
            || reply.bytes_after != 0
            || reply.value_len != 1
        {
            return Err("The window PID property is malformed.".into());
        }
        Ok(reply.value32().and_then(|mut values| values.next()))
    }

    fn title(
        connection: &Client<'_>,
        window: Window,
        atom: Atom,
        expected_type: Atom,
    ) -> Result<Option<String>, String> {
        let reply = connection
            .get_property(false, window, atom, expected_type, 0, 1024)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if reply.type_ == 0 {
            return Ok(None);
        }
        if reply.bytes_after != 0 {
            return Err("The window title exceeds the 4 KiB snap limit.".into());
        }
        Ok(crate::x11::decode_text_property(reply.format, &reply.value))
    }

    fn error(error: impl std::fmt::Display) -> String {
        error.to_string()
    }
}
