use super::{
    Settings,
    store::{Snapshot, Store},
};
use crate::background_task::BackgroundTask;
use crate::ui_notice::Notice;
use gif_from_screen_localization::Message;

enum Stored {
    Loaded(Snapshot),
    Saved { revision: u64, snapshot: Snapshot },
}

pub(super) struct SettingsIo {
    store: Option<Store>,
    task: BackgroundTask<Stored, ()>,
    attempted_load: bool,
    baseline: Option<Snapshot>,
    touched: bool,
    revision: u64,
    saved_revision: u64,
    save_pending: bool,
    notice: Option<Notice>,
}

impl SettingsIo {
    pub fn new(store: Option<Store>) -> Self {
        Self {
            store,
            task: BackgroundTask::default(),
            attempted_load: false,
            baseline: None,
            touched: false,
            revision: 0,
            saved_revision: 0,
            save_pending: false,
            notice: None,
        }
    }

    #[cfg(not(test))]
    pub fn from_environment() -> Self {
        match Store::from_environment() {
            Ok(store) => Self::new(Some(store)),
            Err(error) => {
                let mut io = Self::new(None);
                io.notice = Some(Notice::new(
                    Message::ShortcutsSettingsUnavailable,
                    &[("error", &error)],
                ));
                io
            }
        }
    }

    #[cfg(test)]
    pub fn without_store() -> Self {
        Self::new(None)
    }

    pub fn edited(&mut self, save: bool) {
        self.touched = true;
        if save {
            self.revision = self.revision.wrapping_add(1);
            self.save_pending = true;
            if self.store.is_some() && self.baseline.is_some() {
                self.notice = Some(Message::ShortcutsSettingsSaving.into());
            }
        }
    }

    pub fn is_running(&self) -> bool {
        self.task.is_running()
    }
    pub fn has_pending_work(&self) -> bool {
        self.task.is_running()
            || (self.store.is_some()
                && (!self.attempted_load || (self.save_pending && self.baseline.is_some())))
    }
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }
    pub fn needs_reload(&self) -> bool {
        self.store.is_some()
            && self.attempted_load
            && self.baseline.is_none()
            && !self.task.is_running()
    }
    pub fn retry_load(&mut self) {
        if !self.task.is_running() {
            self.attempted_load = false;
            self.baseline = None;
            self.notice = None;
            self.save_pending = self.revision != self.saved_revision;
        }
    }

    pub fn poll(&mut self, current: &Settings) -> Option<Settings> {
        let mut loaded = None;
        if let Some(result) = self.task.poll() {
            match result {
                Ok(Stored::Loaded(snapshot)) => loaded = self.loaded(snapshot),
                Ok(Stored::Saved { revision, snapshot }) => {
                    self.baseline = Some(snapshot);
                    self.saved_revision = revision;
                    self.save_pending = revision != self.revision;
                    self.notice = Some(
                        if self.save_pending {
                            Message::ShortcutsSettingsSaving
                        } else {
                            Message::ShortcutsSettingsSaved
                        }
                        .into(),
                    );
                }
                Err(error) => {
                    self.notice = Some(Notice::new(
                        Message::ShortcutsSettingsIoFailed,
                        &[("error", &error)],
                    ));
                    self.save_pending = false;
                    self.baseline = None;
                }
            }
        }
        if !self.task.is_running()
            && let Some(store) = self.store.clone()
        {
            if !self.attempted_load {
                self.attempted_load = true;
                if let Err(error) = self.task.start("shortcut-settings-load", move |_| {
                    store.load().map(Stored::Loaded)
                }) {
                    self.notice = Some(Notice::new(
                        Message::ShortcutsSettingsLoadStartFailed,
                        &[("error", &error)],
                    ));
                }
            } else if self.save_pending
                && let Some(baseline) = self.baseline.clone()
            {
                let revision = self.revision;
                let settings = current.clone();
                if let Err(error) = self.task.start("shortcut-settings-save", move |_| {
                    store
                        .save(&baseline, settings)
                        .map(|snapshot| Stored::Saved { revision, snapshot })
                }) {
                    self.notice = Some(Notice::new(
                        Message::ShortcutsSettingsSaveStartFailed,
                        &[("error", &error)],
                    ));
                    self.save_pending = false;
                } else {
                    self.notice = Some(Message::ShortcutsSettingsSaving.into());
                }
            }
        }
        loaded
    }

    fn loaded(&mut self, snapshot: Snapshot) -> Option<Settings> {
        let settings = if self.touched {
            self.notice = Some(Message::ShortcutsSettingsNewerEditsKept.into());
            None
        } else {
            self.notice = None;
            Some(snapshot.settings.clone())
        };
        self.baseline = Some(snapshot);
        settings
    }
}

#[cfg(test)]
mod tests;
