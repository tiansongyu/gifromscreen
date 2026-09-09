use eframe::egui;
use gif_from_screen_localization::Message;

use super::{
    draft::{Draft, GestureKind, Handle, Point, ShapeTool},
    geometry::{GeometryCache, handles},
};
use crate::ui_notice::Notice;

pub(super) const MAX_INPUT_EVENTS: usize = 512;
const MAX_HIT_QUERIES: usize = 1024;
const MAX_HIT_CANDIDATES: usize = 64;
pub(super) const HANDLE_RADIUS: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Mapping {
    pub(super) widget: egui::Id,
    pub(super) layer: egui::LayerId,
    pub(super) viewport: egui::ViewportId,
    pub(super) painted: egui::Rect,
    pub(super) visible: egui::Rect,
    pub(super) rendered: [u32; 2],
    pub(super) to_global: egui::emath::TSTransform,
    ppp: f32,
}

impl Mapping {
    fn new(ui: &egui::Ui, response: &egui::Response, rendered: [u32; 2]) -> Option<Self> {
        let transform = ui
            .ctx()
            .layer_transform_to_global(response.layer_id)
            .unwrap_or_default();
        let inverse = transform.inverse();
        let ppp = ui.ctx().pixels_per_point();
        let visible = response
            .rect
            .intersect(response.interact_rect)
            .intersect(ui.clip_rect());
        (response.rect.is_finite()
            && response.rect.is_positive()
            && visible.is_finite()
            && visible.is_positive()
            && !rendered.contains(&0)
            && transform.scaling.is_finite()
            && transform.scaling > 0.0
            && transform.translation.is_finite()
            && inverse.scaling.is_finite()
            && inverse.translation.is_finite()
            && ppp.is_finite()
            && ppp > 0.0
            && ui.is_visible()
            && ui.is_enabled()
            && response.enabled())
        .then_some(Self {
            widget: response.id,
            layer: response.layer_id,
            viewport: ui.ctx().viewport_id(),
            painted: response.rect,
            visible,
            rendered,
            to_global: transform,
            ppp,
        })
    }
    pub(super) fn contains(self, position: egui::Pos2) -> bool {
        let local = self.to_global.inverse() * position;
        local.is_finite()
            && local.x >= self.visible.min.x
            && local.x < self.visible.max.x
            && local.y >= self.visible.min.y
            && local.y < self.visible.max.y
    }
    fn point(self, position: egui::Pos2) -> Option<Point> {
        let local = self.to_global.inverse() * position;
        if !local.is_finite() {
            return None;
        }
        let x = ((f64::from(local.x) - f64::from(self.painted.min.x))
            / f64::from(self.painted.width())
            * f64::from(self.rendered[0])
            * 100.0)
            .clamp(0.0, f64::from(self.rendered[0]) * 100.0);
        let y = ((f64::from(local.y) - f64::from(self.painted.min.y))
            / f64::from(self.painted.height())
            * f64::from(self.rendered[1])
            * 100.0)
            .clamp(0.0, f64::from(self.rendered[1]) * 100.0);
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        // begin validates the canvas against the fixed hundredth-coordinate domain.
        #[allow(clippy::cast_possible_truncation)]
        Some([x.round() as i64, y.round() as i64])
    }
    pub(super) fn screen(self, point: gif_from_screen_render::InkPoint) -> egui::Pos2 {
        #[allow(clippy::cast_possible_truncation)]
        egui::pos2(
            (f64::from(self.painted.left())
                + point.x / f64::from(self.rendered[0]) * f64::from(self.painted.width()))
                as f32,
            (f64::from(self.painted.top())
                + point.y / f64::from(self.rendered[1]) * f64::from(self.painted.height()))
                as f32,
        )
    }
    pub(super) fn handle_position(
        self,
        handle: Handle,
        point: gif_from_screen_render::InkPoint,
        rotation: u16,
    ) -> egui::Pos2 {
        let mut position = self.screen(point);
        if handle == Handle::Rotate {
            let (sin, cos) = (f32::from(rotation) / 100.0).to_radians().sin_cos();
            // Draft canvases are bounded to 131070 pixels per axis, exactly
            // representable by f32. This is UI guide placement, not stored data.
            #[allow(clippy::cast_precision_loss)]
            let direction = egui::vec2(
                sin * self.painted.width() / self.rendered[0] as f32,
                -cos * self.painted.height() / self.rendered[1] as f32,
            )
            .normalized();
            position += direction * 24.0;
        }
        let margin = HANDLE_RADIUS + 2.0;
        position.x = position.x.clamp(
            self.visible.left() + margin.min(self.visible.width() / 2.0),
            self.visible.right() - margin.min(self.visible.width() / 2.0),
        );
        position.y = position.y.clamp(
            self.visible.top() + margin.min(self.visible.height() / 2.0),
            self.visible.bottom() - margin.min(self.visible.height() / 2.0),
        );
        position
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Seen {
    pub(super) frame: u64,
    pub(super) mapping: Mapping,
}

#[derive(Default)]
pub(super) struct PreviewInput {
    pub(super) seen: Option<Seen>,
    pub(super) active: Option<Mapping>,
    pub(super) blocked: bool,
    consumed: Option<(egui::ViewportId, u64)>,
    native_events: Option<(u64, Mapping, Vec<egui::Event>)>,
}

impl PreviewInput {
    /// Strip only focused-canvas rotation wheels before egui can turn Ctrl into
    /// global zoom or Shift into scroll. Keep the original bounded event order
    /// for canvas dispatch, and leave every other widget's wheel untouched.
    pub(super) fn capture_wheels(
        &mut self,
        context: &egui::Context,
        raw: &mut egui::RawInput,
        enabled: bool,
        draft: &Draft,
    ) {
        self.native_events = None;
        let Some(seen) = self.seen else {
            return;
        };
        if !enabled
            || !raw.focused
            || !draft.is_active()
            || draft.stale
            || self.active.is_some()
            || self.blocked
            || draft.selected.is_empty()
            || raw.events.len() > MAX_INPUT_EVENTS
            || seen.mapping.viewport != raw.viewport_id
            || !context
                .read_response(seen.mapping.widget)
                .is_some_and(|response| response.has_focus())
            || !context
                .input(|input| input.pointer.hover_pos())
                .is_some_and(|pos| seen.mapping.contains(pos))
            || raw.events.iter().any(|event| match event {
                egui::Event::PointerButton { .. }
                | egui::Event::PointerGone
                | egui::Event::WindowFocused(false) => true,
                egui::Event::PointerMoved(pos) => !seen.mapping.contains(*pos),
                _ => false,
            })
            || !raw.events.iter().any(rotation_wheel)
        {
            return;
        }
        let events = raw
            .events
            .iter()
            .filter(|event| canvas_event(event))
            .cloned()
            .collect();
        raw.events.retain(|event| !rotation_wheel(event));
        self.native_events = Some((
            context.cumulative_frame_nr_for(raw.viewport_id),
            seen.mapping,
            events,
        ));
    }

    pub(super) fn invalidate(&mut self, context: &egui::Context, draft: &mut Draft) {
        draft.cancel_gesture();
        self.active = None;
        self.seen = None;
        self.blocked = context.input(|input| input.pointer.primary_down());
        self.consumed = Some((context.viewport_id(), context.cumulative_frame_nr()));
    }
    pub(super) fn update(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rendered: [u32; 2],
        enabled: bool,
        draft: &mut Draft,
        geometry: &mut GeometryCache,
    ) -> Result<Option<Mapping>, Notice> {
        let Some(mapping) = Mapping::new(ui, response, rendered) else {
            self.invalidate(ui.ctx(), draft);
            return Ok(None);
        };
        if !enabled
            || !draft.is_active()
            || draft.stale
            || draft.canvas() != Some(rendered)
            || !ui.input(|input| input.focused)
            || ui.input(|input| input.pointer.button_down(egui::PointerButton::Middle))
        {
            self.invalidate(ui.ctx(), draft);
            return Ok(Some(mapping));
        }
        let frame = ui.ctx().cumulative_frame_nr();
        let previous = self.seen.replace(Seen { frame, mapping });
        let stamp = (mapping.viewport, frame);
        if self.active.is_some_and(|old| old != mapping)
            || previous.is_some_and(|old| old.frame > frame)
        {
            self.abort(ui.ctx(), draft);
            self.consumed = Some(stamp);
            return Ok(Some(mapping));
        }
        if self.consumed == Some(stamp) {
            return Ok(Some(mapping));
        }
        self.consumed = Some(stamp);
        let captured = self
            .native_events
            .take()
            .filter(|(stamp, old, _)| *stamp == frame && *old == mapping);
        let events = captured.map(|(_, _, events)| events).or_else(|| {
            ui.input(|input| {
                (input.events.len() <= MAX_INPUT_EVENTS).then(|| {
                    input
                        .events
                        .iter()
                        .filter(|event| canvas_event(event))
                        .cloned()
                        .collect::<Vec<_>>()
                })
            })
        });
        let Some(events) = events else {
            self.abort(ui.ctx(), draft);
            return Err(Message::VectorInputLimit.into());
        };
        if self.blocked {
            self.blocked = ui.input(|input| input.pointer.primary_down());
            return Ok(Some(mapping));
        }
        if self.active.is_some() && ui.ctx().dragged_id().is_some_and(|id| id != response.id) {
            self.abort(ui.ctx(), draft);
            return Ok(Some(mapping));
        }
        let stable = previous.is_some_and(|old| old.frame < frame && old.mapping == mapping);
        let result = self.process(response, mapping, stable, &events, draft, geometry);
        if result.is_err() {
            self.abort(ui.ctx(), draft);
        }
        if self.active.is_some() && !ui.input(|input| input.pointer.primary_down()) {
            self.abort(ui.ctx(), draft);
        }
        result.map(|()| Some(mapping))
    }

    fn abort(&mut self, context: &egui::Context, draft: &mut Draft) {
        draft.cancel_gesture();
        self.active = None;
        self.blocked = context.input(|input| input.pointer.primary_down());
    }

    fn process(
        &mut self,
        response: &egui::Response,
        mapping: Mapping,
        stable: bool,
        events: &[egui::Event],
        draft: &mut Draft,
        geometry: &mut GeometryCache,
    ) -> Result<(), Notice> {
        let mut keyboard = response.has_focus();
        let mut budget = MAX_HIT_QUERIES;
        for event in events {
            match *event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                } => {
                    if !mapping.contains(pos) {
                        keyboard = false;
                        continue;
                    }
                    if !stable || !owns_primary(response, pos) {
                        self.abort(&response.ctx, draft);
                        return Ok(());
                    }
                    let Some(point) = mapping.point(pos) else {
                        self.abort(&response.ctx, draft);
                        return Ok(());
                    };
                    if self.active.is_some() {
                        self.abort(&response.ctx, draft);
                        return Ok(());
                    }
                    begin_at(draft, geometry, mapping, point, pos, modifiers, &mut budget)?;
                    self.active = Some(mapping);
                    keyboard = true;
                    response.request_focus();
                }
                egui::Event::PointerMoved(pos) if self.active.is_some() => {
                    let Some(point) = mapping.point(pos) else {
                        self.abort(&response.ctx, draft);
                        return Ok(());
                    };
                    draft.update(point)?;
                }
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                } if self.active.is_some() => {
                    let Some(point) = mapping.point(pos) else {
                        self.abort(&response.ctx, draft);
                        return Ok(());
                    };
                    draft.update(point)?;
                    if let Some(GestureKind::Marquee { start, end, .. }) =
                        draft.gesture.as_ref().map(|g| g.kind)
                    {
                        let hits = geometry.marquee(draft, start, end, &mut budget)?;
                        draft.finish_marquee(&hits)?;
                    } else {
                        draft.finish();
                    }
                    self.active = None;
                    self.blocked = false;
                }
                egui::Event::PointerGone | egui::Event::WindowFocused(false) => {
                    self.abort(&response.ctx, draft);
                    keyboard = false;
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if keyboard => {
                    match key {
                        egui::Key::Escape => {
                            self.abort(&response.ctx, draft);
                        }
                        egui::Key::Delete | egui::Key::Backspace => {
                            draft.delete_selected()?;
                            self.active = None;
                            self.blocked = response.ctx.input(|input| input.pointer.primary_down());
                        }
                        _ => continue,
                    }
                    response.ctx.input_mut(|input| {
                        input.consume_key(modifiers, key);
                    });
                }
                egui::Event::MouseWheel {
                    delta, modifiers, ..
                } if keyboard && self.active.is_none() => {
                    let Some(step) = rotation_step(modifiers) else {
                        continue;
                    };
                    if delta.y.is_finite() && delta.y != 0.0 {
                        draft.rotate_by(if delta.y > 0.0 { step } else { -step })?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn canvas_event(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::PointerButton { .. }
            | egui::Event::PointerMoved(_)
            | egui::Event::PointerGone
            | egui::Event::WindowFocused(_)
            | egui::Event::Key { .. }
            | egui::Event::MouseWheel { .. }
    )
}

fn rotation_wheel(event: &egui::Event) -> bool {
    matches!(event, egui::Event::MouseWheel { delta, modifiers, .. }
        if delta.y.is_finite() && delta.y != 0.0 && rotation_step(*modifiers).is_some())
}

fn begin_at(
    draft: &mut Draft,
    geometry: &mut GeometryCache,
    mapping: Mapping,
    point: Point,
    pos: egui::Pos2,
    modifiers: egui::Modifiers,
    budget: &mut usize,
) -> Result<(), Notice> {
    if draft.tool == ShapeTool::Select {
        let handle = draft.primary().and_then(|object| {
            handles(object).into_iter().rev().find_map(|(handle, at)| {
                (mapping
                    .handle_position(handle, at, object.shape.rotation_hundredths)
                    .distance(mapping.to_global.inverse() * pos)
                    <= HANDLE_RADIUS + 2.0)
                    .then_some(handle)
            })
        });
        if let Some(handle) = handle {
            draft.begin_handle(point, handle)?;
        } else {
            let hit = geometry.hit(draft, point, budget)?;
            draft.select(point, hit, modifiers.ctrl)?;
        }
    } else {
        draft.insert(point)?;
    }
    Ok(())
}

pub(super) fn rotation_step(modifiers: egui::Modifiers) -> Option<i32> {
    if modifiers == egui::Modifiers::ALT {
        Some(9_000)
    } else if modifiers.ctrl && !modifiers.alt && !modifiers.shift && !modifiers.mac_cmd {
        Some(100)
    } else if modifiers == egui::Modifiers::SHIFT {
        Some(2_000)
    } else {
        None
    }
}

fn owns_primary(response: &egui::Response, point: egui::Pos2) -> bool {
    if !response.sense.senses_click()
        || !response.sense.senses_drag()
        || response.ctx.input(|input| input.pointer.interact_pos()) != Some(point)
        || response.ctx.layer_id_at(point) != Some(response.layer_id)
    {
        return false;
    }
    let owns = response.is_pointer_button_down_on()
        || response.drag_started_by(egui::PointerButton::Primary)
        || response.dragged_by(egui::PointerButton::Primary)
        || response.drag_stopped_by(egui::PointerButton::Primary)
        || response.clicked_by(egui::PointerButton::Primary);
    if !owns {
        return false;
    }
    let (clicked, candidates) = response.ctx.interaction_snapshot(|s| {
        (
            s.clicked,
            (s.contains_pointer.len() <= MAX_HIT_CANDIDATES)
                .then(|| s.contains_pointer.iter().copied().collect::<Vec<_>>()),
        )
    });
    let Some(candidates) = candidates else {
        return false;
    };
    if clicked.is_some_and(|id| id != response.id) {
        return false;
    }
    !candidates
        .into_iter()
        .filter(|id| *id != response.id)
        .any(|id| {
            response.ctx.read_response(id).is_some_and(|other| {
                let local = response
                    .ctx
                    .layer_transform_from_global(other.layer_id)
                    .unwrap_or_default()
                    * point;
                other.enabled()
                    && other.sense.senses_click()
                    && other.interact_rect.contains(local)
                    && other.is_pointer_button_down_on()
            })
        })
}
