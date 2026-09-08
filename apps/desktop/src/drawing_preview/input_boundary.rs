//! Preserve a genuine Down-position egui arbitration before later pointer samples.
//! Only events are split. `RawInput` timing, viewport, focus, modifiers and files
//! are never rewritten. Pending events survive cancellation/Ready mode and are
//! flushed to egui (not re-authored) in their original viewport and FIFO order.

use eframe::egui;

use super::{MAX_INPUT_EVENTS, invalidate};
use crate::editor_ui::{DrawingDraftPhase, DrawingOverlayDraft};

const MAX_RETAINED_BYTES: usize = 1024 * 1024;

struct Pending {
    viewport: egui::ViewportId,
    events: Vec<egui::Event>,
}

#[derive(Default)]
pub(crate) struct InputBoundary {
    pending: Option<Pending>,
    last_hook: Option<(egui::ViewportId, u64)>,
}

impl InputBoundary {
    /// Normal input/tail scans are bounded. A framework-provided oversized batch
    /// is never newly retained: existing tail is flushed without dropping user
    /// events, then drawing fails closed. That exceptional FIFO merge necessarily
    /// scales with the framework batch; it is not an O(8192) claim about egui.
    pub(crate) fn filter(
        &mut self,
        context: &egui::Context,
        input: &mut egui::RawInput,
        enabled: bool,
        draft: &mut DrawingOverlayDraft,
    ) {
        let stamp = (
            input.viewport_id,
            context.cumulative_frame_nr_for(input.viewport_id),
        );
        if self.last_hook == Some(stamp) {
            return;
        }
        if let Some(pending) = &self.pending
            && pending.viewport != input.viewport_id
        {
            // Never inject a different window's clicks, text or focus changes.
            if input.viewports.contains_key(&pending.viewport) {
                context.request_repaint_of(pending.viewport);
                return;
            }
            // Input for a retired receiving window must not be injected into
            // a new viewport or permanently block its drawing boundary.
            self.pending = None;
            invalidate(context, draft);
        }
        self.last_hook = Some(stamp);
        if let Some(mut pending) = self.pending.take() {
            pending.events.append(&mut input.events);
            input.events = pending.events;
        }
        if !enabled || !input.focused || input.events.len() > MAX_INPUT_EVENTS {
            invalidate(context, draft);
            return;
        }
        if draft.phase != DrawingDraftPhase::Capturing
            || draft.preview_gesture.active.is_some()
            || draft.preview_gesture.blocked
        {
            return;
        }
        let Some(seen) = draft.preview_gesture.seen else {
            return;
        };
        if seen.mapping.viewport != input.viewport_id {
            return;
        }
        let Some((index, position)) = input.events.iter().enumerate().find_map(|(index, event)| {
            if let egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                ..
            } = event
            {
                Some((index, *pos))
            } else {
                None
            }
        }) else {
            return;
        };
        if !seen.mapping.contains(position)
            || context.layer_id_at(position) != Some(seen.mapping.layer)
            || index + 1 == input.events.len()
        {
            return;
        }
        let tail = &input.events[index + 1..];
        if !tail.iter().any(changes_pointer_sample) {
            return;
        }
        if retained_size(tail).is_none_or(|bytes| bytes > MAX_RETAINED_BYTES) {
            // Keep the original framework events untouched and decline drawing;
            // do not retain arbitrary IME/paste/screenshot payloads in this FIFO.
            invalidate(context, draft);
            return;
        }
        self.pending = Some(Pending {
            viewport: input.viewport_id,
            events: input.events.split_off(index + 1),
        });
        context.request_repaint_of(input.viewport_id);
    }
}

fn changes_pointer_sample(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::PointerMoved(_)
            | egui::Event::PointerButton { .. }
            | egui::Event::PointerGone
            | egui::Event::WindowFocused(_)
    )
}

fn retained_size(events: &[egui::Event]) -> Option<usize> {
    events.iter().try_fold(events.len().checked_mul(std::mem::size_of::<egui::Event>())?, |total, event| {
        let payload = match event {
            egui::Event::Text(text) | egui::Event::Paste(text)
            | egui::Event::Ime(egui::ImeEvent::Preedit(text) | egui::ImeEvent::Commit(text)) => text.capacity(),
            egui::Event::Copy | egui::Event::Cut | egui::Event::Key { .. }
            | egui::Event::PointerMoved(_) | egui::Event::MouseMoved(_)
            | egui::Event::PointerButton { .. } | egui::Event::PointerGone
            | egui::Event::Zoom(_) | egui::Event::Touch { .. }
            | egui::Event::MouseWheel { .. } | egui::Event::WindowFocused(_)
            | egui::Event::Ime(egui::ImeEvent::Enabled | egui::ImeEvent::Disabled) => 0,
            _ => return None,
        };
        total.checked_add(payload)
    })
}

#[cfg(test)]
#[path = "input_boundary_tests.rs"]
mod tests;
