//! Owned native recorder border windows; never a transparent full-canvas GL surface.

use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, TryRecvError},
    },
};

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
use std::sync::mpsc;
#[cfg(all(target_os = "linux", feature = "native-x11"))]
use std::thread;

use gif_from_screen_capture::{PhysicalPosition, PhysicalRect};

mod geometry;
#[cfg(all(target_os = "linux", feature = "native-x11"))]
mod native;
#[cfg(all(test, target_os = "linux", feature = "native-x11"))]
mod native_tests;
#[cfg(test)]
mod tests;

const EVENT_LIMIT: usize = 64;
#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
static NEXT_GESTURE_ID: Mutex<u64> = Mutex::new(0);

/// Desired physical border. Debug output deliberately omits screen coordinates.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct GuideRequest {
    /// Strictly increasing, nonzero identity; stale/duplicate requests are rejected.
    pub generation: u64,
    /// Capture rectangle in the selected X11 root's signed physical coordinates; None hides.
    pub region: Option<PhysicalRect>,
    /// Additional old capture rectangle to exclude during a caller-coordinated retarget.
    pub protected_region: Option<PhysicalRect>,
    /// Opaque border thickness, from 1 through 16 physical pixels.
    pub border_width: u16,
}

impl fmt::Debug for GuideRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuideRequest")
            .field("generation", &self.generation)
            .field("show", &self.region.is_some())
            .field("extra_exclusion", &self.protected_region.is_some())
            .field("border_width", &self.border_width)
            .finish_non_exhaustive()
    }
}

/// Server acknowledgement, retained by poll until another request replaces it.
/// This proves X server acceptance, NOT compositor presentation or capture-buffer freshness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuideAck {
    /// Request applied by the dedicated worker.
    pub generation: u64,
    /// At least one border strip is mapped; full-root capture and explicit hide return false.
    pub visible: bool,
}

/// Registration/update lifecycle; a stopped worker owns no remaining windows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuideStatus {
    /// Native connection/window setup is pending.
    Starting,
    /// One latest request is waiting or being applied.
    Updating {
        /// Identity awaiting acknowledgement.
        generation: u64,
    },
    /// Connected and idle, or the latest request has been accepted.
    Ready,
    /// Stop requested; new actions are suppressed while native cleanup finishes.
    Stopping,
    /// The worker terminated and its connection/windows were cleaned up.
    Stopped,
    /// A bounded native setup/update failed and cleanup completed.
    Failed(String),
}

/// Physical edge hit by an explicit primary-button press on an owned strip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "edge names directly describe their physical location"
)]
pub enum GuideEdge {
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    TopLeft,
}

/// Primary pointer gestures begin on owned border windows. A temporary owned grab
/// keeps motion/release on the same gesture while border presentation changes.
/// No root pointer feed or idle pointer grab is installed; hover is not reported.
/// Gesture identities never repeat within this process, even across guide instances.
#[derive(Clone, Copy, Eq, PartialEq)]
#[allow(
    missing_docs,
    reason = "gesture_id identifies one press; generation only identifies its initial presentation"
)]
pub enum GuidePointerEvent {
    Pressed {
        generation: u64,
        gesture_id: u64,
        position: PhysicalPosition,
        edge: GuideEdge,
        modifiers: u16,
    },
    Moved {
        gesture_id: u64,
        position: PhysicalPosition,
    },
    Released {
        gesture_id: u64,
        position: PhysicalPosition,
    },
    Cancelled {
        gesture_id: u64,
    },
}

impl GuidePointerEvent {
    fn gesture_id(self) -> u64 {
        match self {
            Self::Pressed { gesture_id, .. }
            | Self::Moved { gesture_id, .. }
            | Self::Released { gesture_id, .. }
            | Self::Cancelled { gesture_id } => gesture_id,
        }
    }
}

impl fmt::Debug for GuidePointerEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Pressed { .. } => "Pressed",
            Self::Moved { .. } => "Moved",
            Self::Released { .. } => "Released",
            Self::Cancelled { .. } => "Cancelled",
        };
        formatter
            .debug_struct(kind)
            .field("gesture_id", &self.gesture_id())
            .finish_non_exhaustive()
    }
}

/// A nonblocking bounded snapshot of the latest guide state.
pub struct GuideUpdate {
    /// Worker/registration state.
    pub status: GuideStatus,
    /// Retained latest acknowledgement, cleared by a newer request.
    pub ack: Option<GuideAck>,
    /// At most 64 pointer events. Motion coalesces and overload cancels the gesture.
    pub events: Vec<GuidePointerEvent>,
    /// Input events omitted because the bounded queue was full.
    pub dropped_events: u64,
}

struct Shared {
    request: Option<GuideRequest>,
    generation: u64,
    status: GuideStatus,
    ack: Option<GuideAck>,
    events: VecDeque<GuidePointerEvent>,
    dropped: u64,
    active_gesture: Option<u64>,
    cancel_epoch: u64,
}

struct Context {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
}

impl Context {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    fn cancel_gesture(&self) {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.cancel_epoch = shared.cancel_epoch.wrapping_add(1);
        if let Some(gesture_id) = shared.active_gesture.take() {
            shared
                .events
                .retain(|event| event.gesture_id() != gesture_id);
            if !self.cancelled() {
                if shared.events.len() >= EVENT_LIMIT {
                    shared.dropped = shared
                        .dropped
                        .saturating_add(shared.events.len() as u64 + 1);
                    shared.events.clear();
                }
                shared
                    .events
                    .push_back(GuidePointerEvent::Cancelled { gesture_id });
            }
        }
    }
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
impl Context {
    fn request(&self) -> Option<GuideRequest> {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .request
            .take()
    }
    fn connected(&self) {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.cancelled() && shared.request.is_none() {
            shared.status = GuideStatus::Ready;
        }
    }
    fn acknowledge(&self, ack: GuideAck) {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.cancelled() && shared.generation == ack.generation {
            shared.ack = Some(ack);
            shared.status = GuideStatus::Ready;
        }
    }

    fn cancel_epoch(&self) -> u64 {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancel_epoch
    }

    fn active_gesture(&self, gesture_id: u64) -> bool {
        !self.cancelled()
            && self
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .active_gesture
                == Some(gesture_id)
    }

    fn begin_gesture(&self, generation: u64, cancel_epoch: u64) -> Result<Option<u64>, String> {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if self.cancelled()
            || shared.cancel_epoch != cancel_epoch
            || shared.active_gesture.is_some()
            || shared.generation != generation
            || shared
                .ack
                .is_none_or(|ack| ack.generation != generation || !ack.visible)
        {
            return Ok(None);
        }
        let id = {
            let mut next = NEXT_GESTURE_ID
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let id = next
                .checked_add(1)
                .ok_or("Recorder gesture identities exhausted.")?;
            *next = id;
            id
        };
        shared.active_gesture = Some(id);
        Ok(Some(id))
    }
    fn event(&self, event: GuidePointerEvent) {
        if self.cancelled() {
            return;
        }
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if self.cancelled() {
            return;
        }
        if shared.active_gesture != Some(event.gesture_id()) {
            return;
        }
        if matches!(event, GuidePointerEvent::Moved { .. })
            && shared.events.back().is_some_and(|previous| {
                matches!(previous, GuidePointerEvent::Moved { .. })
                    && previous.gesture_id() == event.gesture_id()
            })
        {
            shared.events.pop_back();
        }
        if shared.events.len() >= EVENT_LIMIT {
            shared.dropped = shared
                .dropped
                .saturating_add(shared.events.len() as u64 + 1);
            shared.events.clear();
            shared.active_gesture = None;
            shared.cancel_epoch = shared.cancel_epoch.wrapping_add(1);
            shared.events.push_back(GuidePointerEvent::Cancelled {
                gesture_id: event.gesture_id(),
            });
        } else {
            shared.events.push_back(event);
            if matches!(
                event,
                GuidePointerEvent::Released { .. } | GuidePointerEvent::Cancelled { .. }
            ) {
                shared.active_gesture = None;
            }
        }
    }
}

/// A single dedicated connection/worker owning all native guide windows.
pub struct RecorderGuide {
    context: Context,
    result: Option<Receiver<Result<(), String>>>,
}

impl RecorderGuide {
    /// Starts asynchronous native setup. This does not scan or modify other clients' windows.
    ///
    /// # Errors
    /// Returns unsupported-build or thread-start errors. Native failures appear in poll.
    /// Gestures have a 30-second watchdog; protocol operations and cleanup are also bounded.
    pub fn start(display: Option<String>) -> Result<Self, String> {
        #[cfg(not(all(target_os = "linux", feature = "native-x11")))]
        {
            drop(display);
            Err("Native X11 recorder guides are not included in this build.".into())
        }
        #[cfg(all(target_os = "linux", feature = "native-x11"))]
        {
            let context = new_context();
            let worker = Context {
                shared: Arc::clone(&context.shared),
                cancel: Arc::clone(&context.cancel),
            };
            let (sender, result) = mpsc::sync_channel(1);
            thread::Builder::new()
                .name("x11-recorder-guide".into())
                .spawn(move || {
                    let result = native::run(display.as_deref(), &worker);
                    let _ = sender.send(result);
                })
                .map_err(|error| format!("Could not start recorder guide worker: {error}"))?;
            Ok(Self {
                context,
                result: Some(result),
            })
        }
    }

    /// Coalesces pending updates; an older request may be superseded without an ACK.
    /// During retargeting, the caller must coordinate capture pause/freshness: neither
    /// double exclusion nor a server ACK proves compositor presentation.
    /// Updating or temporarily hiding preserves an active gesture. Its events retain
    /// the same `gesture_id`; only a new press uses the new presentation generation.
    ///
    /// # Errors
    /// Rejects invalid geometry, stale generations, or a stopped registration.
    pub fn request(&self, request: GuideRequest) -> Result<(), String> {
        geometry::validate(request)?;
        let mut shared = self
            .context
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.context.cancelled() || self.result.is_none() {
            return Err("The recorder guide is stopping or stopped.".into());
        }
        if request.generation <= shared.generation {
            return Err("Recorder guide generations must strictly increase.".into());
        }
        shared.ack = None;
        shared.generation = request.generation;
        shared.status = GuideStatus::Updating {
            generation: request.generation,
        };
        shared.request = Some(request);
        Ok(())
    }

    /// Cancels only the current gesture, suppressing its queued motion immediately.
    /// Native pointer ungrab is asynchronous and bounded; later presses get new ids.
    pub fn cancel_gesture(&self) {
        self.context.cancel_gesture();
    }

    /// Immediately suppresses input. Native destruction finishes asynchronously.
    pub fn stop(&self) {
        self.context.cancel.store(true, Ordering::Release);
        let mut shared = self
            .context
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        shared.request = None;
        shared.events.clear();
        shared.ack = None;
        shared.active_gesture = None;
        shared.cancel_epoch = shared.cancel_epoch.wrapping_add(1);
        if self.result.is_some() {
            shared.status = GuideStatus::Stopping;
        }
    }

    /// True until poll observes the worker's completed cleanup.
    pub fn is_running(&self) -> bool {
        self.result.is_some()
    }

    /// Drains input without waiting and retains the latest matching-generation ACK.
    pub fn poll(&mut self) -> GuideUpdate {
        if let Some(receiver) = &self.result {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Recorder guide worker exited unexpectedly.".into()))
                }
            };
            if let Some(result) = result {
                self.result = None;
                let mut shared = self
                    .context
                    .shared
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                shared.status = if self.context.cancelled() {
                    GuideStatus::Stopped
                } else {
                    match result {
                        Ok(()) => GuideStatus::Stopped,
                        Err(error) => GuideStatus::Failed(error),
                    }
                };
                shared.ack = None;
                shared.events.clear();
                shared.request = None;
                shared.active_gesture = None;
            }
        }
        let mut shared = self
            .context
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        GuideUpdate {
            status: shared.status.clone(),
            ack: shared.ack,
            events: shared.events.drain(..).collect(),
            dropped_events: std::mem::take(&mut shared.dropped),
        }
    }
}

impl Drop for RecorderGuide {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
fn new_context() -> Context {
    Context {
        shared: Arc::new(Mutex::new(Shared {
            request: None,
            generation: 0,
            status: GuideStatus::Starting,
            ack: None,
            events: VecDeque::with_capacity(EVENT_LIMIT),
            dropped: 0,
            active_gesture: None,
            cancel_epoch: 0,
        })),
        cancel: Arc::new(AtomicBool::new(false)),
    }
}
