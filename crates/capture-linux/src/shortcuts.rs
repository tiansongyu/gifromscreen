//! Opt-in global recorder shortcuts, independent of captured input metadata.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
};

use crate::LinuxDisplayServer;

#[cfg(all(target_os = "linux", feature = "wayland-portal"))]
mod portal;
#[cfg(all(target_os = "linux", feature = "native-x11"))]
pub(crate) mod x11;

const QUEUE_LIMIT: usize = 64;

/// Optional nonblocking handler for an active recording. Returning true consumes
/// the action on the backend thread, independently of window repaint/visibility.
/// The application must invalidate its recording target at session boundaries.
pub type ShortcutActionHandler = Arc<dyn Fn(ShortcutAction) -> bool + Send + Sync>;

/// Recorder commands. Destructive discard is deliberately not a global action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ShortcutAction {
    /// Start a prepared recorder, or toggle acknowledged pause/resume.
    StartPause,
    /// Stop and save; during countdown, cancel that countdown.
    Stop,
    /// Request one frame in manual snapshot mode.
    Snapshot,
}

impl ShortcutAction {
    /// Stable portal/configuration identity.
    pub const fn id(self) -> &'static str {
        match self {
            Self::StartPause => "start-pause",
            Self::Stop => "stop",
            Self::Snapshot => "snapshot",
        }
    }

    /// English fallback label for the registration dialog.
    pub const fn description(self) -> &'static str {
        match self {
            Self::StartPause => "Start, pause or resume GIF recording",
            Self::Stop => "Stop and save GIF recording",
            Self::Snapshot => "Capture a manual GIF frame",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::StartPause => 0,
            Self::Stop => 1,
            Self::Snapshot => 2,
        }
    }
}

/// Portable named keys; modifiers alone and arbitrary global typing are excluded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutKey {
    /// Function key numbered 1 through 24.
    Function(u8),
    /// An uppercase ASCII letter or digit; requires Control, Alt or Super.
    Character(char),
}

/// Explicit modifiers and one key. No wildcard keyboard grabs are requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent physical modifier flags, not lifecycle states"
)]
pub struct ShortcutTrigger {
    /// Main key.
    pub key: ShortcutKey,
    /// Control modifier.
    pub control: bool,
    /// Alt modifier.
    pub alt: bool,
    /// Shift modifier.
    pub shift: bool,
    /// Super/Meta modifier.
    pub super_key: bool,
}

impl ShortcutTrigger {
    /// Validates the bounded portable trigger syntax.
    ///
    /// # Errors
    /// Rejects unsupported keys and unmodified typing keys.
    pub fn validate(self) -> Result<(), String> {
        match self.key {
            ShortcutKey::Function(1..=24) => Ok(()),
            ShortcutKey::Character(key)
                if (key.is_ascii_uppercase() || key.is_ascii_digit())
                    && (self.control || self.alt || self.super_key) =>
            {
                Ok(())
            }
            _ => Err("Use F1–F24, or an uppercase letter/digit with Control, Alt or Super.".into()),
        }
    }

    /// XDG shortcut trigger, also suitable as an X11 fallback display label.
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        if self.control {
            parts.push("CTRL".to_owned());
        }
        if self.alt {
            parts.push("ALT".to_owned());
        }
        if self.shift {
            parts.push("SHIFT".to_owned());
        }
        if self.super_key {
            parts.push("LOGO".to_owned());
        }
        parts.push(match self.key {
            ShortcutKey::Function(number) => format!("F{number}"),
            // XDG names identify the base layer; Shift is a separate modifier.
            ShortcutKey::Character(key) => key.to_ascii_lowercase().to_string(),
        });
        parts.join("+")
    }
}

/// Requested recorder binding; actual portal triggers may differ.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShortcutBinding {
    /// Action to request.
    pub action: ShortcutAction,
    /// Preferred trigger.
    pub trigger: ShortcutTrigger,
}

/// Conservative defaults avoiding Linux Ctrl+Alt+function-key console switching.
pub fn default_shortcut_bindings() -> Vec<ShortcutBinding> {
    [
        ShortcutAction::StartPause,
        ShortcutAction::Stop,
        ShortcutAction::Snapshot,
    ]
    .into_iter()
    .zip(7..=9)
    .map(|(action, number)| ShortcutBinding {
        action,
        trigger: ShortcutTrigger {
            key: ShortcutKey::Function(number),
            control: true,
            alt: false,
            shift: true,
            super_key: false,
        },
    })
    .collect()
}

/// A binding confirmed by the backend, not merely requested by the app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredShortcut {
    /// Confirmed action.
    pub action: ShortcutAction,
    /// Backend-provided human-readable trigger.
    pub trigger_description: String,
}

/// Lifecycle of a single independently owned registration session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShortcutStatus {
    /// Registration or permission response is pending.
    Registering,
    /// These actions were actually bound; portals may return a subset.
    Active(Vec<RegisteredShortcut>),
    /// Cancellation requested; old events are already disabled.
    Stopping,
    /// The worker and its registrations have stopped.
    Stopped,
    /// Registration or a live backend failed. Recorder buttons remain usable.
    Failed(String),
}

/// One nonblocking UI update. Stop has priority over queued nonterminal actions.
pub struct ShortcutUpdate {
    /// Current registration status.
    pub status: ShortcutStatus,
    /// Bounded, press-edge-deduplicated actions.
    pub actions: Vec<ShortcutAction>,
    /// Nonterminal presses omitted because the bounded queue was full.
    pub dropped_actions: u64,
}

struct Inbox {
    handler: Option<ShortcutActionHandler>,
    status: ShortcutStatus,
    queue: VecDeque<ShortcutAction>,
    stop_pending: bool,
    pressed: [bool; 3],
    #[cfg_attr(
        not(any(feature = "native-x11", feature = "wayland-portal", test)),
        allow(
            dead_code,
            reason = "checked by optional native registration publishers"
        )
    )]
    requested: [bool; 3],
    dropped: u64,
}

/// Backend-only publisher; callbacks never execute application code.
pub(super) struct ShortcutContext {
    cancellation: Arc<AtomicBool>,
    inbox: Arc<Mutex<Inbox>>,
}

#[cfg_attr(
    not(any(feature = "native-x11", feature = "wayland-portal", test)),
    allow(
        dead_code,
        reason = "native event publishers are unused without either optional backend"
    )
)]
impl ShortcutContext {
    pub(super) fn cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
    pub(super) fn cancellation(&self) -> &AtomicBool {
        &self.cancellation
    }
    pub(super) fn registered(&self, bindings: Vec<RegisteredShortcut>) -> Result<(), String> {
        let mut inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        if self.cancelled() {
            return Err("Shortcut registration cancelled.".into());
        }
        let mut seen = [false; 3];
        for binding in &bindings {
            let index = binding.action.index();
            if seen[index]
                || !inbox.requested[index]
                || binding.trigger_description.is_empty()
                || binding.trigger_description.len() > 256
                || binding.trigger_description.chars().any(char::is_control)
            {
                return Err("Backend returned invalid or duplicate shortcut bindings.".into());
            }
            seen[index] = true;
        }
        inbox.status = ShortcutStatus::Active(bindings);
        inbox.pressed = [false; 3];
        inbox.queue.clear();
        inbox.stop_pending = false;
        Ok(())
    }
    pub(super) fn activated(&self, action: ShortcutAction) {
        if self.cancelled() {
            return;
        }
        let mut inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        let ShortcutStatus::Active(bindings) = &inbox.status else {
            return;
        };
        if !bindings.iter().any(|binding| binding.action == action) || inbox.pressed[action.index()]
        {
            return;
        }
        inbox.pressed[action.index()] = true;
        if let Some(handler) = inbox.handler.clone() {
            drop(inbox);
            if handler(action) {
                return;
            }
            inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
            if self.cancelled() || !matches!(inbox.status, ShortcutStatus::Active(_)) {
                return;
            }
        }
        if action == ShortcutAction::Stop {
            inbox.stop_pending = true;
        } else if inbox.queue.len() < QUEUE_LIMIT {
            inbox.queue.push_back(action);
        } else {
            inbox.dropped = inbox.dropped.saturating_add(1);
        }
    }
    pub(super) fn deactivated(&self, action: ShortcutAction) {
        self.inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pressed[action.index()] = false;
    }
    pub(super) fn backend_stopping(&self) {
        let mut inbox = self.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        inbox.queue.clear();
        inbox.stop_pending = false;
        inbox.pressed = [false; 3];
        inbox.status = ShortcutStatus::Stopping;
        inbox.handler = None;
    }
}

/// One worker/session. Call only after the user opts in, and stop when leaving
/// recorder scope. It never installs a persistent system shortcut or input hook.
pub struct GlobalShortcutService {
    context: ShortcutContext,
    result: Option<Receiver<Result<(), String>>>,
}

impl GlobalShortcutService {
    /// Starts a bounded registration worker without blocking the UI.
    ///
    /// # Errors
    /// Rejects invalid/duplicate bindings or thread allocation failure.
    pub fn start(
        display: LinuxDisplayServer,
        bindings: Vec<ShortcutBinding>,
    ) -> Result<Self, String> {
        validate_bindings(&bindings)?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut requested = [false; 3];
        for binding in &bindings {
            requested[binding.action.index()] = true;
        }
        let inbox = Arc::new(Mutex::new(Inbox {
            handler: None,
            status: ShortcutStatus::Registering,
            queue: VecDeque::with_capacity(QUEUE_LIMIT),
            stop_pending: false,
            pressed: [false; 3],
            requested,
            dropped: 0,
        }));
        let worker = ShortcutContext {
            cancellation: Arc::clone(&cancellation),
            inbox: Arc::clone(&inbox),
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("global-recorder-shortcuts".into())
            .spawn(move || {
                let _ = sender.send(run_backend(display, &bindings, &worker));
            })
            .map_err(|error| format!("Could not start shortcut worker: {error}"))?;
        Ok(Self {
            context: ShortcutContext {
                cancellation,
                inbox,
            },
            result: Some(receiver),
        })
    }

    /// Cancels registration and immediately suppresses queued/late actions.
    pub fn stop(&self) {
        self.context.cancellation.store(true, Ordering::Release);
        let mut inbox = self
            .context
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        inbox.queue.clear();
        inbox.stop_pending = false;
        inbox.handler = None;
        if self.result.is_some() {
            inbox.status = ShortcutStatus::Stopping;
        }
    }

    /// Whether an old worker still owns the registration slot.
    pub fn is_running(&self) -> bool {
        self.result.is_some()
    }

    /// Changes the active recording destination and drops older queued UI actions.
    /// Handlers must do bounded, nonblocking work; never perform UI work here.
    pub fn set_action_handler(&self, handler: Option<ShortcutActionHandler>) {
        let mut inbox = self
            .context
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        inbox.queue.clear();
        inbox.stop_pending = false;
        inbox.handler = if self.context.cancelled() {
            None
        } else {
            handler
        };
    }

    /// Drains bounded actions and observes backend termination without waiting.
    pub fn poll(&mut self) -> ShortcutUpdate {
        if let Some(receiver) = &self.result {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("Shortcut worker exited unexpectedly.".into()))
                }
            };
            if let Some(result) = result {
                self.result = None;
                let mut inbox = self
                    .context
                    .inbox
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                inbox.status = if self.context.cancelled() {
                    ShortcutStatus::Stopped
                } else {
                    match result {
                        Ok(()) => ShortcutStatus::Stopped,
                        Err(error) => ShortcutStatus::Failed(error),
                    }
                };
                inbox.queue.clear();
                inbox.stop_pending = false;
                inbox.pressed = [false; 3];
                inbox.handler = None;
            }
        }
        let mut inbox = self
            .context
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let actions = if self.context.cancelled() {
            inbox.queue.clear();
            inbox.stop_pending = false;
            Vec::new()
        } else if inbox.stop_pending {
            inbox.stop_pending = false;
            inbox.queue.clear();
            vec![ShortcutAction::Stop]
        } else {
            inbox.queue.drain(..).collect()
        };
        ShortcutUpdate {
            status: inbox.status.clone(),
            actions,
            dropped_actions: std::mem::take(&mut inbox.dropped),
        }
    }
}

impl Drop for GlobalShortcutService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Validates before either backend sees any global registration requests.
///
/// # Errors
/// Rejects empty/oversized lists, duplicate actions/triggers or invalid keys.
pub fn validate_bindings(bindings: &[ShortcutBinding]) -> Result<(), String> {
    if !(1..=3).contains(&bindings.len()) {
        return Err("Configure one to three recorder shortcuts.".into());
    }
    for (index, binding) in bindings.iter().enumerate() {
        binding.trigger.validate()?;
        if bindings[..index]
            .iter()
            .any(|other| other.action == binding.action || other.trigger == binding.trigger)
        {
            return Err("Each recorder action and shortcut trigger must be unique.".into());
        }
    }
    Ok(())
}

fn run_backend(
    display: LinuxDisplayServer,
    bindings: &[ShortcutBinding],
    context: &ShortcutContext,
) -> Result<(), String> {
    match display {
        #[cfg(all(target_os = "linux", feature = "native-x11"))]
        LinuxDisplayServer::X11 => x11::run(bindings, context),
        #[cfg(all(target_os = "linux", feature = "wayland-portal"))]
        LinuxDisplayServer::Wayland => portal::run(bindings, context),
        #[allow(
            unreachable_patterns,
            reason = "feature-disabled builds need an unavailable backend"
        )]
        _ => {
            let _ = (bindings, context);
            Err("This build does not include the selected global-shortcut backend.".into())
        }
    }
}

#[cfg(test)]
mod tests;
