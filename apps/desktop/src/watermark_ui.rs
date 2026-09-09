use eframe::egui;
use gif_from_screen_domain::{BlendMode, PhysicalPoint, PhysicalPx, PhysicalSize, UnitError};
use gif_from_screen_localization::{Localizer, Message};

use crate::{
    editor_workspace::{EditorWorkspace, OverlaySelectionAnchor, RasterOverlayEdit},
    ui_notice::Notice,
    watermark_decode_job::{
        DecodedWatermark, WatermarkDecodeError, WatermarkDecodeJob, WatermarkDecodeJobState,
        WatermarkDecodeStartError,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WatermarkUiState {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) item_opacity: u8,
    pub(crate) track_opacity: u8,
    pub(crate) blend_mode: BlendMode,
    pub(crate) z_index: i32,
}

/// A decode result may only be committed to the authoring target that requested it.
pub(crate) struct PendingWatermark {
    target: OverlaySelectionAnchor,
    settings: WatermarkUiState,
}

impl PendingWatermark {
    pub(crate) fn start(
        settings: &WatermarkUiState,
        workspace: &EditorWorkspace,
        job: &mut WatermarkDecodeJob,
    ) -> Result<Self, Notice> {
        let target = workspace
            .overlay_selection_anchor()
            .map_err(|error| diagnostic(Message::WatermarkStartFailed, &error))?;
        if settings.name.trim().is_empty() {
            return Err(Message::WatermarkNameRequired.into());
        }
        if settings.item_opacity == 0 || settings.track_opacity == 0 {
            return Err(Message::WatermarkOpacityRequired.into());
        }
        job.start(settings.path.trim().into())
            .map_err(|error| match error {
                WatermarkDecodeStartError::InvalidInput(error) => {
                    decode_failure(&error, Message::WatermarkStartFailed)
                }
                error => diagnostic(Message::WatermarkStartFailed, &error),
            })?;
        Ok(Self {
            target,
            settings: settings.clone(),
        })
    }

    pub(crate) fn commit(
        self,
        workspace: &mut EditorWorkspace,
        decoded: &DecodedWatermark,
    ) -> Result<(), Notice> {
        if !self.target.matches(workspace) {
            return Err(Message::WatermarkTargetChanged.into());
        }
        let edit = build_raster_edit(&self.settings, decoded)?;
        workspace
            .add_raster_overlay_for_selection(edit, &decoded.rgba)
            .map_err(|error| diagnostic(Message::WatermarkCommitFailed, &error))?;
        Ok(())
    }
}

impl Default for WatermarkUiState {
    fn default() -> Self {
        Self {
            path: String::new(),
            // This is authoring data, not a UI label. Switching languages must
            // never rename the draft or the eventual persisted overlay track.
            name: "Watermark".to_owned(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            item_opacity: 255,
            track_opacity: 255,
            blend_mode: BlendMode::Normal,
            z_index: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum WatermarkUiAction {
    #[default]
    None,
    Start,
}

pub(crate) fn show_watermark_ui(
    ui: &mut egui::Ui,
    state: &mut WatermarkUiState,
    job_state: WatermarkDecodeJobState,
    has_selection: bool,
    localizer: Localizer,
) -> WatermarkUiAction {
    let running = job_state == WatermarkDecodeJobState::Running;
    let mut action = WatermarkUiAction::None;
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong(localizer.text(Message::WatermarkTitle));
            ui.weak(localizer.text(Message::WatermarkFormatsHint));
        });
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(localizer.text(Message::WatermarkImagePath));
                ui.add(
                    egui::TextEdit::singleline(&mut state.path)
                        .id_salt("watermark_path")
                        .desired_width(360.0)
                        .hint_text("/path/to/logo.png"),
                );
                ui.label(localizer.text(Message::EditorOverlayName));
                ui.add(
                    egui::TextEdit::singleline(&mut state.name)
                        .id_salt("watermark_name")
                        .desired_width(110.0),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("X/Y/W/H");
                ui.add(egui::DragValue::new(&mut state.x));
                ui.add(egui::DragValue::new(&mut state.y));
                ui.add(egui::DragValue::new(&mut state.width));
                ui.add(egui::DragValue::new(&mut state.height));
                ui.weak(localizer.text(Message::WatermarkSourceSizeHint));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(localizer.text(Message::WatermarkImageOpacity));
                ui.add(egui::DragValue::new(&mut state.item_opacity).range(1..=u8::MAX));
                ui.label(localizer.text(Message::EditorTrackOpacity));
                ui.add(egui::DragValue::new(&mut state.track_opacity).range(1..=u8::MAX));
                ui.label("Z");
                ui.add(egui::DragValue::new(&mut state.z_index));
                ui.label(localizer.text(Message::EditorOverlayBlend));
                egui::ComboBox::from_id_salt("watermark_blend")
                    .selected_text(blend_label(state.blend_mode, localizer))
                    .show_ui(ui, |ui| {
                        for blend in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
                            ui.selectable_value(
                                &mut state.blend_mode,
                                blend,
                                blend_label(blend, localizer),
                            );
                        }
                    });
            });
        });
        if running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(localizer.text(Message::WatermarkDecoding));
            });
        } else if ui
            .add_enabled(
                has_selection,
                egui::Button::new(localizer.text(Message::WatermarkAdd)),
            )
            .clicked()
        {
            action = WatermarkUiAction::Start;
        }
        if !has_selection {
            ui.weak(localizer.text(Message::WatermarkSelectFrames));
        }
    });
    action
}

pub(crate) fn build_raster_edit(
    state: &WatermarkUiState,
    decoded: &DecodedWatermark,
) -> Result<RasterOverlayEdit, Notice> {
    if state.name.trim().is_empty() {
        return Err(Message::WatermarkNameRequired.into());
    }
    if state.item_opacity == 0 || state.track_opacity == 0 {
        return Err(Message::WatermarkImageTrackOpacityRequired.into());
    }
    let width = if state.width == 0 {
        decoded.size.width.get()
    } else {
        state.width
    };
    let height = if state.height == 0 {
        decoded.size.height.get()
    } else {
        state.height
    };
    let display_size = PhysicalSize::new(width, height).map_err(|error| match error {
        UnitError::EmptyPhysicalSize => Notice::with_messages(
            Message::WatermarkInvalidDisplaySize,
            &[],
            &[("error", Message::CropEmptySize)],
        ),
        error => diagnostic(Message::WatermarkInvalidDisplaySize, &error),
    })?;
    Ok(RasterOverlayEdit {
        name: state.name.trim().to_owned(),
        source_size: decoded.size,
        position: PhysicalPoint {
            x: PhysicalPx::new(state.x),
            y: PhysicalPx::new(state.y),
        },
        display_size,
        item_opacity: state.item_opacity,
        track_opacity: state.track_opacity,
        blend_mode: state.blend_mode,
        z_index: state.z_index,
    })
}

fn blend_label(mode: BlendMode, localizer: Localizer) -> &'static str {
    localizer.text(match mode {
        BlendMode::Normal => Message::EditorBlendNormal,
        BlendMode::Multiply => Message::EditorBlendMultiply,
        BlendMode::Screen => Message::EditorBlendScreen,
    })
}

fn diagnostic(message: Message, error: &impl std::fmt::Display) -> Notice {
    Notice::new(message, &[("error", &error.to_string())])
}

/// Application-owned validation has identities; external errors remain literal data.
fn decode_failure(error: &WatermarkDecodeError, fallback: Message) -> Notice {
    let (message, path) = match error {
        WatermarkDecodeError::NotFound { path } => (Message::WatermarkFileNotFound, path),
        WatermarkDecodeError::NotFile { path } => (Message::WatermarkNotFile, path),
        WatermarkDecodeError::InvalidExtension { path } => {
            (Message::WatermarkInvalidExtension, path)
        }
        WatermarkDecodeError::WorkerExited => return Message::WatermarkNoResult.into(),
        error => return diagnostic(fallback, error),
    };
    Notice::new(message, &[("path", &path.display().to_string())])
}

pub(crate) fn finish_watermark(
    result: Option<Result<DecodedWatermark, WatermarkDecodeError>>,
    pending: Option<PendingWatermark>,
    workspace: Option<&mut EditorWorkspace>,
) -> Notice {
    let decoded = match result {
        Some(Ok(decoded)) => decoded,
        Some(Err(error)) => return decode_failure(&error, Message::WatermarkDecodeFailed),
        None => return Message::WatermarkNoResult.into(),
    };
    let Some(workspace) = workspace else {
        return Message::WatermarkProjectClosed.into();
    };
    let Some(pending) = pending else {
        return Message::WatermarkTargetLost.into();
    };
    match pending.commit(workspace, &decoded) {
        Ok(()) => Notice::new(
            Message::WatermarkAdded,
            &[
                ("width", &decoded.size.width.get().to_string()),
                ("height", &decoded.size.height.get().to_string()),
                ("path", &decoded.source_path.display().to_string()),
            ],
        ),
        Err(notice) => notice,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gif_from_screen_localization::find_language;

    use super::*;

    fn language(tag: &str) -> Localizer {
        Localizer::new(find_language(tag).unwrap())
    }

    fn context(font_scale: f32) -> egui::Context {
        let context = egui::Context::default();
        crate::preferences::fonts::install(&context);
        context.style_mut(|style| {
            style.animation_time = 0.0;
            for font in style.text_styles.values_mut() {
                font.size *= font_scale;
            }
        });
        context
    }

    fn frame(
        context: &egui::Context,
        state: &mut WatermarkUiState,
        tag: &str,
        job: WatermarkDecodeJobState,
        selected: bool,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, WatermarkUiAction) {
        let mut action = WatermarkUiAction::None;
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(480.0, 640.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    action = show_watermark_ui(ui, state, job, selected, language(tag));
                });
            },
        );
        (output, action)
    }

    fn label_position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == label
                {
                    let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
                    assert!(shape.clip_rect.contains_rect(rect), "clipped {label}");
                    Some(rect.center())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing visible {label}"))
    }

    fn pointer(position: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn settings() -> WatermarkUiState {
        WatermarkUiState {
            path: "/home/用户/{width}/水印.png".into(),
            name: "  用户 {name} Normal 水印  ".into(),
            x: 3,
            y: 7,
            width: 0,
            height: 9,
            item_opacity: 123,
            track_opacity: 234,
            z_index: -2,
            blend_mode: BlendMode::Screen,
        }
    }

    #[test]
    fn language_switch_keeps_all_watermark_values_and_actual_control_ids() {
        let context = context(1.0);
        let mut state = settings();
        for blend in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
            state.blend_mode = blend;
            let before = state.clone();
            let mut first = None;
            for tag in ["en", "zh", "en"] {
                for _ in 0..2 {
                    frame(
                        &context,
                        &mut state,
                        tag,
                        WatermarkDecodeJobState::Idle,
                        true,
                        vec![],
                    );
                }
                let (output, _) = frame(
                    &context,
                    &mut state,
                    tag,
                    WatermarkDecodeJobState::Idle,
                    true,
                    vec![],
                );
                let position = label_position(&output, language(tag).text(Message::WatermarkAdd));
                frame(
                    &context,
                    &mut state,
                    tag,
                    WatermarkDecodeJobState::Idle,
                    true,
                    vec![egui::Event::PointerMoved(position)],
                );
                let mut ids: Vec<_> =
                    context.interaction_snapshot(|s| s.hovered.iter().copied().collect());
                ids.sort_by_key(egui::Id::value);
                assert!(!ids.is_empty());
                if let Some(first) = &first {
                    assert_eq!(&ids, first);
                } else {
                    first = Some(ids);
                }
                assert_eq!(state, before);
            }
            for job in [
                WatermarkDecodeJobState::Running,
                WatermarkDecodeJobState::Finished,
            ] {
                frame(&context, &mut state, "zh", job, true, vec![]);
                assert_eq!(state, before);
            }
        }
        assert_eq!(WatermarkUiState::default().name, "Watermark");
    }

    #[test]
    fn visible_localized_watermark_button_really_starts_and_selection_disables_it() {
        for tag in ["en", "zh"] {
            for selected in [true, false] {
                let context = context(1.5);
                let mut state = settings();
                let before = state.clone();
                frame(
                    &context,
                    &mut state,
                    tag,
                    WatermarkDecodeJobState::Idle,
                    selected,
                    vec![],
                );
                let (output, _) = frame(
                    &context,
                    &mut state,
                    tag,
                    WatermarkDecodeJobState::Idle,
                    selected,
                    vec![],
                );
                let position = label_position(&output, language(tag).text(Message::WatermarkAdd));
                assert_eq!(
                    frame(
                        &context,
                        &mut state,
                        tag,
                        WatermarkDecodeJobState::Idle,
                        selected,
                        pointer(position, true)
                    )
                    .1,
                    WatermarkUiAction::None
                );
                let action = frame(
                    &context,
                    &mut state,
                    tag,
                    WatermarkDecodeJobState::Idle,
                    selected,
                    pointer(position, false),
                )
                .1;
                assert_eq!(
                    action,
                    if selected {
                        WatermarkUiAction::Start
                    } else {
                        WatermarkUiAction::None
                    }
                );
                assert_eq!(state, before);
            }
        }
    }

    #[test]
    fn running_watermark_cannot_edit_focused_form_or_trigger_a_second_start() {
        let context = context(1.0);
        let mut state = settings();
        state.name = "水印".into();
        frame(
            &context,
            &mut state,
            "zh",
            WatermarkDecodeJobState::Idle,
            true,
            vec![],
        );
        let (output, _) = frame(
            &context,
            &mut state,
            "zh",
            WatermarkDecodeJobState::Idle,
            true,
            vec![],
        );
        let position = label_position(&output, &state.name);
        frame(
            &context,
            &mut state,
            "zh",
            WatermarkDecodeJobState::Idle,
            true,
            pointer(position, true),
        );
        frame(
            &context,
            &mut state,
            "zh",
            WatermarkDecodeJobState::Idle,
            true,
            pointer(position, false),
        );
        assert!(context.memory(egui::Memory::focused).is_some());
        let before = state.clone();
        let (output, action) = frame(
            &context,
            &mut state,
            "zh",
            WatermarkDecodeJobState::Running,
            true,
            vec![egui::Event::Text("替换用户输入".into())],
        );
        label_position(&output, language("zh").text(Message::WatermarkDecoding));
        assert_eq!(action, WatermarkUiAction::None);
        assert_eq!(state, before);
    }

    #[test]
    fn focused_watermark_name_remains_editable_after_language_switch() {
        let context = context(1.0);
        let mut state = WatermarkUiState {
            name: "Logo".into(),
            ..settings()
        };
        for _ in 0..2 {
            frame(
                &context,
                &mut state,
                "en",
                WatermarkDecodeJobState::Idle,
                true,
                vec![],
            );
        }
        let (output, _) = frame(
            &context,
            &mut state,
            "en",
            WatermarkDecodeJobState::Idle,
            true,
            vec![],
        );
        let position = label_position(&output, &state.name);
        frame(
            &context,
            &mut state,
            "en",
            WatermarkDecodeJobState::Idle,
            true,
            pointer(position, true),
        );
        frame(
            &context,
            &mut state,
            "en",
            WatermarkDecodeJobState::Idle,
            true,
            pointer(position, false),
        );
        let focused = context
            .memory(egui::Memory::focused)
            .expect("actual name click focuses TextEdit");
        frame(
            &context,
            &mut state,
            "zh",
            WatermarkDecodeJobState::Idle,
            true,
            vec![],
        );
        assert_eq!(context.memory(egui::Memory::focused), Some(focused));
        frame(
            &context,
            &mut state,
            "zh",
            WatermarkDecodeJobState::Idle,
            true,
            vec![egui::Event::Text("{name}用户".into())],
        );
        assert!(state.name.contains("{name}用户"));
        let edited = state.clone();
        frame(
            &context,
            &mut state,
            "en",
            WatermarkDecodeJobState::Idle,
            true,
            vec![],
        );
        assert_eq!(state, edited);
        assert_eq!(context.memory(egui::Memory::focused), Some(focused));
    }

    #[test]
    fn retained_watermark_validation_and_raw_diagnostics_switch_without_reclassification() {
        let source = decoded();
        for (state, message) in [
            (
                WatermarkUiState {
                    name: "  ".into(),
                    ..settings()
                },
                Message::WatermarkNameRequired,
            ),
            (
                WatermarkUiState {
                    item_opacity: 0,
                    ..settings()
                },
                Message::WatermarkImageTrackOpacityRequired,
            ),
            (
                WatermarkUiState {
                    track_opacity: 0,
                    ..settings()
                },
                Message::WatermarkImageTrackOpacityRequired,
            ),
        ] {
            let mut error = build_raster_edit(&state, &source).err().unwrap();
            assert_eq!(error.message_id(), Some(message));
            let english = error.to_string();
            error.refresh(language("zh"));
            assert_eq!(&*error, language("zh").text(message));
            error.refresh(language("en"));
            assert_eq!(&*error, english);
        }
        let raw = "Watermark name is required.\n原始 IO {error} {path}";
        let error = WatermarkDecodeError::Open {
            path: PathBuf::from("/用户/{path}.png"),
            source: std::io::Error::other(raw),
        };
        let notice = finish_watermark(Some(Err(error)), None, None);
        assert_eq!(notice.message_id(), Some(Message::WatermarkDecodeFailed));
        for tag in ["en", "zh", "en"] {
            let text = notice.render(language(tag));
            assert!(text.contains(raw));
            assert!(text.contains("/用户/{path}.png"));
        }
        for (error, message) in [
            (
                WatermarkDecodeError::NotFound {
                    path: "/用户/{width}.png".into(),
                },
                Message::WatermarkFileNotFound,
            ),
            (
                WatermarkDecodeError::NotFile {
                    path: "/用户/{width}.png".into(),
                },
                Message::WatermarkNotFile,
            ),
            (
                WatermarkDecodeError::InvalidExtension {
                    path: "/用户/{width}.png".into(),
                },
                Message::WatermarkInvalidExtension,
            ),
        ] {
            let notice = finish_watermark(Some(Err(error)), None, None);
            assert_eq!(notice.message_id(), Some(message));
            assert!(notice.render(language("zh")).contains("/用户/{width}.png"));
        }
        for result in [None, Some(Err(WatermarkDecodeError::WorkerExited))] {
            assert_eq!(
                finish_watermark(result, None, None).message_id(),
                Some(Message::WatermarkNoResult)
            );
        }
        assert_eq!(
            finish_watermark(Some(Ok(decoded())), None, None).message_id(),
            Some(Message::WatermarkProjectClosed)
        );
    }

    fn decoded() -> DecodedWatermark {
        DecodedWatermark {
            source_path: PathBuf::from("logo.png"),
            size: PhysicalSize::new(20, 10).unwrap(),
            rgba: vec![0; 20 * 10 * 4],
        }
    }

    #[test]
    fn zero_display_dimensions_use_source_and_explicit_values_override_them() {
        let source = decoded();
        let natural = build_raster_edit(&WatermarkUiState::default(), &source).unwrap();
        assert_eq!(natural.display_size, source.size);

        let resized = build_raster_edit(
            &WatermarkUiState {
                x: 4,
                y: 5,
                width: 100,
                height: 50,
                blend_mode: BlendMode::Screen,
                ..WatermarkUiState::default()
            },
            &source,
        )
        .unwrap();
        assert_eq!(resized.position.x.get(), 4);
        assert_eq!(resized.position.y.get(), 5);
        assert_eq!(resized.display_size, PhysicalSize::new(100, 50).unwrap());
        assert_eq!(resized.blend_mode, BlendMode::Screen);
        for (width, height, expected) in [(0, 7, [20, 7]), (7, 0, [7, 10])] {
            let state = WatermarkUiState {
                width,
                height,
                ..settings()
            };
            let edit = build_raster_edit(&state, &source).unwrap();
            assert_eq!(
                edit.display_size,
                PhysicalSize::new(expected[0], expected[1]).unwrap()
            );
            assert_eq!(edit.name, "用户 {name} Normal 水印");
            assert_eq!(state.name, "  用户 {name} Normal 水印  ");
        }
    }

    #[test]
    fn invisible_or_unnamed_watermarks_are_rejected_before_persistence() {
        let source = decoded();
        for state in [
            WatermarkUiState {
                name: String::new(),
                ..WatermarkUiState::default()
            },
            WatermarkUiState {
                item_opacity: 0,
                ..WatermarkUiState::default()
            },
            WatermarkUiState {
                track_opacity: 0,
                ..WatermarkUiState::default()
            },
        ] {
            assert!(build_raster_edit(&state, &source).is_err());
        }
    }
}
