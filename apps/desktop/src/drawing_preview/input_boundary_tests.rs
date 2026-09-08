use eframe::egui;

use super::{InputBoundary, MAX_INPUT_EVENTS, MAX_RETAINED_BYTES, Pending, retained_size};
use crate::editor_ui::{DrawingDraftPhase, DrawingOverlayDraft};

fn raw(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 300.0),
        )),
        events,
        ..egui::RawInput::default()
    }
}

fn primary(pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: egui::pos2(80.0, 80.0),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn draw(context: &egui::Context, input: egui::RawInput, draft: &mut DrawingOverlayDraft) {
    let _ = context.run(input, |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let rect = egui::Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(200.0, 120.0));
            let response = ui.interact(
                rect,
                egui::Id::new("boundary-image"),
                egui::Sense::click_and_drag(),
            );
            crate::drawing_preview::update(ui, &response, [200, 120], true, draft);
        });
    });
}

fn armed() -> (egui::Context, InputBoundary, DrawingOverlayDraft) {
    let context = egui::Context::default();
    let mut draft = DrawingOverlayDraft::default();
    draft.begin();
    draw(&context, raw(Vec::new()), &mut draft);
    draw(&context, raw(Vec::new()), &mut draft);
    (context, InputBoundary::default(), draft)
}

fn varied_tail() -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(egui::pos2(110.0, 100.0)),
        egui::Event::Key {
            key: egui::Key::A,
            physical_key: Some(egui::Key::B),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::SHIFT,
        },
        egui::Event::Text("a中文".into()),
        egui::Event::Ime(egui::ImeEvent::Preedit("候选".into())),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(1.0, -2.0),
            modifiers: egui::Modifiers::CTRL,
        },
        egui::Event::Touch {
            device_id: egui::TouchDeviceId(2),
            id: egui::TouchId(3),
            phase: egui::TouchPhase::Move,
            pos: egui::pos2(42.0, 43.0),
            force: Some(0.7),
        },
        primary(false),
        egui::Event::PointerGone,
        egui::Event::WindowFocused(false),
    ]
}

#[test]
fn split_and_flush_preserve_full_fifo_events_and_current_raw_metadata() {
    let (context, mut boundary, mut draft) = armed();
    let prefix = vec![
        egui::Event::Copy,
        egui::Event::PointerMoved(egui::pos2(80.0, 80.0)),
        primary(true),
    ];
    let tail = varied_tail();
    let mut first = raw([prefix.clone(), tail.clone()].concat());
    first.time = Some(12.5);
    first.modifiers = egui::Modifiers::ALT;
    first.system_theme = Some(egui::Theme::Dark);
    first.hovered_files.push(egui::HoveredFile {
        path: Some("literal-path.png".into()),
        mime: "image/png".into(),
    });
    let mut expected = first.clone();
    expected.events = prefix;
    boundary.filter(&context, &mut first, true, &mut draft);
    assert_eq!(first, expected);
    assert_eq!(boundary.pending.as_ref().unwrap().events, tail);
    boundary.filter(&context, &mut first, true, &mut draft); // repeated hook stamp
    assert_eq!(first, expected);
    draw(&context, first, &mut draft);
    assert!(draft.preview_gesture.active.is_some());

    let new = vec![
        egui::Event::Paste("new input".into()),
        egui::Event::WindowFocused(true),
    ];
    let mut second = raw(new.clone());
    second.time = Some(15.0);
    second.modifiers = egui::Modifiers::CTRL;
    second.predicted_dt = 0.05;
    let mut expected = second.clone();
    expected.events = [tail, new].concat();
    boundary.filter(&context, &mut second, true, &mut draft);
    assert_eq!(second, expected);
    assert!(boundary.pending.is_none());
    boundary.filter(&context, &mut second, true, &mut draft);
    assert_eq!(second, expected);
}

#[test]
fn cancellation_ready_and_disabled_modes_flush_without_replaying_a_draft() {
    for mode in 0..3 {
        let (context, mut boundary, mut draft) = armed();
        let mut first = raw(vec![
            primary(true),
            primary(false),
            egui::Event::Text("keep text".into()),
        ]);
        boundary.filter(&context, &mut first, true, &mut draft);
        draw(&context, first, &mut draft);
        match mode {
            0 => draft.cancel(),
            1 => draft.finish_stroke(),
            _ => {}
        }
        let before = format!("{draft:?}");
        let mut input = raw(Vec::new());
        boundary.filter(&context, &mut input, mode != 2, &mut draft);
        assert_eq!(
            input.events,
            [primary(false), egui::Event::Text("keep text".into())]
        );
        assert!(boundary.pending.is_none());
        draw(&context, input, &mut draft);
        if mode == 1 {
            assert_eq!(format!("{draft:?}"), before);
            assert_eq!(draft.phase, DrawingDraftPhase::Ready);
        } else {
            assert!(draft.points.is_empty());
            assert!(draft.preview_gesture.active.is_none());
        }
    }
}

#[test]
fn live_viewport_tail_is_not_injected_into_another_window_and_retirement_unblocks_it() {
    let (context, mut boundary, mut draft) = armed();
    let retired = egui::ViewportId::from_hash_of("old-window");
    let tail = vec![egui::Event::Text("old window only".into()), primary(false)];
    boundary.pending = Some(Pending {
        viewport: retired,
        events: tail.clone(),
    });
    let mut input = raw(vec![egui::Event::Copy]);
    input
        .viewports
        .insert(retired, egui::ViewportInfo::default());
    let unchanged = input.clone();
    boundary.filter(&context, &mut input, true, &mut draft);
    assert_eq!(input, unchanged);
    assert_eq!(boundary.pending.as_ref().unwrap().events, tail);
    assert!(boundary.last_hook.is_none());
    input.viewports.remove(&retired);
    boundary.filter(&context, &mut input, true, &mut draft);
    assert_eq!(input.events, [egui::Event::Copy]);
    assert!(boundary.pending.is_none());
    assert!(draft.preview_gesture.seen.is_none());
    draw(&context, input, &mut draft);
    draw(&context, raw(Vec::new()), &mut draft);
    let mut fresh = raw(vec![primary(true), primary(false)]);
    boundary.filter(&context, &mut fresh, true, &mut draft);
    assert_eq!(fresh.events, [primary(true)]);
    assert_eq!(boundary.pending.as_ref().unwrap().events, [primary(false)]);
}

#[test]
fn large_string_capacity_is_not_retained_even_when_its_visible_text_is_tiny() {
    for variant in 0..4 {
        let (context, mut boundary, mut draft) = armed();
        let mut text = String::with_capacity(MAX_RETAINED_BYTES + 1);
        text.push('a');
        let payload = match variant {
            0 => egui::Event::Text(text),
            1 => egui::Event::Paste(text),
            2 => egui::Event::Ime(egui::ImeEvent::Preedit(text)),
            _ => egui::Event::Ime(egui::ImeEvent::Commit(text)),
        };
        let mut input = raw(vec![primary(true), payload, primary(false)]);
        assert!(retained_size(&input.events[1..]).unwrap() > MAX_RETAINED_BYTES);
        let expected = input.clone();
        boundary.filter(&context, &mut input, true, &mut draft);
        assert_eq!(input, expected);
        assert!(retained_size(&input.events[1..]).unwrap() > MAX_RETAINED_BYTES);
        assert!(boundary.pending.is_none());
        assert!(draft.preview_gesture.seen.is_none());
    }
}

#[test]
fn oversized_framework_batch_flushes_existing_tail_without_losing_or_retaining_new_events() {
    for already_pending in [false, true] {
        let (context, mut boundary, mut draft) = armed();
        let old = vec![egui::Event::Text("earlier".into()), primary(false)];
        if already_pending {
            boundary.pending = Some(Pending {
                viewport: egui::ViewportId::ROOT,
                events: old.clone(),
            });
        }
        let mut incoming = vec![egui::Event::Copy; MAX_INPUT_EVENTS + 1];
        incoming[0] = primary(true);
        incoming.push(primary(false));
        let expected = if already_pending {
            [old, incoming.clone()].concat()
        } else {
            incoming.clone()
        };
        let mut input = raw(incoming);
        boundary.filter(&context, &mut input, true, &mut draft);
        assert_eq!(input.events, expected);
        assert!(boundary.pending.is_none());
        assert!(draft.preview_gesture.seen.is_none());
        assert!(draft.points.is_empty());
    }
}
