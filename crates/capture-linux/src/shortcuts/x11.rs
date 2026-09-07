//! Owned, exact passive grabs. Never selects raw keyboard input or grabs `AnyKey`.

use super::{
    RegisteredShortcut, ShortcutAction, ShortcutBinding, ShortcutContext, validate_bindings,
};
use std::{thread, time::Duration};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xkb::{self, ConnectionExt as _},
        xproto::{ConnectionExt as _, GrabMode, Mapping, ModMask},
    },
    wrapper::ConnectionExt as _,
};

#[path = "x11/keymap.rs"]
mod keymap;
#[path = "x11/stream.rs"]
pub(crate) mod stream;

pub(crate) type Client<'a> = x11rb::rust_connection::RustConnection<stream::BoundedStream<'a>>;
const DEVICE: u16 = 256; // XkbUseCoreKbd, not a wildcard key/modifier grab.

pub(super) fn run(bindings: &[ShortcutBinding], context: &ShortcutContext) -> Result<(), String> {
    run_display(None, bindings, context)
}

fn run_display(
    display: Option<&str>,
    bindings: &[ShortcutBinding],
    context: &ShortcutContext,
) -> Result<(), String> {
    validate_bindings(bindings)?;
    let connection = stream::connect(display, context.cancellation())?;
    let result = register_and_listen(&connection, bindings, context);
    // Suppress queued/late callbacks before the owned connection releases grabs.
    context.backend_stopping();
    drop(connection);
    if context.cancelled() { Ok(()) } else { result }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Grab {
    code: u8,
    modifiers: u16,
    action: ShortcutAction,
}

struct Grabs<'a, 'ctx> {
    connection: &'a Client<'ctx>,
    installed: Vec<(u32, Grab)>,
}
impl Drop for Grabs<'_, '_> {
    fn drop(&mut self) {
        self.connection.stream().begin_cleanup();
        for (root, grab) in &self.installed {
            let _ = self
                .connection
                .ungrab_key(grab.code, *root, ModMask::from(grab.modifiers));
        }
        let _ = self.connection.flush();
        let _ = self.connection.sync();
        // Closing our connection is the final rollback even if cleanup times out.
        // No other client's grabs, keymap or auto-repeat settings are changed.
    }
}

fn register_and_listen(
    connection: &Client<'_>,
    bindings: &[ShortcutBinding],
    context: &ShortcutContext,
) -> Result<(), String> {
    let mut owned = Grabs {
        connection,
        installed: Vec::new(),
    };
    let result = (|| {
        negotiate(connection)?;
        let mapping = keymap::MappingSnapshot::read(connection)?;
        let grabs = mapping.resolve(bindings)?;
        if connection.setup().roots.is_empty() || connection.setup().roots.len() > 16 {
            return Err("X11 shortcuts require between 1 and 16 screens".into());
        }
        if grabs.len().saturating_mul(connection.setup().roots.len()) > 256 {
            return Err("The explicit shortcut/lock/screen combinations exceed 256 grabs".into());
        }
        for screen in &connection.setup().roots {
            for grab in &grabs {
                if context.cancelled() {
                    return Err("Shortcut registration cancelled".into());
                }
                connection.grab_key(false, screen.root, ModMask::from(grab.modifiers), grab.code,
                    GrabMode::ASYNC, GrabMode::ASYNC).map_err(|e|format!("Could not request X11 shortcut: {e}"))?
                    .check().map_err(|e|format!("X11 shortcut conflict or registration failure; all new grabs are rolled back: {e}"))?;
                owned.installed.push((screen.root, *grab));
            }
        }
        context.registered(
            bindings
                .iter()
                .map(|binding| RegisteredShortcut {
                    action: binding.action,
                    trigger_description: binding.trigger.label(),
                })
                .collect(),
        )?;
        connection.stream().registration_complete();
        listen(connection, &grabs, mapping.group, context)
    })();
    context.backend_stopping();
    drop(owned);
    result
}

fn negotiate(connection: &Client<'_>) -> Result<(), String> {
    let version = connection
        .xkb_use_extension(1, 0)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    if !version.supported {
        return Err("X11 global shortcuts require XKB 1.0".into());
    }
    let flag = xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT;
    let repeat = connection
        .xkb_per_client_flags(
            DEVICE,
            flag,
            flag,
            xkb::BoolCtrl::default(),
            xkb::BoolCtrl::default(),
            xkb::BoolCtrl::default(),
        )
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    if !repeat.supported.contains(flag) || !repeat.value.contains(flag) {
        return Err("X11 server cannot provide per-client detectable auto-repeat; recorder buttons remain available".into());
    }
    connection
        .xkb_select_events(
            DEVICE,
            xkb::EventType::default(),
            xkb::EventType::NEW_KEYBOARD_NOTIFY
                | xkb::EventType::MAP_NOTIFY
                | xkb::EventType::STATE_NOTIFY,
            xkb::MapPart::KEY_TYPES | xkb::MapPart::KEY_SYMS | xkb::MapPart::MODIFIER_MAP,
            xkb::MapPart::KEY_TYPES | xkb::MapPart::KEY_SYMS | xkb::MapPart::MODIFIER_MAP,
            &xkb::SelectEventsAux::default(),
        )
        .map_err(|e| e.to_string())?
        .check()
        .map_err(|e| e.to_string())
}

fn listen(
    connection: &Client<'_>,
    grabs: &[Grab],
    group: u8,
    context: &ShortcutContext,
) -> Result<(), String> {
    let mut held = PressEdges::default();
    while !context.cancelled() {
        for _ in 0..128 {
            if context.cancelled() {
                return Ok(());
            }
            let Some(event) = connection
                .poll_for_event()
                .map_err(|e| format!("X11 shortcut connection failed: {e}"))?
            else {
                break;
            };
            match event {
                Event::KeyPress(event) if event.response_type & 0x80 == 0 => {
                    if ((u16::from(event.state) >> 13) & 3) as u8 != group {
                        return Err(layout_changed());
                    }
                    if let Some(action) =
                        held.press(event.detail, u16::from(event.state) & 0xff, grabs)
                    {
                        context.activated(action);
                    }
                }
                Event::KeyRelease(event) if event.response_type & 0x80 == 0 => {
                    if let Some(action) = held.release(event.detail) {
                        context.deactivated(action);
                    }
                }
                Event::MappingNotify(event)
                    if event.request == Mapping::KEYBOARD || event.request == Mapping::MODIFIER =>
                {
                    return Err(layout_changed());
                }
                Event::XkbMapNotify(_) | Event::XkbNewKeyboardNotify(_) => {
                    return Err(layout_changed());
                }
                Event::XkbStateNotify(event) if u8::from(event.group) != group => {
                    return Err(layout_changed());
                }
                _ => {} // Unbound typing is neither retained nor forwarded.
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn layout_changed() -> String {
    "The X11 keyboard layout or modifier mapping changed. Shortcuts were released; re-enable them to register the new mapping.".into()
}

struct PressEdges {
    held: [Option<ShortcutAction>; 256],
}
impl Default for PressEdges {
    fn default() -> Self {
        Self { held: [None; 256] }
    }
}
impl PressEdges {
    fn press(&mut self, code: u8, modifiers: u16, grabs: &[Grab]) -> Option<ShortcutAction> {
        if self.held[usize::from(code)].is_some() {
            return None;
        }
        let action = grabs
            .iter()
            .find(|grab| grab.code == code && grab.modifiers == modifiers)?
            .action;
        self.held[usize::from(code)] = Some(action);
        Some(action)
    }
    fn release(&mut self, code: u8) -> Option<ShortcutAction> {
        self.held[usize::from(code)].take()
    }
}

#[cfg(test)]
#[path = "x11/tests.rs"]
mod tests;
