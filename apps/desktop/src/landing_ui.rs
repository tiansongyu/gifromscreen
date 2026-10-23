//! Focused launcher: recording first, secondary workflows in named menus.

use crate::{AppView, appearance::Palette};
use eframe::egui::{self, FontId, Pos2, Rect, Stroke, StrokeKind};
use gif_from_screen_localization::{Localizer, Message};

const CONTENT_WIDTH: f32 = 560.0;

#[derive(Clone, Copy)]
pub(crate) enum Icon {
    Screen,
    Camera,
    Board,
    Editor,
}

pub(crate) fn content<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let available = ui.available_width();
    let inset = ((available - CONTENT_WIDTH) * 0.5).max(0.0);
    let mut rect = ui.available_rect_before_wrap();
    rect.min.x += inset;
    rect.max.x = rect.min.x + available.min(CONTENT_WIDTH);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_width(available.min(CONTENT_WIDTH));
        contents(ui)
    })
    .inner
}

pub(crate) fn actions(ui: &mut egui::Ui, localizer: Localizer) -> Option<AppView> {
    let mut action = None;
    ui.columns(4, |columns| {
        for (index, (icon, title, description, view)) in [
            (
                Icon::Screen,
                Message::HomeRecorderTile,
                Message::HomeScreenDescription,
                AppView::ScreenRecorder,
            ),
            (
                Icon::Camera,
                Message::HomeCameraTile,
                Message::HomeCameraDescription,
                AppView::CameraRecorder,
            ),
            (
                Icon::Board,
                Message::HomeBoardTile,
                Message::HomeBoardDescription,
                AppView::BoardRecorder,
            ),
            (
                Icon::Editor,
                Message::HomeEditorTile,
                Message::HomeOpenDescription,
                AppView::Editor,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if card(&mut columns[index], icon, title, description, localizer).clicked() {
                action = Some(view);
            }
        }
    });
    action
}

fn card(
    ui: &mut egui::Ui,
    icon: Icon,
    title: Message,
    description: Message,
    localizer: Localizer,
) -> egui::Response {
    let colors = Palette::of(ui);
    let width = ui.available_width().max(1.0);
    let text_width = (width - 8.0).max(1.0);
    let title_galley = ui.painter().layout(
        localizer.text(title).to_owned(),
        FontId::proportional(14.0),
        colors.text,
        text_width,
    );
    let height = 126.0_f32.max(title_galley.size().y + 72.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            localizer.text(title),
        )
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter().with_clip_rect(ui.clip_rect().intersect(rect));
        let active = response.hovered() || response.has_focus();
        painter.rect(
            rect,
            1,
            if response.hovered() || response.is_pointer_button_down_on() {
                colors.accent_soft
            } else {
                colors.background
            },
            Stroke::new(1.0_f32, if active { colors.accent } else { colors.border }),
            StrokeKind::Inside,
        );
        draw_icon(
            &painter,
            rect.min + egui::vec2(width * 0.5, 48.0),
            34.0,
            icon,
            colors.accent,
        );
        let title_pos = rect.min + egui::vec2((width - title_galley.size().x) * 0.5, 78.0);
        painter.galley(title_pos, title_galley, colors.text);
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!(
            "{}\n{}",
            localizer.text(description),
            localizer.text(Message::HomeDropHint)
        ))
}

pub(crate) fn draw_icon(
    painter: &egui::Painter,
    center: Pos2,
    size: f32,
    icon: Icon,
    color: egui::Color32,
) {
    let rect = Rect::from_center_size(center, egui::Vec2::splat(size));
    let point = |x: f32, y: f32| rect.min + egui::vec2(x * size, y * size);
    let neutral = egui::Color32::from_rgb(125, 125, 125);
    let stroke = Stroke::new(1.5_f32, neutral);
    match icon {
        Icon::Screen => {
            painter.circle_filled(center, size * 0.42, egui::Color32::from_rgb(171, 40, 40));
        }
        Icon::Camera => {
            painter.rect_filled(
                Rect::from_min_max(point(0.0, 0.22), point(1.0, 0.85)),
                1,
                neutral,
            );
            painter.rect_filled(
                Rect::from_min_max(point(0.2, 0.1), point(0.55, 0.3)),
                1,
                neutral,
            );
            painter.circle_stroke(
                point(0.58, 0.53),
                size * 0.21,
                Stroke::new(2.0_f32, egui::Color32::WHITE),
            );
        }
        Icon::Board => {
            painter.rect_stroke(
                Rect::from_min_max(point(0.1, 0.05), point(0.9, 0.95)),
                0,
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment(
                [point(0.27, 0.72), point(0.7, 0.28)],
                Stroke::new(3.0_f32, color),
            );
            painter.line_segment([point(0.27, 0.72), point(0.24, 0.84)], stroke);
        }
        Icon::Editor => {
            painter.rect_stroke(
                Rect::from_min_max(point(0.02, 0.08), point(0.98, 0.92)),
                1,
                stroke,
                StrokeKind::Inside,
            );
            painter.rect_filled(
                Rect::from_min_max(point(0.02, 0.08), point(0.98, 0.27)),
                0,
                neutral,
            );
            painter.line_segment(
                [point(0.15, 0.75), point(0.4, 0.45)],
                Stroke::new(2.0_f32, color),
            );
            painter.line_segment(
                [point(0.4, 0.45), point(0.62, 0.72)],
                Stroke::new(2.0_f32, color),
            );
            painter.line_segment(
                [point(0.62, 0.72), point(0.8, 0.55)],
                Stroke::new(2.0_f32, color),
            );
        }
    }
}

pub(crate) const INITIAL_WINDOW_SIZE: egui::Vec2 = egui::vec2(520.0, 190.0);
pub(crate) const MINIMUM_WINDOW_SIZE: egui::Vec2 = egui::vec2(440.0, 180.0);

/// The launcher owns its size independently of the editor and recorder. Store
/// native logical units so changing UI zoom cannot accumulate size inflation.
pub(crate) struct WindowSizing {
    home_size: egui::Vec2,
    home_maximized: Option<bool>,
    on_home: bool,
    restore: Option<crate::wayland_controller_layout::GeometryTransition>,
}

impl Default for WindowSizing {
    fn default() -> Self {
        Self {
            home_size: INITIAL_WINDOW_SIZE,
            home_maximized: Some(false),
            on_home: true,
            restore: None,
        }
    }
}

impl WindowSizing {
    pub(crate) fn update(&mut self, context: &egui::Context, on_home: bool) {
        let zoom = context.zoom_factor();
        let (size, maximized) =
            context.input(|input| (input.screen_rect.size() * zoom, input.viewport().maximized));
        if on_home != self.on_home {
            self.on_home = on_home;
            self.restore = None;
            if on_home {
                self.restore = Some(crate::wayland_controller_layout::GeometryTransition::new(
                    self.home_size,
                    MINIMUM_WINDOW_SIZE.min(self.home_size),
                    self.home_maximized,
                    std::time::Instant::now(),
                ));
            } else if maximized != Some(true) && size.y < 360.0 {
                context.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                    size.max(egui::vec2(960.0, 680.0)) / zoom,
                ));
            }
        }
        if let Some(restore) = &mut self.restore {
            let update = restore.advance(size, maximized, zoom, std::time::Instant::now());
            for command in update.commands {
                context.send_viewport_cmd(command);
            }
            if update.finished {
                self.restore = None;
            } else {
                context.request_repaint_after(std::time::Duration::from_millis(16));
            }
        } else if on_home {
            // Do not save an editor/recorder size while a native resize is still
            // pending. Normal manual resizing on the home page remains allowed.
            if maximized != Some(true) {
                self.home_size = size;
            }
            self.home_maximized = maximized;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_cards_accept_tab_focus_and_enter_or_space_activation() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let context = egui::Context::default();
            crate::appearance::configure(&context);
            let localizer = crate::test_localizer();
            let mut focused = false;
            let mut clicked = false;
            let mut frame = |events| {
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            Pos2::ZERO,
                            egui::vec2(400.0, 300.0),
                        )),
                        focused: true,
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            let response = card(
                                ui,
                                Icon::Screen,
                                Message::HomeScreenRecorder,
                                Message::HomeScreenDescription,
                                localizer,
                            );
                            focused = response.has_focus();
                            clicked = response.clicked();
                        });
                    },
                );
                (focused, clicked)
            };
            frame(vec![]);
            let keyboard = |key, pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            assert_eq!(frame(vec![keyboard(egui::Key::Tab, true)]), (true, false));
            assert_eq!(
                frame(vec![keyboard(egui::Key::Tab, false), keyboard(key, true)]),
                (true, true)
            );
        }
    }
}
