//! One owned primary-button sequence per ordinary drawing draft.
//! No project writes, fitting, or image processing happen in this adapter.

use eframe::egui;
use gif_from_screen_domain::StrokePoint;

use crate::editor_ui::{DrawingDraftPhase, DrawingOverlayDraft, MAX_DRAWING_DRAFT_POINTS};

const MAX_INPUT_EVENTS: usize = MAX_DRAWING_DRAFT_POINTS * 2;
const MAX_HIT_CANDIDATES: usize = 64;

#[path = "drawing_preview/input_boundary.rs"]
mod input_boundary;
pub(crate) use input_boundary::InputBoundary;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Mapping {
    widget: egui::Id,
    layer: egui::LayerId,
    viewport: egui::ViewportId,
    painted: egui::Rect,
    visible: egui::Rect,
    rendered: [u32; 2],
    to_global: egui::emath::TSTransform,
    // Effective scale includes user zoom. Never multiply it into image points.
    pixels_per_point: f32,
}

impl Mapping {
    fn new(ui: &egui::Ui, response: &egui::Response, rendered: [u32; 2]) -> Option<Self> {
        let pixels_per_point = ui.ctx().pixels_per_point();
        let painted = response.rect;
        let to_global = ui
            .ctx()
            .layer_transform_to_global(response.layer_id)
            .unwrap_or_default();
        let inverse = to_global.inverse();
        let visible = painted
            .intersect(response.interact_rect)
            .intersect(ui.clip_rect());
        (painted.is_finite()
            && painted.is_positive()
            && visible.is_positive()
            && !rendered.contains(&0)
            && to_global.scaling.is_finite()
            && to_global.scaling > 0.0
            && to_global.translation.is_finite()
            && inverse.scaling.is_finite()
            && inverse.translation.is_finite()
            && pixels_per_point.is_finite()
            && pixels_per_point > 0.0
            && ui.is_visible()
            && response.enabled()
            && ui.is_enabled())
        .then_some(Self {
            widget: response.id,
            layer: response.layer_id,
            viewport: ui.ctx().viewport_id(),
            painted,
            visible,
            rendered,
            to_global,
            pixels_per_point,
        })
    }

    fn point(self, position: egui::Pos2) -> Option<StrokePoint> {
        let local = self.to_global.inverse() * position;
        crate::map_drawing_preview_point(self.painted, local, self.rendered).map(|point| {
            StrokePoint {
                point,
                pressure_milli: 1_000,
            }
        })
    }

    fn contains(self, position: egui::Pos2) -> bool {
        position.is_finite() && self.visible.contains(self.to_global.inverse() * position)
    }
}

#[derive(Clone, Copy, Debug)]
struct Seen {
    frame: u64,
    mapping: Mapping,
}

/// Ephemeral input identity, separate from the target anchor and authored points.
#[derive(Debug, Default)]
pub(crate) struct PreviewGesture {
    seen: Option<Seen>,
    active: Option<Mapping>,
    blocked: bool,
    consumed_frame: Option<u64>,
}

impl PreviewGesture {
    pub(crate) fn reset_for_begin(&mut self) {
        self.blocked |= self.active.is_some();
        self.active = None;
    }
}

#[derive(Clone, Copy)]
enum InputEvent {
    Down(egui::Pos2),
    Move(egui::Pos2),
    Up(egui::Pos2),
    Lost,
}

fn pointer_event(event: &egui::Event) -> Option<InputEvent> {
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
        egui::Event::PointerMoved(position) => Some(InputEvent::Move(*position)),
        egui::Event::PointerGone | egui::Event::WindowFocused(false) => Some(InputEvent::Lost),
        _ => None,
    }
}

/// Call when the ordinary drawing preview is hidden, unavailable or not editable.
/// Keep an explicitly armed target/style; discard only its uncompleted points.
/// Observing release here lets a fresh press work when the preview returns.
pub(crate) fn invalidate(context: &egui::Context, draft: &mut DrawingOverlayDraft) {
    if draft.phase == DrawingDraftPhase::Ready {
        return;
    }
    draft.discard_preview_stroke();
    draft.preview_gesture.active = None;
    draft.preview_gesture.seen = None;
    draft.preview_gesture.blocked = context.input(|input| input.pointer.primary_down());
    draft.preview_gesture.consumed_frame = Some(context.cumulative_frame_nr());
}

/// Process raw events once per frame, including Down before egui's drag threshold.
/// The response must be the actual painted image; `enabled` includes editor,
/// playback, crop, worker-loan and other mode ownership gates from the caller.
pub(crate) fn update(
    ui: &egui::Ui,
    response: &egui::Response,
    rendered_size: [u32; 2],
    enabled: bool,
    draft: &mut DrawingOverlayDraft,
) {
    if draft.phase == DrawingDraftPhase::Ready {
        return;
    }
    let Some(mapping) = Mapping::new(ui, response, rendered_size) else {
        invalidate(ui.ctx(), draft);
        return;
    };
    let (focused, primary_down, middle_down) = ui.input(|input| {
        (
            input.focused,
            input.pointer.primary_down(),
            input.pointer.button_down(egui::PointerButton::Middle),
        )
    });
    if !enabled || !focused || middle_down {
        invalidate(ui.ctx(), draft);
        return;
    }
    let frame = ui.ctx().cumulative_frame_nr();
    let mut state = std::mem::take(&mut draft.preview_gesture);
    let previous = state.seen.replace(Seen { frame, mapping });
    if draft.phase == DrawingDraftPhase::Idle {
        state.blocked &= primary_down;
        state.consumed_frame = Some(frame);
    } else if state.active.is_some_and(|original| original != mapping)
        || previous.is_some_and(|seen| seen.frame > frame)
    {
        abort(&mut state, draft, primary_down);
        state.consumed_frame = Some(frame);
    } else if state.consumed_frame != Some(frame) {
        state.consumed_frame = Some(frame);
        let stable = previous.is_some_and(|seen| seen.mapping == mapping && seen.frame < frame);
        if state.blocked {
            state.blocked = primary_down;
        } else {
            process(
                ui,
                response,
                mapping,
                stable,
                primary_down,
                &mut state,
                draft,
            );
        }
    }
    draft.preview_gesture = state;
}

fn abort(state: &mut PreviewGesture, draft: &mut DrawingOverlayDraft, primary_down: bool) {
    draft.discard_preview_stroke();
    state.active = None;
    state.blocked = primary_down;
}

fn owns_primary(response: &egui::Response, position: egui::Pos2) -> bool {
    // Click+drag is required: egui intentionally does not produce a drag for a
    // same-pass Down/Up. It also tracks click and drag candidates independently.
    if !response.sense.senses_click() || !response.sense.senses_drag() {
        return false;
    }
    // egui's public hit snapshot describes its final interact_pos, not every
    // historical Down coordinate. Do not borrow a later image hit to claim a
    // press that may have begun on an overlapping widget. Already-owned strokes
    // still process subsequent Move/Up samples normally.
    if response.ctx.input(|input| input.pointer.interact_pos()) != Some(position) {
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
    let (clicked, candidates) = response.ctx.interaction_snapshot(|snapshot| {
        (
            snapshot.clicked,
            (snapshot.contains_pointer.len() <= MAX_HIT_CANDIDATES).then(|| {
                snapshot
                    .contains_pointer
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
            }),
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
                    * position;
                other.enabled()
                    && other.sense.senses_click()
                    && other.interact_rect.contains(local)
                    && other.is_pointer_button_down_on()
            })
        })
}

fn process(
    ui: &egui::Ui,
    response: &egui::Response,
    mapping: Mapping,
    stable: bool,
    primary_down: bool,
    state: &mut PreviewGesture,
    draft: &mut DrawingOverlayDraft,
) {
    let events = ui.input(|input| {
        // Bound the whole input slice before filtering. Otherwise an unbounded
        // sequence of unrelated events could consume work without hitting a cap.
        (input.events.len() <= MAX_INPUT_EVENTS).then(|| {
            input
                .events
                .iter()
                .filter_map(pointer_event)
                .collect::<Vec<_>>()
        })
    });
    let Some(events) = events else {
        abort(state, draft, primary_down);
        return;
    };
    if state.active.is_some() && ui.ctx().dragged_id().is_some_and(|id| id != response.id) {
        abort(state, draft, primary_down);
        return;
    }
    for event in &events {
        match *event {
            InputEvent::Down(position) => {
                let owned = owns_primary(response, position)
                    && mapping.contains(position)
                    && ui.ctx().layer_id_at(position) == Some(mapping.layer);
                if !stable || !owned || state.active.is_some() {
                    abort(state, draft, primary_down);
                    return;
                }
                state.active = Some(mapping);
                response.request_focus();
                if !append(mapping, position, draft) {
                    abort(state, draft, primary_down);
                    return;
                }
            }
            InputEvent::Move(position) => {
                if state.active.is_some() && !append(mapping, position, draft) {
                    abort(state, draft, primary_down);
                    return;
                }
            }
            InputEvent::Up(position) => {
                if state.active.is_some() {
                    if !append(mapping, position, draft) {
                        abort(state, draft, false);
                        return;
                    }
                    draft.finish_stroke();
                    state.active = None;
                    state.blocked = false;
                    return; // Later motion/Gone does not change the completed endpoint.
                }
            }
            InputEvent::Lost => {
                abort(state, draft, primary_down);
                return;
            }
        }
        if draft.phase == DrawingDraftPhase::Ready {
            state.active = None;
            state.blocked = primary_down;
            return; // The existing 4096-point limit produces a usable Ready draft.
        }
    }
    if state.active.is_some() && !primary_down {
        abort(state, draft, primary_down);
    }
}

fn append(mapping: Mapping, position: egui::Pos2, draft: &mut DrawingOverlayDraft) -> bool {
    let Some(point) = mapping.point(position) else {
        return false;
    };
    draft.push_point(point);
    true
}

#[cfg(test)]
#[path = "drawing_preview_tests.rs"]
mod tests;
