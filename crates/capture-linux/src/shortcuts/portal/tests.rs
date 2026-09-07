//! Private transport fixtures exercise protocol mechanics, not desktop support.

use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use ashpd::zbus::{
    message::Header,
    zvariant::{OwnedValue, Value},
};

use super::*;
use crate::shortcuts::{Inbox, ShortcutStatus, default_shortcut_bindings};

type Dictionary = HashMap<String, OwnedValue>;
type WireShortcuts = Vec<(String, Dictionary)>;

struct PrivateBus {
    child: Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl PrivateBus {
    fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:tmpdir={}",
                directory.path().display()
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("private dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty());
        Self {
            child,
            address: address.trim().to_owned(),
            _directory: directory,
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Copy, Default)]
enum Behavior {
    #[default]
    Grant,
    RefuseCreate,
    RefuseBind,
    PendingCreate,
    PendingBind,
    Empty,
    EarlyClosed,
    InvalidDescription,
    PendingBindReply,
    SlowClose,
}

#[derive(Default)]
struct State {
    version: u32,
    behavior: Behavior,
    creates: AtomicUsize,
    binds: AtomicUsize,
    request_closes: AtomicUsize,
    session_closes: AtomicUsize,
    session: Mutex<Option<OwnedObjectPath>>,
    requested: Mutex<Vec<(String, String)>>,
}

struct RequestClose(Arc<State>);
#[ashpd::zbus::interface(name = "org.freedesktop.portal.Request", crate = "ashpd::zbus")]
impl RequestClose {
    async fn close(&self) {
        self.0.request_closes.fetch_add(1, Ordering::AcqRel);
        if matches!(self.0.behavior, Behavior::SlowClose) {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

struct SessionClose(Arc<State>);
#[ashpd::zbus::interface(name = "org.freedesktop.portal.Session", crate = "ashpd::zbus")]
impl SessionClose {
    async fn close(&self) {
        self.0.session_closes.fetch_add(1, Ordering::AcqRel);
        if matches!(self.0.behavior, Behavior::SlowClose) {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        self.0.version
    }
}

struct MockPortal(Arc<State>);

#[ashpd::zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts", crate = "ashpd::zbus")]
impl MockPortal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        self.0.version
    }

    async fn create_session(
        &self,
        options: Dictionary,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> OwnedObjectPath {
        let request = object_path(&header, &options, "request", "handle_token");
        let session = object_path(&header, &options, "session", "session_handle_token");
        connection
            .object_server()
            .at(request.clone(), RequestClose(Arc::clone(&self.0)))
            .await
            .unwrap();
        connection
            .object_server()
            .at(session.clone(), SessionClose(Arc::clone(&self.0)))
            .await
            .unwrap();
        *self.0.session.lock().unwrap() = Some(session.clone());
        self.0.creates.fetch_add(1, Ordering::AcqRel);
        match self.0.behavior {
            Behavior::PendingCreate => {}
            Behavior::EarlyClosed => {
                connection
                    .emit_signal(
                        None::<&str>,
                        session.as_str(),
                        SESSION_INTERFACE,
                        "Closed",
                        &(Dictionary::new(),),
                    )
                    .await
                    .unwrap();
            }
            Behavior::RefuseCreate => response(connection, &request, 1, Dictionary::new()).await,
            _ => {
                response(
                    connection,
                    &request,
                    0,
                    HashMap::from([("session_handle".into(), value(session.as_str()))]),
                )
                .await;
            }
        }
        request
    }

    async fn bind_shortcuts(
        &self,
        session: OwnedObjectPath,
        shortcuts: WireShortcuts,
        parent_window: String,
        options: Dictionary,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> OwnedObjectPath {
        assert_eq!(Some(&session), self.0.session.lock().unwrap().as_ref());
        assert!(parent_window.is_empty());
        let request = object_path(&header, &options, "request", "handle_token");
        connection
            .object_server()
            .at(request.clone(), RequestClose(Arc::clone(&self.0)))
            .await
            .unwrap();
        *self.0.requested.lock().unwrap() = shortcuts
            .iter()
            .map(|(id, properties)| {
                assert!(properties.contains_key("description"));
                (
                    id.clone(),
                    <&str>::try_from(&properties["preferred_trigger"])
                        .unwrap()
                        .to_owned(),
                )
            })
            .collect();
        self.0.binds.fetch_add(1, Ordering::AcqRel);
        match self.0.behavior {
            Behavior::PendingBind => {}
            Behavior::PendingBindReply => {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Behavior::RefuseBind => response(connection, &request, 2, Dictionary::new()).await,
            _ => {
                let mut shortcuts = if matches!(self.0.behavior, Behavior::Empty) {
                    vec![]
                } else {
                    subset()
                };
                if matches!(self.0.behavior, Behavior::InvalidDescription) {
                    shortcuts[0]
                        .1
                        .insert("trigger_description".into(), value("bad\ntrigger"));
                }
                response(
                    connection,
                    &request,
                    0,
                    HashMap::from([(
                        "shortcuts".into(),
                        OwnedValue::try_from(Value::from(shortcuts)).unwrap(),
                    )]),
                )
                .await;
            }
        }
        request
    }
}

fn value(text: &str) -> OwnedValue {
    OwnedValue::try_from(Value::from(text)).unwrap()
}

fn subset() -> WireShortcuts {
    vec![(
        ShortcutAction::StartPause.id().into(),
        HashMap::from([
            ("description".into(), value("Recorder")),
            ("trigger_description".into(), value("Desktop chose Super+R")),
        ]),
    )]
}

fn object_path(
    header: &Header<'_>,
    options: &Dictionary,
    kind: &str,
    key: &str,
) -> OwnedObjectPath {
    let sender = header
        .sender()
        .unwrap()
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let token = <&str>::try_from(&options[key]).unwrap();
    format!("{DESKTOP_PATH}/{kind}/{sender}/{token}")
        .try_into()
        .unwrap()
}

async fn response(
    connection: &Connection,
    request: &OwnedObjectPath,
    code: u32,
    values: Dictionary,
) {
    // Intentionally emitted before the method reply: ashpd must have subscribed
    // before the request, and the lifecycle watcher must not steal the response.
    connection
        .emit_signal(
            None::<&str>,
            request.as_str(),
            "org.freedesktop.portal.Request",
            "Response",
            &(code, values),
        )
        .await
        .unwrap();
}

struct Fixture {
    _bus: PrivateBus,
    service: Connection,
    client: Connection,
    state: Arc<State>,
    context: ShortcutContext,
}

impl Fixture {
    async fn new(behavior: Behavior) -> Self {
        let bus = PrivateBus::start();
        let service = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(DESTINATION)
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let state = Arc::new(State {
            behavior,
            version: 1,
            ..State::default()
        });
        service
            .object_server()
            .at(DESKTOP_PATH, MockPortal(Arc::clone(&state)))
            .await
            .unwrap();
        let context = ShortcutContext {
            cancellation: Arc::default(),
            inbox: Arc::new(Mutex::new(Inbox {
                handler: None,
                status: ShortcutStatus::Registering,
                queue: VecDeque::new(),
                stop_pending: false,
                pressed: [false; 3],
                requested: [true; 3],
                dropped: 0,
            })),
        };
        Self {
            _bus: bus,
            service,
            client,
            state,
            context,
        }
    }

    async fn run(&self, timeouts: Timeouts) -> Result<(), String> {
        run_connected(
            self.client.clone(),
            &default_shortcut_bindings(),
            &self.context,
            timeouts,
        )
        .await
    }

    async fn active(&self) {
        until(|| {
            matches!(
                self.context.inbox.lock().unwrap().status,
                ShortcutStatus::Active(_)
            )
        })
        .await;
    }

    fn cancel(&self) {
        self.context.cancellation().store(true, Ordering::Release);
    }

    fn session(&self) -> OwnedObjectPath {
        self.state.session.lock().unwrap().clone().unwrap()
    }

    async fn event(&self, session: &OwnedObjectPath, id: &str, member: &str) {
        self.service
            .emit_signal(
                None::<&str>,
                DESKTOP_PATH,
                INTERFACE,
                member,
                &(session, id, 1_u64, Dictionary::new()),
            )
            .await
            .unwrap();
    }

    fn assert_closed(&self) {
        assert_eq!(self.state.creates.load(Ordering::Acquire), 1);
        assert!(self.state.session_closes.load(Ordering::Acquire) >= 1);
        assert!(self.state.request_closes.load(Ordering::Acquire) >= 1);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn timeouts() -> Timeouts {
    Timeouts {
        probe: Duration::from_secs(2),
        request: Duration::from_secs(2),
        close: Duration::from_millis(300),
    }
}

async fn until(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("test condition before deadline");
}

#[test]
fn actual_subset_and_desktop_description_are_not_the_preferred_trigger() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        let (result, ()) = tokio::join!(fixture.run(timeouts()), async {
            fixture.active().await;
            assert_eq!(
                fixture.context.inbox.lock().unwrap().status,
                ShortcutStatus::Active(vec![RegisteredShortcut {
                    action: ShortcutAction::StartPause,
                    trigger_description: "Desktop chose Super+R".into(),
                }])
            );
            assert_eq!(
                fixture.state.requested.lock().unwrap()[0].1,
                "CTRL+SHIFT+F7"
            );
            fixture.cancel();
        });
        result.unwrap();
        fixture.assert_closed();
        assert_eq!(fixture.state.binds.load(Ordering::Acquire), 1);
    });
}

#[test]
fn preferred_character_uses_base_layer_keysym_and_separate_shift_modifier() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        let mut bindings = default_shortcut_bindings();
        bindings[0].trigger.key = crate::shortcuts::ShortcutKey::Character('R');
        let (result, ()) = tokio::join!(
            run_connected(
                fixture.client.clone(),
                &bindings,
                &fixture.context,
                timeouts()
            ),
            async {
                fixture.active().await;
                assert_eq!(fixture.state.requested.lock().unwrap()[0].1, "CTRL+SHIFT+r");
                fixture.cancel();
            }
        );
        result.unwrap();
        fixture.assert_closed();
    });
}

#[test]
fn refused_create_and_bind_never_publish_active_and_close_real_objects() {
    runtime().block_on(async {
        for behavior in [Behavior::RefuseCreate, Behavior::RefuseBind] {
            let fixture = Fixture::new(behavior).await;
            let error = fixture.run(timeouts()).await.unwrap_err();
            assert!(
                error.contains("Recorder buttons remain available"),
                "{error}"
            );
            assert_eq!(
                fixture.context.inbox.lock().unwrap().status,
                ShortcutStatus::Stopping
            );
            fixture.assert_closed();
            assert_eq!(
                fixture.state.binds.load(Ordering::Acquire),
                usize::from(matches!(behavior, Behavior::RefuseBind))
            );
        }
    });
}

#[test]
fn pending_response_cancellation_closes_request_and_session_without_authorization() {
    runtime().block_on(async {
        for behavior in [
            Behavior::PendingCreate,
            Behavior::PendingBind,
            Behavior::PendingBindReply,
        ] {
            let fixture = Fixture::new(behavior).await;
            let start = std::time::Instant::now();
            let (result, ()) = tokio::join!(fixture.run(timeouts()), async {
                until(|| {
                    if matches!(behavior, Behavior::PendingBind | Behavior::PendingBindReply) {
                        fixture.state.binds.load(Ordering::Acquire) == 1
                    } else {
                        fixture.state.creates.load(Ordering::Acquire) == 1
                    }
                })
                .await;
                fixture.cancel();
            });
            assert!(result.unwrap_err().contains("cancelled"));
            assert!(start.elapsed() < Duration::from_secs(1));
            fixture.assert_closed();
            assert_eq!(
                fixture.context.inbox.lock().unwrap().status,
                ShortcutStatus::Stopping
            );
        }
    });
}

#[test]
fn missing_response_times_out_and_releases_session() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::PendingBind).await;
        let error = fixture
            .run(Timeouts {
                request: Duration::from_millis(60),
                ..timeouts()
            })
            .await
            .unwrap_err();
        assert!(error.contains("Timed out"), "{error}");
        fixture.assert_closed();
    });
}

#[test]
fn signals_are_session_and_subset_filtered_and_press_edges_keep_wire_order() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        let (result, ()) = tokio::join!(fixture.run(timeouts()), async {
            fixture.active().await;
            let foreign =
                OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/session/foreign/test")
                    .unwrap();
            let own = fixture.session();
            fixture.event(&foreign, "start-pause", "Activated").await;
            fixture
                .service
                .emit_signal(
                    None::<&str>,
                    foreign.as_str(),
                    SESSION_INTERFACE,
                    "Closed",
                    &(Dictionary::new(),),
                )
                .await
                .unwrap();
            // Another bus peer cannot forge the portal's sender identity.
            fixture
                .client
                .emit_signal(
                    None::<&str>,
                    DESKTOP_PATH,
                    INTERFACE,
                    "Activated",
                    &(&own, "start-pause", 1_u64, Dictionary::new()),
                )
                .await
                .unwrap();
            fixture.event(&own, "unknown", "Activated").await;
            fixture.event(&own, "stop", "Activated").await;
            fixture.event(&own, "start-pause", "Activated").await;
            fixture.event(&foreign, "start-pause", "Deactivated").await;
            fixture.event(&own, "start-pause", "Activated").await;
            fixture.event(&own, "start-pause", "Deactivated").await;
            fixture.event(&own, "start-pause", "Deactivated").await;
            fixture.event(&own, "start-pause", "Activated").await;
            until(|| fixture.context.inbox.lock().unwrap().queue.len() == 2).await;
            let inbox = fixture.context.inbox.lock().unwrap();
            assert_eq!(inbox.queue, VecDeque::from([ShortcutAction::StartPause; 2]));
            assert!(!inbox.stop_pending);
            drop(inbox);
            fixture.cancel();
        });
        result.unwrap();
        fixture.assert_closed();
    });
}

#[test]
fn desktop_closed_signal_and_owner_exit_end_active_registration() {
    runtime().block_on(async {
        for owner_exit in [false, true] {
            let fixture = Fixture::new(Behavior::Grant).await;
            let (result, ()) = tokio::join!(fixture.run(timeouts()), async {
                fixture.active().await;
                if owner_exit {
                    fixture.service.clone().close().await.unwrap();
                } else {
                    fixture
                        .service
                        .emit_signal(
                            None::<&str>,
                            fixture.session().as_str(),
                            SESSION_INTERFACE,
                            "Closed",
                            &(Dictionary::new(),),
                        )
                        .await
                        .unwrap();
                }
            });
            let error = result.unwrap_err();
            assert!(
                error.contains(if owner_exit {
                    "disconnected"
                } else {
                    "closed by the desktop"
                }),
                "{error}"
            );
        }
    });
}

#[test]
fn empty_authorized_subset_does_not_invent_bindings_or_actions() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Empty).await;
        let (result, ()) = tokio::join!(fixture.run(timeouts()), async {
            fixture.active().await;
            assert_eq!(
                fixture.context.inbox.lock().unwrap().status,
                ShortcutStatus::Active(vec![])
            );
            fixture.event(&fixture.session(), "stop", "Activated").await;
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(!fixture.context.inbox.lock().unwrap().stop_pending);
            fixture.cancel();
        });
        result.unwrap();
        fixture.assert_closed();
    });
}

#[test]
fn unsupported_interface_reports_error_without_creating_or_binding() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        fixture
            .service
            .object_server()
            .remove::<MockPortal, _>(DESKTOP_PATH)
            .await
            .unwrap();
        let error = fixture.run(timeouts()).await.unwrap_err();
        assert!(error.contains("GlobalShortcuts"), "{error}");
        assert!(
            error.contains("Recorder buttons remain available"),
            "{error}"
        );
        assert_eq!(fixture.state.creates.load(Ordering::Acquire), 0);
        assert_eq!(fixture.state.binds.load(Ordering::Acquire), 0);
    });
}

#[test]
fn early_session_closed_before_create_response_is_observed_without_waiting_for_timeout() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::EarlyClosed).await;
        let error = fixture.run(timeouts()).await.unwrap_err();
        assert!(error.contains("closed by the desktop"), "{error}");
        assert_eq!(fixture.state.binds.load(Ordering::Acquire), 0);
        fixture.assert_closed();
    });
}

#[test]
fn invalid_actual_trigger_is_rejected_instead_of_shown_as_registered() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::InvalidDescription).await;
        assert!(
            fixture
                .run(timeouts())
                .await
                .unwrap_err()
                .contains("invalid trigger")
        );
        assert_eq!(
            fixture.context.inbox.lock().unwrap().status,
            ShortcutStatus::Stopping
        );
        fixture.assert_closed();
    });
}

#[test]
fn unchanged_notification_preserves_held_state_but_changed_binding_ends_session() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        let (result, ()) = tokio::join!(fixture.run(timeouts()), async {
            fixture.active().await;
            let session = fixture.session();
            fixture.event(&session, "start-pause", "Activated").await;
            fixture
                .service
                .emit_signal(
                    None::<&str>,
                    DESKTOP_PATH,
                    INTERFACE,
                    "ShortcutsChanged",
                    &(&session, subset()),
                )
                .await
                .unwrap();
            fixture.event(&session, "start-pause", "Activated").await;
            // A release + new activation is an ordered barrier proving both prior
            // duplicate presses were consumed without creating a third action.
            fixture.event(&session, "start-pause", "Deactivated").await;
            fixture.event(&session, "start-pause", "Activated").await;
            until(|| fixture.context.inbox.lock().unwrap().queue.len() == 2).await;
            let mut changed = subset();
            changed[0].1.insert(
                "trigger_description".into(),
                value("Desktop changed binding"),
            );
            fixture
                .service
                .emit_signal(
                    None::<&str>,
                    DESKTOP_PATH,
                    INTERFACE,
                    "ShortcutsChanged",
                    &(&session, changed),
                )
                .await
                .unwrap();
        });
        assert!(result.unwrap_err().contains("bindings changed"));
        fixture.assert_closed();
    });
}

#[test]
fn lost_client_connection_terminates_instead_of_remaining_active() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        let (result, ()) = tokio::join!(fixture.run(timeouts()), async {
            fixture.active().await;
            fixture.client.clone().close().await.unwrap();
        });
        assert!(result.is_err());
    });
}

#[test]
fn unresponsive_close_is_bounded_and_queued_actions_are_cleared_before_cleanup() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::SlowClose).await;
        let start = std::time::Instant::now();
        let (result, ()) = tokio::join!(
            fixture.run(Timeouts {
                close: Duration::from_millis(30),
                ..timeouts()
            }),
            async {
                fixture.active().await;
                fixture
                    .event(&fixture.session(), "start-pause", "Activated")
                    .await;
                until(|| fixture.context.inbox.lock().unwrap().queue.len() == 1).await;
                fixture.cancel();
                until(|| fixture.state.session_closes.load(Ordering::Acquire) == 1).await;
                let inbox = fixture.context.inbox.lock().unwrap();
                assert!(inbox.queue.is_empty());
                assert!(!inbox.stop_pending);
                assert_eq!(inbox.status, ShortcutStatus::Stopping);
            }
        );
        result.unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        fixture.assert_closed();
        assert!(
            fixture
                .client
                .call_method(
                    Some(DESTINATION),
                    DESKTOP_PATH,
                    Some(INTERFACE),
                    "Unsupported",
                    &()
                )
                .await
                .is_err()
        );
    });
}
