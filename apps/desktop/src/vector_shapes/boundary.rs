//! Keep Down-position egui arbitration real; do not patch response flags.
use eframe::egui;

use super::{
    draft::Draft,
    input::{MAX_INPUT_EVENTS, PreviewInput},
};

const MAX_RETAINED_BYTES: usize = 1024 * 1024;

struct Pending {
    viewport: egui::ViewportId,
    events: Vec<egui::Event>,
}
#[derive(Default)]
pub(super) struct InputBoundary {
    pending: Option<Pending>,
    last: Option<(egui::ViewportId, u64)>,
}

impl InputBoundary {
    pub(super) fn filter(
        &mut self,
        context: &egui::Context,
        input: &mut egui::RawInput,
        enabled: bool,
        state: &mut PreviewInput,
        draft: &mut Draft,
    ) {
        let stamp = (
            input.viewport_id,
            context.cumulative_frame_nr_for(input.viewport_id),
        );
        if self.last == Some(stamp) {
            return;
        }
        self.filter_fifo(context, input, enabled, state, draft);
        if self.last == Some(stamp) {
            state.capture_wheels(context, input, enabled, draft);
        }
    }

    fn filter_fifo(
        &mut self,
        context: &egui::Context,
        input: &mut egui::RawInput,
        enabled: bool,
        state: &mut PreviewInput,
        draft: &mut Draft,
    ) {
        let stamp = (
            input.viewport_id,
            context.cumulative_frame_nr_for(input.viewport_id),
        );
        if self.last == Some(stamp) {
            return;
        }
        if let Some(pending) = &self.pending
            && pending.viewport != input.viewport_id
        {
            if input.viewports.contains_key(&pending.viewport) {
                context.request_repaint_of(pending.viewport);
                return;
            }
            self.pending = None;
            state.invalidate(context, draft);
        }
        self.last = Some(stamp);
        if let Some(mut pending) = self.pending.take() {
            pending.events.append(&mut input.events);
            input.events = pending.events;
        }
        if !enabled || !input.focused || input.events.len() > MAX_INPUT_EVENTS {
            state.invalidate(context, draft);
            return;
        }
        if !draft.is_active() || draft.stale || state.active.is_some() || state.blocked {
            return;
        }
        let Some(seen) = state.seen else {
            return;
        };
        if seen.mapping.viewport != input.viewport_id {
            return;
        }
        let Some(index) = input.events.iter().position(|event| {
            matches!(
                event,
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                }
            )
        }) else {
            return;
        };
        if index + 1 == input.events.len() {
            return;
        }
        // This also separates a click on a later TextEdit from its following
        // Delete/text events, so old canvas focus cannot consume another field's input.
        if retained_size(&input.events[index + 1..]).is_none_or(|bytes| bytes > MAX_RETAINED_BYTES)
        {
            state.invalidate(context, draft);
            return;
        }
        self.pending = Some(Pending {
            viewport: input.viewport_id,
            events: input.events.split_off(index + 1),
        });
        context.request_repaint_of(input.viewport_id);
    }
}

fn retained_size(events: &[egui::Event]) -> Option<usize> {
    events.iter().try_fold(events.len().checked_mul(std::mem::size_of::<egui::Event>())?, |bytes,event| {
        let payload = match event {
            egui::Event::Text(text) | egui::Event::Paste(text)
            | egui::Event::Ime(egui::ImeEvent::Preedit(text) | egui::ImeEvent::Commit(text)) => text.capacity(),
            egui::Event::Copy | egui::Event::Cut | egui::Event::Key { .. } | egui::Event::PointerMoved(_)
            | egui::Event::MouseMoved(_) | egui::Event::PointerButton { .. } | egui::Event::PointerGone
            | egui::Event::Zoom(_) | egui::Event::Touch { .. } | egui::Event::MouseWheel { .. }
            | egui::Event::WindowFocused(_) | egui::Event::Ime(egui::ImeEvent::Enabled | egui::ImeEvent::Disabled) => 0,
            _ => return None,
        };
        bytes.checked_add(payload)
    })
}
