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

#[test]
fn visibility_buttons_preserve_layer_content_and_roundtrip_through_undo_and_reopen() {
    let (directory, mut workspace, id) = workspace(false);
    let before = workspace.manifest().clone();
    let mut state = EditorUiState::default();
    let context = egui::Context::default();
    let (initial, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    let hide = text_rect(&initial, "Hide");
    assert!(hide.is_positive() && hide.right() <= 480.0 && hide.bottom() <= 360.0);
    for pressed in [true, false] {
        draw(
            &context,
            &mut workspace,
            &mut state,
            vec![
                egui::Event::PointerMoved(hide.center()),
                egui::Event::PointerButton {
                    pos: hide.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    let hidden = workspace.manifest().clone();
    let mut expected_track = before.timeline.overlay_tracks[0].clone();
    expected_track.visible = false;
    assert_eq!(hidden.timeline.overlay_tracks, vec![expected_track]);
    assert_eq!(hidden.assets, before.assets);
    assert_eq!(hidden.timeline.frames, before.timeline.frames);
    let (output, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    let show = text_rect(&output, "Show");
    assert!(show.is_positive());
    for pressed in [true, false] {
        draw(
            &context,
            &mut workspace,
            &mut state,
            vec![
                egui::Event::PointerMoved(show.center()),
                egui::Event::PointerButton {
                    pos: show.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert_eq!(workspace.manifest().timeline, before.timeline);
    workspace.undo().unwrap();
    assert_eq!(workspace.manifest().timeline, hidden.timeline);
    let unchanged = workspace.manifest().clone();
    workspace.set_overlay_track_visibility(id, false).unwrap();
    assert_eq!(
        workspace.manifest(),
        &unchanged,
        "an unchanged property does not create history"
    );
    assert!(
        workspace
            .set_overlay_track_visibility(TrackId::from_u128(999), true)
            .is_err()
    );
    assert_eq!(workspace.manifest(), &unchanged);
    workspace.undo().unwrap();
    assert_eq!(workspace.manifest().timeline, before.timeline);
    workspace.redo().unwrap();
    assert_eq!(workspace.manifest().timeline, hidden.timeline);
    let expected = workspace.manifest().clone();
    let root = workspace.project_root().to_owned();
    drop(workspace);
    let reopened = EditorWorkspace::open(
        &root,
        gif_from_screen_project::LockPolicy::FailIfPresent,
        32,
    )
    .unwrap();
    assert_eq!(reopened.manifest(), &expected);
    assert!(directory.path().exists());
}
