//! Actual ashpd request tokens, retained while its high-level call awaits Response.

use super::{DESTINATION, SESSION_INTERFACE};
use ashpd::zbus::{
    Connection,
    zvariant::{self, OwnedObjectPath, OwnedValue, Type},
};
use std::{collections::HashMap, time::Duration};

pub(super) struct RequestObjects {
    request: OwnedObjectPath,
    pub(super) session: OwnedObjectPath,
}

impl RequestObjects {
    pub(super) fn from_options<T: serde::Serialize + Type>(
        connection: &Connection,
        options: &T,
        session: Option<OwnedObjectPath>,
    ) -> Result<Self, String> {
        // Serialize the library's real options: generating another token would close
        // the wrong object during cancellation before ashpd can return Request.
        let context = zvariant::serialized::Context::new_dbus(zvariant::LE, 0);
        let bytes = zvariant::to_bytes(context, options).map_err(|error| error.to_string())?;
        let (options, _): (HashMap<String, OwnedValue>, _) =
            bytes.deserialize().map_err(|error| error.to_string())?;
        let sender = connection
            .unique_name()
            .ok_or("Global shortcut connection has no unique bus name.")?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let path = |kind: &str, key: &str| -> Result<OwnedObjectPath, String> {
            let value = options
                .get(key)
                .ok_or("Global shortcut options have no handle token.")?;
            let token = <&str>::try_from(value).map_err(|error| error.to_string())?;
            OwnedObjectPath::try_from(format!(
                "/org/freedesktop/portal/desktop/{kind}/{sender}/{token}"
            ))
            .map_err(|error| error.to_string())
        };
        let session = if options.contains_key("session_handle_token") {
            path("session", "session_handle_token")?
        } else {
            session.ok_or("Global shortcut session handle is missing.")?
        };
        Ok(Self {
            request: path("request", "handle_token")?,
            session,
        })
    }

    pub(super) async fn close(&self, connection: &Connection, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, async {
            let _ = tokio::join!(
                connection.call_method(
                    Some(DESTINATION),
                    self.request.as_str(),
                    Some("org.freedesktop.portal.Request"),
                    "Close",
                    &()
                ),
                connection.call_method(
                    Some(DESTINATION),
                    self.session.as_str(),
                    Some(SESSION_INTERFACE),
                    "Close",
                    &()
                )
            );
        })
        .await;
    }
}
