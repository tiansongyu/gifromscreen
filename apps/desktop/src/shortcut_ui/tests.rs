use super::*;
use gif_from_screen_capture_linux::RegisteredShortcut;
use std::sync::{Arc, Mutex};

fn localizer(tag: &str) -> Localizer {
    Localizer::new(gif_from_screen_localization::find_language(tag).unwrap())
}

struct FakeState {
    running: bool,
    status: ShortcutStatus,
    actions: Vec<ShortcutAction>,
    stops: usize,
    handler: Option<ShortcutActionHandler>,
    dropped_actions: u64,
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
            dropped_actions: std::mem::take(&mut state.dropped_actions),
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
                dropped_actions: 0,
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
    assert!(
        tool.status_summary(localizer("en"))
            .unwrap()
            .contains("conflict")
    );
    tool.retry();
    poll(&mut tool, true);
    active(&slot(&starts, 1), 2);
    poll(&mut tool, true);
    assert!(
        tool.status_summary(localizer("en"))
            .unwrap()
            .contains("2/3")
    );
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
    draw_localized(context, tool, events, localizer("en"))
}

fn draw_localized(
    context: &egui::Context,
    tool: &mut ShortcutTool,
    events: Vec<egui::Event>,
    localizer: Localizer,
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
                tool.show(ui, Some(LinuxDisplayServer::X11), localizer);
            });
        },
    )
}

fn click_localized(
    context: &egui::Context,
    tool: &mut ShortcutTool,
    position: egui::Pos2,
    localizer: Localizer,
) {
    for pressed in [true, false] {
        draw_localized(
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
            localizer,
        );
    }
}

#[test]
fn small_window_large_fonts_keep_enable_cancel_disable_and_retry_clickable() {
    exercise_registration_buttons(localizer("en"));
}

#[test]
fn chinese_real_fonts_keep_enable_cancel_disable_and_retry_clickable() {
    exercise_registration_buttons(localizer("zh"));
}

fn exercise_registration_buttons(localizer: Localizer) {
    for scale in [1.0, 1.5, 2.0] {
        let context = egui::Context::default();
        crate::preferences::fonts::install(&context);
        context.style_mut(|style| {
            style.animation_time = 0.0;
            for font in style.text_styles.values_mut() {
                font.size *= scale;
            }
        });
        let (mut tool, starts) = tool();
        let output = draw_localized(&context, &mut tool, Vec::new(), localizer);
        let enable = visible_text(&output, localizer.text(Message::ShortcutsEnable))
            .expect("enable must be visible");
        click_localized(&context, &mut tool, enable.center(), localizer);
        assert!(tool.settings.enabled);
        tool.poll(&context, Some(LinuxDisplayServer::X11), true);
        let output = draw_localized(&context, &mut tool, Vec::new(), localizer);
        let cancel = visible_text(
            &output,
            localizer.text(Message::ShortcutsCancelRegistration),
        )
        .expect("cancel must stay visible");
        assert!(visible_text(&output, localizer.text(Message::ShortcutsDisable)).is_some());
        click_localized(&context, &mut tool, cancel.center(), localizer);
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
        let output = draw_localized(&context, &mut tool, Vec::new(), localizer);
        let retry = visible_text(&output, localizer.text(Message::ShortcutsRetryRegistration))
            .expect("retry must stay visible");
        click_localized(&context, &mut tool, retry.center(), localizer);
        poll(&mut tool, true);
        assert_eq!(starts.lock().unwrap().slots.len(), 3);
    }
}

#[test]
fn live_registration_and_existing_notices_translate_without_rewriting_os_tokens() {
    let (mut tool, starts) = tool();
    let context = egui::Context::default();
    crate::preferences::fonts::install(&context);
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let requested = tool.settings.bindings.clone();
    let raw = "Ctrl+Shift+F7 / 系统 {action}";
    let bindings = vec![RegisteredShortcut {
        action: ShortcutAction::StartPause,
        trigger_description: raw.into(),
    }];
    slot(&starts, 0).lock().unwrap().status = ShortcutStatus::Active(bindings.clone());
    slot(&starts, 0).lock().unwrap().dropped_actions = 9;
    poll(&mut tool, true);
    for language in [localizer("en"), localizer("zh"), localizer("en")] {
        assert_eq!(
            tool.status_summary(language).unwrap(),
            language
                .format(Message::ShortcutsSummaryRegistered, &[("count", "1")])
                .unwrap()
        );
        assert_eq!(
            tool.notice.as_ref().unwrap().render(language),
            language
                .format(Message::ShortcutsQueueOverflow, &[("count", "9")])
                .unwrap()
        );
        let output = draw_localized(&context, &mut tool, Vec::new(), language);
        assert!(
            visible_text(&output, language.text(Message::ShortcutsActuallyRegistered)).is_some()
        );
        let actual = language
            .format(
                Message::ShortcutsRegisteredBinding,
                &[
                    ("action", language.text(Message::ShortcutsActionStartPause)),
                    ("trigger", raw),
                ],
            )
            .unwrap();
        assert!(visible_text(&output, &actual).is_some());
        let missing = language
            .format(
                Message::ShortcutsMissingBinding,
                &[("action", language.text(Message::RecorderStop))],
            )
            .unwrap();
        assert!(visible_text(&output, &missing).is_some());
        assert_eq!(tool.settings.bindings, requested);
        assert_eq!(tool.draft_bindings, requested);
        assert!(matches!(&tool.status, ShortcutStatus::Active(actual) if actual == &bindings));
    }
    assert_eq!(starts.lock().unwrap().slots.len(), 1);
    assert_eq!(slot(&starts, 0).lock().unwrap().stops, 0);
}

#[test]
fn failed_and_inactive_summaries_translate_but_leave_diagnostics_literal() {
    let (mut tool, _) = tool();
    assert!(tool.status_summary(localizer("zh")).is_none());
    tool.settings.enabled = true;
    for (status, message) in [
        (
            ShortcutStatus::Registering,
            Message::ShortcutsSummaryPending,
        ),
        (ShortcutStatus::Stopping, Message::ShortcutsSummaryStopping),
        (ShortcutStatus::Stopped, Message::ShortcutsSummaryInactive),
    ] {
        tool.status = status;
        for language in [localizer("en"), localizer("zh")] {
            assert_eq!(
                tool.status_summary(language).unwrap(),
                language.text(message)
            );
        }
    }
    let raw = "Denied Ctrl+Shift+F7: /tmp/快捷键/{error}";
    tool.status = ShortcutStatus::Failed(raw.into());
    assert_eq!(
        tool.status_summary(localizer("zh")).unwrap(),
        localizer("zh")
            .format(Message::ShortcutsSummaryFailed, &[("error", raw)])
            .unwrap()
    );
}

#[test]
fn invalid_drafts_have_typed_warnings_but_disabling_still_uses_valid_applied_bindings() {
    let (mut tool, starts) = tool();
    tool.set_enabled(true).unwrap();
    poll(&mut tool, true);
    let applied = tool.settings.bindings.clone();
    tool.draft_bindings[0].trigger = ShortcutTrigger {
        key: ShortcutKey::Character('A'),
        control: false,
        shift: true,
        alt: false,
        super_key: false,
    };
    let error = tool.apply_bindings().unwrap_err();
    assert_eq!(error.message_id(), Some(Message::ShortcutsInvalidTrigger));
    assert_eq!(
        error.render(localizer("zh")),
        localizer("zh").text(Message::ShortcutsInvalidTrigger)
    );
    assert_eq!(tool.settings.bindings, applied);
    assert_eq!(slot(&starts, 0).lock().unwrap().stops, 0);
    tool.draft_bindings = applied.clone();
    tool.draft_bindings[1].trigger = tool.draft_bindings[0].trigger;
    let error = tool.apply_bindings().unwrap_err();
    assert_eq!(error.message_id(), Some(Message::ShortcutsUniqueBindings));
    tool.set_enabled(false).unwrap();
    assert!(!tool.settings.enabled);
    assert_eq!(tool.settings.bindings, applied);
    assert_eq!(slot(&starts, 0).lock().unwrap().stops, 1);
    tool.draft_bindings.pop();
    assert_eq!(
        tool.apply_bindings().unwrap_err().message_id(),
        Some(Message::ShortcutsConfigureAll)
    );
}

#[test]
fn localized_action_buttons_keep_semantic_ids_when_caption_or_state_changes_during_press() {
    let context = egui::Context::default();
    crate::preferences::fonts::install(&context);
    let frame = |message, id, events| {
        let mut clicked = false;
        let output = context.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    clicked = view::action_button(ui, id, message, true);
                });
            },
        );
        (output, clicked)
    };
    let press = localizer("en").text(Message::ShortcutsCancelRegistration);
    let release = localizer("zh").text(Message::ShortcutsRetryRegistration);
    let first = frame(press, "cancel", Vec::new()).0;
    let position = visible_text(&first, press).unwrap().left_center() + egui::vec2(2.0, 0.0);
    let event = |pressed| {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    };
    frame(press, "cancel", event(true));
    let (output, clicked) = frame(release, "retry", event(false));
    assert!(
        visible_text(&output, release).unwrap().contains(position),
        "release really hits the replacement button"
    );
    assert!(!clicked, "old Cancel press cannot become translated Retry");
    frame(press, "cancel", Vec::new());
    frame(press, "cancel", event(true));
    let same_action = localizer("zh").text(Message::ShortcutsCancelRegistration);
    let (_, clicked) = frame(same_action, "cancel", event(false));
    assert!(
        clicked,
        "changing only the language retains the same action ID"
    );
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
