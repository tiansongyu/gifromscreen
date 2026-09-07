//! Fixed 640×584 X11 controller layout; this is not a tiny-region layout gate.

use super::*;

const SUMMARY: &str = "Shortcuts: 3/3 registered";

fn viewport() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 584.0))
}

fn frame(
    context: &egui::Context,
    stage: RecorderStage,
    events: Vec<egui::Event>,
    manual: bool,
    input_ready: bool,
) -> (egui::FullOutput, RecorderOverlayAction) {
    let mut input = egui::RawInput {
        screen_rect: Some(viewport()),
        focused: true,
        events,
        ..Default::default()
    };
    let root = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    root.inner_rect = Some(viewport());
    root.outer_rect = Some(viewport());
    let mut action = RecorderOverlayAction::None;
    let output = context.run(input, |context| {
        action = draw_recorder_toolbar(
            context,
            stage,
            Some(WorkflowProgress {
                phase: WorkflowPhase::Capturing,
                frames_captured: 7,
                capture_duration: Duration::from_secs(30),
                playback_duration: Duration::from_millis(700),
                encode: None,
            }),
            manual,
            Some(SUMMARY),
            input_ready,
        );
    });
    (output, action)
}

fn text_rect(output: &egui::FullOutput, label: &str) -> Option<(egui::Rect, egui::Rect)> {
    output.shapes.iter().find_map(|shape| {
        let egui::Shape::Text(text) = &shape.shape else {
            return None;
        };
        (text.galley.text() == label).then(|| {
            (
                egui::Rect::from_min_size(text.pos, text.galley.size()),
                shape.clip_rect,
            )
        })
    })
}

fn visible_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
    let (rect, clip) =
        text_rect(output, label).unwrap_or_else(|| panic!("{label:?} was not painted"));
    assert!(
        viewport().contains_rect(rect),
        "{label:?} outside viewport: {rect:?}"
    );
    assert!(
        clip.contains_rect(rect),
        "{label:?} clipped: rect={rect:?}, clip={clip:?}"
    );
    rect
}

fn context() -> egui::Context {
    let context = egui::Context::default();
    context.style_mut(|style| style.animation_time = 0.0);
    context
}

fn warm(
    context: &egui::Context,
    stage: RecorderStage,
    manual: bool,
    input_ready: bool,
) -> egui::FullOutput {
    frame(context, stage, Vec::new(), manual, input_ready);
    frame(context, stage, Vec::new(), manual, input_ready).0
}

fn click(
    context: &egui::Context,
    stage: RecorderStage,
    pos: egui::Pos2,
    manual: bool,
    input_ready: bool,
) -> RecorderOverlayAction {
    let mut action = RecorderOverlayAction::None;
    for pressed in [true, false] {
        action = frame(
            context,
            stage,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            manual,
            input_ready,
        )
        .1;
    }
    action
}

#[test]
fn every_recorder_stage_keeps_the_bottom_summary_fully_visible_at_640_by_584() {
    let mut failures = Vec::new();
    for (stage, control) in [
        (RecorderStage::Ready, "Start"),
        (RecorderStage::Countdown(3), "Cancel"),
        (RecorderStage::Recording, "Stop"),
        (RecorderStage::Paused, "Resume"),
        (RecorderStage::Finalizing, "Cancel"),
    ] {
        let output = warm(&context(), stage, true, true);
        let Some((summary, clip)) = text_rect(&output, SUMMARY) else {
            failures.push(format!("{stage:?}: summary was not painted"));
            continue;
        };
        let control = visible_rect(&output, control);
        if !viewport().contains_rect(summary) || !clip.contains_rect(summary) {
            failures.push(format!(
                "{stage:?}: summary clipped: {summary:?}, clip={clip:?}"
            ));
        }
        assert!(summary.top() >= viewport().bottom() - RECORDER_TOOLBAR_POINTS);
        assert!(
            summary.top() >= control.bottom(),
            "{stage:?}: summary must follow the controls"
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn ready_countdown_recording_paused_and_finalizing_buttons_receive_their_clicks() {
    for (stage, label, expected) in [
        (RecorderStage::Ready, "Start", RecorderOverlayAction::Start),
        (RecorderStage::Ready, "Cancel", RecorderOverlayAction::Close),
        (
            RecorderStage::Countdown(3),
            "Cancel",
            RecorderOverlayAction::CancelCountdown,
        ),
        (
            RecorderStage::Recording,
            "Pause",
            RecorderOverlayAction::Pause,
        ),
        (
            RecorderStage::Recording,
            "Stop",
            RecorderOverlayAction::Stop,
        ),
        (
            RecorderStage::Recording,
            "Discard",
            RecorderOverlayAction::Discard,
        ),
        (
            RecorderStage::Recording,
            "Take snapshot",
            RecorderOverlayAction::Snapshot,
        ),
        (
            RecorderStage::Paused,
            "Resume",
            RecorderOverlayAction::Resume,
        ),
        (RecorderStage::Paused, "Stop", RecorderOverlayAction::Stop),
        (
            RecorderStage::Paused,
            "Discard",
            RecorderOverlayAction::Discard,
        ),
        (
            RecorderStage::Finalizing,
            "Cancel",
            RecorderOverlayAction::Discard,
        ),
    ] {
        let context = context();
        let output = warm(&context, stage, true, true);
        let rect = visible_rect(&output, label);
        assert_eq!(
            click(&context, stage, rect.center(), true, true),
            expected,
            "{stage:?} {label}"
        );
        visible_rect(&output, SUMMARY);
    }
}

#[test]
fn start_waits_for_input_geometry_but_ready_cancel_remains_clickable() {
    let context = context();
    let output = warm(&context, RecorderStage::Ready, false, false);
    let start = visible_rect(&output, "Start");
    let cancel = visible_rect(&output, "Cancel");
    assert_eq!(
        click(&context, RecorderStage::Ready, start.center(), false, false),
        RecorderOverlayAction::None
    );
    assert_eq!(
        click(
            &context,
            RecorderStage::Ready,
            cancel.center(),
            false,
            false
        ),
        RecorderOverlayAction::Close
    );
    visible_rect(&output, SUMMARY);
}

#[test]
fn periodic_capture_has_no_snapshot_button_and_stop_keeps_its_hit_target() {
    let context = context();
    let output = warm(&context, RecorderStage::Recording, false, true);
    assert!(text_rect(&output, "Take snapshot").is_none());
    let stop = visible_rect(&output, "Stop");
    assert_eq!(
        click(
            &context,
            RecorderStage::Recording,
            stop.center(),
            false,
            true
        ),
        RecorderOverlayAction::Stop
    );
    visible_rect(&output, SUMMARY);
}
