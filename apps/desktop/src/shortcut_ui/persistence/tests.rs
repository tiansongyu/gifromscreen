use super::*;
use std::time::{Duration, Instant};

fn localizer(tag: &str) -> gif_from_screen_localization::Localizer {
    gif_from_screen_localization::Localizer::new(
        gif_from_screen_localization::find_language(tag).unwrap(),
    )
}

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
    assert_eq!(
        io.notice().unwrap().message_id(),
        Some(Message::ShortcutsSettingsSaved)
    );
    for language in [localizer("en"), localizer("zh"), localizer("en")] {
        assert_eq!(
            io.notice().unwrap().render(language),
            language.text(Message::ShortcutsSettingsSaved)
        );
    }
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
    assert!(
        io.notice()
            .unwrap()
            .render(localizer("en"))
            .contains("newer edits")
    );
    assert_eq!(
        io.notice().unwrap().render(localizer("zh")),
        localizer("zh").text(Message::ShortcutsSettingsNewerEditsKept)
    );
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
    let deadline = Instant::now() + Duration::from_secs(3);
    while io.saved_revision != revision {
        io.poll(&latest);
        assert!(
            Instant::now() < deadline,
            "first save acknowledgement did not arrive"
        );
        std::thread::yield_now();
    }
    assert_ne!(io.saved_revision, io.revision);
    assert!(io.save_pending);
    assert_eq!(
        io.notice().unwrap().message_id(),
        Some(Message::ShortcutsSettingsSaving)
    );
    assert_eq!(
        io.notice().unwrap().render(localizer("zh")),
        localizer("zh").text(Message::ShortcutsSettingsSaving)
    );
    wait(&mut io, &latest);
    assert_eq!(store.load().unwrap().settings, latest);
    assert_eq!(
        io.notice().unwrap().message_id(),
        Some(Message::ShortcutsSettingsSaved)
    );
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
    let notice = io.notice().unwrap();
    assert_eq!(
        notice.message_id(),
        Some(Message::ShortcutsSettingsIoFailed)
    );
    assert_ne!(
        notice.render(localizer("zh")),
        notice.render(localizer("en"))
    );
}

#[test]
fn new_edit_immediately_invalidates_saved_notice_but_storeless_edits_do_not_claim_saving() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("shortcuts.json"));
    let mut io = SettingsIo::new(Some(store.clone()));
    wait(&mut io, &Settings::default());
    io.edited(true);
    wait(&mut io, &Settings::default());
    assert_eq!(
        io.notice().unwrap().message_id(),
        Some(Message::ShortcutsSettingsSaved)
    );
    io.edited(true);
    assert_eq!(
        io.notice().unwrap().message_id(),
        Some(Message::ShortcutsSettingsSaving)
    );
    wait(&mut io, &Settings::default());
    assert_eq!(store.load().unwrap().settings, Settings::default());

    let mut memory_only = SettingsIo::without_store();
    memory_only.edited(true);
    memory_only.poll(&Settings::default());
    assert!(memory_only.notice().is_none());
    assert!(!memory_only.has_pending_work());
}

#[test]
fn worker_failure_notice_switches_language_and_preserves_raw_error_arguments() {
    let mut io = SettingsIo::without_store();
    let raw = "permission denied: /tmp/设置 {error} Ctrl+F7";
    io.task
        .start("shortcut-test-error", move |_| Err(raw.into()))
        .unwrap();
    wait(&mut io, &Settings::default());
    assert!(!io.is_running());
    assert!(!io.save_pending);
    for language in [localizer("en"), localizer("zh")] {
        assert_eq!(
            io.notice().unwrap().render(language),
            language
                .format(Message::ShortcutsSettingsIoFailed, &[("error", raw)])
                .unwrap()
        );
    }
}
