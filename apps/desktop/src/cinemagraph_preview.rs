//! Transient canvas gestures and asynchronous outline guides. Never commits a project.

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, StrokeKind};
use gif_from_screen_domain::FrameId;
use gif_from_screen_render::{
    CancellationToken, InkAttributes, InkLimits, InkPath, InkPoint, InkSample, InkSegment, InkTip,
    outline_ink_strokes,
};

use crate::{
    background_task::BackgroundTask,
    cinemagraph_draft::{CinemagraphDraft, CinemagraphTool, InkBounds},
};

const MAX_INPUT_EVENTS: usize = 256;
const GUIDE_SEGMENTS: usize = 32_768;
const HANDLE_RADIUS: f32 = 7.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    rect: Rect,
    size: [u32; 2],
}

impl Mapping {
    fn new(rect: Rect, size: [u32; 2]) -> Option<Self> {
        (rect.is_finite()
            && rect.width() > 0.0
            && rect.height() > 0.0
            && size[0] > 0
            && size[1] > 0)
            .then_some(Self { rect, size })
    }

    fn image(self, position: Pos2) -> InkPoint {
        InkPoint {
            x: (f64::from(position.x) - f64::from(self.rect.min.x)) * f64::from(self.size[0])
                / f64::from(self.rect.width()),
            y: (f64::from(position.y) - f64::from(self.rect.min.y)) * f64::from(self.size[1])
                / f64::from(self.rect.height()),
        }
    }

    fn screen(self, position: InkPoint) -> Pos2 {
        // egui coordinates are f32. This is only display conversion, never
        // fed back into stored image samples and never adds pixel-center/DPI offsets.
        #[allow(clippy::cast_possible_truncation)]
        let mapped = Pos2::new(
            (f64::from(self.rect.min.x)
                + position.x * f64::from(self.rect.width()) / f64::from(self.size[0]))
                as f32,
            (f64::from(self.rect.min.y)
                + position.y * f64::from(self.rect.height()) / f64::from(self.size[1]))
                as f32,
        );
        mapped
    }

    fn inside(self, position: Pos2) -> bool {
        position.x >= self.rect.min.x
            && position.x < self.rect.max.x
            && position.y >= self.rect.min.y
            && position.y < self.rect.max.y
    }

    fn bounds(self, bounds: InkBounds) -> Rect {
        Rect::from_min_max(self.screen(bounds.min), self.screen(bounds.max))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DraftKey {
    generation: u64,
    reference: Option<FrameId>,
    size: [u32; 2],
    epoch: u64,
}

impl DraftKey {
    fn new(draft: &CinemagraphDraft, size: [u32; 2], epoch: u64) -> Self {
        Self {
            generation: draft.generation(),
            reference: draft.reference_frame(),
            size,
            epoch,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum DragKind {
    Pointer,
    Move(InkBounds),
    Resize(InkBounds),
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    mapping: Mapping,
    start: InkPoint,
    last: InkPoint,
    kind: DragKind,
    tool: CinemagraphTool,
}

#[derive(Clone, Copy, Debug)]
enum InputEvent {
    Down(Pos2),
    Move(Pos2),
    Up(Pos2),
    Escape(egui::Modifiers),
    Delete(egui::Modifiers),
    Lost,
}

struct Cancel<'a>(&'a AtomicBool);
impl CancellationToken for Cancel<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Default)]
pub(crate) struct CinemagraphPreview {
    task: BackgroundTask<Vec<(u64, InkPath)>, ()>,
    running_key: Option<DraftKey>,
    shown_key: Option<DraftKey>,
    failed_key: Option<DraftKey>,
    guides: Vec<(u64, InkPath)>,
    guide_error: Option<String>,
    notice: Option<String>,
    drag: Option<Drag>,
    selection_key: Option<(u64, BTreeSet<u64>)>,
    selection_bounds: Option<InkBounds>,
    rollback_pending: bool,
    epoch: u64,
}

impl CinemagraphPreview {
    /// The caller should also close/cancel its draft if no subsequent show occurs.
    pub(crate) fn cancel(&mut self) {
        self.task.cancel();
        self.epoch = self.epoch.wrapping_add(1);
        self.shown_key = None;
        self.failed_key = None;
        self.guides.clear();
        self.guide_error = None;
        self.rollback_pending = true;
        self.selection_key = None;
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        draft: &mut CinemagraphDraft,
        rendered_size: [u32; 2],
        enabled: bool,
    ) {
        if self.rollback_pending {
            self.abort(draft);
            self.rollback_pending = false;
        }
        let Some(mapping) = Mapping::new(response.rect, rendered_size) else {
            self.abort(draft);
            self.task.cancel();
            return;
        };
        let reference_matches = draft
            .reference_size()
            .is_some_and(|size| [size.width.get(), size.height.get()] == rendered_size);
        let enabled = enabled && draft.is_active() && !draft.is_stale() && reference_matches;
        if !enabled {
            self.abort(draft);
            self.task.cancel();
            let _ = self.task.poll();
            self.running_key = None;
            return;
        }
        if self
            .drag
            .is_some_and(|drag| drag.mapping != mapping || drag.tool != draft.tool)
        {
            self.abort(draft);
        }
        self.refresh_bounds(draft);
        self.input(ui, response, draft, mapping);
        self.refresh_bounds(draft);
        let desired = DraftKey::new(draft, rendered_size, self.epoch);
        self.update_guides(draft, desired);
        if self.task.is_running() {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        let painter = ui
            .painter()
            .with_clip_rect(ui.clip_rect().intersect(mapping.rect));
        if self.shown_key == Some(desired) {
            paint_guides(&painter, mapping, &self.guides, draft.selected_ids());
        }
        let input_trace = self.shown_key != Some(desired)
            && self
                .drag
                .is_some_and(|drag| drag.tool == CinemagraphTool::Pen);
        if input_trace {
            paint_input_trace(&painter, mapping, draft);
        }
        self.paint_selection(&painter, mapping, draft);
        self.paint_cursor(ui, &painter, mapping, draft);
        let status = self
            .notice
            .as_deref()
            .or(self.guide_error.as_deref())
            .unwrap_or(if self.shown_key == Some(desired) {
                "Outline guide — not final pixels"
            } else if input_trace {
                "Input trace — unfitted; outline guide pending"
            } else {
                "Guide pending — outlines are being calculated"
            });
        painter.text(
            mapping.rect.left_top() + egui::vec2(6.0, 6.0),
            egui::Align2::LEFT_TOP,
            status.chars().take(180).collect::<String>(),
            egui::FontId::proportional(12.0),
            Color32::LIGHT_BLUE,
        );
    }

    fn abort(&mut self, draft: &mut CinemagraphDraft) {
        draft.cancel_gesture();
        self.drag = None;
        self.selection_key = None;
    }

    fn refresh_bounds(&mut self, draft: &CinemagraphDraft) {
        if draft.gesture_active() {
            return;
        }
        let key = (draft.generation(), draft.selected_ids().clone());
        if self.selection_key.as_ref() == Some(&key) {
            return;
        }
        self.selection_bounds = if draft.selected_ids().is_empty() {
            None
        } else {
            match draft.selection_bounds() {
                Ok(bounds) => bounds,
                Err(error) => {
                    self.notice = Some(error);
                    None
                }
            }
        };
        self.selection_key = Some(key);
    }

    fn input(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        draft: &mut CinemagraphDraft,
        mapping: Mapping,
    ) {
        let keyboard = response.has_focus() || self.drag.is_some();
        let (events, focused, primary_down) = ui.input(|input| {
            (
                input
                    .events
                    .iter()
                    .filter_map(event_for_canvas)
                    .take(MAX_INPUT_EVENTS + 1)
                    .collect::<Vec<_>>(),
                input.focused,
                input.pointer.primary_down(),
            )
        });
        if !focused || events.len() > MAX_INPUT_EVENTS {
            self.abort(draft);
            if events.len() > MAX_INPUT_EVENTS {
                self.notice = Some(
                    "Too many pointer events in one update; the gesture was cancelled.".into(),
                );
            }
            return;
        }
        for event in events {
            match event {
                InputEvent::Escape(modifiers) | InputEvent::Delete(modifiers) => {
                    if !keyboard && self.drag.is_none() && !response.has_focus() {
                        continue;
                    }
                    let key = if matches!(event, InputEvent::Escape(_)) {
                        egui::Key::Escape
                    } else {
                        egui::Key::Delete
                    };
                    ui.input_mut(|input| {
                        input.consume_key(modifiers, key);
                    });
                }
                InputEvent::Down(position) => {
                    if !mapping.inside(position)
                        || !ui.clip_rect().contains(position)
                        || ui
                            .ctx()
                            .layer_id_at(position)
                            .is_some_and(|layer| layer != response.layer_id)
                    {
                        continue;
                    }
                    response.request_focus();
                }
                _ => {}
            }
            if let Err(error) = self.event(draft, mapping, event) {
                self.abort(draft);
                self.notice = Some(error);
            }
        }
        if self.drag.is_some()
            && (!primary_down || ui.ctx().dragged_id().is_some_and(|id| id != response.id))
        {
            self.abort(draft);
        }
    }

    fn event(
        &mut self,
        draft: &mut CinemagraphDraft,
        mapping: Mapping,
        event: InputEvent,
    ) -> Result<(), String> {
        match event {
            InputEvent::Down(position) => self.down(draft, mapping, position),
            InputEvent::Move(position) => self.motion(draft, mapping.image(position)),
            InputEvent::Up(position) => {
                let Some(drag) = self.drag else {
                    return Ok(());
                };
                match drag.kind {
                    DragKind::Pointer => draft.pointer_up(mouse_sample(mapping.image(position)))?,
                    DragKind::Move(_) | DragKind::Resize(_) => {
                        self.motion(draft, mapping.image(position))?;
                        draft.finish_selection_transform()?;
                    }
                }
                self.drag = None;
                Ok(())
            }
            InputEvent::Escape(_) | InputEvent::Lost => {
                self.abort(draft);
                Ok(())
            }
            InputEvent::Delete(_) => {
                self.abort(draft);
                draft.delete_selected()
            }
        }
    }

    fn down(
        &mut self,
        draft: &mut CinemagraphDraft,
        mapping: Mapping,
        position: Pos2,
    ) -> Result<(), String> {
        if self.drag.is_some() || !mapping.inside(position) {
            return Ok(());
        }
        let start = mapping.image(position);
        let kind = if draft.tool == CinemagraphTool::Select {
            if let Some(bounds) = self.selection_bounds {
                let rect = mapping.bounds(bounds);
                if rect.max.distance(position) <= HANDLE_RADIUS {
                    DragKind::Resize(bounds)
                } else if rect.contains(position) {
                    DragKind::Move(bounds)
                } else {
                    DragKind::Pointer
                }
            } else {
                DragKind::Pointer
            }
        } else {
            DragKind::Pointer
        };
        match kind {
            DragKind::Pointer => draft.pointer_down(mouse_sample(start))?,
            DragKind::Move(_) | DragKind::Resize(_) => draft.begin_selection_transform()?,
        }
        self.notice = None;
        self.drag = Some(Drag {
            mapping,
            start,
            last: start,
            kind,
            tool: draft.tool,
        });
        Ok(())
    }

    fn motion(&mut self, draft: &mut CinemagraphDraft, position: InkPoint) -> Result<(), String> {
        let Some(drag) = &mut self.drag else {
            return Ok(());
        };
        match drag.kind {
            DragKind::Pointer => draft.pointer_move(mouse_sample(position))?,
            DragKind::Move(_) => draft.update_selection_transform(
                InkPoint { x: 1.0, y: 1.0 },
                InkPoint {
                    x: position.x - drag.start.x,
                    y: position.y - drag.start.y,
                },
            )?,
            DragKind::Resize(bounds) => {
                let Some((scale, translation)) = resize_transform(bounds, drag.start, position)
                else {
                    return Ok(());
                };
                draft.update_selection_transform(scale, translation)?;
            }
        }
        drag.last = position;
        Ok(())
    }

    fn update_guides(&mut self, draft: &CinemagraphDraft, desired: DraftKey) {
        if let Some(result) = self.task.poll()
            && self.running_key.take() == Some(desired)
            && !self.task.is_cancelling()
        {
            match result {
                Ok(paths) => {
                    self.guides = paths;
                    self.shown_key = Some(desired);
                    self.guide_error = None;
                }
                Err(error) => {
                    self.guide_error = Some(format!("Guide unavailable: {error}"));
                    self.failed_key = Some(desired);
                }
            }
        }
        if self.task.is_running() {
            if self.running_key != Some(desired) {
                self.task.cancel();
            }
            return;
        }
        if self.shown_key == Some(desired) || self.failed_key == Some(desired) {
            return;
        }
        self.guide_error = None;
        if draft.strokes().is_empty() {
            self.guides.clear();
            self.shown_key = Some(desired);
            return;
        }
        let ids = draft
            .strokes()
            .iter()
            .map(|stroke| stroke.id)
            .collect::<Vec<_>>();
        let strokes = draft
            .strokes()
            .iter()
            .map(|stroke| stroke.stroke.clone())
            .collect::<Vec<_>>();
        let result = self
            .task
            .start("cinemagraph-outline-guide", move |context| {
                let paths = outline_ink_strokes(
                    &strokes,
                    &InkLimits {
                        max_points: 32_768,
                        max_segments: GUIDE_SEGMENTS,
                        max_work: 4_000_000,
                        max_bytes: 16 * 1024 * 1024,
                    },
                    &Cancel(context.cancellation()),
                )
                .map_err(|error| error.to_string())?;
                if paths.len() != ids.len() {
                    return Err("Outline identities did not match the draft".into());
                }
                Ok(ids.into_iter().zip(paths).collect())
            });
        match result {
            Ok(()) => self.running_key = Some(desired),
            Err(error) => {
                self.guide_error = Some(format!("Guide unavailable: {error}"));
                self.failed_key = Some(desired);
            }
        }
    }

    fn displayed_bounds(&self) -> Option<InkBounds> {
        let Some(drag) = self.drag else {
            return self.selection_bounds;
        };
        match drag.kind {
            DragKind::Pointer => (drag.tool == CinemagraphTool::Select).then_some(InkBounds {
                min: InkPoint {
                    x: drag.start.x.min(drag.last.x),
                    y: drag.start.y.min(drag.last.y),
                },
                max: InkPoint {
                    x: drag.start.x.max(drag.last.x),
                    y: drag.start.y.max(drag.last.y),
                },
            }),
            DragKind::Move(bounds) => Some(transform_bounds(
                bounds,
                InkPoint { x: 1.0, y: 1.0 },
                InkPoint {
                    x: drag.last.x - drag.start.x,
                    y: drag.last.y - drag.start.y,
                },
            )),
            DragKind::Resize(bounds) => resize_transform(bounds, drag.start, drag.last)
                .map(|(scale, translation)| transform_bounds(bounds, scale, translation)),
        }
    }

    fn paint_selection(&self, painter: &egui::Painter, mapping: Mapping, draft: &CinemagraphDraft) {
        if draft.tool != CinemagraphTool::Select {
            return;
        }
        if let Some(bounds) = self.displayed_bounds() {
            let rect = mapping.bounds(bounds);
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(1.0_f32, Color32::LIGHT_BLUE),
                StrokeKind::Inside,
            );
            if self.drag.is_none() && !draft.selected_ids().is_empty() {
                painter.rect_filled(
                    Rect::from_center_size(rect.max, egui::vec2(8.0, 8.0)),
                    1.0,
                    Color32::LIGHT_BLUE,
                );
            }
        }
    }

    fn paint_cursor(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        mapping: Mapping,
        draft: &CinemagraphDraft,
    ) {
        let Some(position) = ui
            .input(|input| input.pointer.hover_pos())
            .filter(|position| mapping.inside(*position))
        else {
            return;
        };
        if draft.tool == CinemagraphTool::Select {
            let icon = self
                .displayed_bounds()
                .map_or(egui::CursorIcon::Crosshair, |bounds| {
                    let rect = mapping.bounds(bounds);
                    if rect.max.distance(position) <= HANDLE_RADIUS {
                        egui::CursorIcon::ResizeNwSe
                    } else if rect.contains(position) {
                        egui::CursorIcon::Move
                    } else {
                        egui::CursorIcon::Crosshair
                    }
                });
            ui.ctx().set_cursor_icon(icon);
            return;
        }
        let attributes = if draft.tool == CinemagraphTool::Pen {
            draft.pen
        } else {
            draft.eraser
        };
        let diameters = tip_dimensions(attributes, 0.5);
        let center = mapping.image(position);
        let corner = mapping.screen(InkPoint {
            x: center.x + diameters[0] / 2.0,
            y: center.y + diameters[1] / 2.0,
        });
        let radius = corner - position;
        let stroke = Stroke::new(
            1.0_f32,
            if draft.tool == CinemagraphTool::Pen {
                Color32::LIGHT_BLUE
            } else {
                Color32::LIGHT_RED
            },
        );
        match attributes.tip {
            InkTip::Ellipse => {
                painter.add(egui::epaint::EllipseShape::stroke(position, radius, stroke));
            }
            InkTip::Rectangle => {
                painter.rect_stroke(
                    Rect::from_center_size(position, radius * 2.0),
                    0.0,
                    stroke,
                    StrokeKind::Middle,
                );
            }
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
}

fn event_for_canvas(event: &egui::Event) -> Option<InputEvent> {
    match event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            ..
        } => Some(if *pressed {
            InputEvent::Down(*pos)
        } else {
            InputEvent::Up(*pos)
        }),
        egui::Event::PointerMoved(pos) => Some(InputEvent::Move(*pos)),
        egui::Event::PointerGone | egui::Event::WindowFocused(false) => Some(InputEvent::Lost),
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => match key {
            egui::Key::Escape => Some(InputEvent::Escape(*modifiers)),
            egui::Key::Delete => Some(InputEvent::Delete(*modifiers)),
            _ => None,
        },
        _ => None,
    }
}

fn mouse_sample(position: InkPoint) -> InkSample {
    InkSample {
        position,
        pressure: 0.5,
    }
}

fn tip_dimensions(attributes: InkAttributes, pressure: f32) -> [f64; 2] {
    let scale = if attributes.ignore_pressure {
        1.0
    } else {
        f64::from(1.5_f32 * pressure + 0.25_f32)
    };
    [attributes.width * scale, attributes.height * scale]
}

fn resize_transform(
    bounds: InkBounds,
    start: InkPoint,
    current: InkPoint,
) -> Option<(InkPoint, InkPoint)> {
    let width = bounds.max.x - bounds.min.x;
    let height = bounds.max.y - bounds.min.y;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale = InkPoint {
        x: (width + current.x - start.x) / width,
        y: (height + current.y - start.y) / height,
    };
    if !scale.x.is_finite() || !scale.y.is_finite() || scale.x <= 0.0 || scale.y <= 0.0 {
        return None;
    }
    Some((
        scale,
        InkPoint {
            x: bounds.min.x * (1.0 - scale.x),
            y: bounds.min.y * (1.0 - scale.y),
        },
    ))
}

fn transform_bounds(bounds: InkBounds, scale: InkPoint, translation: InkPoint) -> InkBounds {
    let map = |point: InkPoint| InkPoint {
        x: point.x * scale.x + translation.x,
        y: point.y * scale.y + translation.y,
    };
    InkBounds {
        min: map(bounds.min),
        max: map(bounds.max),
    }
}

fn trace_indices(length: usize) -> impl Iterator<Item = usize> {
    let count = length.min(1024);
    (0..count).map(move |index| {
        if count <= 1 {
            0
        } else {
            index * (length - 1) / (count - 1)
        }
    })
}

fn paint_input_trace(painter: &egui::Painter, mapping: Mapping, draft: &CinemagraphDraft) {
    let Some(stroke) = draft.strokes().last() else {
        return;
    };
    let points = trace_indices(stroke.stroke.samples.len())
        .map(|index| mapping.screen(stroke.stroke.samples[index].position))
        .collect::<Vec<_>>();
    let color = Color32::from_rgba_unmultiplied(130, 190, 235, 130);
    if points.len() == 1 {
        painter.circle_filled(points[0], 1.5, color);
    } else if !points.is_empty() {
        painter.add(egui::Shape::line(points, Stroke::new(1.0_f32, color)));
    }
}

fn paint_guides(
    painter: &egui::Painter,
    mapping: Mapping,
    guides: &[(u64, InkPath)],
    selected: &BTreeSet<u64>,
) {
    for (id, path) in guides {
        let stroke = Stroke::new(
            1.0_f32,
            if selected.contains(id) {
                Color32::from_rgb(90, 205, 255)
            } else {
                Color32::from_rgb(50, 145, 235)
            },
        );
        for figure in &path.figures {
            let first = mapping.screen(figure.start);
            let mut current = first;
            for segment in &figure.segments {
                match segment {
                    InkSegment::LineTo(to) => {
                        let to = mapping.screen(*to);
                        painter.line_segment([current, to], stroke);
                        current = to;
                    }
                    InkSegment::CubicTo {
                        control1,
                        control2,
                        to,
                    } => {
                        let to = mapping.screen(*to);
                        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                            [
                                current,
                                mapping.screen(*control1),
                                mapping.screen(*control2),
                                to,
                            ],
                            false,
                            Color32::TRANSPARENT,
                            stroke,
                        ));
                        current = to;
                    }
                }
            }
            if current != first {
                painter.line_segment([current, first], stroke);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_workspace::EditorWorkspace;
    use gif_from_screen_application::{
        BlankAnimationProjectOptions, create_blank_animation_project,
    };
    use gif_from_screen_domain::{DurationUs, PhysicalSize, ProjectId, Rgba, UnixTimeMs};

    fn setup() -> (tempfile::TempDir, EditorWorkspace, CinemagraphDraft) {
        let root = tempfile::tempdir().unwrap();
        let project = create_blank_animation_project(
            root.path(),
            BlankAnimationProjectOptions {
                project_id: ProjectId::from_u128(412),
                frame_id: FrameId::from_u128(19),
                app_version: "cinemagraph-preview-test".into(),
                created_at: UnixTimeMs::new(0),
                canvas: PhysicalSize::new(100, 100).unwrap(),
                background: Rgba {
                    red: 30,
                    green: 40,
                    blue: 50,
                    alpha: 255,
                },
                frame_duration: DurationUs::new(100_000).unwrap(),
                frame_limit_bytes: 64 * 1024,
            },
        )
        .unwrap();
        let mut workspace = EditorWorkspace::from_active(project, 16).unwrap();
        workspace.select_first().unwrap();
        let mut draft = CinemagraphDraft::default();
        draft.begin(&workspace).unwrap();
        draft.pen.width = 2.0;
        draft.pen.height = 2.0;
        draft.pen.tip = InkTip::Rectangle;
        (root, workspace, draft)
    }

    fn mapping() -> Mapping {
        Mapping::new(
            Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 200.0)),
            [100, 100],
        )
        .unwrap()
    }

    fn add_line(draft: &mut CinemagraphDraft) {
        draft
            .pointer_down(mouse_sample(InkPoint { x: 20.0, y: 20.0 }))
            .unwrap();
        draft
            .pointer_up(mouse_sample(InkPoint { x: 40.0, y: 40.0 }))
            .unwrap();
    }

    #[test]
    fn mapping_preserves_subpixels_has_no_dpi_or_center_offset_and_never_clamps() {
        let map = Mapping::new(
            Rect::from_min_size(egui::pos2(10.5, 20.25), egui::vec2(200.0, 150.0)),
            [100, 100],
        )
        .unwrap();
        assert_eq!(
            map.image(egui::pos2(10.75, 20.625)),
            InkPoint { x: 0.125, y: 0.25 }
        );
        assert_eq!(map.image(map.rect.min), InkPoint { x: 0.0, y: 0.0 });
        assert_eq!(
            map.image(egui::pos2(230.5, 170.25)),
            InkPoint { x: 110.0, y: 100.0 }
        );
        assert!(!map.inside(map.rect.max));
        assert!(Mapping::new(Rect::NOTHING, [100, 100]).is_none());
        assert!(Mapping::new(map.rect, [0, 100]).is_none());
    }

    #[test]
    fn neutral_mouse_pressure_and_cursor_tip_use_identical_reference_scale() {
        let attributes = InkAttributes {
            width: 8.0,
            height: 4.0,
            ..InkAttributes::default()
        };
        for (pressure, expected) in [(0.0, [2.0_f64, 1.0]), (0.5, [8.0, 4.0]), (1.0, [14.0, 7.0])] {
            assert_eq!(
                tip_dimensions(attributes, pressure).map(f64::to_bits),
                expected.map(f64::to_bits)
            );
        }
        assert_eq!(
            mouse_sample(InkPoint { x: 0.25, y: 0.5 })
                .pressure
                .to_bits(),
            0.5_f32.to_bits()
        );
        assert_eq!(
            tip_dimensions(
                InkAttributes {
                    ignore_pressure: true,
                    ..attributes
                },
                1.0
            )
            .map(f64::to_bits),
            [8.0_f64, 4.0].map(f64::to_bits)
        );
    }

    #[test]
    fn ordered_click_and_drag_keep_dot_and_out_of_frame_samples_without_project_edits() {
        let (_root, workspace, mut draft) = setup();
        let before = workspace.manifest().clone();
        let mut view = CinemagraphPreview::default();
        let map = mapping();
        for event in [
            InputEvent::Down(egui::pos2(-1.0, 1.0)),
            InputEvent::Up(egui::pos2(1.0, 1.0)),
        ] {
            view.event(&mut draft, map, event).unwrap();
        }
        assert!(draft.strokes().is_empty());
        for event in [
            InputEvent::Down(egui::pos2(20.25, 30.5)),
            InputEvent::Up(egui::pos2(20.25, 30.5)),
        ] {
            view.event(&mut draft, map, event).unwrap();
        }
        assert_eq!(
            draft.strokes()[0].stroke.samples,
            [mouse_sample(InkPoint {
                x: 10.125,
                y: 15.25
            })]
        );
        for event in [
            InputEvent::Down(egui::pos2(50.0, 50.0)),
            InputEvent::Move(egui::pos2(225.5, -20.25)),
            InputEvent::Up(egui::pos2(240.0, -40.0)),
        ] {
            view.event(&mut draft, map, event).unwrap();
        }
        assert_eq!(
            draft.strokes()[1].stroke.samples.last().unwrap().position,
            InkPoint { x: 120.0, y: -20.0 }
        );
        assert!(!draft.gesture_active());
        assert_eq!(workspace.manifest(), &before);
    }

    #[test]
    fn move_resize_and_escape_use_original_snapshot_preserving_tip_and_pressure() {
        let (_root, _workspace, mut draft) = setup();
        add_line(&mut draft);
        draft.select_all().unwrap();
        draft.tool = CinemagraphTool::Select;
        let original = draft.strokes().to_vec();
        let mut view = CinemagraphPreview::default();
        view.refresh_bounds(&draft);
        let map = mapping();
        view.event(&mut draft, map, InputEvent::Down(egui::pos2(60.0, 60.0)))
            .unwrap();
        view.event(&mut draft, map, InputEvent::Move(egui::pos2(100.0, 100.0)))
            .unwrap();
        view.event(&mut draft, map, InputEvent::Move(egui::pos2(110.0, 110.0)))
            .unwrap();
        assert_eq!(
            draft.strokes()[0].stroke.samples[0].position,
            InkPoint { x: 45.0, y: 45.0 }
        );
        view.event(&mut draft, map, InputEvent::Escape(egui::Modifiers::NONE))
            .unwrap();
        assert_eq!(draft.strokes(), original);
        view.refresh_bounds(&draft);
        view.event(&mut draft, map, InputEvent::Down(egui::pos2(82.0, 82.0)))
            .unwrap();
        view.event(&mut draft, map, InputEvent::Move(egui::pos2(104.0, 104.0)))
            .unwrap();
        view.event(&mut draft, map, InputEvent::Up(egui::pos2(126.0, 126.0)))
            .unwrap();
        assert_eq!(
            draft.strokes()[0].stroke.samples[0].position,
            InkPoint { x: 21.0, y: 21.0 }
        );
        assert_eq!(
            draft.strokes()[0].stroke.samples[1].position,
            InkPoint { x: 61.0, y: 61.0 }
        );
        assert_eq!(
            draft.strokes()[0].stroke.attributes,
            original[0].stroke.attributes
        );
        assert!(
            draft.strokes()[0]
                .stroke
                .samples
                .iter()
                .all(|sample| sample.pressure.to_bits() == 0.5_f32.to_bits())
        );
    }

    fn ui_frame(
        ctx: &egui::Context,
        view: &mut CinemagraphPreview,
        draft: &mut CinemagraphDraft,
        events: Vec<egui::Event>,
        focused: bool,
    ) -> Rect {
        let mut rect = Rect::NOTHING;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 500.0))),
                events,
                focused,
                ..egui::RawInput::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (_, response) = ui.allocate_exact_size(
                        egui::vec2(200.0, 200.0),
                        egui::Sense::click_and_drag(),
                    );
                    rect = response.rect;
                    view.show(ui, &response, draft, [100, 100], true);
                });
            },
        );
        rect
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn actual_egui_batch_keeps_click_and_escape_after_press_without_waiting_for_drag_threshold() {
        let (_root, workspace, mut draft) = setup();
        let before = workspace.manifest().clone();
        let ctx = egui::Context::default();
        let mut view = CinemagraphPreview::default();
        let rect = ui_frame(&ctx, &mut view, &mut draft, vec![], true);
        let pos = rect.min + egui::vec2(20.25, 30.5);
        ui_frame(
            &ctx,
            &mut view,
            &mut draft,
            vec![
                egui::Event::PointerMoved(pos),
                button(pos, true),
                button(pos, false),
            ],
            true,
        );
        assert_eq!(draft.strokes().len(), 1);
        assert_eq!(draft.strokes()[0].stroke.samples.len(), 1);
        let completed = draft.strokes().to_vec();
        ui_frame(
            &ctx,
            &mut view,
            &mut draft,
            vec![
                button(pos, true),
                egui::Event::PointerMoved(pos + egui::vec2(20.0, 30.0)),
                key(egui::Key::Escape),
                button(pos, false),
            ],
            true,
        );
        assert_eq!(draft.strokes(), completed);
        assert_eq!(workspace.manifest(), &before);
    }

    #[test]
    fn focus_loss_pointer_loss_and_event_overload_cancel_only_the_transient_gesture() {
        for loss in [
            vec![egui::Event::PointerGone],
            vec![egui::Event::WindowFocused(false)],
            vec![egui::Event::PointerMoved(egui::pos2(30.0, 30.0)); MAX_INPUT_EVENTS + 1],
        ] {
            let (_root, workspace, mut draft) = setup();
            let before = workspace.manifest().clone();
            let ctx = egui::Context::default();
            let mut view = CinemagraphPreview::default();
            let rect = ui_frame(&ctx, &mut view, &mut draft, vec![], true);
            let pos = rect.center();
            ui_frame(
                &ctx,
                &mut view,
                &mut draft,
                vec![egui::Event::PointerMoved(pos), button(pos, true)],
                true,
            );
            assert!(draft.gesture_active());
            ui_frame(&ctx, &mut view, &mut draft, loss, true);
            assert!(!draft.gesture_active());
            assert!(draft.strokes().is_empty());
            assert_eq!(workspace.manifest(), &before);
        }
    }

    #[test]
    fn delete_affects_only_selected_draft_strokes_and_input_trace_keeps_endpoints_bounded() {
        let (_root, workspace, mut draft) = setup();
        let before = workspace.manifest().clone();
        add_line(&mut draft);
        draft.select_all().unwrap();
        CinemagraphPreview::default()
            .event(
                &mut draft,
                mapping(),
                InputEvent::Delete(egui::Modifiers::NONE),
            )
            .unwrap();
        assert!(draft.strokes().is_empty());
        assert_eq!(workspace.manifest(), &before);
        assert_eq!(trace_indices(0).count(), 0);
        assert_eq!(trace_indices(1).collect::<Vec<_>>(), [0]);
        let sampled = trace_indices(16_384).collect::<Vec<_>>();
        assert_eq!(sampled.len(), 1024);
        assert_eq!(sampled[0], 0);
        assert_eq!(sampled[1023], 16_383);
        assert!(sampled.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn superseded_worker_keeps_one_slot_and_cannot_publish_a_stale_guide() {
        let (_root, _workspace, mut draft) = setup();
        add_line(&mut draft);
        let mut view = CinemagraphPreview::default();
        let old = DraftKey::new(&draft, [100, 100], 0);
        let (release, wait) = std::sync::mpsc::channel();
        view.task
            .start("blocked-old-guide", move |_| {
                wait.recv().map_err(|error| error.to_string())?;
                Ok(vec![(999, InkPath::default())])
            })
            .unwrap();
        view.running_key = Some(old);
        add_line(&mut draft);
        let current = DraftKey::new(&draft, [100, 100], 0);
        view.update_guides(&draft, current);
        assert!(view.task.is_running());
        assert!(view.task.is_cancelling());
        assert_eq!(view.running_key, Some(old));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while view.shown_key != Some(current) {
            view.update_guides(&draft, current);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(view.guides.len(), 2);
        assert!(view.guides.iter().all(|(id, _)| *id != 999));
    }
}
