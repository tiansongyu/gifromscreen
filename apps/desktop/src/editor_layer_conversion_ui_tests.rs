//! Layer conversion is explicit, whole-layer, and queued without a synchronous project edit.

use super::*;
use gif_from_screen_domain::{
    AnnotationRequest, EditCommand, OverlayId, OverlayItem, OverlayTrack, TimelineSpan, TrackId,
};

fn workspace(unknown_scope: bool) -> (tempfile::TempDir, EditorWorkspace, TrackId) {
    let (directory, mut workspace) = super::tests::transition_workspace();
    let id = TrackId::from_u128(9);
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: OverlayTrack {
                id,
                frame_cells: None,
                annotation: unknown_scope.then(AnnotationRequest::default),
                annotation_scope: None,
                name: "Existing layer".to_owned(),
                visible: true,
                opacity: 255,
                blend_mode: BlendMode::Normal,
                items: vec![OverlayItem {
                    id: OverlayId::from_u128(9),
                    span: TimelineSpan {
                        start: TimeUs::ZERO,
                        duration: DurationUs::new(40_000).unwrap(),
                    },
                    z_index: 0,
                    content: OverlayContent::Shape {
                        kind: ShapeKind::Rectangle,
                        bounds: PhysicalRect::new(0, 0, 1, 1).unwrap(),
                        stroke_width: 0,
                        stroke: Rgba::TRANSPARENT,
                        fill: Some(Rgba {
                            red: 255,
                            green: 0,
                            blue: 0,
                            alpha: 255,
                        }),
                    },
                }],
            },
        })
        .unwrap();
    (directory, workspace, id)
}

fn draw(
    context: &egui::Context,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Vec<EditorUiResult>) {
    let mut results = Vec::new();
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 360.0),
            )),
            events,
            ..egui::RawInput::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                show_overlay_track_list(ui, workspace, state, Instant::now(), &mut results);
            });
        },
    );
    (output, results)
}

fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .find_map(|clipped| {
            if let egui::Shape::Text(text) = &clipped.shape
                && text.galley.text() == label
            {
                Some(
                    egui::Rect::from_min_size(text.pos, text.galley.size())
                        .intersect(clipped.clip_rect),
                )
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("missing label: {label}"))
}

#[test]
fn attach_button_emits_a_request_without_changing_pixels_scope_or_revision() {
    let (_directory, mut workspace, id) = workspace(false);
    let before = workspace.manifest().clone();
    let mut state = EditorUiState::default();
    let context = egui::Context::default();
    let (initial, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    let button = text_rect(&initial, "Attach to frames");
    assert!(button.is_positive() && button.right() <= 480.0);
    for pressed in [true, false] {
        let (_, results) = draw(
            &context,
            &mut workspace,
            &mut state,
            vec![
                egui::Event::PointerMoved(button.center()),
                egui::Event::PointerButton {
                    pos: button.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        if !pressed {
            assert_eq!(results, [Ok(EditorUiAction::ConvertOverlayTrack(id))]);
        }
    }
    assert_eq!(
        workspace.manifest(),
        &before,
        "the host must perform the exclusive asynchronous edit"
    );
}

#[test]
fn unknown_legacy_authoring_scope_cannot_be_silently_inferred_from_visible_items() {
    let (_directory, mut workspace, _) = workspace(true);
    let before = workspace.manifest().clone();
    let mut state = EditorUiState::default();
    let context = egui::Context::default();
    let (initial, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    let button = text_rect(&initial, "Attach to frames");
    for pressed in [true, false] {
        let (_, results) = draw(
            &context,
            &mut workspace,
            &mut state,
            vec![
                egui::Event::PointerMoved(button.center()),
                egui::Event::PointerButton {
                    pos: button.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(results.is_empty());
    }
    assert_eq!(workspace.manifest(), &before);
}
