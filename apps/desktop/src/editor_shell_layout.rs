//! Display-only page layout; mutation, recording and shutdown gates stay in main.

use eframe::egui;
use gif_from_screen_localization::{Localizer, Message};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InspectorLayout {
    SideBySide,
    Stacked,
}

impl InspectorLayout {
    pub(crate) fn for_width(width: f32) -> Self {
        if width >= 900.0 {
            Self::SideBySide
        } else {
            Self::Stacked
        }
    }
}

/// Unequal columns retain the same child-ID order as egui's columns, including
/// the stacked layout, so changing width does not transfer keyboard focus.
pub(crate) fn work_area<R>(
    ui: &mut egui::Ui,
    layout: InspectorLayout,
    contents: impl FnOnce(&mut [egui::Ui]) -> R,
) -> R {
    if layout == InspectorLayout::Stacked {
        return ui.columns(1, contents);
    }
    let id = ui.id().with("inspector-width");
    let available = ui.available_width();
    let mut width = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<f32>(id))
        .unwrap_or(300.0);
    width = width.clamp(240.0, (available * 0.45).max(240.0));
    let gap = 12.0;
    let origin = ui.cursor().min;
    let mut columns: Vec<_> = [
        (origin.x + available - width, width),
        (origin.x, available - width - gap),
    ]
    .into_iter()
    .map(|(x, width)| {
        let rect = egui::Rect::from_min_max(
            egui::pos2(x, origin.y),
            egui::pos2(x + width, ui.max_rect().bottom()),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down_justified(egui::Align::LEFT)),
        );
        child.set_width(width);
        child
    })
    .collect();
    let result = contents(&mut columns);
    let height = columns
        .iter()
        .map(|column| column.min_size().y)
        .fold(0.0_f32, f32::max);
    let divider = egui::Rect::from_min_size(
        egui::pos2(origin.x + available - width - gap, origin.y),
        egui::vec2(gap, height),
    );
    let response = ui
        .interact(divider, id, egui::Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        width = (origin.x + available - pointer.x - gap * 0.5).clamp(240.0, available * 0.45);
        ui.ctx().data_mut(|data| data.insert_persisted(id, width));
        ui.ctx().request_repaint();
    }
    ui.painter().vline(
        divider.center().x,
        divider.y_range(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
    ui.advance_cursor_after_rect(egui::Rect::from_min_size(
        origin,
        egui::vec2(available, height),
    ));
    result
}

pub(crate) fn inspector<R>(
    ui: &mut egui::Ui,
    layout: InspectorLayout,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    // Both branches introduce one ordinary child Ui, keeping the explicit
    // field-ID hierarchy stable. Stacked content inherits the outer clip and
    // leaves wheel/scroll-to-focus requests to the page, without a nested area.
    match layout {
        InspectorLayout::SideBySide => {
            egui::ScrollArea::vertical()
                .id_salt("editor-inspector")
                .max_height(380.0)
                .auto_shrink([false, true])
                .show(ui, contents)
                .inner
        }
        InspectorLayout::Stacked => ui.scope(contents).inner,
    }
}

pub(crate) fn header(
    ui: &mut egui::Ui,
    back_enabled: Option<bool>,
    localizer: Localizer,
    language: impl FnOnce(&mut egui::Ui),
) -> bool {
    // A row starts at the normal interaction height and grows for its widgets.
    // Filling the panel's cached height here feeds that height into wrapped text
    // on later frames, making large-font headers grow while other content scrolls.
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            // Allocate the actions first, before the brand. The left
            // section can wrap within the actual remaining width without hiding it.
            ui.push_id("app-header-language", language);
            ui.with_layout(
                egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                |ui| {
                    let back = back_enabled.is_some_and(|enabled| {
                        ui.push_id("app-header-back", |ui| {
                            ui.add_enabled(
                                enabled,
                                egui::Button::new(localizer.text(Message::BackToHome)),
                            )
                        })
                        .inner
                        .clicked()
                    });
                    if ui.available_width() >= 600.0 {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                        crate::landing_ui::draw_icon(
                            ui.painter(),
                            rect.center(),
                            20.0,
                            crate::landing_ui::Icon::Screen,
                            crate::appearance::Palette::of(ui).accent,
                        );
                    }
                    ui.label(egui::RichText::new(crate::APP_NAME).size(18.0).strong());
                    back
                },
            )
            .inner
        },
    )
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_tracks_pointer_without_accumulating_repaint_delta() {
        let context = egui::Context::default();
        let mut width = 0.0;
        let mut frame = |events| {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 720.0),
                    )),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        work_area(ui, InspectorLayout::SideBySide, |columns| {
                            width = columns[0].available_width();
                            columns[0].allocate_space(egui::vec2(width, 400.0));
                            assert!(columns[1].available_width() > width);
                        });
                    });
                },
            );
            width
        };
        frame(vec![]);
        let start = egui::pos2(966.0, 100.0);
        frame(vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let end = egui::pos2(876.0, 100.0);
        frame(vec![egui::Event::PointerMoved(end)]);
        for _ in 0..4 {
            assert!((frame(vec![]) - 390.0).abs() < 0.01);
        }
        frame(vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!((frame(vec![]) - 390.0).abs() < 0.01);
    }
}
