//! One explicit border gesture owns a stable, invisible `InputOnly` grab window.

use std::time::{Duration, Instant};

use x11rb::protocol::xproto::{ButtonPressEvent, GrabMode, GrabStatus, KeyButMask};

use super::*;

const GESTURE_LIFETIME: Duration = Duration::from_secs(30);

pub(super) struct Gesture {
    id: u64,
    started: Instant,
    event_floor: u64,
}

impl Windows<'_, '_> {
    pub(super) fn create_keeper(&mut self) -> Result<(), String> {
        let id = self.connection.generate_id().map_err(native_error)?;
        self.connection
            .create_window(
                0,
                id,
                self.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new()
                    .override_redirect(1)
                    .event_mask(EventMask::STRUCTURE_NOTIFY),
            )
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        self.keeper = Some(id);
        self.shapes(id, None, None)?;
        self.connection
            .map_window(id)
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        Ok(())
    }

    pub(super) fn service_gesture(&mut self, context: &Context) -> Result<(), String> {
        let epoch = context.cancel_epoch();
        let expired = self
            .gesture
            .as_ref()
            .is_some_and(|gesture| gesture.started.elapsed() >= GESTURE_LIFETIME);
        let revoked = self
            .gesture
            .as_ref()
            .is_some_and(|gesture| !context.active_gesture(gesture.id));
        if epoch != self.cancel_epoch || expired || revoked {
            self.connection.stream().begin_operation();
            self.abort_gesture(context)?;
            self.connection.stream().registration_complete();
            self.cancel_epoch = epoch;
        }
        Ok(())
    }

    pub(super) fn pointer_event(
        &mut self,
        event: &Event,
        sequence: u64,
        context: &Context,
    ) -> Result<(), String> {
        // Cancellation is also checked inside each event batch, not only every
        // 128 events. A callback cannot revive a gesture cancelled by the UI.
        self.service_gesture(context)?;
        match event {
            Event::ButtonPress(event)
                if event.response_type & 0x80 == 0
                    && event.detail == 1
                    && self.gesture.is_none()
                    && sequence >= self.event_floor =>
            {
                self.press(event, context)
            }
            Event::MotionNotify(event)
                if event.response_type & 0x80 == 0 && self.keeper == Some(event.event) =>
            {
                let Some(gesture) = &self.gesture else {
                    return Ok(());
                };
                if sequence < gesture.event_floor {
                    return Ok(());
                }
                if event.root == self.root {
                    context.event(GuidePointerEvent::Moved {
                        gesture_id: gesture.id,
                        position: position(event.root_x, event.root_y),
                    });
                } else {
                    self.connection.stream().begin_operation();
                    self.abort_gesture(context)?;
                    self.connection.stream().registration_complete();
                }
                Ok(())
            }
            Event::ButtonRelease(event)
                if event.response_type & 0x80 == 0
                    && event.detail == 1
                    && self.keeper == Some(event.event) =>
            {
                let Some(gesture) = &self.gesture else {
                    return Ok(());
                };
                if sequence < gesture.event_floor {
                    return Ok(());
                }
                self.connection.stream().begin_operation();
                let gesture = self.gesture.take().ok_or("Recorder gesture disappeared.")?;
                self.ungrab()?;
                context.event(if event.root == self.root {
                    GuidePointerEvent::Released {
                        gesture_id: gesture.id,
                        position: position(event.root_x, event.root_y),
                    }
                } else {
                    GuidePointerEvent::Cancelled {
                        gesture_id: gesture.id,
                    }
                });
                self.connection.stream().registration_complete();
                Ok(())
            }
            _ => Ok(()), // No hover stream and no unowned/root pointer observations.
        }
    }

    fn press(&mut self, event: &ButtonPressEvent, context: &Context) -> Result<(), String> {
        let Some(index) = self.ids.iter().position(|id| *id == event.event) else {
            return Ok(());
        };
        if !self.visible[index] || event.root != self.root {
            return Ok(());
        }
        let Some(request) = self.current else {
            return Ok(());
        };
        let Some(region) = request.region else {
            return Ok(());
        };
        let Some(id) = context.begin_gesture(request.generation, self.cancel_epoch)? else {
            return Ok(());
        };
        let started = Instant::now();
        self.connection.stream().begin_operation();
        let cursor = if index == HANDLE_INDEX {
            self.handle_cursor.unwrap_or(x11rb::NONE)
        } else {
            x11rb::NONE
        };
        let result = self.grab(event.time, id, started, context, cursor)?;
        self.connection.stream().registration_complete();
        if result {
            let position = position(event.root_x, event.root_y);
            context.event(GuidePointerEvent::Pressed {
                generation: request.generation,
                gesture_id: id,
                position,
                edge: hit_edge(index, position, region),
                modifiers: u16::from(event.state) & 0xff,
            });
        }
        Ok(())
    }

    fn grab(
        &mut self,
        time: u32,
        id: u64,
        started: Instant,
        context: &Context,
        cursor: Cursor,
    ) -> Result<bool, String> {
        let keeper = self
            .keeper
            .ok_or("Recorder gesture owner is unavailable.")?;
        let cookie = self
            .connection
            .grab_pointer(
                false,
                keeper,
                EventMask::POINTER_MOTION | EventMask::BUTTON_RELEASE,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                cursor,
                time,
            )
            .map_err(native_error)?;
        let event_floor = cookie.sequence_number();
        let result = cookie.reply().map_err(native_error)?;
        if result.status != GrabStatus::SUCCESS {
            context.event(GuidePointerEvent::Cancelled { gesture_id: id });
            if matches!(
                result.status,
                GrabStatus::INVALID_TIME | GrabStatus::ALREADY_GRABBED
            ) {
                // Rapid click/release/press can leave an older ButtonPress in
                // our queue while a newer implicit grab already exists. Do not
                // ungrab that newer/foreign owner or fail the whole guide;
                // consume this stale press and process the next real event.
                return Ok(false);
            }
            return Err("Could not acquire the explicit recorder border pointer gesture.".into());
        }
        self.gesture = Some(Gesture {
            id,
            started,
            event_floor,
        });
        let pointer = self
            .connection
            .query_pointer(self.root)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        // A very short click can release BEFORE GrabPointer reaches the server.
        // Do not ignore that old release and leave an idle pointer grabbed.
        if !pointer.mask.contains(KeyButMask::BUTTON1) || !context.active_gesture(id) {
            self.abort_gesture(context)?;
            return Ok(false);
        }
        Ok(true)
    }

    fn ungrab(&mut self) -> Result<(), String> {
        let cookie = self
            .connection
            .ungrab_pointer(x11rb::CURRENT_TIME)
            .map_err(native_error)?;
        self.event_floor = cookie.sequence_number();
        cookie.check().map_err(native_error)
    }

    fn abort_gesture(&mut self, context: &Context) -> Result<(), String> {
        if let Some(gesture) = self.gesture.take() {
            self.ungrab()?;
            context.event(GuidePointerEvent::Cancelled {
                gesture_id: gesture.id,
            });
        } else {
            // No owned grab: a reply marker discards pre-cancellation presses
            // without issuing an ungrab against another client's active gesture.
            let barrier = self.connection.get_input_focus().map_err(native_error)?;
            self.event_floor = barrier.sequence_number();
            barrier.reply().map_err(native_error)?;
        }
        Ok(())
    }
}

fn position(x: i16, y: i16) -> PhysicalPosition {
    PhysicalPosition {
        x: i32::from(x),
        y: i32::from(y),
    }
}
