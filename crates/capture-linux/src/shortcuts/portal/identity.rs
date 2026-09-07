//! Host identity belongs to a D-Bus peer, not to the window or process globally.

use ashpd::{Error, zbus};

const REGISTRY: &str = "org.freedesktop.host.portal.Registry";

pub(super) async fn register(connection: &zbus::Connection, app_id: &str) -> Result<(), String> {
    let app_id: ashpd::AppID = app_id
        .parse()
        .map_err(|error| format!("Invalid portal application ID: {error}"))?;
    // ashpd skips host registration inside Flatpak/Snap, where the portal owns
    // sandbox identification. This must use our dedicated connection, never
    // ashpd's process-global cached connection.
    match ashpd::register_host_app_with_connection(connection.clone(), app_id).await {
        Ok(()) => Ok(()),
        Err(error) if registry_unavailable(&error) => Ok(()),
        Err(error) => Err(format!(
            "Could not identify this app to the portal: {error}. Check desktop portal support/permissions and install GifFromScreen with its matching {}.desktop launcher before retrying. No host files were changed.",
            crate::APPLICATION_ID
        )),
    }
}

fn registry_unavailable(error: &Error) -> bool {
    // The official Registry contract permits old/future portals without this
    // interface. The application portal still decides whether cgroup/sandbox
    // identification is sufficient. Never swallow missing service, access
    // denial, missing desktop entry or duplicate/late registration failures.
    match error {
        Error::PortalNotFound(interface) => interface.as_str() == REGISTRY,
        Error::Zbus(zbus::Error::MethodError(name, _, _)) => matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownInterface"
        ),
        Error::Zbus(zbus::Error::FDO(error)) => matches!(
            error.as_ref(),
            zbus::fdo::Error::UnknownMethod(_) | zbus::fdo::Error::UnknownInterface(_)
        ),
        _ => false,
    }
}
