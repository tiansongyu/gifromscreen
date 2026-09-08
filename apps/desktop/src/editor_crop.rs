//! A source-sized crop draft; only explicit Apply reaches the ordered editor command.

use eframe::egui;
use gif_from_screen_domain::{FrameId, PhysicalRect, PhysicalSize};

use crate::editor_workspace::{EditorWorkspace, OverlaySelectionAnchor};

#[derive(Debug)]
struct Session {
    anchor: OverlaySelectionAnchor,
    frame_id: FrameId,
    canvas: PhysicalSize,
    rect: PhysicalRect,
    fields: [String; 4],
    gesture: Option<Gesture>,
}

#[derive(Debug)]
struct Gesture {
    start: [f64; 2],
    painted: egui::Rect,
    previous_rect: PhysicalRect,
    previous_fields: [String; 4],
}

#[derive(Debug, Default)]
pub(crate) struct DirectCropDraft {
    session: Option<Session>,
    notice: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CropOutcome {
    pub(crate) started: bool,
    pub(crate) applied: bool,
}

impl DirectCropDraft {
    pub(crate) const fn active(&self) -> bool {
        self.session.is_some()
    }

    pub(crate) fn begin(
        &mut self,
        workspace: &EditorWorkspace,
        frame_id: FrameId,
        rendered: [u32; 2],
    ) -> Result<(), String> {
        if workspace.selection().current() != Some(frame_id) {
            return Err("Select an original frame before starting a crop draft".into());
        }
        let canvas =
            PhysicalSize::new(rendered[0], rendered[1]).map_err(|error| error.to_string())?;
        let rect =
            PhysicalRect::new(0, 0, rendered[0], rendered[1]).map_err(|error| error.to_string())?;
        self.session = Some(Session {
            anchor: workspace
                .overlay_selection_anchor()
                .map_err(|error| error.to_string())?,
            frame_id,
            canvas,
            rect,
            fields: fields(rect),
            gesture: None,
        });
        self.notice = None;
        Ok(())
    }

    pub(crate) fn cancel(&mut self) {
        self.session = None;
        self.notice = None;
    }

    pub(crate) fn reconcile(&mut self, workspace: &EditorWorkspace) {
        if self.session.as_ref().is_some_and(|session| {
            !session.anchor.matches(workspace)
                || workspace.selection().current() != Some(session.frame_id)
        }) {
            self.session = None;
            self.notice = Some(
                "Crop draft discarded because the project, revision or selection changed.".into(),
            );
        }
    }

    pub(crate) fn cancel_gesture(&mut self) {
        if let Some(session) = &mut self.session
            && let Some(gesture) = session.gesture.take()
        {
            session.rect = gesture.previous_rect;
            session.fields = gesture.previous_fields;
        }
    }

    pub(crate) fn apply(&mut self, workspace: &mut EditorWorkspace) -> Result<(), String> {
        let session = self.session.as_ref().ok_or("No crop draft is active")?;
        if !session.anchor.matches(workspace) {
            self.reconcile(workspace);
            return Err("Crop draft is stale; start a new draft for the current selection".into());
        }
        if session.gesture.is_some() {
            return Err("Finish the crop gesture before applying".into());
        }
        let crop = parse_fields(&session.fields, session.canvas)?;
        // Despite its historical name, this command crops every composed frame,
        // seals prior artwork, updates the canvas and records one undoable edit.
        workspace
            .set_selection_crop(crop)
            .map_err(|error| error.to_string())?;
        self.session = None;
        self.notice =
            Some("Crop applied to all frames. Undo restores the previous image layout.".into());
        Ok(())
    }

    pub(crate) fn show_controls(
        &mut self,
        ui: &mut egui::Ui,
        workspace: &mut EditorWorkspace,
        frame_id: FrameId,
        rendered: [u32; 2],
        enabled: bool,
    ) -> CropOutcome {
        self.reconcile(workspace);
        let mut outcome = CropOutcome::default();
        if self.session.is_none() {
            if ui
                .add_enabled(enabled, egui::Button::new("Crop on preview…"))
                .clicked()
            {
                match self.begin(workspace, frame_id, rendered) {
                    Ok(()) => outcome.started = true,
                    Err(error) => self.notice = Some(error),
                }
            }
        } else {
            ui.strong("Crop draft · applies to all frames");
            ui.small("Drag over the image or edit pixel bounds. Nothing changes until Apply.");
            let session = self.session.as_mut().expect("checked active crop");
            ui.add_enabled_ui(enabled && session.gesture.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (label, value) in ["X", "Y", "W", "H"].into_iter().zip(&mut session.fields)
                    {
                        ui.label(label);
                        ui.add(
                            egui::TextEdit::singleline(value)
                                .desired_width(52.0)
                                .char_limit(10),
                        );
                    }
                });
            });
            let parsed = parse_fields(&session.fields, session.canvas);
            if let Ok(rect) = parsed {
                session.rect = rect;
            }
            if let Err(error) = &parsed {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            let can_apply = enabled && parsed.is_ok() && session.gesture.is_none();
            let mut apply = false;
            let mut cancel = false;
            ui.horizontal_wrapped(|ui| {
                apply = ui
                    .add_enabled(can_apply, egui::Button::new("Apply crop to all frames"))
                    .clicked();
                cancel = ui.button("Cancel crop").clicked();
            });
            if cancel {
                self.cancel();
            } else if apply {
                match self.apply(workspace) {
                    Ok(()) => outcome.applied = true,
                    Err(error) => self.notice = Some(format!("Crop operation failed: {error}")),
                }
            }
        }
        if let Some(notice) = &self.notice {
            ui.label(notice);
        }
        outcome
    }

    pub(crate) fn interact(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rendered: [u32; 2],
        enabled: bool,
    ) {
        let Some(session) = &self.session else {
            return;
        };
        let (focused, lost, escape, released) = ui.input(|input| {
            // A release followed by leaving the window in the same update is
            // a completed gesture. Use its position, not a later pointer move.
            let released = input
                .events
                .iter()
                .take_while(|event| !matches!(event, egui::Event::PointerGone))
                .find_map(|event| match event {
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        ..
                    } => Some(*pos),
                    _ => None,
                });
            (
                input.focused,
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::PointerGone))
                    && released.is_none(),
                input.key_pressed(egui::Key::Escape),
                released,
            )
        });
        if escape && (response.has_focus() || session.gesture.is_some()) {
            self.cancel();
            return;
        }
        let mapping_changed = session.canvas.width.get() != rendered[0]
            || session.canvas.height.get() != rendered[1]
            || session
                .gesture
                .as_ref()
                .is_some_and(|gesture| gesture.painted != response.rect);
        if !enabled || !focused || lost || mapping_changed {
            self.cancel_gesture();
            return;
        }
        let session = self.session.as_mut().expect("crop session checked");
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(start) = ui.input(|input| input.pointer.press_origin())
            && response.rect.contains(start)
            && ui.clip_rect().contains(start)
            && let Some(start) = map_point(response.rect, start, rendered)
        {
            response.request_focus();
            session.gesture = Some(Gesture {
                start,
                painted: response.rect,
                previous_rect: session.rect,
                previous_fields: session.fields.clone(),
            });
        }
        if let Some(gesture) = &session.gesture
            && let Some(position) = released.or_else(|| response.interact_pointer_pos())
            && let Some(end) = map_point(response.rect, position, rendered)
        {
            session.rect = drag_rect(gesture.start, end, session.canvas);
            session.fields = fields(session.rect);
        }
        if released.is_some() || response.drag_stopped_by(egui::PointerButton::Primary) {
            session.gesture = None;
        }
        if response.clicked_by(egui::PointerButton::Primary)
            && let Some(position) = response.interact_pointer_pos()
            && let Some(point) = map_point(response.rect, position, rendered)
        {
            response.request_focus();
            session.rect = drag_rect(point, point, session.canvas);
            session.fields = fields(session.rect);
        }
    }

    pub(crate) fn paint(&self, painter: &egui::Painter, image: egui::Rect, rendered: [u32; 2]) {
        let Some(session) = &self.session else {
            return;
        };
        let crop = crop_on_image(image, session.rect, rendered);
        let shade = egui::Color32::from_black_alpha(110);
        for region in [
            egui::Rect::from_min_max(image.min, egui::pos2(image.right(), crop.top())),
            egui::Rect::from_min_max(egui::pos2(image.left(), crop.bottom()), image.max),
            egui::Rect::from_min_max(
                egui::pos2(image.left(), crop.top()),
                egui::pos2(crop.left(), crop.bottom()),
            ),
            egui::Rect::from_min_max(
                egui::pos2(crop.right(), crop.top()),
                egui::pos2(image.right(), crop.bottom()),
            ),
        ] {
            if region.is_positive() {
                painter.rect_filled(region, 0.0, shade);
            }
        }
        painter.rect_stroke(
            crop,
            0.0,
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(242, 153, 74)),
            egui::StrokeKind::Inside,
        );
    }
}

fn fields(rect: PhysicalRect) -> [String; 4] {
    [
        rect.origin.x.get(),
        rect.origin.y.get(),
        rect.size.width.get(),
        rect.size.height.get(),
    ]
    .map(|value| value.to_string())
}

fn parse_fields(fields: &[String; 4], canvas: PhysicalSize) -> Result<PhysicalRect, String> {
    let mut values = [0; 4];
    for (index, field) in fields.iter().enumerate() {
        values[index] = field
            .trim()
            .parse::<u32>()
            .map_err(|_| "Crop coordinates must be unsigned whole pixels")?;
    }
    let rect = PhysicalRect::new(values[0], values[1], values[2], values[3])
        .map_err(|error| error.to_string())?;
    if !rect.fits_within(canvas) {
        return Err("Crop must fit inside the current rendered image".into());
    }
    Ok(rect)
}

fn map_point(image: egui::Rect, point: egui::Pos2, size: [u32; 2]) -> Option<[f64; 2]> {
    if !image.is_finite()
        || image.width() <= 0.0
        || image.height() <= 0.0
        || !point.is_finite()
        || size.contains(&0)
    {
        return None;
    }
    Some([
        (f64::from(point.x - image.left()) / f64::from(image.width()) * f64::from(size[0]))
            .clamp(0.0, f64::from(size[0])),
        (f64::from(point.y - image.top()) / f64::from(image.height()) * f64::from(size[1]))
            .clamp(0.0, f64::from(size[1])),
    ])
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn drag_rect(start: [f64; 2], end: [f64; 2], canvas: PhysicalSize) -> PhysicalRect {
    // Inputs come only from finite, bounded mapping. A click selects one pixel;
    // drags cover floor(min)..ceil(max), clipped to the actual rendered canvas.
    let bounds = |a: f64, b: f64, limit: u32| {
        let low = (a.min(b).floor() as u32).min(limit - 1);
        let high = (a.max(b).ceil() as u32).clamp(low + 1, limit);
        (low, high - low)
    };
    let (x, width) = bounds(start[0], end[0], canvas.width.get());
    let (y, height) = bounds(start[1], end[1], canvas.height.get());
    PhysicalRect::new(x, y, width, height).expect("bounded non-empty crop")
}

#[allow(clippy::cast_precision_loss)]
fn crop_on_image(image: egui::Rect, crop: PhysicalRect, rendered: [u32; 2]) -> egui::Rect {
    let scale = image.size() / egui::vec2(rendered[0] as f32, rendered[1] as f32);
    let origin =
        image.min + egui::vec2(crop.origin.x.get() as f32, crop.origin.y.get() as f32) * scale;
    egui::Rect::from_min_size(
        origin,
        egui::vec2(crop.size.width.get() as f32, crop.size.height.get() as f32) * scale,
    )
}

#[cfg(test)]
#[path = "editor_crop_tests.rs"]
mod tests;
