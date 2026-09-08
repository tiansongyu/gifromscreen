//! A single owned input child makes a GUI drag handle share the picker's X connection.

use crate::{PickedWindow, WindowSnapBounds};
use gif_from_screen_capture::{PhysicalRect, PhysicalSize};
#[cfg(all(target_os = "linux", feature = "native-x11"))]
use std::sync::mpsc;
use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, TryRecvError},
    },
    time::Duration,
};

#[cfg(all(target_os = "linux", feature = "native-x11"))]
mod native;

/// Latest visible handle geometry in its owning GUI client's physical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DragPickerRequest {
    /// Nonzero monotonically increasing UI layout/selection identity.
    pub generation: u64,
    /// None disables the handle without intercepting invisible controls.
    pub rect: Option<PhysicalRect>,
    /// Exact GUI client size on which this layout was drawn.
    pub parent_size: PhysicalSize,
    /// Selection policy fixed at the initiating press.
    pub bounds: WindowSnapBounds,
}

/// Result of the one gesture this native handle accepted.
#[derive(Debug)]
pub struct DragPickerSelection {
    /// UI request which owned the initiating press.
    pub generation: u64,
    /// None means the user cancelled or released over no eligible target.
    pub picked: Option<PickedWindow>,
}

/// Nonblocking lifecycle observation. Native cleanup precedes terminal results.
pub struct DragPickerUpdate {
    /// Current request whose input rectangle is installed.
    pub ready: Option<u64>,
    /// Request currently selecting (retained until the result is consumed).
    pub active: Option<u64>,
    /// Press atomically claimed, possibly before the root grab is acknowledged.
    /// Block layout changes/Start here, but only `active` permits hiding the GUI.
    pub claimed: Option<u64>,
    /// One terminal result; Ok(None) means stopped before accepting a gesture.
    pub result: Option<Result<Option<DragPickerSelection>, String>>,
    /// The native worker has not yet published completed cleanup.
    pub running: bool,
}

#[derive(Default)]
struct Shared {
    request: Option<DragPickerRequest>,
    ready: Option<u64>,
    active: Option<u64>,
    claimed: Option<u64>,
}

struct Context {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
}

/// One input-only child of an explicitly verified process-owned GUI window.
/// It never changes the parent's properties, focus or geometry. Its Button1
/// passive grab is confined to that child's visible hit rectangle, not the root.
pub struct DragPickerButton {
    context: Context,
    result: Option<Receiver<Result<Option<DragPickerSelection>, String>>>,
}

impl DragPickerButton {
    /// Starts bounded native setup; readiness is reported asynchronously.
    ///
    /// # Errors
    /// Rejects a foreign PID, zero parent, unsupported build or thread failure.
    pub fn start(display: Option<String>, parent: u32, pid: u32) -> Result<Self, String> {
        Self::start_inner(display, parent, pid, Duration::from_secs(30))
    }

    #[cfg(all(test, target_os = "linux", feature = "native-x11"))]
    pub(crate) fn start_with_lifetime_for_test(
        display: Option<String>,
        parent: u32,
        pid: u32,
        lifetime: Duration,
    ) -> Result<Self, String> {
        Self::start_inner(display, parent, pid, lifetime)
    }

    fn start_inner(
        display: Option<String>,
        parent: u32,
        pid: u32,
        lifetime: Duration,
    ) -> Result<Self, String> {
        if parent == 0 || pid != std::process::id() {
            return Err("A drag picker handle must belong to this process's GUI window.".into());
        }
        #[cfg(not(all(target_os = "linux", feature = "native-x11")))]
        {
            let _ = (display, lifetime);
            Err("Native X11 drag handles are not included in this build.".into())
        }
        #[cfg(all(target_os = "linux", feature = "native-x11"))]
        {
            let shared = Arc::new(Mutex::new(Shared::default()));
            let cancel = Arc::new(AtomicBool::new(false));
            let worker = Context {
                shared: Arc::clone(&shared),
                cancel: Arc::clone(&cancel),
            };
            let (send, result) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("x11-picker-drag-handle".into())
                .spawn(move || {
                    let _ = send.send(native::run(
                        display.as_deref(),
                        parent,
                        pid,
                        lifetime,
                        &worker,
                    ));
                })
                .map_err(|error| error.to_string())?;
            Ok(Self {
                context: Context { shared, cancel },
                result: Some(result),
            })
        }
    }

    /// Replaces the latest idle handle request; active gestures keep their origin.
    ///
    /// # Errors
    /// Rejects stale generations, invalid physical rectangles or a stopped/active handle.
    pub fn request(&self, request: DragPickerRequest) -> Result<(), String> {
        if request.generation == 0
            || request.rect.is_some_and(|rect| {
                !rect.fits_within(request.parent_size)
                    || i16::try_from(rect.origin().x).is_err()
                    || i16::try_from(rect.origin().y).is_err()
                    || rect.size().width() > u32::from(u16::MAX)
                    || rect.size().height() > u32::from(u16::MAX)
            })
        {
            return Err("Drag handle must fit its physical GUI client bounds.".into());
        }
        let mut shared = self
            .context
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.cancelled() || self.result.is_none() || shared.claimed.is_some() {
            return Err("Drag handle is stopped or selecting.".into());
        }
        if shared
            .request
            .is_some_and(|previous| request.generation <= previous.generation)
        {
            return Err("Drag handle generation is stale.".into());
        }
        shared.request = Some(request);
        shared.ready = None;
        Ok(())
    }

    /// Requests cancellation; use poll to observe completed native cleanup.
    pub fn stop(&self) {
        self.context.cancel.store(true, Ordering::Release);
    }
    /// Whether cancellation was requested (not a cleanup acknowledgement).
    pub fn cancelled(&self) -> bool {
        self.context.cancel.load(Ordering::Acquire)
    }
    /// True through active selection and its pending cleanup result.
    pub fn is_picking(&self) -> bool {
        self.result.is_some()
            && self
                .context
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .active
                .is_some()
    }

    /// True after an initiating press is claimed, including the root-grab handoff.
    pub fn is_claimed(&self) -> bool {
        self.result.is_some()
            && self
                .context
                .shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .claimed
                .is_some()
    }
    /// Drains at most one result without waiting on native I/O.
    pub fn poll(&mut self) -> DragPickerUpdate {
        let result = match self.result.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => Some(Err(
                "Native drag handle worker exited without a result.".into(),
            )),
            _ => None,
        };
        if result.is_some() {
            self.result = None;
        }
        let mut shared = self
            .context
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if result.is_some() {
            shared.active = None;
            shared.claimed = None;
            shared.ready = None;
        }
        DragPickerUpdate {
            ready: shared.ready,
            active: shared.active,
            claimed: shared.claimed,
            result,
            running: self.result.is_some(),
        }
    }
}

impl Drop for DragPickerButton {
    fn drop(&mut self) {
        self.stop();
    }
}
