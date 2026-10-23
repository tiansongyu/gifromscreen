//! Compact recorder controls. Native placement and capture acknowledgements
//! belong to the caller; this view edits only the desired physical rectangle.

use eframe::egui;
use gif_from_screen_capture::{PhysicalPosition, PhysicalSize};
use gif_from_screen_localization::{Localizer, Message};
use gif_from_screen_workflow::WorkflowProgress;

use crate::{
    MAX_COUNTDOWN_SECONDS, MAX_RECORDING_DURATION_MS, RecorderOverlayAction, RecorderStage,
    RecordingCadenceChoice, RecordingSettings, recorder_geometry::RecorderGeometry,
};

/// Draws independent controls without deriving capture geometry from the UI or
/// issuing viewport commands. `input_ready` gates both Start and Resume; Stop,
/// Pause and cancellation stay usable while native geometry is being updated.
/// This flag is only a drawing-time hint: an editor can commit a new desired
/// region later in the same input batch. The caller must revalidate the final
/// geometry and its native acknowledgements before executing a returned action.
/// Actions are not silently dropped here if the desired rectangle changes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    context: &egui::Context,
    geometry: &mut RecorderGeometry,
    stage: RecorderStage,
    progress: Option<WorkflowProgress>,
    settings: &mut RecordingSettings,
    notice: Option<&str>,
    input_ready: bool,
    snap: &mut crate::window_snap::WindowSnapUi,
    localizer: Localizer,
) -> RecorderOverlayAction {
    let mut action = RecorderOverlayAction::None;
    let settings_id = egui::Id::new("recorder-options-expanded");
    let mut expanded = context
        .data(|data| data.get_temp::<bool>(settings_id))
        .unwrap_or(false);
    let panel = egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(
                    crate::appearance::Palette::for_theme(context.style().visuals.dark_mode)
                        .surface,
                )
                .inner_margin(6)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    crate::appearance::Palette::for_theme(context.style().visuals.dark_mode).border,
                )),
        )
        .show(context, |ui| {
            ui.vertical(|ui| {
                let separate_actions = ui.available_width() < 480.0
                    || ui.text_style_height(&egui::TextStyle::Body) > 20.0;
                if separate_actions {
                    ui.horizontal_wrapped(|ui| {
                        action = primary_controls(
                            ui,
                            stage,
                            settings.cadence == RecordingCadenceChoice::Manual,
                            input_ready,
                            localizer,
                        );
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    show_metrics(
                        ui,
                        geometry,
                        stage,
                        settings,
                        &mut expanded,
                        separate_actions,
                        localizer,
                    );
                    if !separate_actions {
                        action = primary_controls(
                            ui,
                            stage,
                            settings.cadence == RecordingCadenceChoice::Manual,
                            input_ready,
                            localizer,
                        );
                    }
                });
                if stage != RecorderStage::Ready && !(expanded && separate_actions) {
                    ui.horizontal_wrapped(|ui| show_status(ui, stage, progress, localizer));
                }
                if let Some(notice) = notice.filter(|text| !text.trim().is_empty()) {
                    egui::ScrollArea::vertical()
                        .id_salt("recorder-notice")
                        .max_height(32.0)
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(notice).truncate())
                                .on_hover_text(notice)
                        });
                }
                if expanded {
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(ui.available_height().clamp(40.0, 280.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            snap.show(ui, geometry, stage, localizer);
                            show_geometry(ui, geometry, stage, localizer);
                            ui.separator();
                            show_timing(ui, settings, stage, localizer);
                        });
                }
            })
            .response
            .rect
            .height()
                + 12.0
        });
    context.data_mut(|data| {
        data.insert_temp(settings_id, expanded);
        data.insert_temp(
            egui::Id::new("recorder-panel-height"),
            panel.inner.max(40.0),
        );
    });
    if context.input(|input| input.viewport().close_requested()) {
        action = RecorderOverlayAction::Close;
    }
    action
}

fn show_metrics(
    ui: &mut egui::Ui,
    geometry: &mut RecorderGeometry,
    stage: RecorderStage,
    settings: &mut RecordingSettings,
    expanded: &mut bool,
    separate_actions: bool,
    localizer: Localizer,
) {
    if ui
        .selectable_label(*expanded, localizer.text(Message::HomeOptions))
        .clicked()
    {
        *expanded = !*expanded;
    }
    if *expanded && separate_actions {
        return;
    }
    ui.separator();
    ui.add_enabled(
        stage == RecorderStage::Ready,
        egui::DragValue::new(&mut settings.fps)
            .range(1..=240)
            .speed(1.0),
    );
    ui.label("fps");
    ui.separator();
    let size = geometry.region().size();
    let (mut width, mut height) = (size.width(), size.height());
    let bounds = geometry.source().size();
    let mut changed = false;
    let resizing = stage.allows_resizing() && !geometry.size_is_frozen();
    changed |= ui
        .add_enabled(
            resizing,
            egui::DragValue::new(&mut width)
                .range(1..=bounds.width())
                .speed(1.0),
        )
        .changed();
    ui.label("×");
    changed |= ui
        .add_enabled(
            resizing,
            egui::DragValue::new(&mut height)
                .range(1..=bounds.height())
                .speed(1.0),
        )
        .changed();
    ui.label("px");
    if changed && let Ok(size) = PhysicalSize::new(width, height) {
        let _ = geometry.resize(size);
    }
    ui.separator();
}

pub(crate) fn panel_height(context: &egui::Context) -> f32 {
    context
        .data(|data| data.get_temp::<f32>(egui::Id::new("recorder-panel-height")))
        .unwrap_or(40.0)
}

fn primary_controls(
    ui: &mut egui::Ui,
    stage: RecorderStage,
    manual: bool,
    input_ready: bool,
    localizer: Localizer,
) -> RecorderOverlayAction {
    let mut action = RecorderOverlayAction::None;
    let mut button = |message, enabled, requested| {
        // Buttons use sequential auto IDs, not their translated captions.
        // Keep them direct children so horizontal wrapping sees each button.
        if ui
            .add_enabled(
                enabled,
                if requested == RecorderOverlayAction::Start
                    || requested == RecorderOverlayAction::Resume
                    || (requested == RecorderOverlayAction::Stop && stage != RecorderStage::Paused)
                {
                    egui::Button::new(
                        egui::RichText::new(localizer.text(message))
                            .color(egui::Color32::from_rgb(171, 40, 40)),
                    )
                    .wrap()
                } else {
                    egui::Button::new(localizer.text(message)).wrap()
                },
            )
            .clicked()
        {
            action = requested;
        }
    };
    match stage {
        RecorderStage::Ready => {
            button(
                Message::RecorderStart,
                input_ready,
                RecorderOverlayAction::Start,
            );
            button(Message::CancelButton, true, RecorderOverlayAction::Close);
        }
        RecorderStage::Countdown(_) => {
            button(
                Message::CancelButton,
                true,
                RecorderOverlayAction::CancelCountdown,
            );
        }
        RecorderStage::Recording => {
            if manual {
                button(
                    Message::RecorderTakeSnapshot,
                    input_ready,
                    RecorderOverlayAction::Snapshot,
                );
            }
            button(Message::RecorderPause, true, RecorderOverlayAction::Pause);
            button(
                Message::RecorderStopShort,
                true,
                RecorderOverlayAction::Stop,
            );
            button(
                Message::RecorderDiscardShort,
                true,
                RecorderOverlayAction::Discard,
            );
        }
        RecorderStage::Paused => {
            button(
                Message::RecorderResume,
                input_ready,
                RecorderOverlayAction::Resume,
            );
            button(
                Message::RecorderStopShort,
                true,
                RecorderOverlayAction::Stop,
            );
            button(
                Message::RecorderDiscardShort,
                true,
                RecorderOverlayAction::Discard,
            );
        }
        RecorderStage::Finalizing => {
            button(Message::CancelButton, true, RecorderOverlayAction::Discard);
        }
    }
    action
}

fn show_status(
    ui: &mut egui::Ui,
    stage: RecorderStage,
    progress: Option<WorkflowProgress>,
    localizer: Localizer,
) {
    match stage {
        RecorderStage::Ready => {
            ui.strong(localizer.text(Message::RecorderReadyToRecord));
        }
        RecorderStage::Countdown(remaining) => {
            ui.strong(crate::format_message(
                localizer,
                Message::RecorderStartsIn,
                &[("seconds", &remaining.to_string())],
            ));
        }
        RecorderStage::Recording => {
            ui.strong(localizer.text(Message::RecorderRecording));
        }
        RecorderStage::Paused => {
            ui.strong(localizer.text(Message::RecorderPaused));
        }
        RecorderStage::Finalizing => {
            ui.spinner();
            ui.label(localizer.text(Message::RecorderFinalizingProject));
        }
    }
    if let Some(progress) = progress {
        ui.label(crate::format_message(
            localizer,
            Message::RecorderProgressSummary,
            &[
                ("frames", &progress.frames_captured.to_string()),
                (
                    "seconds",
                    &format!("{:.1}", progress.playback_duration.as_secs_f64()),
                ),
            ],
        ))
        .on_hover_text(crate::format_message(
            localizer,
            Message::RecorderSourceSpanHint,
            &[(
                "seconds",
                &format!("{:.3}", progress.capture_duration.as_secs_f64()),
            )],
        ));
    }
}

fn show_geometry(
    ui: &mut egui::Ui,
    geometry: &mut RecorderGeometry,
    stage: RecorderStage,
    localizer: Localizer,
) {
    ui.strong(localizer.text(Message::RecorderPhysicalRegion));

    let before = geometry.region();
    let mut position = before.origin();
    let mut width = before.size().width();
    let mut height = before.size().height();
    let moving = stage.allows_moving();
    let resizing = stage.allows_resizing() && !geometry.size_is_frozen();
    let mut changed = false;
    ui.add_enabled_ui(moving, |ui| {
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .add(egui::DragValue::new(&mut position.x).speed(1).prefix("X: "))
                .changed();
            changed |= ui
                .add(egui::DragValue::new(&mut position.y).speed(1).prefix("Y: "))
                .changed();
        });
    });
    ui.add_enabled_ui(resizing, |ui| {
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .add(
                    egui::DragValue::new(&mut width)
                        .speed(1)
                        .range(1..=geometry.source().size().width())
                        .prefix(localizer.text(Message::RecorderWidthPrefix)),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut height)
                        .speed(1)
                        .range(1..=geometry.source().size().height())
                        .prefix(localizer.text(Message::RecorderHeightPrefix)),
                )
                .changed();
        });
    });
    if changed {
        let result = PhysicalSize::new(width, height)
            .map_err(|_| Message::RecorderInvalidDimensions)
            .and_then(|size| apply_region_edit(geometry, stage, position, size));
        if let Err(error) = result {
            ui.colored_label(ui.visuals().error_fg_color, localizer.text(error));
        }
    }
}

fn apply_region_edit(
    geometry: &mut RecorderGeometry,
    stage: RecorderStage,
    position: PhysicalPosition,
    size: PhysicalSize,
) -> Result<(), Message> {
    if !stage.allows_moving() {
        return Err(Message::RecorderRegionFinalizing);
    }
    let mut candidate = *geometry;
    if size != candidate.region().size() {
        if !stage.allows_resizing() || geometry.size_is_frozen() {
            return Err(Message::RecorderCanvasLocked);
        }
        candidate
            .resize(size)
            .map_err(|_| Message::RecorderDimensionsExceed)?;
    }
    candidate.move_to(position);
    *geometry = candidate;
    Ok(())
}

fn show_timing(
    ui: &mut egui::Ui,
    settings: &mut RecordingSettings,
    stage: RecorderStage,
    localizer: Localizer,
) {
    let ready = stage == RecorderStage::Ready;
    // Disabled controls must not sanitize or otherwise modify live settings.
    let mut countdown = settings.countdown_seconds;
    let mut duration = settings.duration_ms;
    ui.strong(localizer.text(Message::RecorderTiming));
    ui.add_enabled_ui(ready, |ui| {
        ui.label(localizer.text(Message::RecorderCountdownSeconds));
        if ui
            .add(
                egui::DragValue::new(&mut countdown)
                    .range(0..=MAX_COUNTDOWN_SECONDS)
                    .clamp_existing_to_range(false),
            )
            .changed()
            && ready
        {
            settings.countdown_seconds = countdown;
        }
        ui.label(localizer.text(Message::RecorderMaximumDurationMs));
        if ui
            .add(
                egui::DragValue::new(&mut duration)
                    .range(0..=MAX_RECORDING_DURATION_MS)
                    .clamp_existing_to_range(false),
            )
            .changed()
            && ready
        {
            settings.duration_ms = duration;
        }
        ui.small(localizer.text(Message::RecorderManualStopHint));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_capture::PhysicalRect;
    use gif_from_screen_workflow::WorkflowPhase;
    use std::time::Duration;

    struct View {
        context: egui::Context,
        viewport: egui::Rect,
        geometry: RecorderGeometry,
        settings: RecordingSettings,
        stage: RecorderStage,
        ready: bool,
        localizer: Localizer,
        notice: String,
    }

    impl View {
        fn new(width: f32, height: f32, font_scale: f32, stage: RecorderStage) -> Self {
            let context = egui::Context::default();
            context.style_mut(|style| {
                style.animation_time = 0.0;
                for font in style.text_styles.values_mut() {
                    font.size *= font_scale;
                }
            });
            Self {
                context,
                viewport: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height)),
                geometry: RecorderGeometry::new(
                    PhysicalRect::new(-1440, -1000, 2880, 2000).unwrap(),
                    PhysicalRect::new(-100, -200, 640, 420).unwrap(),
                )
                .unwrap(),
                settings: RecordingSettings {
                    cadence: RecordingCadenceChoice::Manual,
                    ..RecordingSettings::default()
                },
                stage,
                ready: true,
                localizer: Localizer::new(
                    gif_from_screen_localization::find_language("en").unwrap(),
                ),
                notice: "Display 1 · waiting for the latest guide acknowledgement. Long status text must not move or cover Stop.".into(),
            }
        }

        fn frame(
            &mut self,
            events: Vec<egui::Event>,
            modifiers: egui::Modifiers,
        ) -> (egui::FullOutput, RecorderOverlayAction) {
            let mut input = egui::RawInput {
                screen_rect: Some(self.viewport),
                focused: true,
                events,
                modifiers,
                ..egui::RawInput::default()
            };
            let root = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            root.inner_rect = Some(self.viewport);
            root.outer_rect = Some(self.viewport);
            let mut action = RecorderOverlayAction::None;
            let output = self.context.run(input, |context| {
                action = draw(
                    context,
                    &mut self.geometry,
                    self.stage,
                    Some(WorkflowProgress {
                        phase: WorkflowPhase::Capturing,
                        frames_captured: 7,
                        capture_duration: Duration::from_secs(30),
                        playback_duration: Duration::from_millis(700),
                        encode: None,
                    }),
                    &mut self.settings,
                    Some(&self.notice),
                    self.ready,
                    &mut crate::window_snap::WindowSnapUi::default(),
                    self.localizer,
                );
            });
            (output, action)
        }

        fn warm(&mut self) -> egui::FullOutput {
            self.frame(Vec::new(), egui::Modifiers::NONE);
            self.frame(Vec::new(), egui::Modifiers::NONE).0
        }

        fn click(&mut self, position: egui::Pos2, shift: bool) -> RecorderOverlayAction {
            let modifiers = egui::Modifiers {
                shift,
                ..egui::Modifiers::NONE
            };
            let mut action = RecorderOverlayAction::None;
            for pressed in [true, false] {
                action = self
                    .frame(
                        vec![
                            egui::Event::PointerMoved(position),
                            egui::Event::PointerButton {
                                pos: position,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers,
                            },
                        ],
                        modifiers,
                    )
                    .1;
            }
            action
        }

        fn seek(&mut self, label: &str) -> egui::Rect {
            for _ in 0..100 {
                let output = self.warm();
                if let Some(rect) = visible_text(&output, self.viewport, label) {
                    return rect;
                }
                let output = self.warm();
                if visible_text(&output, self.viewport, label).is_none() {
                    // Existing reachability tests exercise the settings after an explicit click.
                    if let Some(rect) = visible_text(
                        &output,
                        self.viewport,
                        self.localizer.text(Message::HomeOptions),
                    ) {
                        let id = self
                            .context
                            .memory(|memory| {
                                memory
                                    .data
                                    .get_temp::<bool>(egui::Id::new("test-details-open"))
                            })
                            .unwrap_or(false);
                        if !id {
                            self.click(rect.center(), false);
                            self.context.data_mut(|data| {
                                data.insert_temp(egui::Id::new("test-details-open"), true);
                            });
                        }
                    }
                }
                self.frame(
                    vec![
                        egui::Event::PointerMoved(egui::pos2(20.0, self.viewport.bottom() - 20.0)),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -25.0),
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    egui::Modifiers::NONE,
                );
            }
            panic!("{label:?} is unreachable in {:?}", self.viewport);
        }

        fn edit_number(&mut self, label: &str, value: &str) {
            let position = self.seek(label).center();
            self.click(position, false);
            self.frame(
                vec![
                    egui::Event::Text(value.to_owned()),
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                egui::Modifiers::NONE,
            );
        }
    }

    fn visible_text(
        output: &egui::FullOutput,
        viewport: egui::Rect,
        label: &str,
    ) -> Option<egui::Rect> {
        output.shapes.iter().find_map(|clipped| {
            let egui::Shape::Text(text) = &clipped.shape else {
                return None;
            };
            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            (text.galley.text() == label
                && clipped.clip_rect.contains_rect(rect)
                && viewport.contains_rect(rect))
            .then_some(rect)
        })
    }

    fn actions() -> [(RecorderStage, &'static str, RecorderOverlayAction); 11] {
        [
            (RecorderStage::Ready, "Start", RecorderOverlayAction::Start),
            (RecorderStage::Ready, "Cancel", RecorderOverlayAction::Close),
            (
                RecorderStage::Countdown(3),
                "Cancel",
                RecorderOverlayAction::CancelCountdown,
            ),
            (
                RecorderStage::Recording,
                "Take snapshot",
                RecorderOverlayAction::Snapshot,
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
        ]
    }

    #[test]
    fn controller_text_layout_audit() {
        for (width, scale) in [(640.0, 1.0), (480.0, 1.0), (320.0, 1.5), (240.0, 2.0)] {
            for tag in ["en", "zh"] {
                for stage in [
                    RecorderStage::Ready,
                    RecorderStage::Recording,
                    RecorderStage::Paused,
                ] {
                    let mut view = View::new(width, 400.0, 1.0, stage);
                    crate::appearance::configure(&view.context);
                    crate::preferences::fonts::install(&view.context);
                    view.context.style_mut(|style| {
                        for font in style.text_styles.values_mut() {
                            font.size *= scale;
                        }
                    });
                    view.localizer =
                        Localizer::new(gif_from_screen_localization::find_language(tag).unwrap());
                    for expanded in [false, true] {
                        view.context.data_mut(|data| {
                            data.insert_temp(egui::Id::new("recorder-options-expanded"), expanded);
                        });
                        let output = view.warm();
                        let issues = crate::editor_shell_layout_tests::text_layout_issues(&output);
                        assert!(
                            issues.is_empty(),
                            "{width}/{scale}/{tag}/{stage:?}/{expanded}: {}",
                            issues.join("\n")
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn primary_buttons_are_visible_and_clickable_in_small_windows_and_large_fonts() {
        for (width, height, scale) in [
            (320.0, 240.0, 1.0),
            (320.0, 240.0, 2.0),
            (240.0, 240.0, 2.0),
        ] {
            for (stage, label, expected) in actions() {
                let mut view = View::new(width, height, scale, stage);
                let geometry_before = view.geometry;
                let output = view.warm();
                let rect = visible_text(&output, view.viewport, label).unwrap_or_else(|| {
                    panic!("{label:?} clipped at {width}x{height}, font {scale}")
                });
                assert_eq!(view.click(rect.center(), false), expected);
                // Intent delivery itself must not move/resize/freeze the source;
                // the root controller owns the post-draw transaction boundary.
                assert_eq!(view.geometry, geometry_before);
            }
        }
    }

    fn chinese() -> Localizer {
        Localizer::new(gif_from_screen_localization::find_language("zh").unwrap())
    }

    fn action_message(stage: RecorderStage, action: RecorderOverlayAction) -> Message {
        match action {
            RecorderOverlayAction::Start => Message::RecorderStart,
            RecorderOverlayAction::Pause => Message::RecorderPause,
            RecorderOverlayAction::Resume => Message::RecorderResume,
            RecorderOverlayAction::Stop => Message::RecorderStopShort,
            RecorderOverlayAction::Snapshot => Message::RecorderTakeSnapshot,
            RecorderOverlayAction::Discard if stage != RecorderStage::Finalizing => {
                Message::RecorderDiscardShort
            }
            RecorderOverlayAction::Discard
            | RecorderOverlayAction::CancelCountdown
            | RecorderOverlayAction::Close => Message::CancelButton,
            RecorderOverlayAction::None => panic!("not a primary controller action"),
        }
    }

    #[test]
    fn chinese_controls_with_real_fonts_zoom_and_long_notices_keep_all_actions_reachable() {
        for (width, height, scale, zoom) in [
            (320.0, 240.0, 1.0, 1.0),
            (240.0, 240.0, 2.0, 1.0),
            (640.0, 584.0, 1.5, 1.5),
        ] {
            for (stage, _, action) in actions() {
                let mut view = View::new(width, height, scale, stage);
                crate::preferences::fonts::install(&view.context);
                view.context.set_zoom_factor(zoom);
                view.localizer = chinese();
                view.notice =
                    "显示器 {source}：原生录制区域尚未完成更新。/tmp/录像 {原始路径} ".repeat(24);
                let before = view.geometry;
                let label = view.localizer.text(action_message(stage, action));
                let output = view.warm();
                let rect = visible_text(&output, view.viewport, label).unwrap_or_else(|| {
                    panic!("{label:?} clipped at {width}x{height}, font {scale}, zoom {zoom}")
                });
                assert_eq!(view.click(rect.center(), false), action);
                assert_eq!(view.geometry, before);
            }
        }
    }

    #[test]
    fn chinese_native_ack_gates_and_scrolling_preserve_stop_and_capture_settings() {
        for (stage, _, action) in actions() {
            let mut view = View::new(320.0, 240.0, 2.0, stage);
            crate::preferences::fonts::install(&view.context);
            view.localizer = chinese();
            view.ready = false;
            let before = view.geometry;
            let label = view.localizer.text(action_message(stage, action));
            let rect = visible_text(&view.warm(), view.viewport, label).unwrap();
            let expected = if matches!(
                action,
                RecorderOverlayAction::Start
                    | RecorderOverlayAction::Resume
                    | RecorderOverlayAction::Snapshot
            ) {
                RecorderOverlayAction::None
            } else {
                action
            };
            assert_eq!(view.click(rect.center(), false), expected);
            assert_eq!(view.geometry, before);
        }
        let mut view = View::new(320.0, 240.0, 2.0, RecorderStage::Recording);
        crate::preferences::fonts::install(&view.context);
        view.localizer = chinese();
        view.settings.duration_ms = MAX_RECORDING_DURATION_MS + 1;
        view.settings.countdown_seconds = MAX_COUNTDOWN_SECONDS + 1;
        let before = view.geometry;
        let stop = visible_text(
            &view.warm(),
            view.viewport,
            chinese().text(Message::RecorderStopShort),
        )
        .unwrap();
        view.seek(chinese().text(Message::RecorderManualStopHint));
        assert_eq!(
            visible_text(
                &view.warm(),
                view.viewport,
                chinese().text(Message::RecorderStopShort)
            ),
            Some(stop)
        );
        assert_eq!(view.geometry, before);
        assert_eq!(view.settings.duration_ms, MAX_RECORDING_DURATION_MS + 1);
        assert_eq!(view.settings.countdown_seconds, MAX_COUNTDOWN_SECONDS + 1);
    }

    #[test]
    fn switching_language_preserves_numeric_edit_semantics() {
        let mut view = View::new(420.0, 320.0, 1.0, RecorderStage::Ready);
        crate::preferences::fonts::install(&view.context);
        view.warm();
        let before = view.geometry.region();
        view.localizer = chinese();
        view.warm();
        view.edit_number("3", "6");
        view.edit_number("0", "1500");
        assert_eq!(view.settings.countdown_seconds, 6);
        assert_eq!(view.settings.duration_ms, 1500);
        assert_eq!(view.geometry.region().size(), before.size());
    }

    #[test]
    fn pending_native_ack_blocks_start_resume_snapshot_but_never_stop_or_cancel() {
        for (stage, label, action) in actions() {
            let mut view = View::new(320.0, 240.0, 2.0, stage);
            view.ready = false;
            let output = view.warm();
            let rect = visible_text(&output, view.viewport, label).unwrap();
            let expected = if matches!(
                action,
                RecorderOverlayAction::Start
                    | RecorderOverlayAction::Resume
                    | RecorderOverlayAction::Snapshot
            ) {
                RecorderOverlayAction::None
            } else {
                action
            };
            assert_eq!(view.click(rect.center(), false), expected);
        }
    }

    #[test]
    fn controller_requested_height_grows_and_shrinks_with_settings() {
        let mut view = View::new(360.0, 600.0, 1.0, RecorderStage::Ready);
        view.notice.clear();
        let output = view.warm();
        let compact = panel_height(&view.context);
        assert!(compact <= 180.0);
        let header = visible_text(&output, view.viewport, "Options").unwrap();
        view.click(header.center(), false);
        view.warm();
        let expanded = panel_height(&view.context);
        assert!(expanded > compact + 100.0);
        let output = view.warm();
        let header = visible_text(&output, view.viewport, "Options").unwrap();
        view.click(header.center(), false);
        view.warm();
        assert!((panel_height(&view.context) - compact).abs() < 0.01);
    }

    #[test]
    fn default_panel_hides_precision_controls_and_removes_step_buttons() {
        let mut view = View::new(360.0, 180.0, 1.0, RecorderStage::Ready);
        view.notice.clear();
        let output = view.warm();
        assert!(visible_text(&output, view.viewport, "Options").is_some());
        assert!(visible_text(&output, view.viewport, "X+").is_none());
        assert!(visible_text(&output, view.viewport, "Timing").is_none());
        assert!(visible_text(&output, view.viewport, "Start").is_some());
        view.seek("Timing");
    }

    #[test]
    fn scrolling_settings_cannot_move_primary_actions_and_timing_remains_reachable() {
        let mut view = View::new(320.0, 240.0, 2.0, RecorderStage::Ready);
        let first = view.warm();
        let start_before = visible_text(&first, view.viewport, "Start").unwrap();
        view.seek("0 ms means stop manually.");
        let after = view.warm();
        assert_eq!(
            visible_text(&after, view.viewport, "Start"),
            Some(start_before)
        );
    }

    #[test]
    fn viewport_and_zoom_changes_do_not_change_authoritative_geometry_or_live_timing() {
        let mut view = View::new(640.0, 420.0, 1.0, RecorderStage::Recording);
        view.settings.duration_ms = MAX_RECORDING_DURATION_MS + 1;
        view.settings.countdown_seconds = MAX_COUNTDOWN_SECONDS + 1;
        let before = view.geometry;
        view.warm();
        view.context.set_zoom_factor(1.25);
        view.viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 240.0));
        view.warm();
        view.seek("0 ms means stop manually.");
        assert_eq!(view.geometry, before);
        assert_eq!(view.settings.duration_ms, MAX_RECORDING_DURATION_MS + 1);
        assert_eq!(view.settings.countdown_seconds, MAX_COUNTDOWN_SECONDS + 1);
    }

    #[test]
    fn region_edits_are_atomic_and_stage_gate_does_not_depend_on_caller_freezing() {
        let mut view = View::new(320.0, 240.0, 1.0, RecorderStage::Ready);
        let before = view.geometry;
        for stage in [
            RecorderStage::Countdown(3),
            RecorderStage::Recording,
            RecorderStage::Paused,
            RecorderStage::Finalizing,
        ] {
            assert!(
                apply_region_edit(
                    &mut view.geometry,
                    stage,
                    PhysicalPosition { x: 10, y: 20 },
                    PhysicalSize::new(100, 100).unwrap()
                )
                .is_err()
            );
            assert_eq!(view.geometry, before);
        }
        assert!(
            apply_region_edit(
                &mut view.geometry,
                RecorderStage::Ready,
                PhysicalPosition { x: 10, y: 20 },
                PhysicalSize::new(3000, 100).unwrap()
            )
            .is_err()
        );
        assert_eq!(view.geometry, before);
        apply_region_edit(
            &mut view.geometry,
            RecorderStage::Ready,
            PhysicalPosition {
                x: i32::MAX,
                y: i32::MAX,
            },
            PhysicalSize::new(100, 100).unwrap(),
        )
        .unwrap();
        assert_eq!(
            view.geometry.region(),
            PhysicalRect::new(1340, 900, 100, 100).unwrap()
        );
    }

    #[test]
    fn snapshot_is_not_offered_for_automatic_cadence() {
        let mut view = View::new(320.0, 240.0, 1.0, RecorderStage::Recording);
        view.settings.cadence = RecordingCadenceChoice::FixedFps;
        let output = view.warm();
        assert!(visible_text(&output, view.viewport, "Take snapshot").is_none());
        assert!(visible_text(&output, view.viewport, "Stop").is_some());
    }

    #[test]
    fn ready_timing_fields_accept_edits_in_the_scrolling_body() {
        let mut view = View::new(320.0, 240.0, 1.0, RecorderStage::Ready);
        view.edit_number("3", "6");
        assert_eq!(view.settings.countdown_seconds, 6);
        view.edit_number("0", "1500");
        assert_eq!(view.settings.duration_ms, 1500);
        view.edit_number("1500", "0");
        assert_eq!(view.settings.duration_ms, 0);
    }

    #[test]
    fn finalizing_disables_physical_movement_and_sends_no_viewport_geometry_commands() {
        let mut view = View::new(320.0, 240.0, 1.0, RecorderStage::Finalizing);
        let before = view.geometry;
        let position = view.seek("X: -100").center();
        assert_eq!(view.click(position, true), RecorderOverlayAction::None);
        assert_eq!(view.geometry, before);
        let output = view.warm();
        assert!(
            output
                .viewport_output
                .values()
                .all(|viewport| viewport.commands.is_empty())
        );
    }

    fn arrow(key: egui::Key, shift: bool, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers {
                shift,
                ..egui::Modifiers::NONE
            },
        }
    }

    #[test]
    fn arrow_keys_no_longer_move_the_recording_region() {
        for stage in [
            RecorderStage::Ready,
            RecorderStage::Countdown(2),
            RecorderStage::Recording,
            RecorderStage::Paused,
        ] {
            let mut view = View::new(420.0, 300.0, 1.0, stage);
            let output = view.warm();
            assert!(visible_text(&output, view.viewport, "Move with arrow keys").is_none());
            let before = view.geometry;
            // Even stale focus from the old control cannot enable movement.
            view.context.memory_mut(|memory| {
                memory.request_focus(egui::Id::new("x11-recorder-keyboard-move"));
            });
            for key in [
                egui::Key::ArrowLeft,
                egui::Key::ArrowRight,
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
            ] {
                view.frame(
                    vec![arrow(key, false, false), arrow(key, true, true)],
                    egui::Modifiers::SHIFT,
                );
                assert_eq!(view.geometry, before);
            }
        }
    }
}
