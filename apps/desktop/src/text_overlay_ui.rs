//! Caption authoring owns its bounded worker and selection anchor, not application-wide job flags.

use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

use eframe::egui;
use gif_from_screen_domain::{
    DurationUs, FrameId, HorizontalAlignment, OverlayContent, OverlayTrack, PhysicalPoint,
    PhysicalPx, PhysicalSize, Rgba, TrackId,
};
use gif_from_screen_localization::{Localizer, Message};
use gif_from_screen_text::{
    MAX_TEXT_BYTES, MAX_TEXT_EDGE, TextError, TextImage, TextRasterizer, TextRequest,
};

use crate::editor_workspace::{
    EditorWorkspace, OverlaySelectionAnchor, TextOverlayDraft, TitleFrameRequest,
};
use crate::ui_notice::Notice;

#[derive(Clone, Copy)]
enum TextOperation {
    Add,
    Replace(TrackId),
    Title {
        after: Option<FrameId>,
        duration: DurationUs,
        background: Rgba,
    },
}

struct PendingText {
    anchor: OverlaySelectionAnchor,
    request: TextRequest,
    position: PhysicalPoint,
    receiver: Receiver<Result<TextImage, TextError>>,
    cancelled: bool,
    operation: TextOperation,
}

pub(crate) struct TextOverlayTool {
    text: String,
    font_family: String,
    font_size: u16,
    alignment: HorizontalAlignment,
    position: [u32; 2],
    size: [u32; 2],
    foreground: [u8; 4],
    background: [u8; 4],
    with_background: bool,
    pending: Option<PendingText>,
    editing: Option<(TrackId, OverlaySelectionAnchor)>,
    title: bool,
    title_at_start: bool,
    title_duration_ms: u64,
    title_background: [u8; 4],
}

impl Default for TextOverlayTool {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_family: "sans-serif".to_owned(),
            font_size: 24,
            alignment: HorizontalAlignment::Start,
            position: [0, 0],
            size: [0, 0],
            foreground: [255; 4],
            background: [0, 0, 0, 160],
            with_background: true,
            pending: None,
            editing: None,
            title: false,
            title_at_start: true,
            title_duration_ms: 1000,
            title_background: [24, 28, 36, 255],
        }
    }
}

impl TextOverlayTool {
    pub(crate) fn is_running(&self) -> bool {
        self.pending.is_some()
    }

    /// Poll even while the tool tab is hidden, and never mutate a changed or closed project.
    pub(crate) fn poll(&mut self, workspace: Option<&mut EditorWorkspace>) -> Option<Notice> {
        let pending = self.pending.as_ref()?;
        let result = match pending.receiver.try_recv() {
            Ok(result) => result.map_err(|error| text_failure(&error)),
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(Message::TextWorkerExited.into()),
        };
        let pending = self.pending.take()?;
        if pending.cancelled {
            return Some(Message::TextCancelled.into());
        }
        Some(match result {
            Err(error) => error,
            Ok(image) => match workspace {
                Some(workspace) if pending.anchor.matches(workspace) => {
                    let result = match pending.operation {
                        TextOperation::Add => workspace
                            .add_text_overlay_for_selection(
                                &pending.request,
                                &image,
                                pending.position,
                            )
                            .map(|_| ()),
                        TextOperation::Replace(track_id) => workspace
                            .replace_text_overlay(
                                track_id,
                                &pending.request,
                                &image,
                                pending.position,
                            )
                            .map(|_| ()),
                        TextOperation::Title {
                            after,
                            duration,
                            background,
                        } => workspace
                            .insert_title_frame(
                                &TitleFrameRequest {
                                    after,
                                    duration,
                                    background,
                                    text: pending.request,
                                    position: pending.position,
                                },
                                &image,
                            )
                            .map(|_| ()),
                    };
                    match result {
                        Ok(()) => {
                            if let Some((_, anchor)) = &mut self.editing
                                && let Ok(updated) = workspace.overlay_selection_anchor()
                            {
                                *anchor = updated;
                            }
                            match pending.operation {
                                TextOperation::Add => Message::TextAdded,
                                TextOperation::Replace(_) => Message::TextUpdated,
                                TextOperation::Title { .. } => Message::TextTitleInserted,
                            }
                            .into()
                        }
                        Err(error) => diagnostic(Message::TextSaveFailed, &error),
                    }
                }
                _ => Message::TextTargetChanged.into(),
            },
        })
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &EditorWorkspace,
        localizer: Localizer,
    ) -> Option<Notice> {
        let mut notice = None;
        ui.group(|ui| {
            ui.strong(localizer.text(Message::EditorContentText));
            ui.add_enabled_ui(!self.is_running(), |ui| {
                self.show_saved_text(ui, workspace, &mut notice, localizer);
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .id_salt("caption_text")
                        .hint_text(localizer.text(Message::TextCaptionHint))
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .char_limit(MAX_TEXT_BYTES),
                );
                self.show_style(ui, localizer);
                egui::CollapsingHeader::new(localizer.text(Message::TextPlacement))
                    .id_salt("caption-placement")
                    .show(ui, |ui| {
                        self.show_placement(ui, localizer);
                        ui.weak(localizer.text(Message::TextAutomaticBoxHint));
                    });
                self.show_title_options(ui, localizer);
                let action_label = if self.editing.is_some() {
                    Message::TextSaveChanges
                } else if self.title {
                    Message::TextInsertTitle
                } else {
                    Message::TextAddSelected
                };
                if ui
                    .add_enabled(
                        (self.title && self.editing.is_none()) || !workspace.selection().is_empty(),
                        egui::Button::new(localizer.text(action_label)),
                    )
                    .clicked()
                    && let Err(error) = self.start(workspace)
                {
                    notice = Some(error);
                }
            });
            if self.is_running() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        localizer.text(
                            if self
                                .pending
                                .as_ref()
                                .is_some_and(|pending| pending.cancelled)
                            {
                                Message::TextCancelling
                            } else {
                                Message::TextPreparing
                            },
                        ),
                    );
                });
                if ui
                    .small_button(localizer.text(Message::CancelButton))
                    .clicked()
                    && let Some(pending) = &mut self.pending
                {
                    // Retain the worker slot until completion so repeated cancellation cannot
                    // spawn an unbounded number of font discovery/rasterization threads.
                    pending.cancelled = true;
                    notice = Some(Message::TextCancelling.into());
                }
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
        });
        if let Some(notice) = &mut notice {
            notice.refresh(localizer);
        }
        notice
    }

    fn show_style(&mut self, ui: &mut egui::Ui, localizer: Localizer) {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let label_width = [
            Message::TextFont,
            Message::TextFontSize,
            Message::TextAlignment,
            Message::TextColor,
            Message::ImageEffectBackground,
        ]
        .map(|message| {
            ui.painter()
                .layout_no_wrap(
                    localizer.text(message).to_owned(),
                    font.clone(),
                    ui.visuals().text_color(),
                )
                .size()
                .x
        })
        .into_iter()
        .fold(0.0_f32, f32::max);
        let value_width =
            (ui.available_width() - label_width - ui.spacing().item_spacing.x).max(1.0);
        egui::Grid::new("caption_style")
            .num_columns(2)
            .min_col_width(0.0)
            .show(ui, |ui| {
                ui.label(localizer.text(Message::TextFont));
                ui.add(
                    egui::TextEdit::singleline(&mut self.font_family)
                        .id_salt("caption_font_family")
                        .desired_width(value_width)
                        .char_limit(256),
                );
                ui.end_row();
                ui.label(localizer.text(Message::TextFontSize));
                ui.push_id("caption_font_size", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.font_size)
                            .range(1..=512)
                            .suffix(" px"),
                    )
                });
                ui.end_row();
                ui.label(localizer.text(Message::TextAlignment));
                egui::ComboBox::from_id_salt("caption_alignment")
                    .selected_text(localizer.text(alignment_label(self.alignment)))
                    .show_ui(ui, |ui| {
                        for alignment in [
                            HorizontalAlignment::Start,
                            HorizontalAlignment::Center,
                            HorizontalAlignment::End,
                        ] {
                            ui.selectable_value(
                                &mut self.alignment,
                                alignment,
                                localizer.text(alignment_label(alignment)),
                            );
                        }
                    });
                ui.end_row();
                ui.label(localizer.text(Message::TextColor));
                edit_color(ui, "caption_foreground", &mut self.foreground);
                ui.end_row();
                let label = ui.label(localizer.text(Message::ImageEffectBackground));
                ui.horizontal(|ui| {
                    ui.push_id("caption_with_background", |ui| {
                        ui.checkbox(&mut self.with_background, "")
                            .labelled_by(label.id)
                    });
                    if self.with_background {
                        edit_color(ui, "caption_background", &mut self.background);
                    }
                });
                ui.end_row();
            });
    }

    fn show_placement(&mut self, ui: &mut egui::Ui, localizer: Localizer) {
        let [x, y] = &mut self.position;
        let [width, height] = &mut self.size;
        egui::Grid::new("caption_bounds")
            .num_columns(2)
            .show(ui, |ui| {
                for (id, message, value, maximum) in [
                    ("caption_x", Message::CropFieldX, x, u32::MAX),
                    ("caption_y", Message::CropFieldY, y, u32::MAX),
                    (
                        "caption_width",
                        Message::RecorderWidth,
                        width,
                        MAX_TEXT_EDGE,
                    ),
                    (
                        "caption_height",
                        Message::RecorderHeight,
                        height,
                        MAX_TEXT_EDGE,
                    ),
                ] {
                    ui.label(localizer.text(message));
                    ui.push_id(id, |ui| {
                        ui.add(egui::DragValue::new(value).range(0..=maximum))
                    });
                    ui.end_row();
                }
            });
    }

    fn request(&self, canvas: PhysicalSize) -> Result<(TextRequest, PhysicalPoint), Notice> {
        let width = resolve_extent(self.position[0], self.size[0], canvas.width.get(), 4096)?;
        let height = resolve_extent(self.position[1], self.size[1], canvas.height.get(), 160)?;
        let request = TextRequest {
            text: self.text.clone(),
            font_family: self.font_family.trim().to_owned(),
            font_size_px: self.font_size,
            size: PhysicalSize::new(width, height)
                .map_err(|error| diagnostic(Message::TextStartFailed, &error))?,
            foreground: rgba(self.foreground),
            background: self.with_background.then(|| rgba(self.background)),
            alignment: self.alignment,
        };
        request.validate().map_err(|error| text_failure(&error))?;
        Ok((
            request,
            PhysicalPoint {
                x: PhysicalPx::new(self.position[0]),
                y: PhysicalPx::new(self.position[1]),
            },
        ))
    }

    fn start(&mut self, workspace: &EditorWorkspace) -> Result<(), Notice> {
        if self.is_running() {
            return Err(Message::TextAlreadyPreparing.into());
        }
        let anchor = if self.title && self.editing.is_none() {
            workspace.project_edit_anchor()
        } else {
            workspace
                .overlay_selection_anchor()
                .map_err(|error| diagnostic(Message::TextStartFailed, &error))?
        };
        let operation = if let Some((track_id, loaded)) = &self.editing {
            if !loaded.matches(workspace) {
                return Err(Message::TextLoadedTargetChanged.into());
            }
            TextOperation::Replace(*track_id)
        } else if self.title {
            TextOperation::Title {
                after: if self.title_at_start {
                    None
                } else {
                    Some(
                        workspace
                            .selection()
                            .current()
                            .ok_or(Message::TextTitleNeedsCurrent)?,
                    )
                },
                duration: self
                    .title_duration_ms
                    .checked_mul(1000)
                    .and_then(DurationUs::new)
                    .ok_or(Message::TextTitleDurationRequired)?,
                background: rgba(self.title_background),
            }
        } else {
            TextOperation::Add
        };
        let canvas = match &operation {
            TextOperation::Replace(track_id) => workspace
                .text_overlay_authoring_size(*track_id)
                .map_err(|error| diagnostic(Message::TextStartFailed, &error))?,
            TextOperation::Add => workspace
                .selected_authoring_size()
                .map_err(|error| diagnostic(Message::TextStartFailed, &error))?,
            TextOperation::Title { .. } => workspace.manifest().canvas.size,
        };
        let (request, position) = self.request(canvas)?;
        let work = request.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("gfs-text-overlay".to_owned())
            .spawn(move || {
                let result = TextRasterizer::with_system_fonts().rasterize(&work);
                let _ = sender.send(result);
            })
            .map_err(|error| diagnostic(Message::TextWorkerStartFailed, &error))?;
        self.pending = Some(PendingText {
            anchor,
            request,
            position,
            receiver,
            cancelled: false,
            operation,
        });
        Ok(())
    }

    fn show_saved_text(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &EditorWorkspace,
        notice: &mut Option<Notice>,
        localizer: Localizer,
    ) {
        let selected_label = self
            .editing
            .as_ref()
            .and_then(|(id, _)| {
                workspace
                    .manifest()
                    .timeline
                    .overlay_tracks
                    .iter()
                    .enumerate()
                    .find(|(_, track)| track.id == *id)
            })
            .map_or_else(
                || localizer.text(Message::TextEditSaved).to_owned(),
                |(index, track)| saved_group_label(index, track, localizer),
            );
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("saved-caption")
                .width(ui.available_width().min(340.0))
                .truncate()
                .selected_text(selected_label.as_str())
                .show_ui(ui, |ui| {
                    for (index, track) in workspace
                        .manifest()
                        .timeline
                        .overlay_tracks
                        .iter()
                        .enumerate()
                    {
                        let Some((_, OverlayContent::Text { .. })) =
                            track.all_mark_contents().next()
                        else {
                            continue;
                        };
                        if ui
                            .push_id(track.id, |ui| {
                                ui.selectable_label(
                                    self.editing.as_ref().is_some_and(|(id, _)| *id == track.id),
                                    saved_group_label(index, track, localizer),
                                )
                            })
                            .inner
                            .clicked()
                            && let Err(error) = self.load(workspace, track.id)
                        {
                            *notice = Some(error);
                        }
                    }
                })
                .response
                .on_hover_text(&selected_label);
            if self.editing.is_some() && ui.button(localizer.text(Message::TextNew)).clicked() {
                self.editing = None;
            }
        });
    }

    fn load(&mut self, workspace: &EditorWorkspace, track_id: TrackId) -> Result<(), Notice> {
        let draft: TextOverlayDraft = workspace
            .text_overlay_draft(track_id)
            .map_err(|error| diagnostic(Message::TextLoadFailed, &error))?;
        let anchor = workspace
            .overlay_selection_anchor()
            .map_err(|error| diagnostic(Message::TextLoadFailed, &error))?;
        self.text = draft.request.text;
        self.font_family = draft.request.font_family;
        self.font_size = draft.request.font_size_px;
        self.alignment = draft.request.alignment;
        self.position = [draft.position.x.get(), draft.position.y.get()];
        self.size = [
            draft.request.size.width.get(),
            draft.request.size.height.get(),
        ];
        self.foreground = color_bytes(draft.request.foreground);
        self.with_background = draft.request.background.is_some();
        if let Some(background) = draft.request.background {
            self.background = color_bytes(background);
        }
        self.editing = Some((draft.track_id, anchor));
        self.title = false;
        Ok(())
    }

    fn show_title_options(&mut self, ui: &mut egui::Ui, localizer: Localizer) {
        if self.editing.is_some() {
            return;
        }
        ui.push_id("caption_as_title", |ui| {
            ui.checkbox(&mut self.title, localizer.text(Message::TextAsTitle))
        });
        if self.title {
            egui::Grid::new("caption_title_fields")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(localizer.text(Message::TextTitlePosition));
                    egui::ComboBox::from_id_salt("caption_title_position")
                        .selected_text(localizer.text(if self.title_at_start {
                            Message::TextAtStart
                        } else {
                            Message::TextAfterCurrent
                        }))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.title_at_start,
                                true,
                                localizer.text(Message::TextAtStart),
                            );
                            ui.selectable_value(
                                &mut self.title_at_start,
                                false,
                                localizer.text(Message::TextAfterCurrent),
                            );
                        });
                    ui.end_row();
                    ui.label(localizer.text(Message::RecorderDuration));
                    ui.push_id("caption_title_duration", |ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.title_duration_ms)
                                .range(1..=3_600_000)
                                .suffix(" ms"),
                        );
                    });
                    ui.end_row();
                    ui.label(localizer.text(Message::EditorStatsCanvas));
                    edit_color(ui, "caption_title_background", &mut self.title_background);
                    ui.end_row();
                });
        }
    }
}

fn diagnostic(message: Message, error: &impl std::fmt::Display) -> Notice {
    Notice::new(message, &[("error", &error.to_string())])
}

fn edit_color(ui: &mut egui::Ui, id: &'static str, value: &mut [u8; 4]) -> egui::Response {
    // egui's sRGBA conversion round-trips even without input. Preserve the
    // authored bytes until an actual color edit, including while disabled.
    let mut edited = *value;
    let response = ui
        .push_id(id, |ui| {
            ui.color_edit_button_srgba_unmultiplied(&mut edited)
        })
        .inner;
    if response.changed() {
        *value = edited;
    }
    response
}

fn text_failure(error: &TextError) -> Notice {
    match error {
        TextError::Empty => Message::TextEmpty.into(),
        TextError::TooLong => Notice::new(
            Message::TextTooLong,
            &[("maximum", &MAX_TEXT_BYTES.to_string())],
        ),
        TextError::FontSize => Notice::new(
            Message::TextFontSizeRange,
            &[("minimum", "1"), ("maximum", "512")],
        ),
        TextError::FontFamily => Notice::new(Message::TextFontFamilyLength, &[("maximum", "256")]),
        TextError::FontNotFound => Message::TextFontMissing.into(),
        TextError::GlyphBudget => Message::TextGlyphBudget.into(),
        TextError::Dimensions => Notice::new(
            Message::TextDimensions,
            &[("minimum", "1"), ("maximum", &MAX_TEXT_EDGE.to_string())],
        ),
        TextError::Invisible => Message::TextInvisible.into(),
        TextError::MissingGlyphs => Message::TextMissingGlyphs.into(),
        TextError::DoesNotFit => Message::TextDoesNotFit.into(),
        TextError::Allocation => Message::TextAllocation.into(),
    }
}

const fn alignment_label(alignment: HorizontalAlignment) -> Message {
    match alignment {
        HorizontalAlignment::Start => Message::TextAlignLeft,
        HorizontalAlignment::Center => Message::TextAlignCenter,
        HorizontalAlignment::End => Message::TextAlignRight,
    }
}

fn saved_group_label(index: usize, track: &OverlayTrack, localizer: Localizer) -> String {
    let (count, message) = match &track.frame_cells {
        Some(cells) => (
            cells.len(),
            if cells.len() == 1 {
                Message::TextSavedFrame
            } else {
                Message::TextSavedFrames
            },
        ),
        None => (
            track.items.len(),
            if track.items.len() == 1 {
                Message::TextSavedItem
            } else {
                Message::TextSavedItems
            },
        ),
    };
    Notice::localized(
        localizer,
        message,
        &[
            ("layer", &(index + 1).to_string()),
            ("name", &track.name),
            ("count", &count.to_string()),
        ],
    )
    .to_string()
}

fn color_bytes(color: Rgba) -> [u8; 4] {
    [color.red, color.green, color.blue, color.alpha]
}

fn resolve_extent(
    origin: u32,
    requested: u32,
    canvas: u32,
    automatic_limit: u32,
) -> Result<u32, Notice> {
    let available = canvas
        .checked_sub(origin)
        .filter(|value| *value != 0)
        .ok_or(Message::TextPositionInside)?;
    let extent = if requested == 0 {
        available.min(automatic_limit)
    } else {
        requested
    };
    if extent > available {
        return Err(Message::TextBoxFits.into());
    }
    Ok(extent)
}

fn rgba([red, green, blue, alpha]: [u8; 4]) -> Rgba {
    Rgba {
        red,
        green,
        blue,
        alpha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn language(tag: &str) -> Localizer {
        Localizer::new(gif_from_screen_localization::find_language(tag).unwrap())
    }

    fn context(zoom: f32) -> egui::Context {
        let context = egui::Context::default();
        crate::preferences::fonts::install(&context);
        context.set_zoom_factor(zoom);
        context.style_mut(|style| style.animation_time = 0.0);
        context
    }

    fn draw_tool(
        context: &egui::Context,
        tool: &mut TextOverlayTool,
        workspace: &EditorWorkspace,
        tag: &str,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<Notice>) {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 1000.0) / context.zoom_factor(),
            )),
            events,
            focused: true,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(1.0);
        let mut notice = None;
        let output = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    notice = tool.show(ui, workspace, language(tag));
                });
            });
        });
        (output, notice)
    }

    fn click_tool(
        context: &egui::Context,
        tool: &mut TextOverlayTool,
        workspace: &EditorWorkspace,
        tag: &str,
        position: egui::Pos2,
    ) -> Option<Notice> {
        let mut notice = None;
        for pressed in [true, false] {
            let current = draw_tool(
                context,
                tool,
                workspace,
                tag,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .1;
            if current.is_some() {
                notice = current;
            }
        }
        notice
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Fields {
        text: String,
        font: String,
        font_size: u16,
        alignment: HorizontalAlignment,
        position: [u32; 2],
        size: [u32; 2],
        foreground: [u8; 4],
        background: [u8; 4],
        with_background: bool,
        title: bool,
        title_at_start: bool,
        title_duration_ms: u64,
        title_background: [u8; 4],
    }

    fn fields(tool: &TextOverlayTool) -> Fields {
        Fields {
            text: tool.text.clone(),
            font: tool.font_family.clone(),
            font_size: tool.font_size,
            alignment: tool.alignment,
            position: tool.position,
            size: tool.size,
            foreground: tool.foreground,
            background: tool.background,
            with_background: tool.with_background,
            title: tool.title,
            title_at_start: tool.title_at_start,
            title_duration_ms: tool.title_duration_ms,
            title_background: tool.title_background,
        }
    }

    fn draft() -> TextOverlayTool {
        TextOverlayTool {
            text: "Literal {name}\n用户文字".into(),
            font_family: "Ubuntu".into(),
            font_size: 17,
            alignment: HorizontalAlignment::Center,
            position: [13, 29],
            size: [140, 110],
            foreground: [1, 2, 3, 254],
            background: [4, 5, 6, 123],
            title: true,
            title_at_start: false,
            title_duration_ms: 1234,
            title_background: [7, 8, 9, 210],
            ..Default::default()
        }
    }

    fn interactive_response(context: &egui::Context, position: egui::Pos2) -> egui::Response {
        let hovered: Vec<_> =
            context.interaction_snapshot(|state| state.hovered.iter().copied().collect());
        hovered
            .into_iter()
            .filter_map(|id| context.read_response(id))
            .filter(|response| response.sense.senses_click() && response.rect.contains(position))
            .min_by(|left, right| left.rect.area().total_cmp(&right.rect.area()))
            .expect("pointer hits an actual input control")
    }

    #[test]
    fn text_title_fields_are_paired_and_keep_values_and_ids_across_language_and_zoom() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = workspace(&directory.path().join("layout.gfsproj"));
        for zoom in [1.0, 1.5] {
            let context = context(zoom);
            let mut tool = draft();
            let before = fields(&tool);
            let mut first_ids = None;
            for tag in ["en", "zh", "en"] {
                for _ in 0..3 {
                    draw_tool(&context, &mut tool, &workspace, tag, vec![]);
                }
                let (output, _) = draw_tool(&context, &mut tool, &workspace, tag, vec![]);
                let placement =
                    saved_label_rect(&output, language(tag).text(Message::TextPlacement));
                if tag == "en" && first_ids.is_none() {
                    click_tool(&context, &mut tool, &workspace, tag, placement.center());
                    draw_tool(&context, &mut tool, &workspace, tag, vec![]);
                }
                let mut ids = Vec::new();
                for (message, value) in [
                    (Message::TextFont, "Ubuntu"),
                    (Message::TextFontSize, "17 px"),
                    (
                        Message::TextAlignment,
                        language(tag).text(Message::TextAlignCenter),
                    ),
                    (Message::CropFieldX, "13"),
                    (Message::CropFieldY, "29"),
                    (Message::RecorderWidth, "140"),
                    (Message::RecorderHeight, "110"),
                    (
                        Message::TextTitlePosition,
                        language(tag).text(Message::TextAfterCurrent),
                    ),
                    (Message::RecorderDuration, "1234 ms"),
                ] {
                    let (output, _) = draw_tool(&context, &mut tool, &workspace, tag, vec![]);
                    let label = saved_label_rect(&output, language(tag).text(message));
                    let value = saved_label_rect(&output, value);
                    assert!(label.is_positive() && value.is_positive());
                    draw_tool(
                        &context,
                        &mut tool,
                        &workspace,
                        tag,
                        vec![egui::Event::PointerMoved(value.center())],
                    );
                    let response = interactive_response(&context, value.center());
                    assert!(
                        label.right() < response.rect.left(),
                        "label/value columns overlap"
                    );
                    assert!(
                        label.y_range().contains(response.rect.center().y)
                            && response.rect.y_range().contains(label.center().y),
                        "label/value rows split"
                    );
                    assert!(
                        response.rect.right() <= 480.0 / zoom,
                        "input exceeds actual viewport"
                    );
                    ids.push(response.id);
                }
                if let Some(first_ids) = &first_ids {
                    assert_eq!(&ids, first_ids);
                } else {
                    first_ids = Some(ids);
                }
                assert_eq!(fields(&tool), before);
            }
        }
    }

    #[test]
    fn all_three_real_localized_action_buttons_use_typed_validation_without_mutating_project() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("actions.gfsproj"));
        completed_tool(&workspace, false).poll(Some(&mut workspace));
        let id = workspace.manifest().timeline.overlay_tracks[0].id;
        let before = workspace.manifest().clone();
        for tag in ["en", "zh"] {
            for action in [
                Message::TextAddSelected,
                Message::TextInsertTitle,
                Message::TextSaveChanges,
            ] {
                let context = context(1.5);
                let mut tool = TextOverlayTool::default();
                if action == Message::TextInsertTitle {
                    tool.title = true;
                }
                if action == Message::TextSaveChanges {
                    tool.load(&workspace, id).unwrap();
                    tool.text.clear();
                }
                for _ in 0..3 {
                    draw_tool(&context, &mut tool, &workspace, tag, vec![]);
                }
                let (output, _) = draw_tool(&context, &mut tool, &workspace, tag, vec![]);
                let button = saved_label_rect(&output, language(tag).text(action));
                assert!(button.is_positive());
                let notice =
                    click_tool(&context, &mut tool, &workspace, tag, button.center()).unwrap();
                assert_eq!(notice.message_id(), Some(Message::TextEmpty));
                assert_eq!(&*notice, language(tag).text(Message::TextEmpty));
                assert_eq!(workspace.manifest(), &before);
                assert!(!tool.is_running());
            }
        }
    }

    #[test]
    fn focused_text_and_font_survive_language_change_and_pending_work_locks_form() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("focus.gfsproj"));
        let context = context(1.5);
        let mut tool = draft();
        tool.title = false;
        tool.text = "Literal {text}".into();
        for target in ["Literal {text}", "Ubuntu"] {
            for _ in 0..3 {
                draw_tool(&context, &mut tool, &workspace, "en", vec![]);
            }
            let (output, _) = draw_tool(&context, &mut tool, &workspace, "en", vec![]);
            let point = saved_label_rect(&output, target).center();
            click_tool(&context, &mut tool, &workspace, "en", point);
            let focus = context.memory(egui::Memory::focused).unwrap();
            draw_tool(&context, &mut tool, &workspace, "zh", vec![]);
            assert_eq!(context.memory(egui::Memory::focused), Some(focus));
            draw_tool(
                &context,
                &mut tool,
                &workspace,
                "zh",
                vec![egui::Event::Text("{用户}".into())],
            );
        }
        assert!(tool.text.contains("{用户}") && tool.font_family.contains("{用户}"));
        let before = fields(&tool);
        let manifest = workspace.manifest().clone();
        let (request, position) = tool.request(workspace.manifest().canvas.size).unwrap();
        let (sender, receiver) = mpsc::channel();
        tool.pending = Some(PendingText {
            anchor: workspace.overlay_selection_anchor().unwrap(),
            request,
            position,
            receiver,
            cancelled: false,
            operation: TextOperation::Add,
        });
        draw_tool(
            &context,
            &mut tool,
            &workspace,
            "zh",
            vec![egui::Event::Text("must not enter".into())],
        );
        assert_eq!(fields(&tool), before);
        let (output, _) = draw_tool(&context, &mut tool, &workspace, "zh", vec![]);
        let cancel = saved_label_rect(&output, language("zh").text(Message::CancelButton));
        let notice = click_tool(&context, &mut tool, &workspace, "zh", cancel.center()).unwrap();
        assert_eq!(notice.message_id(), Some(Message::TextCancelling));
        assert!(tool.is_running());
        assert!(tool.poll(Some(&mut workspace)).is_none());
        assert_eq!(
            tool.start(&workspace).unwrap_err().message_id(),
            Some(Message::TextAlreadyPreparing)
        );
        sender.send(Err(TextError::FontNotFound)).unwrap();
        let notice = tool.poll(Some(&mut workspace)).unwrap();
        assert_eq!(notice.message_id(), Some(Message::TextCancelled));
        assert_eq!(
            notice.render(language("zh")),
            language("zh").text(Message::TextCancelled)
        );
        assert_eq!(workspace.manifest(), &manifest);
    }

    fn assert_notice_languages(notice: &Notice, message: Message) {
        assert_eq!(notice.message_id(), Some(message));
        let english = notice.render(language("en"));
        let chinese = notice.render(language("zh"));
        assert_ne!(english, chinese);
        let mut retained = notice.clone();
        retained.refresh(language("zh"));
        assert_eq!(&*retained, chinese);
        retained.refresh(language("en"));
        assert_eq!(&*retained, english);
    }

    #[test]
    fn all_text_error_variants_have_typed_late_rendering_and_diagnostics_stay_literal() {
        for (error, message) in [
            (TextError::Empty, Message::TextEmpty),
            (TextError::TooLong, Message::TextTooLong),
            (TextError::FontSize, Message::TextFontSizeRange),
            (TextError::FontFamily, Message::TextFontFamilyLength),
            (TextError::FontNotFound, Message::TextFontMissing),
            (TextError::GlyphBudget, Message::TextGlyphBudget),
            (TextError::Dimensions, Message::TextDimensions),
            (TextError::Invisible, Message::TextInvisible),
            (TextError::MissingGlyphs, Message::TextMissingGlyphs),
            (TextError::DoesNotFit, Message::TextDoesNotFit),
            (TextError::Allocation, Message::TextAllocation),
        ] {
            let notice = text_failure(&error);
            assert_eq!(notice.render(language("en")), error.to_string());
            assert_notice_languages(&notice, message);
        }
        let raw = "Enter some text first. 用户/{error}/{name}\nraw backend detail";
        for message in [
            Message::TextStartFailed,
            Message::TextWorkerStartFailed,
            Message::TextSaveFailed,
            Message::TextLoadFailed,
        ] {
            let notice = diagnostic(message, &raw);
            assert_notice_languages(&notice, message);
            for tag in ["en", "zh"] {
                assert!(notice.render(language(tag)).contains(raw));
            }
        }
    }

    #[test]
    fn title_start_validation_remains_typed_and_does_not_spawn_or_edit() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("title-validation.gfsproj"));
        let before = workspace.manifest().clone();
        let mut tool = TextOverlayTool {
            text: "Title".into(),
            title: true,
            ..Default::default()
        };
        for duration in [0, u64::MAX] {
            tool.title_duration_ms = duration;
            let notice = tool.start(&workspace).unwrap_err();
            assert_notice_languages(&notice, Message::TextTitleDurationRequired);
            assert!(!tool.is_running());
        }
        tool.title_duration_ms = 1000;
        tool.title_at_start = false;
        workspace.clear_selection();
        assert_notice_languages(
            &tool.start(&workspace).unwrap_err(),
            Message::TextTitleNeedsCurrent,
        );
        tool.title_at_start = true;
        tool.text.clear();
        assert_notice_languages(&tool.start(&workspace).unwrap_err(), Message::TextEmpty);
        assert!(!tool.is_running());
        assert_eq!(workspace.manifest(), &before);
    }

    #[test]
    fn title_completions_keep_frozen_parameters_both_positions_and_undo_redo_journal_reopen() {
        let directory = tempfile::tempdir().unwrap();
        for after_current in [false, true] {
            let root = directory.path().join(if after_current {
                "after.gfsproj"
            } else {
                "start.gfsproj"
            });
            let mut workspace = workspace(&root);
            let original = workspace.manifest().timeline.clone();
            let mut tool = completed_tool(&workspace, false);
            tool.pending.as_mut().unwrap().operation = TextOperation::Title {
                after: after_current.then_some(original.frames[0].id),
                duration: DurationUs::new(1_234_000).unwrap(),
                background: rgba([7, 8, 9, 210]),
            };
            tool.text = "New form {text} must not replace queued Test".into();
            tool.font_family = "Changed missing font".into();
            tool.title_duration_ms = 9;
            tool.title_background = [255; 4];
            let notice = tool.poll(Some(&mut workspace)).unwrap();
            assert_notice_languages(&notice, Message::TextTitleInserted);
            let committed = workspace.manifest().clone();
            let index = usize::from(after_current);
            let title = &committed.timeline.frames[index];
            assert_eq!(title.duration.get(), 1_234_000);
            assert_eq!(workspace.selection().current(), Some(title.id));
            assert!(committed.timeline.overlay_tracks.iter().any(|track| {
                track.all_mark_contents().any(|(_, content)|
                matches!(content, OverlayContent::Text { text, .. } if text == "Test"))
            }));
            assert!(tool.poll(Some(&mut workspace)).is_none());
            assert!(workspace.undo().unwrap());
            assert_eq!(workspace.manifest().timeline, original);
            assert!(workspace.redo().unwrap());
            assert_eq!(workspace.manifest().timeline, committed.timeline);
            assert_eq!(workspace.manifest().assets, committed.assets);
            drop(workspace);
            let reopened = EditorWorkspace::open(
                &root,
                gif_from_screen_project::LockPolicy::FailIfPresent,
                20,
            )
            .unwrap();
            assert_eq!(reopened.manifest().timeline, committed.timeline);
            assert_eq!(reopened.manifest().assets, committed.assets);
        }
    }

    #[test]
    fn worker_disconnect_error_and_stale_target_keep_distinct_notice_identities() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("worker.gfsproj"));
        let before = workspace.manifest().clone();
        for (error, expected) in [
            (None, Message::TextWorkerExited),
            (Some(TextError::FontNotFound), Message::TextFontMissing),
        ] {
            let mut tool = completed_tool(&workspace, false);
            let (sender, receiver) = mpsc::channel();
            if let Some(error) = error {
                sender.send(Err(error)).unwrap();
            }
            drop(sender);
            tool.pending.as_mut().unwrap().receiver = receiver;
            assert_notice_languages(&tool.poll(Some(&mut workspace)).unwrap(), expected);
            assert!(!tool.is_running());
            assert_eq!(workspace.manifest(), &before);
        }
        let mut tool = completed_tool(&workspace, false);
        workspace.clear_selection();
        assert_notice_languages(
            &tool.poll(Some(&mut workspace)).unwrap(),
            Message::TextTargetChanged,
        );
        assert_eq!(workspace.manifest(), &before);
    }

    #[test]
    fn automatic_box_is_bounded_to_remaining_canvas() {
        let tool = TextOverlayTool {
            text: "Caption".to_owned(),
            position: [10, 20],
            ..Default::default()
        };
        let (request, position) = tool.request(PhysicalSize::new(320, 200).unwrap()).unwrap();
        assert_eq!(request.size, PhysicalSize::new(310, 160).unwrap());
        assert_eq!(position.x.get(), 10);
        assert_eq!(resolve_extent(4, 0, 5, 160).unwrap(), 1);
    }

    #[test]
    fn invalid_placement_is_rejected_before_spawning() {
        assert!(resolve_extent(5, 0, 5, 160).is_err());
        assert!(resolve_extent(u32::MAX, 0, 5, 160).is_err());
        assert!(resolve_extent(3, 3, 5, 160).is_err());
        let tool = TextOverlayTool::default();
        assert!(tool.request(PhysicalSize::new(320, 200).unwrap()).is_err());
    }

    fn workspace(root: &std::path::Path) -> EditorWorkspace {
        use gif_from_screen_application::{
            BlankAnimationProjectOptions, create_blank_animation_project,
        };
        use gif_from_screen_domain::{DurationUs, FrameId, ProjectId, UnixTimeMs};
        let project = create_blank_animation_project(
            root,
            BlankAnimationProjectOptions {
                project_id: ProjectId::from_u128(71),
                frame_id: FrameId::from_u128(72),
                app_version: "test".to_owned(),
                created_at: UnixTimeMs::new(0),
                canvas: PhysicalSize::new(320, 200).unwrap(),
                background: Rgba::TRANSPARENT,
                frame_duration: DurationUs::new(100_000).unwrap(),
                frame_limit_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        let mut workspace = EditorWorkspace::from_active(project, 20).unwrap();
        workspace.select_first().unwrap();
        workspace
    }

    fn completed_tool(workspace: &EditorWorkspace, cancelled: bool) -> TextOverlayTool {
        let mut tool = TextOverlayTool {
            text: "Test".to_owned(),
            ..Default::default()
        };
        let (request, position) = tool.request(workspace.manifest().canvas.size).unwrap();
        let (sender, receiver) = mpsc::channel();
        let image = TextRasterizer::bundled_only().rasterize(&request).unwrap();
        sender.send(Ok(image)).unwrap();
        tool.pending = Some(PendingText {
            anchor: workspace.overlay_selection_anchor().unwrap(),
            request,
            position,
            receiver,
            cancelled,
            operation: TextOperation::Add,
        });
        tool
    }

    fn copied_caption_groups(root: &std::path::Path) -> (EditorWorkspace, [TrackId; 2]) {
        let mut workspace = workspace(root);
        workspace
            .add_overlay_for_selection(
                "Backdrop".to_owned(),
                OverlayContent::Shape {
                    kind: gif_from_screen_domain::ShapeKind::Rectangle,
                    bounds: gif_from_screen_domain::PhysicalRect::new(0, 0, 1, 1).unwrap(),
                    stroke_width: 0,
                    stroke: Rgba::TRANSPARENT,
                    fill: None,
                },
                0,
                255,
                gif_from_screen_domain::BlendMode::Normal,
            )
            .unwrap();
        let mut initial = completed_tool(&workspace, false);
        initial.poll(Some(&mut workspace)).unwrap();
        workspace.copy_selection().unwrap();
        workspace.paste_after_current().unwrap();
        let ids: Vec<_> = workspace
            .manifest()
            .timeline
            .overlay_tracks
            .iter()
            .filter(|track| {
                track
                    .all_mark_contents()
                    .next()
                    .is_some_and(|(_, content)| matches!(content, OverlayContent::Text { .. }))
            })
            .map(|track| track.id)
            .collect();
        let ids: [TrackId; 2] = ids.try_into().unwrap();
        assert_ne!(ids[0], ids[1]);
        let mut draft = workspace.text_overlay_draft(ids[1]).unwrap();
        // Equal names and owner counts, but distinguishable saved requests.
        // This changes only the fixture before read-only chooser interaction.
        draft.request.text = "Copy".to_owned();
        draft.request.font_size_px = 17;
        let image = TextRasterizer::bundled_only()
            .rasterize(&draft.request)
            .unwrap();
        workspace
            .replace_text_overlay(ids[1], &draft.request, &image, draft.position)
            .unwrap();
        (workspace, ids)
    }

    fn draw_saved_chooser(
        context: &egui::Context,
        tool: &mut TextOverlayTool,
        workspace: &EditorWorkspace,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut notice = None;
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 360.0),
                )),
                events,
                ..egui::RawInput::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    tool.show_saved_text(ui, workspace, &mut notice, language("en"));
                });
            },
        );
        assert!(notice.is_none(), "{notice:?}");
        output
    }

    fn saved_label_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == label
                {
                    Some(
                        egui::Rect::from_min_size(text.pos, text.galley.size())
                            .intersect(shape.clip_rect),
                    )
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing saved-text label: {label}"))
    }

    fn click_saved_chooser(
        context: &egui::Context,
        tool: &mut TextOverlayTool,
        workspace: &EditorWorkspace,
        point: egui::Pos2,
    ) {
        for pressed in [true, false] {
            let _ = draw_saved_chooser(
                context,
                tool,
                workspace,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    fn assert_loaded_request(tool: &TextOverlayTool, workspace: &EditorWorkspace, id: TrackId) {
        let draft = workspace.text_overlay_draft(id).unwrap();
        let (request, position) = tool.request(workspace.manifest().canvas.size).unwrap();
        assert_eq!(position, draft.position);
        assert_eq!(request.text, draft.request.text);
        assert_eq!(request.font_family, draft.request.font_family);
        assert_eq!(request.font_size_px, draft.request.font_size_px);
        assert_eq!(request.size, draft.request.size);
        assert_eq!(request.foreground, draft.request.foreground);
        assert_eq!(request.background, draft.request.background);
        assert_eq!(request.alignment, draft.request.alignment);
    }

    #[test]
    fn copied_same_name_caption_choices_use_full_layer_numbers_and_load_exact_track_without_editing()
     {
        let directory = tempfile::tempdir().unwrap();
        let (workspace, ids) =
            copied_caption_groups(&directory.path().join("copied-captions.gfsproj"));
        let tracks = &workspace.manifest().timeline.overlay_tracks;
        assert_eq!(
            [tracks[1].name.as_str(), tracks[3].name.as_str()],
            ["Text", "Text"]
        );
        assert_eq!(
            [
                tracks[1].frame_cells.as_ref().unwrap().len(),
                tracks[3].frame_cells.as_ref().unwrap().len()
            ],
            [1, 1]
        );
        let labels = [
            saved_group_label(1, &tracks[1], language("en")),
            saved_group_label(3, &tracks[3], language("en")),
        ];
        assert_eq!(
            labels,
            ["Layer 2 · Text · 1 frame", "Layer 4 · Text · 1 frame"]
        );
        let before = workspace.manifest().clone();
        let context = egui::Context::default();
        let mut tool = TextOverlayTool::default();
        let mut button_label = "Edit saved text…".to_owned();
        // Select the copied group first, then the original: identical names
        // and counts never substitute text matching for the stable TrackId.
        for selected in [1, 0] {
            let output = draw_saved_chooser(&context, &mut tool, &workspace, Vec::new());
            let button = saved_label_rect(&output, &button_label);
            click_saved_chooser(&context, &mut tool, &workspace, button.center());
            let opened = draw_saved_chooser(&context, &mut tool, &workspace, Vec::new());
            assert!(saved_label_rect(&opened, &labels[0]).is_positive());
            let choice = saved_label_rect(&opened, &labels[selected]);
            assert!(choice.is_positive());
            click_saved_chooser(&context, &mut tool, &workspace, choice.center());
            let closed = draw_saved_chooser(&context, &mut tool, &workspace, Vec::new());
            assert_eq!(tool.editing.as_ref().unwrap().0, ids[selected]);
            assert_loaded_request(&tool, &workspace, ids[selected]);
            assert!(saved_label_rect(&closed, &labels[selected]).is_positive());
            assert_eq!(workspace.manifest(), &before);
            assert!(!tool.is_running());
            button_label.clone_from(&labels[selected]);
        }
    }

    #[test]
    fn completed_text_is_committed_once_and_undoable() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("text.gfsproj"));
        let revision = workspace.manifest().revision;
        let mut tool = completed_tool(&workspace, false);
        assert!(
            tool.poll(Some(&mut workspace))
                .unwrap()
                .contains("Text added")
        );
        assert!(workspace.manifest().revision > revision);
        assert_eq!(workspace.manifest().timeline.overlay_tracks.len(), 1);
        assert!(tool.poll(Some(&mut workspace)).is_none());
        workspace.undo().unwrap();
        assert!(workspace.manifest().timeline.overlay_tracks.is_empty());
    }

    #[test]
    fn saved_caption_loads_and_applies_to_the_same_track() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("text.gfsproj"));
        let mut tool = completed_tool(&workspace, false);
        tool.poll(Some(&mut workspace)).unwrap();
        let track = workspace.manifest().timeline.overlay_tracks[0].clone();
        tool.load(&workspace, track.id).unwrap();
        assert_eq!(tool.text, "Test");
        tool.text = "Edited".to_owned();
        let (request, position) = tool.request(workspace.manifest().canvas.size).unwrap();
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(TextRasterizer::bundled_only()
                .rasterize(&request)
                .unwrap()))
            .unwrap();
        tool.pending = Some(PendingText {
            anchor: workspace.overlay_selection_anchor().unwrap(),
            request,
            position,
            receiver,
            cancelled: false,
            operation: TextOperation::Replace(track.id),
        });
        assert!(
            tool.poll(Some(&mut workspace))
                .unwrap()
                .contains("Text updated")
        );
        let updated = &workspace.manifest().timeline.overlay_tracks[0];
        assert_eq!(updated.id, track.id);
        let cells = updated.frame_cells.as_ref().unwrap();
        let original_cells = track.frame_cells.as_ref().unwrap();
        assert_eq!(cells[0].marks[0].id, original_cells[0].marks[0].id);
        assert_eq!(cells[0].scopes, original_cells[0].scopes);
        assert_eq!(cells[0].frame_id, original_cells[0].frame_id);
        assert!(
            matches!(&cells[0].marks[0].content, OverlayContent::Text { text, .. } if text == "Edited")
        );
    }

    #[test]
    fn title_worker_completion_inserts_a_selected_frame_and_can_be_undone() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("title.gfsproj"));
        let original = workspace.manifest().timeline.clone();
        let mut tool = completed_tool(&workspace, false);
        tool.pending.as_mut().unwrap().operation = TextOperation::Title {
            after: None,
            duration: DurationUs::new(1_000_000).unwrap(),
            background: Rgba::TRANSPARENT,
        };
        assert!(
            tool.poll(Some(&mut workspace))
                .unwrap()
                .contains("Title frame inserted")
        );
        assert_eq!(workspace.manifest().timeline.frames.len(), 2);
        assert_eq!(
            workspace.selection().current(),
            Some(workspace.manifest().timeline.frames[0].id)
        );
        workspace.undo().unwrap();
        assert_eq!(workspace.manifest().timeline, original);
    }

    #[test]
    fn cancelled_changed_and_closed_targets_never_commit() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory.path().join("text.gfsproj"));
        let revision = workspace.manifest().revision;
        let mut cancelled = completed_tool(&workspace, true);
        assert!(
            cancelled
                .poll(Some(&mut workspace))
                .unwrap()
                .contains("cancelled")
        );
        assert_eq!(workspace.manifest().revision, revision);
        let mut changed = completed_tool(&workspace, false);
        workspace.clear_selection();
        assert!(
            changed
                .poll(Some(&mut workspace))
                .unwrap()
                .contains("selection changed")
        );
        assert_eq!(workspace.manifest().revision, revision);
        workspace.select_first().unwrap();
        let mut closed = completed_tool(&workspace, false);
        assert!(closed.poll(None).unwrap().contains("selection changed"));
        assert_eq!(workspace.manifest().revision, revision);
    }
}
