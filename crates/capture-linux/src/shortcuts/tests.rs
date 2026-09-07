use super::*;

fn service() -> (
    GlobalShortcutService,
    ShortcutContext,
    mpsc::SyncSender<Result<(), String>>,
) {
    let inbox = Arc::new(Mutex::new(Inbox {
        handler: None,
        status: ShortcutStatus::Registering,
        queue: VecDeque::with_capacity(QUEUE_LIMIT),
        stop_pending: false,
        pressed: [false; 3],
        requested: [true; 3],
        dropped: 0,
    }));
    let cancellation = Arc::new(AtomicBool::new(false));
    let backend = ShortcutContext {
        cancellation: Arc::clone(&cancellation),
        inbox: Arc::clone(&inbox),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    (
        GlobalShortcutService {
            context: ShortcutContext {
                cancellation,
                inbox,
            },
            result: Some(receiver),
        },
        backend,
        sender,
    )
}

fn confirmed(actions: &[ShortcutAction]) -> Vec<RegisteredShortcut> {
    actions
        .iter()
        .map(|action| RegisteredShortcut {
            action: *action,
            trigger_description: format!("Test {}", action.id()),
        })
        .collect()
}

#[test]
fn modifiers_and_key_ranges_are_validated_before_native_calls() {
    let defaults = default_shortcut_bindings();
    validate_bindings(&defaults).unwrap();
    assert_eq!(defaults[0].trigger.label(), "CTRL+SHIFT+F7");
    assert!(validate_bindings(&[]).is_err());
    let mut bindings = defaults.clone();
    bindings[1].action = bindings[0].action;
    assert!(validate_bindings(&bindings).is_err());
    bindings = defaults.clone();
    bindings[1].trigger = bindings[0].trigger;
    assert!(validate_bindings(&bindings).is_err());
    for key in [
        ShortcutKey::Function(0),
        ShortcutKey::Function(25),
        ShortcutKey::Character('a'),
        ShortcutKey::Character('界'),
    ] {
        let trigger = ShortcutTrigger {
            key,
            ..defaults[0].trigger
        };
        assert!(trigger.validate().is_err());
    }
    let mut trigger = ShortcutTrigger {
        key: ShortcutKey::Character('R'),
        control: false,
        alt: false,
        shift: true,
        super_key: false,
    };
    assert!(trigger.validate().is_err());
    trigger.alt = true;
    assert!(trigger.validate().is_ok());
    assert_eq!(trigger.label(), "ALT+SHIFT+r");
}

#[test]
fn only_confirmed_actions_publish_ordered_press_edges() {
    let (mut service, backend, _sender) = service();
    backend.activated(ShortcutAction::Stop);
    assert!(service.poll().actions.is_empty());
    backend
        .registered(confirmed(&[
            ShortcutAction::Snapshot,
            ShortcutAction::StartPause,
        ]))
        .unwrap();
    backend.activated(ShortcutAction::Snapshot);
    backend.activated(ShortcutAction::Snapshot);
    backend.activated(ShortcutAction::StartPause);
    backend.activated(ShortcutAction::Stop); // Not returned by portal.
    backend.deactivated(ShortcutAction::Snapshot);
    backend.activated(ShortcutAction::Snapshot);
    assert_eq!(
        service.poll().actions,
        [
            ShortcutAction::Snapshot,
            ShortcutAction::StartPause,
            ShortcutAction::Snapshot
        ]
    );
    backend.activated(ShortcutAction::Snapshot);
    assert!(service.poll().actions.is_empty());
}

#[test]
fn queue_is_bounded_and_stop_cannot_be_starved_by_snapshot_flood() {
    let (mut service, backend, _sender) = service();
    backend
        .registered(confirmed(&[ShortcutAction::Snapshot, ShortcutAction::Stop]))
        .unwrap();
    for _ in 0..1_000 {
        backend.activated(ShortcutAction::Snapshot);
        backend.deactivated(ShortcutAction::Snapshot);
    }
    let update = service.poll();
    assert_eq!(update.actions.len(), QUEUE_LIMIT);
    assert_eq!(update.dropped_actions, 1_000 - QUEUE_LIMIT as u64);
    for _ in 0..100 {
        backend.activated(ShortcutAction::Snapshot);
        backend.deactivated(ShortcutAction::Snapshot);
    }
    backend.activated(ShortcutAction::Stop);
    assert_eq!(service.poll().actions, [ShortcutAction::Stop]);
    assert!(service.poll().actions.is_empty());
}

#[test]
fn cancelling_clears_queued_actions_and_waits_for_the_owned_worker() {
    let (mut service, backend, sender) = service();
    backend
        .registered(confirmed(&[ShortcutAction::Stop]))
        .unwrap();
    backend.activated(ShortcutAction::Stop);
    service.stop();
    assert!(backend.cancellation().load(Ordering::Acquire));
    backend.deactivated(ShortcutAction::Stop);
    backend.activated(ShortcutAction::Stop);
    assert!(
        backend
            .registered(confirmed(&[ShortcutAction::Stop]))
            .is_err()
    );
    assert_eq!(service.poll().status, ShortcutStatus::Stopping);
    assert!(service.poll().actions.is_empty());
    assert!(service.is_running());
    sender
        .send(Err("cancelled during permission".into()))
        .unwrap();
    assert_eq!(service.poll().status, ShortcutStatus::Stopped);
    assert!(!service.is_running());
}

#[test]
fn backend_failure_revokes_pending_actions_before_slow_cleanup() {
    let (mut service, backend, sender) = service();
    backend
        .registered(confirmed(&[ShortcutAction::StartPause]))
        .unwrap();
    backend.activated(ShortcutAction::StartPause);
    backend.backend_stopping();
    backend.deactivated(ShortcutAction::StartPause);
    backend.activated(ShortcutAction::StartPause);
    assert!(service.poll().actions.is_empty());
    assert!(service.is_running());
    assert!(!backend.cancelled());
    sender.send(Err("connection lost".into())).unwrap();
    assert_eq!(
        service.poll().status,
        ShortcutStatus::Failed("connection lost".into())
    );
    assert!(!service.is_running());
}

#[test]
fn invalid_registration_and_unexpected_worker_exit_never_publish_actions() {
    let (mut service, backend, sender) = service();
    assert!(
        backend
            .registered(confirmed(&[ShortcutAction::Stop, ShortcutAction::Stop]))
            .is_err()
    );
    for label in [String::new(), "x".repeat(257), "F7\nSpoofed status".into()] {
        assert!(
            backend
                .registered(vec![RegisteredShortcut {
                    action: ShortcutAction::Stop,
                    trigger_description: label
                }])
                .is_err()
        );
    }
    backend
        .registered(confirmed(&[ShortcutAction::Snapshot]))
        .unwrap();
    backend.activated(ShortcutAction::Snapshot);
    drop(sender);
    let update = service.poll();
    assert!(matches!(update.status, ShortcutStatus::Failed(_)));
    assert!(update.actions.is_empty());
}

#[test]
fn active_recording_handler_does_not_wait_for_a_visible_window_or_ui_poll() {
    let (mut service, backend, _sender) = service();
    backend
        .registered(confirmed(&[ShortcutAction::Stop, ShortcutAction::Snapshot]))
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let received = Arc::clone(&calls);
    service.set_action_handler(Some(Arc::new(move |action| {
        received.lock().unwrap().push(action);
        true
    })));
    backend.activated(ShortcutAction::Snapshot);
    backend.deactivated(ShortcutAction::Snapshot);
    backend.activated(ShortcutAction::Stop);
    assert_eq!(
        *calls.lock().unwrap(),
        [ShortcutAction::Snapshot, ShortcutAction::Stop]
    );
    assert!(service.poll().actions.is_empty());
    backend.backend_stopping();
    backend.deactivated(ShortcutAction::Stop);
    backend.activated(ShortcutAction::Stop);
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[test]
fn switching_action_destinations_discards_older_ui_events() {
    let (mut service, backend, _sender) = service();
    backend
        .registered(confirmed(&[ShortcutAction::StartPause]))
        .unwrap();
    backend.activated(ShortcutAction::StartPause);
    service.set_action_handler(Some(Arc::new(|_| false)));
    assert!(service.poll().actions.is_empty());
    backend.deactivated(ShortcutAction::StartPause);
    backend.activated(ShortcutAction::StartPause);
    assert_eq!(service.poll().actions, [ShortcutAction::StartPause]);
    service.stop();
    service.set_action_handler(Some(Arc::new(|_| {
        panic!("cancelled service invoked a new handler")
    })));
    backend.deactivated(ShortcutAction::StartPause);
    backend.activated(ShortcutAction::StartPause);
    assert!(service.poll().actions.is_empty());
}
