use super::*;
use std::time::{Duration, Instant};

fn wait(io: &mut SettingsIo, current: &Settings) -> Option<Settings> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut loaded = None;
    loop {
        if let Some(settings) = io.poll(current) {
            loaded = Some(settings);
        }
        if !io.has_pending_work() {
            return loaded;
        }
        assert!(
            Instant::now() < deadline,
            "settings worker did not terminate"
        );
        std::thread::yield_now();
    }
}

#[test]
fn save_and_reopen_preserve_enabled_preference_without_starting_any_service() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("settings/shortcuts.json"));
    let mut io = SettingsIo::new(Some(store.clone()));
    assert_eq!(
        wait(&mut io, &Settings::default()),
        Some(Settings::default())
    );
    let settings = Settings {
        enabled: true,
        ..Settings::default()
    };
    io.edited(true);
    wait(&mut io, &settings);
    let mut reopened = SettingsIo::new(Some(store));
    assert_eq!(wait(&mut reopened, &Settings::default()), Some(settings));
}

#[test]
fn late_load_keeps_unsaved_draft_edits_and_does_not_overwrite_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("shortcuts.json"));
    let settings = Settings {
        enabled: true,
        ..Settings::default()
    };
    let snapshot = store
        .save(&store.load().unwrap(), settings.clone())
        .unwrap();
    let mut io = SettingsIo::new(Some(store.clone()));
    let (release, receive) = std::sync::mpsc::channel();
    io.attempted_load = true;
    io.task
        .start("blocked-shortcut-load", move |_| {
            receive.recv().map_err(|error| error.to_string())?;
            Ok(Stored::Loaded(snapshot))
        })
        .unwrap();
    io.edited(false);
    assert!(io.poll(&Settings::default()).is_none());
    release.send(()).unwrap();
    assert!(wait(&mut io, &Settings::default()).is_none());
    assert!(io.notice().unwrap().contains("newer edits"));
    assert_eq!(store.load().unwrap().settings, settings);
}

#[test]
fn edits_during_inflight_save_are_written_after_the_first_snapshot_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("shortcuts.json"));
    let mut io = SettingsIo::new(Some(store.clone()));
    wait(&mut io, &Settings::default());
    let first = Settings {
        enabled: true,
        ..Settings::default()
    };
    io.edited(true);
    let revision = io.revision;
    let baseline = io.baseline.clone().unwrap();
    let (release, receive) = std::sync::mpsc::channel();
    let writer = store.clone();
    io.task
        .start("blocked-shortcut-save", move |_| {
            receive.recv().map_err(|error| error.to_string())?;
            writer
                .save(&baseline, first)
                .map(|snapshot| Stored::Saved { revision, snapshot })
        })
        .unwrap();
    let mut latest = Settings::default();
    latest.bindings[0].trigger.key = super::super::ShortcutKey::Function(12);
    io.edited(true);
    release.send(()).unwrap();
    wait(&mut io, &latest);
    assert_eq!(store.load().unwrap().settings, latest);
}

#[test]
fn corrupt_load_finishes_once_without_overwriting_or_retrying_each_frame() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shortcuts.json");
    let store = Store::new(path.clone());
    store
        .save(&store.load().unwrap(), Settings::default())
        .unwrap();
    std::fs::write(&path, b"broken").unwrap();
    let mut io = SettingsIo::new(Some(store));
    assert!(wait(&mut io, &Settings::default()).is_none());
    assert!(io.needs_reload());
    for _ in 0..5 {
        assert!(io.poll(&Settings::default()).is_none());
        assert!(!io.is_running());
    }
    assert_eq!(std::fs::read(path).unwrap(), b"broken");
}
