//! Identity ordering on a private bus, not a substitute for desktop acceptance.

use super::*;

#[derive(Clone, Copy)]
enum RegistryMode {
    Grant,
    Denied,
    MissingDesktop,
    Pending,
    UnknownMethod,
    UnknownInterface,
    ServiceUnknown,
}

#[derive(Default)]
struct RegistryState {
    calls: AtomicUsize,
    peers: Mutex<HashMap<String, String>>,
}

struct MockRegistry {
    mode: RegistryMode,
    version: u32,
    registry: Arc<RegistryState>,
    portal: Arc<State>,
}

#[ashpd::zbus::interface(name = "org.freedesktop.host.portal.Registry", crate = "ashpd::zbus")]
impl MockRegistry {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        self.version
    }

    async fn register(
        &self,
        app_id: String,
        options: Dictionary,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.portal.calls.lock().unwrap().push("registry");
        self.registry.calls.fetch_add(1, Ordering::AcqRel);
        assert!(options.is_empty());
        if app_id != crate::APPLICATION_ID {
            return Err(zbus::fdo::Error::InvalidArgs("Wrong application ID".into()));
        }
        match self.mode {
            RegistryMode::Denied => {
                return Err(zbus::fdo::Error::AccessDenied(
                    "Registry registration denied".into(),
                ));
            }
            RegistryMode::MissingDesktop => {
                return Err(zbus::fdo::Error::Failed(
                    "App info not found for application".into(),
                ));
            }
            RegistryMode::Pending => {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            RegistryMode::UnknownMethod => {
                return Err(zbus::fdo::Error::UnknownMethod(
                    "Registry.Register unavailable".into(),
                ));
            }
            RegistryMode::UnknownInterface => {
                return Err(zbus::fdo::Error::UnknownInterface(
                    "Registry unavailable".into(),
                ));
            }
            RegistryMode::ServiceUnknown => {
                return Err(zbus::fdo::Error::ServiceUnknown(
                    "Portal service vanished".into(),
                ));
            }
            RegistryMode::Grant => {}
        }
        let sender = header.sender().unwrap().as_str().to_owned();
        let mut peers = self.registry.peers.lock().unwrap();
        if peers.contains_key(&sender) {
            return Err(zbus::fdo::Error::Failed(
                "Connection already associated with an application ID".into(),
            ));
        }
        peers.insert(sender, app_id);
        Ok(())
    }
}

async fn attach(fixture: &Fixture, mode: RegistryMode) -> Arc<RegistryState> {
    let registry = Arc::new(RegistryState::default());
    fixture
        .service
        .object_server()
        .at(
            DESKTOP_PATH,
            MockRegistry {
                mode,
                version: 1,
                registry: Arc::clone(&registry),
                portal: Arc::clone(&fixture.state),
            },
        )
        .await
        .unwrap();
    registry
}

pub(super) async fn install_supported_registry(fixture: &Fixture) {
    attach(fixture, RegistryMode::Grant).await;
}

async fn granted_run(fixture: &Fixture, connection: Connection) {
    let bindings = default_shortcut_bindings();
    let (result, ()) = tokio::join!(
        run_connected(connection, &bindings, &fixture.context, timeouts()),
        async {
            fixture.active().await;
            fixture.cancel();
        }
    );
    result.unwrap();
}

#[test]
fn each_dedicated_connection_registers_once_before_any_global_portal_call() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        let registry = attach(&fixture, RegistryMode::Grant).await;
        granted_run(&fixture, fixture.client.clone()).await;
        let calls = fixture.state.calls.lock().unwrap().clone();
        assert_eq!(calls.first(), Some(&"registry"));
        assert!(calls.iter().position(|call| *call == "global").unwrap() > 0);
        assert_eq!(registry.calls.load(Ordering::Acquire), 1);
        assert_eq!(
            registry.peers.lock().unwrap().values().next().unwrap(),
            crate::APPLICATION_ID
        );

        fixture
            .context
            .cancellation()
            .store(false, Ordering::Release);
        fixture.state.calls.lock().unwrap().clear();
        let second = zbus::connection::Builder::address(fixture.bus.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        granted_run(&fixture, second).await;
        assert_eq!(
            fixture.state.calls.lock().unwrap().first(),
            Some(&"registry")
        );
        assert_eq!(registry.calls.load(Ordering::Acquire), 2);
        assert_eq!(
            registry.peers.lock().unwrap().len(),
            2,
            "identity is per unique bus peer, not once per process"
        );
        assert_eq!(fixture.state.creates.load(Ordering::Acquire), 2);
        assert_eq!(fixture.state.binds.load(Ordering::Acquire), 2);
    });
}

#[test]
fn missing_registry_interface_and_exact_missing_methods_remain_compatible() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        granted_run(&fixture, fixture.client.clone()).await;
        assert_eq!(fixture.state.calls.lock().unwrap().first(), Some(&"global"));
        for mode in [RegistryMode::UnknownMethod, RegistryMode::UnknownInterface] {
            let fixture = Fixture::new(Behavior::Grant).await;
            let registry = attach(&fixture, mode).await;
            granted_run(&fixture, fixture.client.clone()).await;
            assert_eq!(registry.calls.load(Ordering::Acquire), 1);
            assert_eq!(fixture.state.creates.load(Ordering::Acquire), 1);
        }
    });
}

#[test]
fn denied_missing_desktop_and_missing_service_do_not_fall_through_to_global_calls() {
    runtime().block_on(async {
        for (mode, expected) in [
            (RegistryMode::Denied, "denied"),
            (RegistryMode::MissingDesktop, "App info not found"),
            (RegistryMode::ServiceUnknown, "Portal service vanished"),
        ] {
            let fixture = Fixture::new(Behavior::Grant).await;
            let registry = attach(&fixture, mode).await;
            let error = fixture.run(timeouts()).await.unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(
                error.contains(&format!("{}.desktop", crate::APPLICATION_ID)),
                "{error}"
            );
            assert_eq!(fixture.state.calls.lock().unwrap().as_slice(), ["registry"]);
            assert_eq!(registry.calls.load(Ordering::Acquire), 1);
            assert_eq!(fixture.state.creates.load(Ordering::Acquire), 0);
        }
    });
}

#[test]
fn duplicate_registration_is_an_error_not_a_missing_interface_fallback() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        let registry = attach(&fixture, RegistryMode::Grant).await;
        super::super::identity::register(&fixture.client, crate::APPLICATION_ID)
            .await
            .unwrap();
        let error = fixture.run(timeouts()).await.unwrap_err();
        assert!(error.contains("already associated"), "{error}");
        assert_eq!(registry.calls.load(Ordering::Acquire), 2);
        assert_eq!(fixture.state.creates.load(Ordering::Acquire), 0);
        assert!(!fixture.state.calls.lock().unwrap().contains(&"global"));
    });
}

#[test]
fn invalid_id_is_rejected_before_bus_and_wrong_valid_id_remains_an_error() {
    runtime().block_on(async {
        let fixture = Fixture::new(Behavior::Grant).await;
        let registry = attach(&fixture, RegistryMode::Grant).await;
        assert!(
            super::super::identity::register(&fixture.client, "not an app id")
                .await
                .unwrap_err()
                .contains("Invalid portal application ID")
        );
        assert_eq!(registry.calls.load(Ordering::Acquire), 0);
        let error = super::super::identity::register(&fixture.client, "org.example.WrongApp")
            .await
            .unwrap_err();
        assert!(error.contains("Wrong application ID"));
        assert_eq!(registry.calls.load(Ordering::Acquire), 1);
        assert!(registry.peers.lock().unwrap().is_empty());
        assert_eq!(fixture.state.creates.load(Ordering::Acquire), 0);
    });
}

#[test]
fn identity_cancellation_and_timeout_disconnect_without_creating_a_portal_session() {
    runtime().block_on(async {
        for cancel in [true, false] {
            let fixture = Fixture::new(Behavior::Grant).await;
            let registry = attach(&fixture, RegistryMode::Pending).await;
            let bounds = Timeouts {
                probe: Duration::from_millis(80),
                ..timeouts()
            };
            let started = std::time::Instant::now();
            let (result, ()) = tokio::join!(fixture.run(bounds), async {
                until(|| registry.calls.load(Ordering::Acquire) == 1).await;
                if cancel {
                    fixture.cancel();
                }
            });
            let error = result.unwrap_err();
            assert!(
                error.contains(if cancel { "cancelled" } else { "Timed out" }),
                "{error}"
            );
            assert!(started.elapsed() < Duration::from_secs(1));
            assert_eq!(fixture.state.calls.lock().unwrap().as_slice(), ["registry"]);
            assert_eq!(fixture.state.request_closes.load(Ordering::Acquire), 0);
            assert_eq!(fixture.state.session_closes.load(Ordering::Acquire), 0);
            let bus = zbus::fdo::DBusProxy::new(&fixture.service).await.unwrap();
            assert!(
                !bus.name_has_owner(
                    fixture
                        .client
                        .unique_name()
                        .unwrap()
                        .as_str()
                        .try_into()
                        .unwrap()
                )
                .await
                .unwrap()
            );
        }
    });
}
