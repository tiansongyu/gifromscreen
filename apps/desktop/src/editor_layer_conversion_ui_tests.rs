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
                show_overlay_track_list(
                    ui,
                    workspace,
                    state,
                    Instant::now(),
                    &mut results,
                    crate::test_localizer(),
                );
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

fn many_tracks(count: usize) -> (tempfile::TempDir, EditorWorkspace) {
    let (directory, mut workspace, original_id) = workspace(false);
    let template = workspace.manifest().timeline.overlay_tracks[0].clone();
    let mut commands = vec![EditCommand::RemoveOverlayTrack {
        track_id: original_id,
    }];
    for number in 1..=count {
        let mut track = template.clone();
        track.id = numbered_track(number);
        track.name = format!("Stored layer {number}");
        track.items[0].id = OverlayId::from_u128(u128::try_from(number).unwrap() + 2_000);
        commands.push(EditCommand::UpsertOverlayTrack { track });
    }
    workspace
        .execute(EditCommand::Compound { commands })
        .unwrap();
    (directory, workspace)
}

fn numbered_track(number: usize) -> TrackId {
    TrackId::from_u128(u128::try_from(number).unwrap() + 1_000)
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

fn click(
    context: &egui::Context,
    workspace: &mut EditorWorkspace,
    state: &mut EditorUiState,
    label: &str,
) -> Vec<EditorUiResult> {
    let (output, _) = draw(context, workspace, state, Vec::new());
    let button = fully_visible_text_rect(&output, label);
    let mut results = Vec::new();
    for pressed in [true, false] {
        results.extend(draw(context, workspace, state, pointer(button.center(), pressed)).1);
    }
    results
}

fn fully_visible_text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 360.0));
    output
        .shapes
        .iter()
        .find_map(|clipped| {
            let egui::Shape::Text(text) = &clipped.shape else {
                return None;
            };
            if text.galley.text() != label {
                return None;
            }
            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            // Do not intersect before asserting: that would hide clipped labels.
            assert!(
                rect.is_positive() && clipped.clip_rect.contains_rect(rect),
                "clipped {label}: {rect:?}"
            );
            assert!(
                viewport.contains_rect(rect.expand(2.0)),
                "offscreen {label}: {rect:?}"
            );
            Some(rect)
        })
        .unwrap_or_else(|| panic!("missing visible control {label}"))
}

#[test]
fn pagination_ranges_cover_zero_64_65_128_and_129_without_gaps_or_overflow() {
    for total in [0_usize, 1, 64, 65, 128, 129] {
        let mut pagination = OverlayTrackPagination::default();
        let last = total.saturating_sub(1) / OVERLAY_TRACK_PAGE_SIZE;
        let mut covered = Vec::new();
        for page in 0..=last {
            pagination.page = page;
            let range = pagination.range(total);
            assert!(range.len() <= 64);
            assert_eq!(range.start, page * 64);
            covered.extend(range);
        }
        assert_eq!(covered, (0..total).collect::<Vec<_>>());
        pagination.page = usize::MAX;
        pagination.clamp(total);
        assert_eq!(pagination.page, last);
        assert_eq!(pagination.range(total).end, total);
    }
    let mut pagination = OverlayTrackPagination {
        page: usize::MAX,
        ..OverlayTrackPagination::default()
    };
    pagination.clamp(usize::MAX);
    assert_eq!(pagination.range(usize::MAX).end, usize::MAX);
}

#[test]
fn sixty_fifth_layer_hide_attach_and_remove_target_only_its_stable_identity() {
    let (_directory, mut workspace) = many_tracks(65);
    let original = workspace.manifest().clone();
    let mut state = EditorUiState::default();
    let context = egui::Context::default();
    assert!(click(&context, &mut workspace, &mut state, "Next page").is_empty());
    assert_eq!(state.overlay_track_pagination.page, 1);
    let (page, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    fully_visible_text_rect(&page, "Layers 65–65 of 65 · Page 2 / 2");
    let hide = click(&context, &mut workspace, &mut state, "Hide");
    assert!(hide.contains(&Ok(EditorUiAction::Project(
        EditorUiOperation::SetOverlayVisibility
    ))));
    assert_eq!(
        &workspace.manifest().timeline.overlay_tracks[..64],
        &original.timeline.overlay_tracks[..64]
    );
    let mut expected = original.timeline.overlay_tracks[64].clone();
    expected.visible = false;
    assert_eq!(workspace.manifest().timeline.overlay_tracks[64], expected);
    assert_eq!(
        workspace.manifest().timeline.frames,
        original.timeline.frames
    );
    assert_eq!(workspace.manifest().assets, original.assets);
    let before_attach = workspace.manifest().clone();
    assert_eq!(
        click(&context, &mut workspace, &mut state, "Attach to frames"),
        [Ok(EditorUiAction::ConvertOverlayTrack(numbered_track(65)))]
    );
    assert_eq!(workspace.manifest(), &before_attach);
    let removal = click(&context, &mut workspace, &mut state, "Remove track");
    assert!(removal.contains(&Ok(EditorUiAction::Project(
        EditorUiOperation::RemoveOverlayTrack
    ))));
    assert_eq!(
        workspace.manifest().timeline.overlay_tracks,
        original.timeline.overlay_tracks[..64]
    );
    assert_eq!(state.overlay_track_pagination.page, 0);
    workspace.undo().unwrap();
    assert_eq!(workspace.manifest().timeline, before_attach.timeline);
    click(&context, &mut workspace, &mut state, "Next page");
    click(&context, &mut workspace, &mut state, "Show");
    assert_eq!(workspace.manifest().timeline, original.timeline);
}

#[test]
fn removing_the_last_page_clamps_and_undo_redo_keeps_all_layers_reachable() {
    let (_directory, mut workspace) = many_tracks(129);
    let before = workspace.manifest().timeline.clone();
    let mut state = EditorUiState::default();
    let context = egui::Context::default();
    click(&context, &mut workspace, &mut state, "Next page");
    click(&context, &mut workspace, &mut state, "Next page");
    let (last, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    fully_visible_text_rect(&last, "Layers 129–129 of 129 · Page 3 / 3");
    click(&context, &mut workspace, &mut state, "Remove track");
    assert_eq!(state.overlay_track_pagination.page, 1);
    assert_eq!(workspace.manifest().timeline.overlay_tracks.len(), 128);
    workspace.undo().unwrap();
    assert_eq!(workspace.manifest().timeline, before);
    click(&context, &mut workspace, &mut state, "Next page");
    assert_eq!(state.overlay_track_pagination.page, 2);
    workspace.redo().unwrap();
    let (last, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    assert_eq!(state.overlay_track_pagination.page, 1);
    fully_visible_text_rect(&last, "Layers 65–128 of 128 · Page 2 / 2");
    assert!(click(&context, &mut workspace, &mut state, "Next page").is_empty());
    assert_eq!(state.overlay_track_pagination.page, 1);
}

#[test]
fn project_change_resets_pagination_even_when_a_copied_project_reuses_its_id() {
    let (_first_directory, mut first) = many_tracks(65);
    let (_second_directory, mut second) = many_tracks(129);
    assert_eq!(first.manifest().project_id, second.manifest().project_id);
    let context = egui::Context::default();
    let mut state = EditorUiState::default();
    click(&context, &mut first, &mut state, "Next page");
    assert_eq!(state.overlay_track_pagination.page, 1);
    draw(&context, &mut second, &mut state, Vec::new());
    assert_eq!(state.overlay_track_pagination.page, 0);
    let (_empty_directory, mut empty) = many_tracks(0);
    state.overlay_track_pagination.page = usize::MAX;
    draw(&context, &mut empty, &mut state, Vec::new());
    assert_eq!(state.overlay_track_pagination.range(0), 0..0);
    assert_eq!(state.overlay_track_pagination.page, 0);
}

#[test]
fn narrow_large_font_pagination_and_first_row_actions_remain_visible() {
    for scale in [1.0, 1.5, 2.0] {
        let (_directory, mut workspace) = many_tracks(65);
        let context = egui::Context::default();
        let mut style = (*context.style()).clone();
        for font in style.text_styles.values_mut() {
            font.size *= scale;
        }
        context.set_style(style);
        let mut state = EditorUiState::default();
        click(&context, &mut workspace, &mut state, "Next page");
        let (last, _) = draw(&context, &mut workspace, &mut state, Vec::new());
        for control in [
            "Previous page",
            "Next page",
            "Hide",
            "Attach to frames",
            "Remove track",
        ] {
            fully_visible_text_rect(&last, control);
        }
        click(&context, &mut workspace, &mut state, "Hide");
        assert!(!workspace.manifest().timeline.overlay_tracks[64].visible);
    }
}

#[test]
fn pointer_release_after_page_change_cannot_activate_a_different_track() {
    let (_directory, mut workspace) = many_tracks(65);
    let before = workspace.manifest().clone();
    let context = egui::Context::default();
    let mut state = EditorUiState::default();
    click(&context, &mut workspace, &mut state, "Next page");
    let (last, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    let last_remove = fully_visible_text_rect(&last, "Remove track");
    draw(
        &context,
        &mut workspace,
        &mut state,
        pointer(last_remove.center(), true),
    );
    state.overlay_track_pagination.page = 0;
    let (first, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    let first_remove = fully_visible_text_rect(&first, "Remove track");
    let (_, actions) = draw(
        &context,
        &mut workspace,
        &mut state,
        pointer(first_remove.center(), false),
    );
    assert!(actions.is_empty());
    assert_eq!(workspace.manifest(), &before);
}

#[test]
fn scrolling_reaches_layer_128_without_scrolling_away_page_navigation() {
    let (_directory, mut workspace) = many_tracks(128);
    let original = workspace.manifest().timeline.overlay_tracks.clone();
    let context = egui::Context::default();
    let mut state = EditorUiState::default();
    click(&context, &mut workspace, &mut state, "Next page");
    draw(
        &context,
        &mut workspace,
        &mut state,
        vec![
            egui::Event::PointerMoved(egui::pos2(240.0, 260.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -100_000.0),
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    // Settle egui's scroll smoothing without any wall-clock sleeping.
    let mut output = None;
    for _ in 0..20 {
        output = Some(draw(&context, &mut workspace, &mut state, Vec::new()).0);
    }
    let output = output.unwrap();
    fully_visible_text_rect(&output, "Previous page");
    fully_visible_text_rect(&output, "Next page");
    let last_label = "Layer 128 · Stored layer 128 · Shape · 1 item(s) · Time-anchored";
    fully_visible_text_rect(&output, last_label);
    let remove = output
        .shapes
        .iter()
        .filter_map(|clipped| {
            let egui::Shape::Text(text) = &clipped.shape else {
                return None;
            };
            if text.galley.text() != "Remove track" {
                return None;
            }
            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            clipped.clip_rect.contains_rect(rect).then_some(rect)
        })
        .next_back()
        .expect("last visible row has its remove button");
    for pressed in [true, false] {
        draw(
            &context,
            &mut workspace,
            &mut state,
            pointer(remove.center(), pressed),
        );
    }
    assert_eq!(
        workspace.manifest().timeline.overlay_tracks,
        original[..127]
    );
    assert_eq!(state.overlay_track_pagination.page, 1);
}

#[test]
fn removing_a_pressed_row_cannot_transfer_its_click_to_the_next_track_on_the_same_page() {
    let (_directory, mut workspace) = many_tracks(66);
    let context = egui::Context::default();
    let mut state = EditorUiState::default();
    click(&context, &mut workspace, &mut state, "Next page");
    let (before, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    let pressed = fully_visible_text_rect(&before, "Remove track");
    draw(
        &context,
        &mut workspace,
        &mut state,
        pointer(pressed.center(), true),
    );
    workspace.remove_overlay_track(numbered_track(65)).unwrap();
    let expected = workspace.manifest().clone();
    let (after, _) = draw(&context, &mut workspace, &mut state, Vec::new());
    assert_eq!(state.overlay_track_pagination.page, 1);
    let replacement = fully_visible_text_rect(&after, "Remove track");
    let (_, actions) = draw(
        &context,
        &mut workspace,
        &mut state,
        pointer(replacement.center(), false),
    );
    assert!(actions.is_empty());
    assert_eq!(workspace.manifest(), &expected);
    assert_eq!(
        workspace
            .manifest()
            .timeline
            .overlay_tracks
            .last()
            .unwrap()
            .id,
        numbered_track(66)
    );
}
