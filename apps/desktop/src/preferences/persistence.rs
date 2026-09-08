//! One asynchronous read/write at a time; newer UI choices survive older results.

use gif_from_screen_localization::Preferences;

use super::store::{Snapshot, Store};
use crate::background_task::BackgroundTask;

enum Completed {
    Loaded(Snapshot),
    Saved(Snapshot),
}

#[derive(Debug, Default, PartialEq)]
pub(super) enum Status {
    #[default]
    Idle,
    Unsaved,
    Loading,
    Saving,
    Saved,
    LoadFailed(String),
    SaveFailed(String),
}

#[derive(Default)]
pub(super) struct SettingsIo {
    store: Option<Store>,
    task: BackgroundTask<Completed, ()>,
    baseline: Option<Snapshot>,
    attempted_load: bool,
    edited: bool,
    save_pending: bool,
    status: Status,
}

impl SettingsIo {
    pub(super) fn new(store: Result<Store, String>) -> Self {
        match store {
            Ok(store) => Self {
                store: Some(store),
                ..Self::default()
            },
            Err(error) => Self {
                status: Status::LoadFailed(error),
                ..Self::default()
            },
        }
    }

    pub(super) fn status(&self) -> &Status {
        &self.status
    }

    pub(super) fn edited(&mut self) {
        self.edited = true;
        self.save_pending = true;
        if matches!(self.status, Status::Idle | Status::Saved) {
            self.status = Status::Unsaved;
        }
    }

    pub(super) fn has_pending_work(&self) -> bool {
        self.task.is_running()
            || self.store.is_some()
                && (!self.attempted_load || self.save_pending && self.baseline.is_some())
    }

    pub(super) fn can_reload(&self) -> bool {
        self.store.is_some() && !self.task.is_running()
    }

    /// Explicitly discard unsaved UI changes and read the latest file. A failed
    /// save never silently adopts a new baseline and overwrites another writer.
    pub(super) fn reload(&mut self) {
        if self.can_reload() {
            self.attempted_load = false;
            self.baseline = None;
            self.edited = false;
            self.save_pending = false;
            self.status = Status::Idle;
        }
    }

    pub(super) fn poll(&mut self, current: &Preferences) -> Option<Preferences> {
        let mut loaded = None;
        if let Some(result) = self.task.poll() {
            match result {
                Ok(Completed::Loaded(snapshot)) => {
                    if !self.edited {
                        loaded = Some(snapshot.preferences.clone());
                    }
                    self.baseline = Some(snapshot);
                    self.status = Status::Idle;
                }
                Ok(Completed::Saved(snapshot)) => {
                    self.save_pending = snapshot.preferences != *current;
                    self.baseline = Some(snapshot);
                    self.status = Status::Saved;
                }
                Err(error) => self.failed(error),
            }
        }
        if self.task.is_running() {
            return loaded;
        }
        let Some(store) = self.store.clone() else {
            return loaded;
        };
        if !self.attempted_load {
            self.attempted_load = true;
            self.status = Status::Loading;
            if let Err(error) = self.task.start("load-language-preferences", move |_| {
                store.load().map(Completed::Loaded)
            }) {
                self.failed(error);
            }
        } else if self.save_pending
            && let Some(baseline) = self.baseline.clone()
        {
            let preferences = current.clone();
            self.status = Status::Saving;
            if let Err(error) = self.task.start("save-language-preferences", move |_| {
                store.save(&baseline, preferences).map(Completed::Saved)
            }) {
                self.failed(error);
            }
        }
        loaded
    }

    fn failed(&mut self, error: String) {
        self.status = if self.status == Status::Loading {
            Status::LoadFailed(error)
        } else {
            Status::SaveFailed(error)
        };
        self.baseline = None;
        self.save_pending = false;
    }
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod tests;
