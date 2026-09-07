//! Compact recorder controls. Native placement and capture acknowledgements
//! belong to the caller; this view edits only the desired physical rectangle.

use eframe::egui;
use gif_from_screen_capture::{PhysicalPosition, PhysicalSize};
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
) -> RecorderOverlayAction {
    let mut action = RecorderOverlayAction::None;
    egui::TopBottomPanel::bottom("x11_controller_actions")
        .resizable(false)
        .show(context, |ui| {
            action = primary_controls(
                ui,
                stage,
                settings.cadence == RecordingCadenceChoice::Manual,
                input_ready,
            );
        });
    egui::CentralPanel::default().show(context, |ui| {
        egui::ScrollArea::vertical()
            .id_salt("x11_controller_settings")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                show_status(ui, stage, progress);
                if let Some(notice) = notice {
                    ui.add(egui::Label::new(notice).wrap());
                }
                if !input_ready {
                    ui.label("Waiting for the recording guide and capture region to be ready.");
                }
                ui.separator();
                show_geometry(ui, geometry, stage);
                ui.separator();
                show_timing(ui, settings, stage);
            });
    });
    if context.input(|input| input.viewport().close_requested()) {
        action = RecorderOverlayAction::Close;
    }
    action
}

fn primary_controls(
    ui: &mut egui::Ui,
    stage: RecorderStage,
    manual: bool,
    input_ready: bool,
) -> RecorderOverlayAction {
    let mut action = RecorderOverlayAction::None;
    ui.horizontal_wrapped(|ui| {
        let mut button = |label, enabled, requested| {
            if ui
                .add_enabled(enabled, egui::Button::new(label).wrap())
                .clicked()
            {
                action = requested;
            }
        };
        match stage {
            RecorderStage::Ready => {
                button("Start", input_ready, RecorderOverlayAction::Start);
                button("Cancel", true, RecorderOverlayAction::Close);
            }
            RecorderStage::Countdown(_) => {
                button("Cancel", true, RecorderOverlayAction::CancelCountdown);
            }
            RecorderStage::Recording => {
                if manual {
                    button(
                        "Take snapshot",
                        input_ready,
                        RecorderOverlayAction::Snapshot,
                    );
                }
                button("Pause", true, RecorderOverlayAction::Pause);
                button("Stop", true, RecorderOverlayAction::Stop);
                button("Discard", true, RecorderOverlayAction::Discard);
            }
            RecorderStage::Paused => {
                button("Resume", input_ready, RecorderOverlayAction::Resume);
                button("Stop", true, RecorderOverlayAction::Stop);
                button("Discard", true, RecorderOverlayAction::Discard);
            }
            RecorderStage::Finalizing => {
                button("Cancel", true, RecorderOverlayAction::Discard);
            }
        }
    });
    action
}

fn show_status(ui: &mut egui::Ui, stage: RecorderStage, progress: Option<WorkflowProgress>) {
    match stage {
        RecorderStage::Ready => {
            ui.strong("Ready to record");
        }
        RecorderStage::Countdown(remaining) => {
            ui.strong(format!("Recording starts in {remaining}s"));
        }
        RecorderStage::Recording => {
            ui.strong("Recording");
        }
        RecorderStage::Paused => {
            ui.strong("Paused");
        }
        RecorderStage::Finalizing => {
            ui.spinner();
            ui.label("Finalizing recoverable project…");
        }
    }
    if let Some(progress) = progress {
        ui.label(format!(
            "{} frames · GIF {:.1}s",
            progress.frames_captured,
            progress.playback_duration.as_secs_f64()
        ))
        .on_hover_text(format!(
            "Source sample span: {:.3}s. Playback timing does not change the captured input clock.",
            progress.capture_duration.as_secs_f64()
        ));
    }
}

fn show_geometry(ui: &mut egui::Ui, geometry: &mut RecorderGeometry, stage: RecorderStage) {
    ui.strong("Recording region · physical pixels");
    ui.label("Global desktop coordinates; negative monitor positions are supported.");
    show_position_buttons(ui, geometry, stage);

    let before = geometry.region();
    let mut position = before.origin();
    let mut width = before.size().width();
    let mut height = before.size().height();
    let moving = stage.allows_moving();
    let resizing = stage.allows_resizing() && !geometry.size_is_frozen();
    let mut changed = false;
    ui.add_enabled_ui(moving, |ui| {
        changed |= ui
            .add(egui::DragValue::new(&mut position.x).speed(1).prefix("X: "))
            .changed();
        changed |= ui
            .add(egui::DragValue::new(&mut position.y).speed(1).prefix("Y: "))
            .changed();
    });
    ui.add_enabled_ui(resizing, |ui| {
        changed |= ui
            .add(
                egui::DragValue::new(&mut width)
                    .speed(1)
                    .range(1..=geometry.source().size().width())
                    .prefix("Width: "),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut height)
                    .speed(1)
                    .range(1..=geometry.source().size().height())
                    .prefix("Height: "),
            )
            .changed();
    });
    if !resizing {
        ui.label("Canvas size is locked; only the recording position can change.");
    }
    if changed {
        let result = PhysicalSize::new(width, height)
            .map_err(|error| error.to_string())
            .and_then(|size| apply_region_edit(geometry, stage, position, size));
        if let Err(error) = result {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }
}

fn show_position_buttons(ui: &mut egui::Ui, geometry: &mut RecorderGeometry, stage: RecorderStage) {
    let step = if ui.input(|input| input.modifiers.shift) {
        10
    } else {
        1
    };
    ui.add_enabled_ui(stage.allows_moving(), |ui| {
        ui.horizontal_wrapped(|ui| {
            for (label, dx, dy) in [
                ("X−", -step, 0),
                ("X+", step, 0),
                ("Y−", 0, -step),
                ("Y+", 0, step),
            ] {
                if ui.button(label).clicked() {
                    geometry.move_by(dx, dy);
                }
            }
        });
    });
    ui.small("Move 1 px; hold Shift for 10 px. Position is clamped to the source.");
}

fn apply_region_edit(
    geometry: &mut RecorderGeometry,
    stage: RecorderStage,
    position: PhysicalPosition,
    size: PhysicalSize,
) -> Result<(), String> {
    if !stage.allows_moving() {
        return Err("The recording region cannot change while finalizing".into());
    }
    let mut candidate = *geometry;
    if size != candidate.region().size() {
        if !stage.allows_resizing() {
            return Err("Recording canvas size is locked".into());
        }
        candidate.resize(size)?;
    }
    candidate.move_to(position);
    *geometry = candidate;
    Ok(())
}

fn show_timing(ui: &mut egui::Ui, settings: &mut RecordingSettings, stage: RecorderStage) {
    let ready = stage == RecorderStage::Ready;
    // Disabled controls must not sanitize or otherwise modify live settings.
    let mut countdown = settings.countdown_seconds;
    let mut duration = settings.duration_ms;
    ui.strong("Timing");
    ui.add_enabled_ui(ready, |ui| {
        ui.label("Start countdown (seconds)");
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
        ui.label("Maximum capture duration (ms)");
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
        ui.small("0 ms means stop manually.");
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
                    Some("Display 1 · waiting for the latest guide acknowledgement. Long status text must not move or cover Stop."),
                    self.ready,
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
                self.frame(
                    vec![
                        egui::Event::PointerMoved(egui::pos2(20.0, 30.0)),
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
    fn moving_uses_physical_pixels_and_shift_ten_without_changing_settings_coordinates() {
        let mut view = View::new(320.0, 240.0, 1.0, RecorderStage::Paused);
        view.geometry.freeze_size();
        let settings_region = (
            view.settings.region_x,
            view.settings.region_y,
            view.settings.region_width,
            view.settings.region_height,
        );
        let button = view.seek("X+");
        assert_eq!(
            view.click(button.center(), false),
            RecorderOverlayAction::None
        );
        assert_eq!(view.geometry.region().origin().x, -99);
        assert_eq!(
            view.click(button.center(), true),
            RecorderOverlayAction::None
        );
        assert_eq!(view.geometry.region().origin().x, -89);
        assert_eq!(
            view.geometry.region().size(),
            PhysicalSize::new(640, 420).unwrap()
        );
        assert_eq!(
            (
                view.settings.region_x,
                view.settings.region_y,
                view.settings.region_width,
                view.settings.region_height
            ),
            settings_region
        );
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
        let position = view.seek("X+").center();
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
}
