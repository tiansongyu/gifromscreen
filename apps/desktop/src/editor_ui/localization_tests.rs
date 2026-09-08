use eframe::egui;
use gif_from_screen_application::{BlankAnimationProjectOptions, create_blank_animation_project};
use gif_from_screen_domain::{
    DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize, ProjectId, Rgba, UnixTimeMs,
};
use gif_from_screen_localization::{Localizer, Message, find_language};

use super::{
    EditorToolTab, EditorUiAction, EditorUiOperation, EditorUiResult, EditorUiState,
    show_editor_chrome, show_editor_tool_panel,
};
use crate::editor_workspace::EditorWorkspace;

fn language(tag: &str) -> Localizer {
    Localizer::new(find_language(tag).unwrap())
}

#[derive(Clone, Copy)]
enum Surface {
    Chrome,
    Tool(EditorToolTab),
}

struct Harness {
    _directory: tempfile::TempDir,
    context: egui::Context,
    workspace: EditorWorkspace,
    state: EditorUiState,
    size: egui::Vec2,
}

impl Harness {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut project = create_blank_animation_project(
            directory.path().join("中文-user-name.gfsproj"),
            BlankAnimationProjectOptions {
                project_id: ProjectId::from_u128(123),
                frame_id: FrameId::from_u128(1),
                app_version: "localized-editor-tests".to_owned(),
                created_at: UnixTimeMs::new(0),
                canvas: PhysicalSize::new(48, 32).unwrap(),
                background: Rgba {
                    red: 209,
                    green: 56,
                    blue: 61,
                    alpha: 255,
                },
                frame_duration: DurationUs::new(100_000).unwrap(),
                frame_limit_bytes: 48 * 32 * 4,
            },
        )
        .unwrap();
        let original = project.manifest().timeline.frames[0].clone();
        let frames = [
            FrameClip {
                id: FrameId::from_u128(2),
                duration: DurationUs::new(200_000).unwrap(),
                ..original.clone()
            },
            FrameClip {
                id: FrameId::from_u128(3),
                duration: DurationUs::new(300_000).unwrap(),
                ..original
            },
        ]
        .to_vec();
        project
            .commit(EditCommand::InsertFrames { index: 1, frames })
            .unwrap();
        let mut workspace = EditorWorkspace::from_active(project, 32).unwrap();
        workspace.select_first().unwrap();
        let context = egui::Context::default();
        crate::preferences::fonts::install(&context);
        Self {
            _directory: directory,
            context,
            workspace,
            state: EditorUiState::default(),
            size: egui::vec2(900.0, 720.0),
        }
    }

    fn frame(
        &mut self,
        surface: Surface,
        localizer: Localizer,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<EditorUiResult>) {
        let mut results = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
            events,
            ..egui::RawInput::default()
        };
        let output = self.context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                results = match surface {
                    Surface::Chrome => {
                        show_editor_chrome(ui, &mut self.workspace, &mut self.state, localizer)
                    }
                    Surface::Tool(tool) => {
                        self.state.active_tool = tool;
                        show_editor_tool_panel(ui, &mut self.workspace, &mut self.state, localizer)
                    }
                };
            });
        });
        (output, results)
    }

    fn settled(&mut self, surface: Surface, localizer: Localizer) -> egui::FullOutput {
        for _ in 0..2 {
            let (_, results) = self.frame(surface, localizer, Vec::new());
            assert!(
                results.is_empty(),
                "painting alone emitted an action: {results:?}"
            );
        }
        self.frame(surface, localizer, Vec::new()).0
    }

    fn click(
        &mut self,
        surface: Surface,
        localizer: Localizer,
        key: Message,
    ) -> Vec<EditorUiResult> {
        self.click_text(surface, localizer, localizer.text(key))
    }

    fn click_text(
        &mut self,
        surface: Surface,
        localizer: Localizer,
        label: &str,
    ) -> Vec<EditorUiResult> {
        let output = self.settled(surface, localizer);
        let (rect, clip) = text_rect(&output, label);
        assert!(
            clip.contains_rect(rect),
            "{label} is clipped: {rect:?} / {clip:?}"
        );
        assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size).contains_rect(rect));
        let pos = rect.center();
        let mut results = Vec::new();
        for pressed in [true, false] {
            results.extend(
                self.frame(
                    surface,
                    localizer,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                )
                .1,
            );
        }
        assert!(
            results.iter().all(Result::is_ok),
            "{label} failed: {results:?}"
        );
        results
    }
}

fn text_rect(output: &egui::FullOutput, label: &str) -> (egui::Rect, egui::Rect) {
    fn find(shape: &egui::Shape, label: &str) -> Option<egui::Rect> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
            _ => None,
        }
    }
    output
        .shapes
        .iter()
        .find_map(|clipped| find(&clipped.shape, label).map(|rect| (rect, clipped.clip_rect)))
        .unwrap_or_else(|| panic!("missing editor text: {label}"))
}

fn editable_inputs(state: &EditorUiState) -> Vec<&str> {
    [
        &state.frame_number_input,
        &state.time_ms_input,
        &state.time_range_start_ms_input,
        &state.time_range_end_ms_input,
        &state.duration_us_input,
        &state.percentage_input,
        &state.frame_expression,
        &state.crop_x_input,
        &state.crop_y_input,
        &state.crop_width_input,
        &state.crop_height_input,
        &state.resize_width_input,
        &state.resize_height_input,
        &state.reduce_keep_every_input,
        &state.duplicate_threshold_input,
    ]
    .into_iter()
    .map(String::as_str)
    .collect()
}

fn durations(workspace: &EditorWorkspace) -> Vec<u64> {
    workspace
        .manifest()
        .timeline
        .frames
        .iter()
        .map(|frame| frame.duration.get())
        .collect()
}

#[test]
fn changing_editor_language_is_read_only_for_project_selection_timing_and_clipboard() {
    let mut harness = Harness::new();
    harness
        .workspace
        .toggle_selection(FrameId::from_u128(3))
        .unwrap();
    harness.workspace.copy_selection().unwrap();
    harness.state.frame_expression = "3-1".to_owned();
    harness.state.duration_us_input = "-1500".to_owned();
    let original = harness.workspace.manifest().clone();
    let selection = harness.workspace.selection().clone();
    let clipboard = harness.workspace.selected_clipboard_id();
    let inputs = editable_inputs(&harness.state)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let root = harness.workspace.project_root().to_owned();
    let disk = std::fs::read(root.join("manifest.json")).unwrap();
    let journal = std::fs::read(root.join("journal.ndjson")).unwrap();
    for tag in ["en", "zh", "en"] {
        harness.settled(Surface::Chrome, language(tag));
        for tool in [
            EditorToolTab::Frames,
            EditorToolTab::Timing,
            EditorToolTab::Transform,
        ] {
            harness.settled(Surface::Tool(tool), language(tag));
        }
        assert_eq!(harness.workspace.manifest(), &original);
        assert_eq!(harness.workspace.selection(), &selection);
        assert_eq!(harness.workspace.selected_clipboard_id(), clipboard);
        assert_eq!(harness.workspace.clipboard_len(), 2);
        assert_eq!(editable_inputs(&harness.state), inputs);
        assert_eq!(harness.workspace.project_root(), root);
        assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), disk);
        assert_eq!(std::fs::read(root.join("journal.ndjson")).unwrap(), journal);
    }
}

#[test]
fn chinese_navigation_and_filmstrip_keep_frame_identity_and_half_open_times() {
    let mut harness = Harness::new();
    let original = harness.workspace.manifest().clone();
    let chinese = language("zh");
    let results = harness.click(Surface::Chrome, chinese, Message::EditorNext);
    assert!(results.contains(&Ok(EditorUiAction::Selection(
        EditorUiOperation::SelectNext
    ))));
    assert_eq!(
        harness.workspace.selection().current(),
        Some(FrameId::from_u128(2))
    );
    harness.state.frame_number_input = "3".to_owned();
    harness.click(Surface::Chrome, chinese, Message::EditorGo);
    assert_eq!(
        harness.workspace.selection().current(),
        Some(FrameId::from_u128(3))
    );
    for (time, expected) in [("99", 1), ("100", 2), ("300", 3)] {
        harness.state.time_ms_input = time.to_owned();
        harness.click(Surface::Chrome, chinese, Message::EditorGoToTime);
        assert_eq!(
            harness.workspace.selection().current(),
            Some(FrameId::from_u128(expected))
        );
    }
    let frame_label =
        crate::format_message(chinese, Message::EditorFrameNumber, &[("number", "1")]);
    harness.click_text(Surface::Chrome, chinese, &frame_label);
    assert_eq!(
        harness.workspace.selection().current(),
        Some(FrameId::from_u128(1))
    );
    assert_eq!(harness.workspace.manifest(), &original);
}

#[test]
fn chinese_copy_delete_undo_redo_and_paste_use_the_existing_frame_commands() {
    let mut harness = Harness::new();
    let chinese = language("zh");
    let frames = Surface::Tool(EditorToolTab::Frames);
    let original = harness.workspace.manifest().timeline.frames.clone();
    harness.click(frames, chinese, Message::EditorSelectAll);
    let copied = harness.click(frames, chinese, Message::EditorCopy);
    assert!(copied.contains(&Ok(EditorUiAction::Clipboard {
        operation: EditorUiOperation::Copy,
        frames: 3
    })));
    harness.click(frames, chinese, Message::EditorDelete);
    assert!(harness.workspace.manifest().timeline.frames.is_empty());
    harness.click(Surface::Chrome, chinese, Message::EditorUndo);
    assert_eq!(harness.workspace.manifest().timeline.frames, original);
    harness.click(Surface::Chrome, chinese, Message::EditorRedo);
    assert!(harness.workspace.manifest().timeline.frames.is_empty());
    harness.click(Surface::Chrome, chinese, Message::EditorUndo);
    harness.click(frames, chinese, Message::EditorPaste);
    let actual = &harness.workspace.manifest().timeline.frames;
    assert_eq!(actual.len(), 6);
    let copied = actual
        .iter()
        .filter(|frame| !original.iter().any(|source| source.id == frame.id))
        .collect::<Vec<_>>();
    assert_eq!(copied.len(), 3);
    for (copy, source) in copied.into_iter().zip(&original) {
        assert_eq!(copy.asset_id, source.asset_id);
        assert_eq!(copy.duration, source.duration);
        assert_eq!(copy.capture_metadata, source.capture_metadata);
        assert_eq!(copy.capture_clock, source.capture_clock);
    }
}

#[test]
fn chinese_delay_and_time_range_actions_do_not_reinterpret_units_or_selection() {
    let mut harness = Harness::new();
    let chinese = language("zh");
    let timing = Surface::Tool(EditorToolTab::Timing);
    harness
        .workspace
        .toggle_selection(FrameId::from_u128(3))
        .unwrap();
    harness.state.duration_us_input = "125000".to_owned();
    harness.click(timing, chinese, Message::EditorOverrideDelay);
    assert_eq!(durations(&harness.workspace), [125_000, 200_000, 125_000]);
    harness.state.time_range_start_ms_input = "125".to_owned();
    harness.state.time_range_end_ms_input = "325".to_owned();
    harness.click(timing, chinese, Message::EditorSelectRange);
    assert_eq!(harness.workspace.selection().len(), 1);
    assert_eq!(
        harness.workspace.selection().current(),
        Some(FrameId::from_u128(2))
    );
    harness.state.percentage_input = "150".to_owned();
    harness.click(timing, chinese, Message::EditorScaleDelay);
    assert_eq!(durations(&harness.workspace), [125_000, 300_000, 125_000]);
    harness.click(Surface::Chrome, chinese, Message::EditorUndo);
    assert_eq!(durations(&harness.workspace), [125_000, 200_000, 125_000]);
}

#[test]
fn chinese_crop_action_remains_whole_animation_and_undo_restores_original_frames() {
    let mut harness = Harness::new();
    let original = harness.workspace.manifest().timeline.frames.clone();
    let chinese = language("zh");
    harness.state.crop_x_input = "2".to_owned();
    harness.state.crop_y_input = "3".to_owned();
    harness.state.crop_width_input = "24".to_owned();
    harness.state.crop_height_input = "16".to_owned();
    let result = harness.click(
        Surface::Tool(EditorToolTab::Transform),
        chinese,
        Message::EditorApplyCrop,
    );
    assert!(result.contains(&Ok(EditorUiAction::Project(EditorUiOperation::ApplyCrop))));
    assert_eq!(harness.workspace.selection().len(), 1);
    assert_eq!(
        harness.workspace.manifest().canvas.size,
        PhysicalSize::new(24, 16).unwrap()
    );
    for frame in &harness.workspace.manifest().timeline.frames {
        assert_eq!(
            gif_from_screen_domain::FrameGeometryPlan::new(
                frame,
                PhysicalSize::new(48, 32).unwrap()
            )
            .unwrap()
            .output_size(),
            PhysicalSize::new(24, 16).unwrap()
        );
    }
    assert_eq!(durations(&harness.workspace), [100_000, 200_000, 300_000]);
    harness.click(Surface::Chrome, chinese, Message::EditorUndo);
    assert_eq!(harness.workspace.manifest().timeline.frames, original);
    assert_eq!(
        harness.workspace.manifest().canvas.size,
        PhysicalSize::new(48, 32).unwrap()
    );
}

#[test]
fn chinese_primary_controls_wrap_in_narrow_panels_with_larger_fonts() {
    for font_scale in [1.0, 1.5] {
        let mut harness = Harness::new();
        harness.size = egui::vec2(480.0, 600.0);
        harness.context.style_mut(|style| {
            for font in style.text_styles.values_mut() {
                font.size *= font_scale;
            }
        });
        for (surface, labels) in [
            (
                Surface::Chrome,
                vec![
                    Message::EditorFirst,
                    Message::EditorNext,
                    Message::EditorGoToTime,
                    Message::EditorFramesTab,
                    Message::EditorTransformTab,
                ],
            ),
            (
                Surface::Tool(EditorToolTab::Frames),
                vec![
                    Message::EditorApplySelection,
                    Message::EditorCopy,
                    Message::EditorPaste,
                    Message::EditorDelete,
                ],
            ),
            (
                Surface::Tool(EditorToolTab::Timing),
                vec![
                    Message::EditorOverrideDelay,
                    Message::EditorAdjustSignedDelay,
                    Message::EditorScaleDelay,
                    Message::EditorSelectRange,
                ],
            ),
            (
                Surface::Tool(EditorToolTab::Transform),
                vec![
                    Message::EditorApplyCrop,
                    Message::EditorRemoveLastCrop,
                    Message::EditorResize,
                    Message::EditorRotateRight,
                ],
            ),
        ] {
            let output = harness.settled(surface, language("zh"));
            for key in labels {
                let label = language("zh").text(key);
                let (rect, clip) = text_rect(&output, label);
                assert!(
                    clip.contains_rect(rect),
                    "clipped {label} at font scale {font_scale}: {rect:?} / {clip:?}"
                );
                assert!(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, harness.size).contains_rect(rect)
                );
            }
        }
    }
}

#[test]
fn translated_transition_and_clipboard_headers_keep_their_open_state() {
    let mut harness = Harness::new();
    harness
        .context
        .style_mut(|style| style.animation_time = 0.0);
    harness.workspace.copy_selection().unwrap();
    let original = harness.workspace.manifest().clone();
    let clipboard = harness.workspace.selected_clipboard_id();
    let timing = Surface::Tool(EditorToolTab::Timing);
    harness.click(timing, language("en"), Message::EditorTransitions);
    for tag in ["en", "zh", "en"] {
        let output = harness.settled(timing, language(tag));
        text_rect(&output, language(tag).text(Message::EditorTransitionType));
    }
    let frames = Surface::Tool(EditorToolTab::Frames);
    let heading = crate::format_message(
        language("en"),
        Message::EditorClipboardHistory,
        &[("count", "1")],
    );
    harness.click_text(frames, language("en"), &heading);
    for tag in ["en", "zh", "en"] {
        let output = harness.settled(frames, language(tag));
        text_rect(
            &output,
            language(tag).text(Message::EditorClearClipboardHistory),
        );
    }
    assert_eq!(harness.workspace.manifest(), &original);
    assert_eq!(harness.workspace.selected_clipboard_id(), clipboard);
    assert_eq!(harness.workspace.clipboard_len(), 1);
}

#[test]
fn clipboard_clear_success_keeps_message_identity_for_later_language_switches() {
    let mut harness = Harness::new();
    harness.workspace.copy_selection().unwrap();
    let original = harness.workspace.manifest().clone();
    let frames = Surface::Tool(EditorToolTab::Frames);
    let chinese = language("zh");
    let heading =
        crate::format_message(chinese, Message::EditorClipboardHistory, &[("count", "1")]);
    harness
        .context
        .style_mut(|style| style.animation_time = 0.0);
    harness.click_text(frames, chinese, &heading);
    let results = harness.click(frames, chinese, Message::EditorClearClipboardHistory);
    let notice = results
        .into_iter()
        .find_map(|result| match result.unwrap() {
            EditorUiAction::Notice {
                operation: EditorUiOperation::ClearClipboardHistory,
                message,
            } => Some(message),
            _ => None,
        })
        .expect("clear history notice");
    assert_eq!(
        notice.message_id(),
        Some(Message::EditorClipboardHistoryCleared)
    );
    assert_eq!(notice.render(language("en")), "Clipboard history cleared.");
    assert_eq!(
        notice.render(chinese),
        chinese.text(Message::EditorClipboardHistoryCleared)
    );
    assert_eq!(harness.workspace.clipboard_len(), 0);
    assert_eq!(harness.workspace.manifest(), &original);
}
