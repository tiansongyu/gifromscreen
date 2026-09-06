//! Close the real portal objects when ashpd is still awaiting their Response signal.

use ashpd::zbus::{
    Connection,
    zvariant::{self, OwnedObjectPath, OwnedValue, Type},
};
use gif_from_screen_capture::{CaptureError, CaptureErrorKind, RecoveryHint};
use std::{
    collections::HashMap,
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const CANCEL_POLL: Duration = Duration::from_millis(20);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn cancelled_error() -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::PermissionRequired,
        "Wayland source preparation was cancelled",
        RecoveryHint::None,
    )
}

/// A high-level ashpd call awaits Response internally, so its Request cannot
/// yet be borrowed. These are the *actual supplied tokens*, not guessed paths.
pub(super) struct RequestObjects {
    connection: Connection,
    request: OwnedObjectPath,
    pub(super) session: Option<OwnedObjectPath>,
}

impl RequestObjects {
    pub(super) fn from_options<T: serde::Serialize + Type>(
        connection: &Connection,
        options: &T,
        session: Option<OwnedObjectPath>,
    ) -> Result<Self, CaptureError> {
        let context = zvariant::serialized::Context::new_dbus(zvariant::LE, 0);
        let bytes = zvariant::to_bytes(context, options).map_err(|error| setup_error(&error))?;
        let (options, _): (HashMap<String, OwnedValue>, _) =
            bytes.deserialize().map_err(|error| setup_error(&error))?;
        let sender = connection
            .unique_name()
            .ok_or_else(|| setup_error(&"portal connection has no bus identity"))?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let path = |kind: &str, key: &str| -> Result<OwnedObjectPath, CaptureError> {
            let value = options
                .get(key)
                .ok_or_else(|| setup_error(&"portal options have no handle token"))?;
            let token = <&str>::try_from(value).map_err(|error| setup_error(&error))?;
            OwnedObjectPath::try_from(format!(
                "/org/freedesktop/portal/desktop/{kind}/{sender}/{token}"
            ))
            .map_err(|error| setup_error(&error))
        };
        let session = if options.contains_key("session_handle_token") {
            Some(path("session", "session_handle_token")?)
        } else {
            session
        };
        Ok(Self {
            connection: connection.clone(),
            request: path("request", "handle_token")?,
            session,
        })
    }

    pub(super) async fn close(&self) {
        // Both messages are sent even if one object disappeared after a racing response.
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
            let request = close_path(
                &self.connection,
                &self.request,
                "org.freedesktop.portal.Request",
            );
            let session = async {
                if let Some(path) = &self.session {
                    close_path(&self.connection, path, "org.freedesktop.portal.Session").await;
                }
            };
            tokio::join!(request, session);
        })
        .await;
        // The caller relinquishes its owned portal connection on cancellation;
        // disconnect cleanup also covers a service that ignored/bounded out Close.
    }
}

async fn close_path(connection: &Connection, path: &OwnedObjectPath, interface: &str) {
    let _ = connection
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            path.as_str(),
            Some(interface),
            "Close",
            &(),
        )
        .await;
}

pub(super) async fn cancellable<T>(
    cancellation: &AtomicBool,
    future: impl Future<Output = Result<T, CaptureError>>,
    cleanup: impl Future<Output = ()>,
) -> Result<T, CaptureError> {
    tokio::pin!(future);
    loop {
        if cancellation.load(Ordering::Acquire) {
            cleanup.await;
            return Err(cancelled_error());
        }
        tokio::select! {
            biased;
            result=&mut future=>{
                if cancellation.load(Ordering::Acquire){cleanup.await;return Err(cancelled_error());}
                return result;
            }
            ()=tokio::time::sleep(CANCEL_POLL)=>{}
        }
    }
}

fn setup_error(error: &dyn std::fmt::Display) -> CaptureError {
    CaptureError::new(
        CaptureErrorKind::Platform,
        format!("Could not track the cancellable portal request: {error}"),
        RecoveryHint::Retry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ashpd::desktop::{
        CreateSessionOptions,
        screencast::{SelectSourcesOptions, StartCastOptions},
    };
    use ashpd::zbus;
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        sync::{Arc, atomic::AtomicUsize},
    };

    struct PrivateBus {
        child: Child,
        address: String,
        _directory: tempfile::TempDir,
    }
    impl PrivateBus {
        fn start() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .arg(format!(
                    "--address=unix:tmpdir={}",
                    directory.path().display()
                ))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("portal cancellation transport tests require dbus-daemon");
            let mut bus = Self {
                child,
                address: String::new(),
                _directory: directory,
            };
            BufReader::new(bus.child.stdout.take().unwrap())
                .read_line(&mut bus.address)
                .unwrap();
            bus.address = bus.address.trim().to_owned();
            assert!(!bus.address.is_empty());
            bus
        }
    }
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    struct RequestClose(Arc<AtomicUsize>);
    #[ashpd::zbus::interface(name = "org.freedesktop.portal.Request", crate = "ashpd::zbus")]
    impl RequestClose {
        fn close(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }
    struct SessionClose(Arc<AtomicUsize>);
    #[ashpd::zbus::interface(name = "org.freedesktop.portal.Session", crate = "ashpd::zbus")]
    impl SessionClose {
        fn close(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn cancelling_each_pending_stage_closes_its_real_request_and_session_paths() {
        let bus = PrivateBus::start();
        let runtime = super::super::portal_runtime().unwrap();
        runtime.block_on(async {
            let client = ashpd::zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap();
            let service = ashpd::zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .name("org.freedesktop.portal.Desktop")
                .unwrap()
                .build()
                .await
                .unwrap();
            for stage in 0..3 {
                let create =
                    RequestObjects::from_options(&client, &CreateSessionOptions::default(), None)
                        .unwrap();
                let objects = match stage {
                    0 => create,
                    1 => RequestObjects::from_options(
                        &client,
                        &SelectSourcesOptions::default(),
                        create.session,
                    )
                    .unwrap(),
                    _ => RequestObjects::from_options(
                        &client,
                        &StartCastOptions::default(),
                        create.session,
                    )
                    .unwrap(),
                };
                let request_closed = Arc::new(AtomicUsize::new(0));
                let session_closed = Arc::new(AtomicUsize::new(0));
                service
                    .object_server()
                    .at(
                        objects.request.clone(),
                        RequestClose(Arc::clone(&request_closed)),
                    )
                    .await
                    .unwrap();
                service
                    .object_server()
                    .at(
                        objects.session.clone().unwrap(),
                        SessionClose(Arc::clone(&session_closed)),
                    )
                    .await
                    .unwrap();
                let cancellation = Arc::new(AtomicBool::new(false));
                let trigger = Arc::clone(&cancellation);
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    trigger.store(true, Ordering::Release);
                });
                let result = tokio::time::timeout(
                    Duration::from_secs(2),
                    cancellable(
                        &cancellation,
                        std::future::pending::<Result<(), CaptureError>>(),
                        objects.close(),
                    ),
                )
                .await
                .unwrap();
                assert!(result.unwrap_err().to_string().contains("cancelled"));
                assert_eq!(request_closed.load(Ordering::Acquire), 1);
                assert_eq!(session_closed.load(Ordering::Acquire), 1);
            }
        });
    }

    #[test]
    fn grant_cancel_race_runs_cleanup_and_never_returns_a_granted_resource() {
        let runtime = super::super::portal_runtime().unwrap();
        let cancellation = AtomicBool::new(false);
        let cleaned = AtomicBool::new(false);
        let result = runtime.block_on(cancellable(
            &cancellation,
            async {
                cancellation.store(true, Ordering::Release);
                Ok(42)
            },
            async {
                cleaned.store(true, Ordering::Release);
            },
        ));
        assert!(result.is_err());
        assert!(cleaned.load(Ordering::Acquire));
        let polled = AtomicBool::new(false);
        let result = runtime.block_on(cancellable(
            &cancellation,
            async {
                polled.store(true, Ordering::Release);
                Ok(42)
            },
            async {},
        ));
        assert!(result.is_err());
        assert!(!polled.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_success_and_failures_preserve_their_result_without_closing() {
        let runtime = super::super::portal_runtime().unwrap();
        let cancellation = AtomicBool::new(false);
        let cleaned = AtomicBool::new(false);
        assert_eq!(
            runtime
                .block_on(cancellable(&cancellation, async { Ok(42) }, async {
                    cleaned.store(true, Ordering::Release);
                }))
                .unwrap(),
            42
        );
        let expected = setup_error(&"unavailable");
        let result: Result<(), CaptureError> = runtime.block_on(cancellable(
            &cancellation,
            async { Err(expected.clone()) },
            async {
                cleaned.store(true, Ordering::Release);
            },
        ));
        assert_eq!(result.unwrap_err(), expected);
        assert!(!cleaned.load(Ordering::Acquire));
    }
}
