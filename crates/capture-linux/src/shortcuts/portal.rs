//! One owned `GlobalShortcuts` session; no captured keystrokes or shared portal bus.

use std::{future::Future, sync::atomic::Ordering, time::Duration};

use ashpd::{
    desktop::{
        CreateSessionOptions,
        global_shortcuts::{
            Activated, BindShortcutsOptions, Deactivated, GlobalShortcuts, NewShortcut, Shortcut,
            ShortcutsChanged,
        },
    },
    zbus::{
        self, Connection, Message, MessageStream,
        names::{InterfaceName, MemberName},
        zvariant::{ObjectPath, OwnedObjectPath},
    },
};
use futures_util::StreamExt;

use super::{RegisteredShortcut, ShortcutAction, ShortcutBinding, ShortcutContext};

mod identity;
mod objects;
#[cfg(test)]
mod tests;

use objects::RequestObjects;

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";
const CANCEL_POLL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy)]
struct Timeouts {
    probe: Duration,
    request: Duration,
    close: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            probe: Duration::from_secs(5),
            request: Duration::from_secs(120),
            close: Duration::from_secs(2),
        }
    }
}

pub(super) fn run(bindings: &[ShortcutBinding], context: &ShortcutContext) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Global shortcut runtime: {error}"))?;
    runtime.block_on(async {
        let timeouts = Timeouts::default();
        let connection = phase(
            context,
            "connect to session bus",
            timeouts.probe,
            Connection::session(),
            None,
        )
        .await?;
        run_connected(connection, bindings, context, timeouts).await
    })
}

async fn run_connected(
    connection: Connection,
    bindings: &[ShortcutBinding],
    context: &ShortcutContext,
    timeouts: Timeouts,
) -> Result<(), String> {
    let mut objects = None;
    let result = run_session(&connection, bindings, context, timeouts, &mut objects).await;
    context.backend_stopping();
    if let Some(objects) = objects {
        objects.close(&connection, timeouts.close).await;
    }
    // Never use ashpd's process-wide shared connection: disconnecting this connection
    // also releases a late-created session if the portal ignored Request.Close.
    let _ = tokio::time::timeout(timeouts.close, connection.close()).await;
    result
}

async fn run_session(
    connection: &Connection,
    bindings: &[ShortcutBinding],
    context: &ShortcutContext,
    timeouts: Timeouts,
    objects: &mut Option<RequestObjects>,
) -> Result<(), String> {
    // Register this bus peer before constructing any application portal proxy.
    // A window's Wayland app_id does not identify an independent D-Bus connection.
    phase(
        context,
        "register the application identity for global shortcuts",
        timeouts.probe,
        identity::register(connection, crate::APPLICATION_ID),
        None,
    )
    .await?;
    let portal = phase(
        context,
        "open GlobalShortcuts portal",
        timeouts.probe,
        GlobalShortcuts::with_connection(connection.clone()),
        None,
    )
    .await?;
    // ashpd deliberately defaults missing version properties to 1. Here absence is
    // a real unsupported-desktop result, not evidence that registration succeeded.
    let version = phase(
        context,
        "query GlobalShortcuts support",
        timeouts.probe,
        portal.get_property::<u32>("version"),
        None,
    )
    .await?;
    if version == 0 {
        return Err("GlobalShortcuts portal has an unsupported version (0). Recorder buttons remain available.".into());
    }
    let options = CreateSessionOptions::default();
    *objects = Some(RequestObjects::from_options(connection, &options, None)?);
    let session_path = objects.as_ref().expect("created options").session.clone();
    let mut watch = phase(
        context,
        "subscribe to GlobalShortcuts lifecycle",
        timeouts.probe,
        Watch::new(&portal, session_path.clone()),
        None,
    )
    .await?;
    let session = phase(
        context,
        "create GlobalShortcuts session",
        timeouts.request,
        portal.create_session(options),
        Some(&mut watch),
    )
    .await?;
    let shortcuts: Vec<_> = bindings
        .iter()
        .map(|binding| {
            let preferred = binding.trigger.label();
            NewShortcut::new(binding.action.id(), binding.action.description())
                .preferred_trigger(preferred.as_str())
        })
        .collect();
    let options = BindShortcutsOptions::default();
    *objects = Some(RequestObjects::from_options(
        connection,
        &options,
        Some(session_path),
    )?);
    let response = phase(
        context,
        "bind GlobalShortcuts (permission may be refused)",
        timeouts.request,
        portal.bind_shortcuts(&session, &shortcuts, None, options),
        Some(&mut watch),
    )
    .await?
    .response()
    .map_err(|error| {
        format!("Global shortcut permission response: {error}. Recorder buttons remain available.")
    })?;
    let actual = registered(bindings, response.shortcuts())?;
    context.registered(actual.clone())?;
    loop {
        if context.cancelled() {
            return Ok(());
        }
        tokio::select! {
            message = watch.next() => dispatch(&message?, bindings, &actual, &watch.session, context)?,
            () = tokio::time::sleep(CANCEL_POLL) => {}
        }
    }
}

fn registered(
    bindings: &[ShortcutBinding],
    shortcuts: &[Shortcut],
) -> Result<Vec<RegisteredShortcut>, String> {
    if shortcuts.len() > bindings.len() {
        return Err("GlobalShortcuts returned more bindings than requested.".into());
    }
    let mut actual = Vec::with_capacity(shortcuts.len());
    let mut seen = Vec::with_capacity(shortcuts.len());
    for shortcut in shortcuts {
        let action = action_for(bindings, shortcut.id())
            .ok_or_else(|| "GlobalShortcuts returned an unrequested action.".to_owned())?;
        if seen.contains(&action) {
            return Err("GlobalShortcuts returned a duplicate action.".into());
        }
        seen.push(action);
        let description = shortcut.trigger_description();
        if description.len() > 256 || description.chars().any(char::is_control) {
            return Err("GlobalShortcuts returned an invalid trigger description.".into());
        }
        // KDE can return persisted actions with no assigned combination. They
        // are not active bindings, and must never fall back to our preference.
        if description.trim().is_empty() {
            continue;
        }
        actual.push(RegisteredShortcut {
            action,
            trigger_description: description.to_owned(),
        });
    }
    // Portal array ordering is not binding identity; harmless reordered change
    // notifications must not terminate a working session.
    actual.sort_by_key(|binding| binding.action);
    Ok(actual)
}

fn action_for(bindings: &[ShortcutBinding], id: &str) -> Option<ShortcutAction> {
    bindings
        .iter()
        .find(|binding| binding.action.id() == id)
        .map(|binding| binding.action)
}

fn dispatch(
    message: &Message,
    requested: &[ShortcutBinding],
    actual: &[RegisteredShortcut],
    session: &OwnedObjectPath,
    context: &ShortcutContext,
) -> Result<(), String> {
    let header = message.header();
    if header.interface().map(InterfaceName::as_str) != Some(INTERFACE)
        || header.path().map(ObjectPath::as_str) != Some(DESKTOP_PATH)
    {
        return Ok(());
    }
    let event = match header.member().map(MemberName::as_str) {
        Some("Activated") => {
            let signal: Activated = message
                .body()
                .deserialize()
                .map_err(|error| signal_error(&error))?;
            (
                signal.session_handle().to_owned(),
                signal.shortcut_id().to_owned(),
                true,
            )
        }
        Some("Deactivated") => {
            let signal: Deactivated = message
                .body()
                .deserialize()
                .map_err(|error| signal_error(&error))?;
            (
                signal.session_handle().to_owned(),
                signal.shortcut_id().to_owned(),
                false,
            )
        }
        Some("ShortcutsChanged") => {
            let signal: ShortcutsChanged = message
                .body()
                .deserialize()
                .map_err(|error| signal_error(&error))?;
            if signal.session_handle().as_str() == session.as_str()
                && registered(requested, signal.shortcuts())? != actual
            {
                // Re-registration would reset held-key state. End this session instead
                // of dispatching against stale bindings or inventing a press edge.
                return Err("Global shortcut bindings changed. Re-enable global shortcuts to use the updated bindings; recorder buttons remain available.".into());
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    if event.0.as_str() == session.as_str()
        && let Some(action) = actual
            .iter()
            .find(|binding| binding.action.id() == event.1)
            .map(|binding| binding.action)
    {
        if event.2 {
            context.activated(action);
        } else {
            context.deactivated(action);
        }
    }
    Ok(())
}

fn signal_error(error: &zbus::Error) -> String {
    format!("Invalid GlobalShortcuts signal: {error}")
}

struct Watch {
    signals: MessageStream,
    owners: zbus::proxy::OwnerChangedStream<'static>,
    session: OwnedObjectPath,
}

impl Watch {
    async fn new(portal: &GlobalShortcuts, session: OwnedObjectPath) -> Result<Self, zbus::Error> {
        let owners = portal.receive_owner_changed().await?;
        let bus = zbus::fdo::DBusProxy::new(portal.connection()).await?;
        let owner = bus.get_name_owner(DESTINATION.try_into()?).await?;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(owner)?
            .path_namespace(DESKTOP_PATH)?
            .build();
        // One ordered stream preserves press/release order, including queued bursts.
        // It also sees a Closed signal emitted before CreateSession returns.
        let signals = MessageStream::for_match_rule(rule, portal.connection(), Some(64)).await?;
        Ok(Self {
            signals,
            owners,
            session,
        })
    }

    async fn next(&mut self) -> Result<Message, String> {
        tokio::select! {
            biased;
            _ = self.owners.next() => Err("GlobalShortcuts portal disconnected or changed owner. Recorder buttons remain available.".into()),
            signal = self.signals.next() => {
                let message = signal.ok_or_else(|| "GlobalShortcuts bus connection closed.".to_owned())?.map_err(|error| signal_error(&error))?;
                let header = message.header();
                if header.interface().map(InterfaceName::as_str) == Some(SESSION_INTERFACE)
                    && header.path().map(ObjectPath::as_str) == Some(self.session.as_str())
                    && header.member().map(MemberName::as_str) == Some("Closed") {
                    return Err("GlobalShortcuts session was closed by the desktop. Recorder buttons remain available.".into());
                }
                Ok(message)
            }
        }
    }
}

async fn phase<T, E: std::fmt::Display>(
    context: &ShortcutContext,
    label: &str,
    timeout: Duration,
    future: impl Future<Output = Result<T, E>>,
    mut watch: Option<&mut Watch>,
) -> Result<T, String> {
    tokio::pin!(future);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let cancellation = context.cancellation();
    loop {
        if cancellation.load(Ordering::Acquire) {
            return Err("Global shortcut registration cancelled.".into());
        }
        tokio::select! {
            biased;
            () = &mut deadline => return Err(format!("Timed out while trying to {label}. Recorder buttons remain available.")),
            signal = async { match &mut watch { Some(watch) => watch.next().await, None => std::future::pending().await } } => { signal?; }
            result = &mut future => {
                if context.cancelled() { return Err("Global shortcut registration cancelled.".into()); }
                return result.map_err(|error| format!("Could not {label}: {error}. Recorder buttons remain available."));
            }
            () = tokio::time::sleep(CANCEL_POLL) => {}
        }
    }
}
