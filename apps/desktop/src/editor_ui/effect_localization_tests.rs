use super::*;
use gif_from_screen_application::{BlankAnimationProjectOptions, create_blank_animation_project};
use gif_from_screen_domain::{EditCommand, FrameClip, FrameRenderStep, UnixTimeMs};

fn language(tag: &str) -> Localizer {
    Localizer::new(gif_from_screen_localization::find_language(tag).unwrap())
}

struct Harness {
    _directory: tempfile::TempDir,
    context: egui::Context,
    workspace: EditorWorkspace,
    state: EditorUiState,
    language: Localizer,
    enabled: bool,
}

impl Harness {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut project = create_blank_animation_project(
            directory.path().join("效果 {name}.gfsproj"),
            BlankAnimationProjectOptions {
                project_id: ProjectId::from_u128(1),
                frame_id: FrameId::from_u128(1),
                app_version: "effect-localization-test".into(),
                created_at: UnixTimeMs::new(0),
                canvas: PhysicalSize::new(32, 24).unwrap(),
                background: Rgba {
                    red: 17,
                    green: 93,
                    blue: 169,
                    alpha: 255,
                },
                frame_duration: DurationUs::new(100_000).unwrap(),
                frame_limit_bytes: 32 * 24 * 4,
            },
        )
        .unwrap();
        let first = project.manifest().timeline.frames[0].clone();
        project
            .commit(EditCommand::InsertFrames {
                index: 1,
                frames: vec![
                    FrameClip {
                        id: FrameId::from_u128(2),
                        ..first.clone()
                    },
                    FrameClip {
                        id: FrameId::from_u128(3),
                        ..first
                    },
                ],
            })
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
            language: language("zh"),
            enabled: true,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> (egui::FullOutput, Vec<EditorUiResult>) {
        let mut results = Vec::new();
        let output = self.context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(760.0, 560.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    ui.add_enabled_ui(self.enabled, |ui| {
                        show_effect_toolbar(
                            ui,
                            &mut self.workspace,
                            &mut self.state,
                            Instant::now(),
                            &mut results,
                            self.language,
                        );
                    });
                });
            },
        );
        (output, results)
    }

    fn position(&mut self, label: &str) -> egui::Pos2 {
        self.frame(Vec::new());
        let output = self.frame(Vec::new()).0;
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == label
                {
                    let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                    shape.clip_rect.contains_rect(rect).then_some(rect.center())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing visible effect control {label:?}"))
    }

    fn click(&mut self, message: Message) -> Vec<EditorUiResult> {
        let position = self.position(self.language.text(message));
        let events = |pressed| {
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
        assert!(self.frame(events(true)).1.is_empty());
        self.frame(events(false)).1
    }
}

fn spellings(state: &EditorUiState) -> Vec<String> {
    [
        &state.effect_index_input,
        &state.effect_region_x_input,
        &state.effect_region_y_input,
        &state.effect_region_width_input,
        &state.effect_region_height_input,
        &state.effect_blur_radius_input,
        &state.effect_pixel_block_input,
        &state.effect_tone_percent_input,
        &state.effect_border_top_input,
        &state.effect_border_right_input,
        &state.effect_border_bottom_input,
        &state.effect_border_left_input,
        &state.effect_shadow_offset_x_input,
        &state.effect_shadow_offset_y_input,
        &state.effect_shadow_blur_input,
        &state.effect_color_red_input,
        &state.effect_color_green_input,
        &state.effect_color_blue_input,
        &state.effect_color_alpha_input,
    ]
    .into_iter()
    .cloned()
    .collect()
}

#[test]
fn all_eight_effect_families_keep_machine_choices_and_original_numeric_spellings_across_languages()
{
    let mut harness = Harness::new();
    harness.state.effect_region_x_input = " +0 ".into();
    harness.state.effect_blur_radius_input = "002".into();
    harness.state.effect_shadow_offset_x_input = "-004".into();
    harness.state.effect_color_alpha_input = "0255".into();
    harness.state.effect_index_input = "001".into();
    let original = spellings(&harness.state);
    let manifest = harness.workspace.manifest().clone();
    for choice in [
        EffectChoice::Blur,
        EffectChoice::Pixelate,
        EffectChoice::Darken,
        EffectChoice::Lighten,
        EffectChoice::Border,
        EffectChoice::Shadow,
        EffectChoice::ImageBorder,
        EffectChoice::ImageShadow,
    ] {
        harness.state.effect_choice = choice;
        let effect = build_image_effect(&harness.state).unwrap();
        for tag in ["en", "zh", "en"] {
            harness.language = language(tag);
            assert!(harness.frame(Vec::new()).1.is_empty());
            assert_eq!(spellings(&harness.state), original);
            assert_eq!(harness.state.effect_choice, choice);
            assert_eq!(build_image_effect(&harness.state).unwrap(), effect);
            assert_eq!(harness.workspace.manifest(), &manifest);
        }
    }
}

#[test]
fn chinese_blur_action_applies_once_to_selected_frames_only_and_supports_undo() {
    let mut harness = Harness::new();
    harness
        .workspace
        .toggle_selection(FrameId::from_u128(3))
        .unwrap();
    let before = harness.workspace.manifest().clone();
    let results = harness.click(Message::EffectsAdd);
    assert!(matches!(
        results.as_slice(),
        [Ok(EditorUiAction::Project(EditorUiOperation::AddEffect))]
    ));
    assert!(harness.frame(Vec::new()).1.is_empty());
    let after = harness.workspace.manifest();
    assert_eq!(after.revision.get(), before.revision.get() + 1);
    assert_eq!(after.timeline.frames[1], before.timeline.frames[1]);
    for index in [0, 2] {
        assert!(
            after.timeline.frames[index]
                .render_steps
                .iter()
                .any(|step| matches!(
                    step,
                    FrameRenderStep::Effect {
                        effect: Effect::Blur { .. }
                    }
                ))
        );
        assert_eq!(
            after.timeline.frames[index].asset_id,
            before.timeline.frames[index].asset_id
        );
        assert_eq!(
            after.timeline.frames[index].capture_metadata,
            before.timeline.frames[index].capture_metadata
        );
    }
    harness.workspace.undo().unwrap();
    assert_eq!(
        harness.workspace.manifest().timeline.frames,
        before.timeline.frames
    );
}

#[test]
fn expanded_shadow_and_replacement_border_keep_all_frame_canvas_scope_and_undo() {
    let mut harness = Harness::new();
    let before = harness.workspace.manifest().clone();
    harness.state.effect_choice = EffectChoice::ImageShadow;
    let expected = harness
        .state
        .image_shadow
        .placement(before.canvas.size)
        .unwrap()
        .output_size;
    assert!(matches!(
        harness.click(Message::EffectsAdd).as_slice(),
        [Ok(EditorUiAction::Project(EditorUiOperation::AddEffect))]
    ));
    assert_eq!(harness.workspace.manifest().canvas.size, expected);
    assert!(
        harness
            .workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .all(|frame| frame
                .render_steps
                .iter()
                .any(|step| matches!(step, FrameRenderStep::ImageShadow { .. })))
    );
    assert_eq!(harness.workspace.selection().len(), 1);
    harness.state.effect_choice = EffectChoice::ImageBorder;
    harness.state.image_border.widths.top_milli = -3_000;
    harness.state.image_border.widths.left_milli = -1_000;
    let expected = harness
        .state
        .image_border
        .placement(before.canvas.size)
        .unwrap()
        .output_size;
    let replaced = harness.click(Message::EffectsReplace);
    assert!(matches!(
        replaced.as_slice(),
        [Ok(EditorUiAction::Project(
            EditorUiOperation::ReplaceEffect
        ))]
    ));
    assert_eq!(harness.workspace.manifest().canvas.size, expected);
    assert!(
        harness
            .workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .all(|frame| frame
                .render_steps
                .iter()
                .any(|step| matches!(step, FrameRenderStep::ImageBorder { .. })))
    );
    let applied = harness.workspace.manifest().clone();
    assert!(matches!(
        harness.click(Message::EffectsClear).as_slice(),
        [Ok(EditorUiAction::Project(EditorUiOperation::ClearEffects))]
    ));
    assert_eq!(harness.workspace.manifest().canvas.size, before.canvas.size);
    harness.workspace.undo().unwrap();
    assert_eq!(
        harness.workspace.manifest().timeline.frames,
        applied.timeline.frames
    );
    assert_eq!(harness.workspace.manifest().canvas, applied.canvas);
}

#[test]
fn typed_field_and_range_failures_translate_after_failure_without_mutating_the_project() {
    let mut harness = Harness::new();
    let before = harness.workspace.manifest().clone();
    harness.state.effect_blur_radius_input = "not {field} / 数字".into();
    let results = harness.click(Message::EffectsAdd);
    let failure = results[0].as_ref().unwrap_err();
    assert_eq!(failure.operation, EditorUiOperation::AddEffect);
    assert_eq!(
        failure.message.message_id(),
        Some(Message::EditorInputInvalid)
    );
    assert!(
        failure
            .message
            .render(language("zh"))
            .contains(language("zh").text(Message::EffectFieldBlurRadius))
    );
    assert_eq!(harness.state.effect_blur_radius_input, "not {field} / 数字");
    assert_eq!(harness.workspace.manifest(), &before);
    harness.state.effect_blur_radius_input = "0".into();
    let error = build_effect(&harness.state).unwrap_err();
    assert_eq!(error.message_id(), Some(Message::EffectBlurRange));
    for localizer in [language("en"), language("zh"), language("en")] {
        assert_eq!(
            error.render(localizer),
            localizer
                .format(
                    Message::EffectBlurRange,
                    &[("maximum", &MAX_FRAME_EFFECT_BLUR_RADIUS.to_string())]
                )
                .unwrap()
        );
    }
    harness.state.effect_blur_radius_input = "2".into();
    harness.state.effect_region_width_input = "9999".into();
    let results = harness.click(Message::EffectsAdd);
    assert_eq!(
        results[0].as_ref().unwrap_err().message.message_id(),
        None,
        "backend diagnostics remain raw"
    );
    assert_eq!(harness.workspace.manifest(), &before);
}

#[test]
fn disabled_effect_controls_do_not_mutate_values_or_apply_actions() {
    let mut harness = Harness::new();
    harness.enabled = false;
    let before = harness.workspace.manifest().clone();
    let inputs = spellings(&harness.state);
    for message in [
        Message::EffectsAdd,
        Message::EffectsReplace,
        Message::EffectsClear,
    ] {
        assert!(harness.click(message).is_empty());
        assert_eq!(harness.workspace.manifest(), &before);
        assert_eq!(spellings(&harness.state), inputs);
    }
}

#[test]
fn legacy_effect_color_and_region_validation_keep_numeric_limits_and_translate_nested_details() {
    let mut state = EditorUiState {
        effect_color_alpha_input: "0".into(),
        ..EditorUiState::default()
    };
    assert_eq!(
        parse_effect_color(&state).unwrap_err().message_id(),
        Some(Message::EffectAlphaPositive)
    );
    state.effect_color_alpha_input = "0255".into();
    assert_eq!(parse_effect_color(&state).unwrap().alpha, 255);
    state.effect_color_red_input = "256".into();
    let invalid = parse_effect_color(&state).unwrap_err();
    assert_eq!(invalid.message_id(), Some(Message::EditorInputInvalid));
    assert!(
        invalid
            .render(language("zh"))
            .contains(language("zh").text(Message::EffectFieldRed))
    );
    state.effect_region_width_input = "0".into();
    let error = parse_effect_region(&state).unwrap_err();
    for localizer in [language("en"), language("zh")] {
        assert_eq!(
            error.render(localizer),
            localizer
                .format(
                    Message::EffectInvalidRegion,
                    &[("error", localizer.text(Message::CropEmptySize))]
                )
                .unwrap()
        );
    }
    state.effect_region_x_input = u32::MAX.to_string();
    state.effect_region_width_input = "1".into();
    let error = parse_effect_region(&state).unwrap_err();
    assert!(
        error
            .render(language("zh"))
            .contains(language("zh").text(Message::CropCoordinateOverflow))
    );
    state.effect_index_input = "0".into();
    assert_eq!(
        parse_effect_index(&state).unwrap_err().message_id(),
        Some(Message::EffectIndexPositive)
    );
    state.effect_index_input = " +02 ".into();
    assert_eq!(parse_effect_index(&state).unwrap(), 1);
    assert_eq!(state.effect_index_input, " +02 ");
}
