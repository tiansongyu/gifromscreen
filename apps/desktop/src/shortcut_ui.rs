//! Opt-in shortcut settings and one recorder-scoped registration slot.

use std::time::Duration;

use eframe::egui;
use gif_from_screen_capture_linux::{
    GlobalShortcutService, LinuxDisplayServer, ShortcutAction, ShortcutActionHandler,
    ShortcutBinding, ShortcutKey, ShortcutStatus, ShortcutTrigger, ShortcutUpdate,
    default_shortcut_bindings, validate_bindings,
};

mod persistence;
mod store;
mod view;

#[cfg(test)]
mod tests;

use persistence::SettingsIo;

#[derive(Clone, Debug, PartialEq)]
struct Settings {
    enabled: bool,
    bindings: Vec<ShortcutBinding>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            bindings: default_shortcut_bindings(),
        }
    }
}

impl Settings {
    fn validate(&self) -> Result<(), String> {
        if self.bindings.len() != 3 {
            return Err("Configure all three recorder actions.".into());
        }
        validate_bindings(&self.bindings)
    }
}

/// The narrow backend contract also permits tests without touching a real desktop.
trait Registration {
    fn stop(&self);
    fn set_action_handler(&self, handler: Option<ShortcutActionHandler>);
    fn is_running(&self) -> bool;
    fn poll(&mut self) -> ShortcutUpdate;
}

impl Registration for GlobalShortcutService {
    fn stop(&self) {
        Self::stop(self);
    }
    fn set_action_handler(&self, handler: Option<ShortcutActionHandler>) {
        Self::set_action_handler(self, handler);
    }
    fn is_running(&self) -> bool {
        Self::is_running(self)
    }
    fn poll(&mut self) -> ShortcutUpdate {
        Self::poll(self)
    }
}

type StartRegistration = Box<
    dyn FnMut(LinuxDisplayServer, Vec<ShortcutBinding>) -> Result<Box<dyn Registration>, String>,
>;

pub(crate) struct ShortcutTool {
    settings: Settings,
    draft_bindings: Vec<ShortcutBinding>,
    io: SettingsIo,
    slot: Option<Box<dyn Registration>>,
    recording_handler: Option<ShortcutActionHandler>,
    start: StartRegistration,
    status: ShortcutStatus,
    scope: bool,
    display: Option<LinuxDisplayServer>,
    generation: u64,
    slot_generation: u64,
    stopping: bool,
    attempted_generation: Option<u64>,
    shutdown_requested: bool,
    notice: Option<String>,
}

impl Default for ShortcutTool {
    fn default() -> Self {
        #[cfg(not(test))]
        let io = SettingsIo::from_environment();
        #[cfg(test)]
        let io = SettingsIo::without_store();
        Self::new(
            io,
            Box::new(|display, bindings| {
                GlobalShortcutService::start(display, bindings)
                    .map(|service| Box::new(service) as Box<dyn Registration>)
            }),
        )
    }
}

impl ShortcutTool {
    fn new(io: SettingsIo, start: StartRegistration) -> Self {
        let settings = Settings::default();
        Self {
            draft_bindings: settings.bindings.clone(),
            settings,
            io,
            slot: None,
            recording_handler: None,
            start,
            status: ShortcutStatus::Stopped,
            scope: false,
            display: None,
            generation: 0,
            slot_generation: 0,
            stopping: false,
            attempted_generation: None,
            shutdown_requested: false,
            notice: None,
        }
    }

    /// Call at a recording boundary even if the recorder page remains visible.
    /// Old callbacks are suppressed immediately; another worker waits for termination.
    pub(crate) fn reset_recording_scope(&mut self) {
        self.set_recording_handler(None);
        self.invalidate();
    }

    pub(crate) fn set_recording_handler(&mut self, handler: Option<ShortcutActionHandler>) {
        self.recording_handler.clone_from(&handler);
        if let Some(slot) = &self.slot {
            slot.set_action_handler(handler);
        }
    }

    /// The close handler keeps polling until old registrations and settings I/O finish.
    pub(crate) fn is_active(&self) -> bool {
        self.slot.as_ref().is_some_and(|slot| slot.is_running()) || self.io.has_pending_work()
    }

    pub(crate) fn shutdown(&mut self) {
        self.shutdown_requested = true;
        self.set_recording_handler(None);
        self.invalidate();
    }

    pub(crate) fn status_summary(&self) -> Option<String> {
        if !self.settings.enabled {
            return None;
        }
        Some(match &self.status {
            ShortcutStatus::Active(bindings) => {
                format!("Shortcuts: {}/3 registered", bindings.len())
            }
            ShortcutStatus::Registering => {
                "Shortcuts: registration pending; use recorder buttons".into()
            }
            ShortcutStatus::Stopping => "Shortcuts: stopping old registration; use buttons".into(),
            ShortcutStatus::Stopped => "Shortcuts: inactive; use recorder buttons".into(),
            ShortcutStatus::Failed(error) => format!(
                "Shortcuts unavailable: {}",
                error.chars().take(160).collect::<String>()
            ),
        })
    }

    /// Nonblocking: this never calls the recorder or waits for a permission response.
    pub(crate) fn poll(
        &mut self,
        context: &egui::Context,
        display: Option<LinuxDisplayServer>,
        in_recorder_scope: bool,
    ) -> Vec<ShortcutAction> {
        if let Some(settings) = self.io.poll(&self.settings) {
            self.draft_bindings = settings.bindings.clone();
            self.settings = settings;
            self.invalidate();
        }
        if self.scope != in_recorder_scope || self.display != display {
            if !in_recorder_scope || (self.display.is_some() && self.display != display) {
                self.set_recording_handler(None);
            }
            self.scope = in_recorder_scope;
            self.display = display;
            self.invalidate();
        }
        let allowed = self.settings.enabled
            && self.scope
            && self.display.is_some()
            && !self.shutdown_requested;
        if !allowed {
            self.stop_slot();
        }
        let mut actions = Vec::new();
        if let Some(slot) = &mut self.slot {
            let update = slot.poll();
            if allowed
                && !self.stopping
                && self.slot_generation == self.generation
                && matches!(update.status, ShortcutStatus::Active(_))
            {
                actions = update.actions;
            }
            if update.dropped_actions > 0 {
                self.notice = Some(format!(
                    "{} shortcut presses exceeded the bounded queue. Recorder buttons remain available.",
                    update.dropped_actions
                ));
            }
            self.status = update.status;
            if !slot.is_running() {
                self.slot = None;
                if self.slot_generation == self.generation {
                    self.attempted_generation = Some(self.generation);
                }
            }
        }
        if allowed && self.slot.is_none() && self.attempted_generation != Some(self.generation) {
            self.attempted_generation = Some(self.generation);
            if let Some(display) = self.display {
                match (self.start)(display, self.settings.bindings.clone()) {
                    Ok(slot) => {
                        slot.set_action_handler(self.recording_handler.clone());
                        self.slot = Some(slot);
                        self.slot_generation = self.generation;
                        self.stopping = false;
                        self.status = ShortcutStatus::Registering;
                    }
                    Err(error) => self.status = ShortcutStatus::Failed(error),
                }
            }
        }
        if self.slot.as_ref().is_some_and(|slot| slot.is_running()) || self.io.is_running() {
            context.request_repaint_after(Duration::from_millis(33));
        }
        actions
    }

    fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        let settings = Settings {
            enabled,
            bindings: if enabled {
                self.draft_bindings.clone()
            } else {
                self.settings.bindings.clone()
            },
        };
        self.accept(settings)
    }

    fn apply_bindings(&mut self) -> Result<(), String> {
        self.accept(Settings {
            enabled: self.settings.enabled,
            bindings: self.draft_bindings.clone(),
        })
    }

    fn accept(&mut self, settings: Settings) -> Result<(), String> {
        settings.validate()?;
        self.io.edited(true);
        if self.settings != settings {
            self.settings = settings;
            self.invalidate();
        }
        self.notice = None;
        Ok(())
    }

    fn retry(&mut self) {
        self.notice = None;
        self.invalidate();
    }

    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.stop_slot();
    }

    fn stop_slot(&mut self) {
        if let Some(slot) = &self.slot
            && !self.stopping
        {
            slot.stop();
            self.stopping = true;
            self.status = ShortcutStatus::Stopping;
        }
    }
}

impl Drop for ShortcutTool {
    fn drop(&mut self) {
        self.stop_slot();
    }
}
