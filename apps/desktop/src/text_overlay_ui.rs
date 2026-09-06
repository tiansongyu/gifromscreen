//! Caption authoring owns its bounded worker and selection anchor, not application-wide job flags.

use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

use eframe::egui;
use gif_from_screen_domain::{
    DurationUs, FrameId, HorizontalAlignment, OverlayContent, PhysicalPoint, PhysicalPx,
    PhysicalSize, Rgba, TrackId,
};
use gif_from_screen_text::{MAX_TEXT_BYTES, TextImage, TextRasterizer, TextRequest};

use crate::editor_workspace::{
    EditorWorkspace, OverlaySelectionAnchor, TextOverlayDraft, TitleFrameRequest,
};

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
    receiver: Receiver<Result<TextImage, String>>,
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
    pub(crate) fn poll(&mut self, workspace: Option<&mut EditorWorkspace>) -> Option<String> {
        let pending = self.pending.as_ref()?;
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Text worker exited. You can retry.".to_owned()),
        };
        let pending = self.pending.take()?;
        if pending.cancelled {
            return Some("Text cancelled. The project is unchanged.".to_owned());
        }
        Some(match result {
            Err(error) => format!("Could not add text: {error}"),
            Ok(image) => match workspace {
                Some(workspace) if pending.anchor.matches(workspace) => {
                    let result = match pending.operation {
                        TextOperation::Add => workspace.add_text_overlay_for_selection(&pending.request, &image, pending.position).map(|_| ()),
                        TextOperation::Replace(track_id) => workspace.replace_text_overlay(track_id, &pending.request, &image, pending.position).map(|_| ()),
                        TextOperation::Title { after, duration, background } => workspace.insert_title_frame(&TitleFrameRequest {
                            after, duration, background, text: pending.request, position: pending.position,
                        }, &image).map(|_| ()),
                    };
                    match result {
                        Ok(()) => {
                            if let Some((_, anchor)) = &mut self.editing
                                && let Ok(updated) = workspace.overlay_selection_anchor()
                            {
                                *anchor = updated;
                            }
                            match pending.operation {
                                TextOperation::Add => "Text added to the selected frames. Its appearance is saved in the project.",
                                TextOperation::Replace(_) => "Text updated. Timing and layer settings are unchanged.",
                                TextOperation::Title { .. } => "Title frame inserted and selected. Undo restores the original timeline.",
                            }.to_owned()
                        }
                        Err(error) => format!("Could not save text: {error}"),
                    }
                }
                _ => "The project or frame selection changed while preparing text. Nothing was added; select the intended frames and retry.".to_owned(),
            },
        })
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &EditorWorkspace,
    ) -> Option<String> {
        let mut notice = None;
        ui.group(|ui| {
            ui.strong("Text");
            ui.add_enabled_ui(!self.is_running(), |ui| {
                self.show_saved_text(ui, workspace, &mut notice);
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .hint_text("Add a caption…")
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .char_limit(MAX_TEXT_BYTES),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("Font");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.font_family)
                            .desired_width(150.0)
                            .char_limit(256),
                    );
                    ui.add(
                        egui::DragValue::new(&mut self.font_size)
                            .range(1..=512)
                            .suffix(" px"),
                    );
                    for (alignment, label) in [
                        (HorizontalAlignment::Start, "Left"),
                        (HorizontalAlignment::Center, "Center"),
                        (HorizontalAlignment::End, "Right"),
                    ] {
                        ui.selectable_value(&mut self.alignment, alignment, label);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Text color");
                    ui.color_edit_button_srgba_unmultiplied(&mut self.foreground);
                    ui.checkbox(&mut self.with_background, "Background");
                    if self.with_background {
                        ui.color_edit_button_srgba_unmultiplied(&mut self.background);
                    }
                });
                egui::CollapsingHeader::new("Placement and text box")
                    .id_salt("caption-placement")
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::DragValue::new(&mut self.position[0]).prefix("X "));
                            ui.add(egui::DragValue::new(&mut self.position[1]).prefix("Y "));
                            ui.add(
                                egui::DragValue::new(&mut self.size[0])
                                    .range(0..=4096)
                                    .prefix("Width "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.size[1])
                                    .range(0..=4096)
                                    .prefix("Height "),
                            );
                        });
                        ui.weak(
                            "Zero uses the available canvas width and up to 160 pixels of height.",
                        );
                    });
                self.show_title_options(ui);
                let action_label = if self.editing.is_some() {
                    "Save text changes"
                } else if self.title {
                    "Insert title frame"
                } else {
                    "Add text to selected frames"
                };
                if ui
                    .add_enabled(
                        (self.title && self.editing.is_none()) || !workspace.selection().is_empty(),
                        egui::Button::new(action_label),
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
                    ui.label("Preparing text…");
                });
                if ui.small_button("Cancel").clicked()
                    && let Some(pending) = &mut self.pending
                {
                    // Retain the worker slot until completion so repeated cancellation cannot
                    // spawn an unbounded number of font discovery/rasterization threads.
                    pending.cancelled = true;
                    notice = Some("Cancelling text. Nothing will be added.".to_owned());
                }
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
        });
        notice
    }

    fn request(&self, canvas: PhysicalSize) -> Result<(TextRequest, PhysicalPoint), String> {
        let width = resolve_extent(self.position[0], self.size[0], canvas.width.get(), 4096)?;
        let height = resolve_extent(self.position[1], self.size[1], canvas.height.get(), 160)?;
        let request = TextRequest {
            text: self.text.clone(),
            font_family: self.font_family.trim().to_owned(),
            font_size_px: self.font_size,
            size: PhysicalSize::new(width, height).map_err(|error| error.to_string())?,
            foreground: rgba(self.foreground),
            background: self.with_background.then(|| rgba(self.background)),
            alignment: self.alignment,
        };
        request.validate().map_err(|error| error.to_string())?;
        Ok((
            request,
            PhysicalPoint {
                x: PhysicalPx::new(self.position[0]),
                y: PhysicalPx::new(self.position[1]),
            },
        ))
    }

    fn start(&mut self, workspace: &EditorWorkspace) -> Result<(), String> {
        if self.is_running() {
            return Err("Text is already being prepared.".to_owned());
        }
        let anchor = if self.title && self.editing.is_none() {
            workspace.project_edit_anchor()
        } else {
            workspace
                .overlay_selection_anchor()
                .map_err(|error| error.to_string())?
        };
        let operation = if let Some((track_id, loaded)) = &self.editing {
            if !loaded.matches(workspace) {
                return Err("The project or selection changed since loading this text. Choose the saved text again before editing.".to_owned());
            }
            TextOperation::Replace(*track_id)
        } else if self.title {
            TextOperation::Title {
                after: if self.title_at_start {
                    None
                } else {
                    Some(workspace.selection().current().ok_or_else(|| {
                        "Select a frame to insert a title after it, or choose At start.".to_owned()
                    })?)
                },
                duration: self
                    .title_duration_ms
                    .checked_mul(1000)
                    .and_then(DurationUs::new)
                    .ok_or_else(|| {
                        "Title duration must be positive and fit the project clock.".to_owned()
                    })?,
                background: rgba(self.title_background),
            }
        } else {
            TextOperation::Add
        };
        let (request, position) = self.request(workspace.manifest().canvas.size)?;
        let work = request.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("gfs-text-overlay".to_owned())
            .spawn(move || {
                let result = TextRasterizer::with_system_fonts()
                    .rasterize(&work)
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            })
            .map_err(|error| format!("Could not start text worker: {error}"))?;
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
        notice: &mut Option<String>,
    ) {
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("saved-caption")
                .selected_text(if self.editing.is_some() {
                    "Editing saved text"
                } else {
                    "Edit saved text…"
                })
                .show_ui(ui, |ui| {
                    for track in &workspace.manifest().timeline.overlay_tracks {
                        let Some(item) = track.items.first() else {
                            continue;
                        };
                        let OverlayContent::Text { text, .. } = &item.content else {
                            continue;
                        };
                        let label = text.chars().take(40).collect::<String>();
                        if ui
                            .selectable_label(
                                self.editing.as_ref().is_some_and(|(id, _)| *id == track.id),
                                label,
                            )
                            .clicked()
                            && let Err(error) = self.load(workspace, track.id)
                        {
                            *notice = Some(error);
                        }
                    }
                });
            if self.editing.is_some() && ui.button("New text").clicked() {
                self.editing = None;
            }
        });
    }

    fn load(&mut self, workspace: &EditorWorkspace, track_id: TrackId) -> Result<(), String> {
        let draft: TextOverlayDraft = workspace
            .text_overlay_draft(track_id)
            .map_err(|error| error.to_string())?;
        let anchor = workspace
            .overlay_selection_anchor()
            .map_err(|error| error.to_string())?;
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

    fn show_title_options(&mut self, ui: &mut egui::Ui) {
        if self.editing.is_some() {
            return;
        }
        ui.checkbox(&mut self.title, "Insert as a title frame");
        if self.title {
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut self.title_at_start, true, "At start");
                ui.radio_value(&mut self.title_at_start, false, "After current frame");
                ui.add(
                    egui::DragValue::new(&mut self.title_duration_ms)
                        .range(1..=3_600_000)
                        .suffix(" ms"),
                );
                ui.label("Canvas");
                ui.color_edit_button_srgba_unmultiplied(&mut self.title_background);
            });
        }
    }
}

fn color_bytes(color: Rgba) -> [u8; 4] {
    [color.red, color.green, color.blue, color.alpha]
}

fn resolve_extent(
    origin: u32,
    requested: u32,
    canvas: u32,
    automatic_limit: u32,
) -> Result<u32, String> {
    let available = canvas
        .checked_sub(origin)
        .filter(|value| *value != 0)
        .ok_or_else(|| "The text position must be inside the canvas.".to_owned())?;
    let extent = if requested == 0 {
        available.min(automatic_limit)
    } else {
        requested
    };
    if extent > available {
        return Err("The text box must fit inside the canvas.".to_owned());
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
        assert_eq!(updated.items[0].id, track.items[0].id);
        assert_eq!(updated.items[0].span, track.items[0].span);
        assert!(
            matches!(&updated.items[0].content, OverlayContent::Text { text, .. } if text == "Edited")
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
