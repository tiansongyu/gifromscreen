use super::*;
use gif_from_screen_capture_linux::RegisteredShortcut;
use std::sync::{Arc, Mutex};

struct FakeState {
    running: bool,
    status: ShortcutStatus,
    actions: Vec<ShortcutAction>,
    stops: usize,
    handler: Option<ShortcutActionHandler>,
}
struct Fake(Arc<Mutex<FakeState>>);
impl Registration for Fake {
    fn set_action_handler(&self, handler: Option<ShortcutActionHandler>) {
        self.0.lock().unwrap().handler = handler;
    }
    fn stop(&self) {
        let mut state = self.0.lock().unwrap();
        state.stops += 1;
        state.status = ShortcutStatus::Stopping;
    }
    fn is_running(&self) -> bool {
        self.0.lock().unwrap().running
    }
    fn poll(&mut self) -> ShortcutUpdate {
        let mut state = self.0.lock().unwrap();
        ShortcutUpdate {
            status: state.status.clone(),
            actions: std::mem::take(&mut state.actions),
            dropped_actions: 0,
        }
    }
}

#[derive(Default)]
struct Starts {
    slots: Vec<Arc<Mutex<FakeState>>>,
    bindings: Vec<Vec<ShortcutBinding>>,
    displays: Vec<LinuxDisplayServer>,
}

fn tool() -> (ShortcutTool, Arc<Mutex<Starts>>) {
    tool_with_io(SettingsIo::without_store())
}

fn tool_with_io(io: SettingsIo) -> (ShortcutTool, Arc<Mutex<Starts>>) {
    let starts = Arc::new(Mutex::new(Starts::default()));
    let output = Arc::clone(&starts);
    let tool = ShortcutTool::new(
        io,
        Box::new(move |display, bindings| {
            let state = Arc::new(Mutex::new(FakeState {
                running: true,
                status: ShortcutStatus::Registering,
                actions: Vec::new(),
                stops: 0,
                handler: None,
            }));
            let mut starts = output.lock().unwrap();
            starts.slots.push(Arc::clone(&state));
            starts.bindings.push(bindings);
            starts.displays.push(display);
            Ok(Box::new(Fake(state)))
        }),
    );
    (tool, starts)
}

fn slot(starts: &Arc<Mutex<Starts>>, index: usize) -> Arc<Mutex<FakeState>> {
    Arc::clone(&starts.lock().unwrap().slots[index])
}
fn active(state: &Arc<Mutex<FakeState>>, count: usize) {
    state.lock().unwrap().status = ShortcutStatus::Active(
        default_shortcut_bindings()
            .into_iter()
            .take(count)
            .map(|binding| RegisteredShortcut {
                action: binding.action,
                trigger_description: format!("Confirmed {}", binding.trigger.label()),
            })
            .collect(),
    );
}
fn terminal(state: &Arc<Mutex<FakeState>>) {
    let mut state = state.lock().unwrap();
    state.running = false;
    state.status = ShortcutStatus::Stopped;
}
fn poll(tool: &mut ShortcutTool, scope: bool) -> Vec<ShortcutAction> {
    tool.poll(
        &egui::Context::default(),
        Some(LinuxDisplayServer::X11),
        scope,
    )
}

#[test]
fn default_off_and_enabled_launcher_never_start_registration() {
    let (mut tool, starts) = tool();
    assert_eq!(tool.settings.bindings, default_shortcut_bindings());
    assert!(poll(&mut tool, true).is_empty());
    assert!(starts.lock().unwrap().slots.is_empty());
    tool.set_enabled(true).unwrap();
    assert!(poll(&mut tool, false).is_empty());
    assert!(starts.lock().unwrap().slots.is_empty());
    poll(&mut tool, true);
    assert_eq!(starts.lock().unwrap().slots.len(), 1);
    active(&slot(&starts, 0), 3);
    slot(&starts, 0).lock().unwrap().actions =
        vec![ShortcutAction::StartPause, ShortcutAction::Snapshot];
    assert_eq!(
        poll(&mut tool, true),
        [ShortcutAction::StartPause, ShortcutAction::Snapshot]
    );
}

#[test]
fn disable_and_scope_reset_drop_old_actions_before_any_new_registration() {
    let (mut tool, starts) = tool();
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let old = slot(&starts, 0);
    active(&old, 3);
    old.lock().unwrap().actions = vec![ShortcutAction::Stop];
    tool.set_enabled(false).unwrap();
    assert_eq!(old.lock().unwrap().stops, 1);
    assert!(poll(&mut tool, true).is_empty());
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    assert_eq!(starts.lock().unwrap().slots.len(), 1);
    terminal(&old);
    poll(&mut tool, true);
    assert_eq!(starts.lock().unwrap().slots.len(), 2);
    let current = slot(&starts, 1);
    active(&current, 3);
    current.lock().unwrap().actions = vec![ShortcutAction::StartPause];
    tool.reset_recording_scope();
    assert!(poll(&mut tool, true).is_empty());
    assert_eq!(current.lock().unwrap().stops, 1);
    assert_eq!(starts.lock().unwrap().slots.len(), 2);
}

#[test]
fn applying_bindings_waits_for_old_terminal_and_does_not_leak_its_events() {
    let (mut tool, starts) = tool();
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let old = slot(&starts, 0);
    active(&old, 3);
    tool.draft_bindings[0].trigger.key = ShortcutKey::Function(12);
    tool.apply_bindings().unwrap();
    old.lock().unwrap().actions = vec![ShortcutAction::Stop];
    for _ in 0..4 {
        assert!(poll(&mut tool, true).is_empty());
    }
    assert_eq!(starts.lock().unwrap().slots.len(), 1);
    terminal(&old);
    assert!(poll(&mut tool, true).is_empty());
    assert_eq!(
        starts.lock().unwrap().bindings[1][0].trigger.key,
        ShortcutKey::Function(12)
    );
    active(&slot(&starts, 1), 3);
    old.lock().unwrap().actions = vec![ShortcutAction::Snapshot];
    assert!(poll(&mut tool, true).is_empty());
}

#[test]
fn failure_requires_explicit_retry_and_partial_registration_is_preserved() {
    let (mut tool, starts) = tool();
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let first = slot(&starts, 0);
    {
        let mut state = first.lock().unwrap();
        state.running = false;
        state.status = ShortcutStatus::Failed("Key conflict".into());
    }
    poll(&mut tool, true);
    for _ in 0..10 {
        poll(&mut tool, true);
    }
    assert_eq!(starts.lock().unwrap().slots.len(), 1);
    assert!(tool.status_summary().unwrap().contains("conflict"));
    tool.retry();
    poll(&mut tool, true);
    active(&slot(&starts, 1), 2);
    poll(&mut tool, true);
    assert!(tool.status_summary().unwrap().contains("2/3"));
    assert!(matches!(&tool.status, ShortcutStatus::Active(bindings) if bindings.len() == 2));
}

#[test]
fn leaving_scope_switching_display_and_shutdown_never_forward_stale_actions() {
    let (mut tool, starts) = tool();
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let first = slot(&starts, 0);
    active(&first, 3);
    first.lock().unwrap().actions = vec![ShortcutAction::Stop];
    assert!(poll(&mut tool, false).is_empty());
    terminal(&first);
    poll(&mut tool, false);
    assert!(!tool.is_active());
    tool.poll(
        &egui::Context::default(),
        Some(LinuxDisplayServer::Wayland),
        true,
    );
    assert_eq!(
        starts.lock().unwrap().displays[1],
        LinuxDisplayServer::Wayland
    );
    let second = slot(&starts, 1);
    active(&second, 3);
    second.lock().unwrap().actions = vec![ShortcutAction::Snapshot];
    tool.shutdown();
    assert!(
        tool.poll(
            &egui::Context::default(),
            Some(LinuxDisplayServer::Wayland),
            true
        )
        .is_empty()
    );
    assert!(tool.is_active());
    terminal(&second);
    tool.poll(
        &egui::Context::default(),
        Some(LinuxDisplayServer::Wayland),
        true,
    );
    assert!(!tool.is_active());
    assert_eq!(starts.lock().unwrap().slots.len(), 2);
    assert!(
        tool.settings.enabled,
        "shutdown must preserve the next-run preference"
    );
}

#[test]
fn invalid_edit_does_not_replace_registered_bindings() {
    let (mut tool, starts) = tool();
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let old = tool.settings.bindings.clone();
    tool.draft_bindings[1].trigger = tool.draft_bindings[0].trigger;
    assert!(tool.apply_bindings().is_err());
    assert_eq!(tool.settings.bindings, old);
    assert_eq!(slot(&starts, 0).lock().unwrap().stops, 0);
}

#[test]
fn loaded_enabled_preference_waits_for_recorder_scope_and_shutdown_flushes_edits() {
    use std::time::{Duration, Instant};
    let directory = tempfile::tempdir().unwrap();
    let store = store::Store::new(directory.path().join("shortcuts.json"));
    store
        .save(
            &store.load().unwrap(),
            Settings {
                enabled: true,
                ..Settings::default()
            },
        )
        .unwrap();
    let (mut tool, starts) = tool_with_io(SettingsIo::new(Some(store.clone())));
    let deadline = Instant::now() + Duration::from_secs(3);
    while tool.is_active() {
        assert!(poll(&mut tool, false).is_empty());
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(tool.settings.enabled);
    assert!(starts.lock().unwrap().slots.is_empty());
    tool.set_enabled(false).unwrap();
    tool.shutdown();
    while tool.is_active() {
        assert!(poll(&mut tool, true).is_empty());
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(!store.load().unwrap().settings.enabled);
    assert!(starts.lock().unwrap().slots.is_empty());
}

fn visible_text(output: &egui::FullOutput, wanted: &str) -> Option<egui::Rect> {
    output.shapes.iter().find_map(|shape| {
        if let egui::Shape::Text(text) = &shape.shape
            && text.galley.text() == wanted
        {
            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            shape.clip_rect.contains_rect(rect).then_some(rect)
        } else {
            None
        }
    })
}

fn draw(
    context: &egui::Context,
    tool: &mut ShortcutTool,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 480.0),
            )),
            events,
            focused: true,
            ..egui::RawInput::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                tool.show(ui, Some(LinuxDisplayServer::X11));
            });
        },
    )
}

fn click(context: &egui::Context, tool: &mut ShortcutTool, position: egui::Pos2) {
    for pressed in [true, false] {
        draw(
            context,
            tool,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

#[test]
fn small_window_large_fonts_keep_enable_cancel_disable_and_retry_clickable() {
    for scale in [1.0, 1.5, 2.0] {
        let context = egui::Context::default();
        context.style_mut(|style| {
            style.animation_time = 0.0;
            for font in style.text_styles.values_mut() {
                font.size *= scale;
            }
        });
        let (mut tool, starts) = tool();
        let output = draw(&context, &mut tool, Vec::new());
        let enable = visible_text(&output, "Enable shortcuts").expect("enable must be visible");
        click(&context, &mut tool, enable.center());
        assert!(tool.settings.enabled);
        tool.poll(&context, Some(LinuxDisplayServer::X11), true);
        let output = draw(&context, &mut tool, Vec::new());
        let cancel =
            visible_text(&output, "Cancel registration").expect("cancel must stay visible");
        assert!(visible_text(&output, "Disable shortcuts").is_some());
        click(&context, &mut tool, cancel.center());
        assert!(!tool.settings.enabled);
        assert_eq!(slot(&starts, 0).lock().unwrap().stops, 1);
        terminal(&slot(&starts, 0));
        poll(&mut tool, true);
        tool.set_enabled(true).unwrap();
        poll(&mut tool, true);
        let current = slot(&starts, 1);
        {
            let mut state = current.lock().unwrap();
            state.status = ShortcutStatus::Failed("Permission denied".into());
            state.running = false;
        }
        poll(&mut tool, true);
        let output = draw(&context, &mut tool, Vec::new());
        let retry = visible_text(&output, "Retry registration").expect("retry must stay visible");
        click(&context, &mut tool, retry.center());
        poll(&mut tool, true);
        assert_eq!(starts.lock().unwrap().slots.len(), 3);
    }
}

#[test]
fn failed_start_does_not_retry_until_requested_and_no_display_never_starts() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&attempts);
    let mut tool = ShortcutTool::new(
        SettingsIo::without_store(),
        Box::new(move |_, _| {
            observed.fetch_add(1, Ordering::Relaxed);
            Err("Could not allocate a shortcut worker".into())
        }),
    );
    tool.set_enabled(true).unwrap();
    assert!(tool.poll(&egui::Context::default(), None, true).is_empty());
    assert_eq!(attempts.load(Ordering::Relaxed), 0);
    for _ in 0..5 {
        assert!(poll(&mut tool, true).is_empty());
    }
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    tool.retry();
    poll(&mut tool, true);
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
}

#[test]
fn registration_status_change_cannot_turn_a_cancel_press_into_retry() {
    let context = egui::Context::default();
    let (mut tool, starts) = tool();
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let output = draw(&context, &mut tool, Vec::new());
    let position = visible_text(&output, "Cancel registration")
        .unwrap()
        .center();
    draw(
        &context,
        &mut tool,
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    let state = slot(&starts, 0);
    {
        let mut state = state.lock().unwrap();
        state.running = false;
        state.status = ShortcutStatus::Failed("Permission denied".into());
    }
    poll(&mut tool, true);
    draw(
        &context,
        &mut tool,
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    poll(&mut tool, true);
    assert_eq!(
        starts.lock().unwrap().slots.len(),
        1,
        "a stale cancel release must not trigger retry"
    );
    assert!(matches!(tool.status, ShortcutStatus::Failed(_)));
}

#[test]
fn recording_handler_reaches_current_and_future_slots_and_is_cleared_at_boundaries() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (mut tool, starts) = tool();
    let received = Arc::new(AtomicUsize::new(0));
    let output = Arc::clone(&received);
    tool.set_recording_handler(Some(Arc::new(move |_| {
        output.fetch_add(1, Ordering::Relaxed);
        true
    })));
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let first = slot(&starts, 0);
    let handler = first.lock().unwrap().handler.clone().unwrap();
    assert!(handler(ShortcutAction::Stop));
    assert_eq!(received.load(Ordering::Relaxed), 1);
    tool.draft_bindings[0].trigger.key = ShortcutKey::Function(12);
    tool.apply_bindings().unwrap();
    terminal(&first);
    poll(&mut tool, true);
    let second = slot(&starts, 1);
    assert!(second.lock().unwrap().handler.is_some());
    tool.set_recording_handler(None);
    assert!(second.lock().unwrap().handler.is_none());
    tool.set_recording_handler(Some(handler));
    assert!(second.lock().unwrap().handler.is_some());
    tool.reset_recording_scope();
    assert!(second.lock().unwrap().handler.is_none());
    assert!(tool.recording_handler.is_none());
    terminal(&second);
    poll(&mut tool, true);
    assert!(slot(&starts, 2).lock().unwrap().handler.is_none());
    tool.shutdown();
}
