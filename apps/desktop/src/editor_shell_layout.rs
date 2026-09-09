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

    pub(crate) const fn columns(self) -> usize {
        match self {
            Self::SideBySide => 2,
            Self::Stacked => 1,
        }
    }
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
            // Allocate the action first, before the optional subtitle. The left
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
                    ui.heading(crate::APP_NAME);
                    let subtitle = localizer.text(Message::HeaderPreview);
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let width = ui
                        .painter()
                        .layout_no_wrap(subtitle.to_owned(), font, ui.visuals().text_color())
                        .size()
                        .x;
                    if width + ui.spacing().item_spacing.x * 2.0 + 8.0 <= ui.available_width() {
                        ui.separator();
                        ui.label(subtitle);
                    }
                    back
                },
            )
            .inner
        },
    )
    .inner
}
