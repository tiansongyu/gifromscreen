use std::{fs, time::Instant};

use eframe::egui;
use gif_from_screen_domain::{
    AnnotationRequest, EditCommand, FrameClip, FrameId, OverlayId, OverlayItem, OverlayTrack,
    PhysicalPoint, Rgba, StrokePoint, TimelineSpan, TrackId,
};
use gif_from_screen_localization::{Localizer, Message, find_language};

use super::{
    DrawingDraftPhase, EditorUiAction, EditorUiOperation, EditorUiResult, EditorUiState,
    MAX_DRAWING_DRAFT_POINTS, ShapeOverlayUiState, build_drawing_overlay, build_shape_overlay,
    format_optional_duration, repair_journal_notice, show_drawing_overlay_controls,
    show_editor_statistics, show_overlay_track_list, show_project_storage_toolbar,
    show_shape_overlay_toolbar,
};
use crate::editor_workspace::EditorWorkspace;

fn language(tag: &str) -> Localizer {
    Localizer::new(find_language(tag).unwrap())
}

#[derive(Clone, Copy)]
enum Panel {
    Shape,
    Drawing,
    Layers,
    Project,
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
        let (directory, mut workspace) = super::tests::transition_workspace();
        let third = FrameClip {
            id: FrameId::from_u128(3),
            ..workspace.manifest().timeline.frames[0].clone()
        };
        workspace
            .execute(EditCommand::InsertFrames {
                index: 2,
                frames: vec![third],
            })
            .unwrap();
        workspace.toggle_selection(FrameId::from_u128(3)).unwrap();
        let mut state = EditorUiState::default();
        state.shape_overlay.name = "Keep {name} 原始 shape".into();
        state.shape_overlay.width = 1;
        state.shape_overlay.height = 1;
        state.shape_overlay.fill_enabled = true;
        state.drawing_overlay.name = "Keep {points} 原始 drawing".into();
        let context = egui::Context::default();
        crate::preferences::fonts::install(&context);
        Self {
            _directory: directory,
            context,
            workspace,
            state,
            size: egui::vec2(760.0, 620.0),
        }
    }

    fn frame(
        &mut self,
        panel: Panel,
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
                let now = Instant::now();
                match panel {
                    Panel::Shape => show_shape_overlay_toolbar(
                        ui,
                        &mut self.workspace,
                        &mut self.state,
                        now,
                        &mut results,
                        localizer,
                    ),
                    Panel::Drawing => show_drawing_overlay_controls(
                        ui,
                        &mut self.workspace,
                        &mut self.state,
                        now,
                        &mut results,
                        localizer,
                    ),
                    Panel::Layers => show_overlay_track_list(
                        ui,
                        &mut self.workspace,
                        &mut self.state,
                        now,
                        &mut results,
                        localizer,
                    ),
                    Panel::Project => {
                        show_project_storage_toolbar(
                            ui,
                            &mut self.workspace,
                            &mut results,
                            localizer,
                        );
                        show_editor_statistics(ui, &self.workspace, &mut results, localizer);
                    }
                }
            });
        });
        (output, results)
    }

    fn settled(&mut self, panel: Panel, localizer: Localizer) -> egui::FullOutput {
        for _ in 0..2 {
            assert!(self.frame(panel, localizer, Vec::new()).1.is_empty());
        }
        self.frame(panel, localizer, Vec::new()).0
    }

    fn click(
        &mut self,
        panel: Panel,
        localizer: Localizer,
        message: Message,
    ) -> Vec<EditorUiResult> {
        let output = self.settled(panel, localizer);
        let point = text_rect(&output, localizer.text(message)).center();
        [true, false]
            .into_iter()
            .flat_map(|pressed| self.frame(panel, localizer, pointer(point, pressed)).1)
            .collect()
    }

    fn ready(&mut self) {
        self.state
            .drawing_overlay
            .begin_for_selection(&self.workspace)
            .unwrap();
        self.state.drawing_overlay.push_point(StrokePoint {
            point: PhysicalPoint::default(),
            pressure_milli: 1_000,
        });
        self.state.drawing_overlay.finish_stroke();
    }

    fn legacy(&mut self, count: usize, unknown: bool) {
        let content = build_shape_overlay(
            &self.state.shape_overlay,
            self.workspace.manifest().canvas.size,
        )
        .unwrap();
        let commands = (0..count)
            .map(|index| {
                let id = u128::try_from(index).unwrap() + 500;
                EditCommand::UpsertOverlayTrack {
                    track: OverlayTrack {
                        id: TrackId::from_u128(id),
                        name: format!("Literal {{name}} 层 {index}"),
                        visible: true,
                        opacity: 255,
                        blend_mode: gif_from_screen_domain::BlendMode::Normal,
                        frame_cells: None,
                        annotation: unknown.then(AnnotationRequest::default),
                        annotation_scope: None,
                        items: vec![OverlayItem {
                            id: OverlayId::from_u128(id),
                            z_index: 0,
                            span: TimelineSpan {
                                start: super::TimeUs::ZERO,
                                duration: super::DurationUs::new(10_000).unwrap(),
                            },
                            content: content.clone(),
                        }],
                    },
                }
            })
            .collect();
        self.workspace
            .execute(EditCommand::Compound { commands })
            .unwrap();
    }

    fn assert_reopens(self) {
        let expected = self.workspace.manifest().clone();
        let root = self.workspace.project_root().to_owned();
        drop(self.workspace);
        let reopened = EditorWorkspace::open(
            &root,
            gif_from_screen_project::LockPolicy::FailIfPresent,
            32,
        )
        .unwrap();
        assert_eq!(reopened.manifest(), &expected);
    }
}

fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

fn texts(output: &egui::FullOutput) -> Vec<(&str, egui::Rect, egui::Rect)> {
    output
        .shapes
        .iter()
        .filter_map(|clipped| {
            let egui::Shape::Text(text) = &clipped.shape else {
                return None;
            };
            Some((
                text.galley.text(),
                text.galley.rect.translate(text.pos.to_vec2()),
                clipped.clip_rect,
            ))
        })
        .collect()
}

fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
    let (_, rect, clip) = texts(output)
        .into_iter()
        .find(|(text, _, _)| *text == label)
        .unwrap_or_else(|| panic!("missing localized control {label}"));
    assert!(
        rect.is_positive() && clip.contains_rect(rect),
        "clipped {label}: {rect:?} / {clip:?}"
    );
    rect
}

#[test]
fn switching_language_keeps_confirmed_draft_user_values_and_project_bytes() {
    let mut harness = Harness::new();
    harness.legacy(1, false);
    harness.ready();
    let draft = format!("{:?}", harness.state.drawing_overlay);
    let shape = format!("{:?}", harness.state.shape_overlay);
    let original = harness.workspace.manifest().clone();
    let selection = harness.workspace.selection().clone();
    let disk = fs::read(harness.workspace.project_root().join("manifest.json")).unwrap();
    let journal = fs::read(harness.workspace.project_root().join("journal.ndjson")).unwrap();
    for tag in ["en", "zh", "en"] {
        for panel in [Panel::Shape, Panel::Drawing, Panel::Layers, Panel::Project] {
            harness.settled(panel, language(tag));
        }
        assert_eq!(format!("{:?}", harness.state.drawing_overlay), draft);
        assert_eq!(format!("{:?}", harness.state.shape_overlay), shape);
        assert_eq!(harness.workspace.manifest(), &original);
        assert_eq!(harness.workspace.selection(), &selection);
        assert_eq!(
            fs::read(harness.workspace.project_root().join("manifest.json")).unwrap(),
            disk
        );
        assert_eq!(
            fs::read(harness.workspace.project_root().join("journal.ndjson")).unwrap(),
            journal
        );
    }
}

#[test]
fn chinese_shape_and_drawing_commit_preserve_selection_gaps_stages_and_undo_reopen() {
    let mut harness = Harness::new();
    let chinese = language("zh");
    let before = harness.workspace.manifest().clone();
    let shape = build_shape_overlay(&harness.state.shape_overlay, before.canvas.size).unwrap();
    let actions = harness.click(Panel::Shape, chinese, Message::EditorAddShapeOverlay);
    assert_eq!(
        actions,
        [Ok(EditorUiAction::Project(
            EditorUiOperation::AddShapeOverlay
        ))]
    );
    let track = &harness.workspace.manifest().timeline.overlay_tracks[0];
    assert_eq!(track.name, "Keep {name} 原始 shape");
    assert!(track.items.is_empty());
    let cells = track.frame_cells.as_ref().unwrap();
    assert_eq!(
        cells.iter().map(|cell| cell.frame_id).collect::<Vec<_>>(),
        [FrameId::from_u128(1), FrameId::from_u128(3)]
    );
    assert!(
        cells
            .iter()
            .all(|cell| cell.stage.is_some() && cell.marks[0].content == shape)
    );
    assert_eq!(
        harness.workspace.manifest().timeline.frames[1],
        before.timeline.frames[1]
    );
    let shape_manifest = harness.workspace.manifest().clone();
    harness.ready();
    let drawing = build_drawing_overlay(&harness.state.drawing_overlay).unwrap();
    let actions = harness.click(Panel::Drawing, chinese, Message::EditorCommitDrawing);
    assert_eq!(
        actions,
        [Ok(EditorUiAction::Project(
            EditorUiOperation::AddDrawingOverlay
        ))]
    );
    assert_eq!(harness.state.drawing_overlay.phase, DrawingDraftPhase::Idle);
    let track = &harness.workspace.manifest().timeline.overlay_tracks[1];
    assert_eq!(track.name, "Keep {points} 原始 drawing");
    assert!(
        track
            .all_mark_contents()
            .all(|(_, content)| content == &drawing)
    );
    let authored = harness.workspace.manifest().clone();
    harness.workspace.undo().unwrap();
    assert_eq!(
        harness.workspace.manifest().timeline,
        shape_manifest.timeline
    );
    harness.workspace.redo().unwrap();
    assert_eq!(harness.workspace.manifest().timeline, authored.timeline);
    harness.assert_reopens();
}

#[test]
fn chinese_visibility_and_removal_keep_artwork_and_have_undo_and_reopen() {
    let mut harness = Harness::new();
    harness.legacy(1, false);
    let original = harness.workspace.manifest().clone();
    let chinese = language("zh");
    assert_eq!(
        harness.click(Panel::Layers, chinese, Message::EditorHideLayer),
        [Ok(EditorUiAction::Project(
            EditorUiOperation::SetOverlayVisibility
        ))]
    );
    let hidden = harness.workspace.manifest().clone();
    let mut expected = original.timeline.overlay_tracks[0].clone();
    expected.visible = false;
    assert_eq!(hidden.timeline.overlay_tracks, [expected]);
    assert_eq!(hidden.timeline.frames, original.timeline.frames);
    assert_eq!(hidden.assets, original.assets);
    harness.click(Panel::Layers, chinese, Message::EditorShowLayer);
    assert_eq!(harness.workspace.manifest().timeline, original.timeline);
    harness.workspace.undo().unwrap();
    assert_eq!(harness.workspace.manifest().timeline, hidden.timeline);
    assert_eq!(
        harness.click(Panel::Layers, chinese, Message::EditorRemoveTrack),
        [Ok(EditorUiAction::Project(
            EditorUiOperation::RemoveOverlayTrack
        ))]
    );
    assert!(
        harness
            .workspace
            .manifest()
            .timeline
            .overlay_tracks
            .is_empty()
    );
    harness.workspace.undo().unwrap();
    assert_eq!(harness.workspace.manifest().timeline, hidden.timeline);
    harness.assert_reopens();
}

#[test]
fn chinese_attach_keeps_known_and_unknown_legacy_scope_boundaries() {
    for unknown in [false, true] {
        let mut harness = Harness::new();
        harness.legacy(1, unknown);
        let original = harness.workspace.manifest().clone();
        let actions = harness.click(Panel::Layers, language("zh"), Message::EditorAttachFrames);
        if unknown {
            assert!(actions.is_empty());
        } else {
            assert_eq!(
                actions,
                [Ok(EditorUiAction::ConvertOverlayTrack(TrackId::from_u128(
                    500
                )))]
            );
        }
        assert_eq!(harness.workspace.manifest(), &original);
    }
}

#[test]
fn chinese_pagination_is_bounded_and_retains_page_on_language_switch_at_large_zoom() {
    let mut harness = Harness::new();
    harness.legacy(129, false);
    harness.size = egui::vec2(480.0, 360.0);
    harness.context.set_zoom_factor(1.5);
    let original = harness.workspace.manifest().clone();
    let chinese = language("zh");
    for page in [1, 2] {
        assert!(
            harness
                .click(Panel::Layers, chinese, Message::EditorNextLayerPage)
                .is_empty()
        );
        assert_eq!(harness.state.overlay_track_pagination.page, page);
    }
    let output = harness.settled(Panel::Layers, language("en"));
    assert_eq!(harness.state.overlay_track_pagination.page, 2);
    text_rect(&output, "Layers 129–129 of 129 · Page 3 / 3");
    assert!(
        harness
            .click(Panel::Layers, chinese, Message::EditorPreviousLayerPage)
            .is_empty()
    );
    assert_eq!(harness.state.overlay_track_pagination.range(129), 64..128);
    assert_eq!(harness.workspace.manifest(), &original);
}

#[test]
fn project_buttons_notices_and_statistics_are_localized_without_reinterpreting_values() {
    let mut harness = Harness::new();
    let chinese = language("zh");
    let original = harness.workspace.manifest().clone();
    let actions = harness.click(Panel::Project, chinese, Message::EditorSaveCheckpoint);
    assert!(
        matches!(&actions[..], [Ok(EditorUiAction::Notice { operation: EditorUiOperation::SaveCheckpoint, message })]
        if message.message_id() == Some(Message::EditorCheckpointSaved) && message.render(chinese) == chinese.text(Message::EditorCheckpointSaved))
    );
    let actions = harness.click(Panel::Project, chinese, Message::EditorSaveCompact);
    assert!(
        matches!(&actions[..], [Ok(EditorUiAction::Notice { operation: EditorUiOperation::SaveAndCompact, message })]
        if message.message_id() == Some(Message::EditorCompacted))
    );
    harness.click(Panel::Project, chinese, Message::EditorStatistics);
    harness
        .context
        .style_mut(|style| style.animation_time = 0.0);
    let output = harness.settled(Panel::Project, language("en"));
    text_rect(&output, "Minimum delay");
    assert!(
        texts(&output)
            .iter()
            .any(|(text, _, _)| *text == "0.010000 s")
    );
    assert_eq!(harness.workspace.manifest(), &original);
    assert_eq!(
        fs::read(harness.workspace.project_root().join("journal.ndjson")).unwrap(),
        b""
    );
    let notice = repair_journal_notice(Some(std::path::Path::new("/tmp/{path}/保留.ndjson")));
    assert_eq!(notice.message_id(), Some(Message::EditorJournalPreserved));
    assert!(notice.render(chinese).contains("/tmp/{path}/保留.ndjson"));
    assert_eq!(
        repair_journal_notice(None).render(chinese),
        chinese.text(Message::EditorJournalClean)
    );
    assert_eq!(
        format_optional_duration(None, chinese),
        chinese.text(Message::EditorOptionalNone)
    );
    assert_eq!(
        format_optional_duration(Some(u64::MAX), chinese),
        "18446744073709.551615 s"
    );
    harness.assert_reopens();
}

#[test]
fn application_validation_uses_typed_messages_and_keeps_failed_ready_draft() {
    let mut harness = Harness::new();
    harness.state.shape_overlay.name.clear();
    let original = harness.workspace.manifest().clone();
    let actions = harness.click(Panel::Shape, language("zh"), Message::EditorAddShapeOverlay);
    assert!(
        matches!(&actions[..], [Err(failure)] if failure.message.message_id() == Some(Message::EditorShapeNameRequired))
    );
    harness.ready();
    harness.state.drawing_overlay.name.clear();
    let before = format!("{:?}", harness.state.drawing_overlay);
    let actions = harness.click(Panel::Drawing, language("zh"), Message::EditorCommitDrawing);
    assert!(
        matches!(&actions[..], [Err(failure)] if failure.message.message_id() == Some(Message::EditorDrawingNameRequired))
    );
    assert_eq!(format!("{:?}", harness.state.drawing_overlay), before);
    assert_eq!(harness.workspace.manifest(), &original);
    let invalid = ShapeOverlayUiState {
        width: 0,
        ..ShapeOverlayUiState::default()
    };
    let error = build_shape_overlay(&invalid, original.canvas.size).unwrap_err();
    assert_eq!(error.message_id(), Some(Message::EditorShapeBoundsInvalid));
    let raw = Rgba {
        alpha: 0,
        ..Rgba::TRANSPARENT
    };
    harness.state.drawing_overlay.name = "literal".into();
    harness.state.drawing_overlay.color = raw;
    assert_eq!(
        build_drawing_overlay(&harness.state.drawing_overlay)
            .unwrap_err()
            .message_id(),
        Some(Message::EditorDrawingVisibleRequired)
    );
    harness.state.drawing_overlay.color.alpha = 255;
    harness.state.drawing_overlay.points = vec![
        StrokePoint {
            point: PhysicalPoint::default(),
            pressure_milli: 1_000
        };
        MAX_DRAWING_DRAFT_POINTS + 1
    ];
    let error = build_drawing_overlay(&harness.state.drawing_overlay).unwrap_err();
    assert_eq!(
        error.message_id(),
        Some(Message::EditorDrawingTooManyPoints)
    );
    assert!(error.render(language("zh")).contains("4096"));
}
