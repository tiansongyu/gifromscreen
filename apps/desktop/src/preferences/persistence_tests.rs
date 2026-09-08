use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

fn preference(tag: &str) -> Preferences {
    Preferences {
        language: gif_from_screen_localization::LanguagePreference::explicit(tag).unwrap(),
    }
}

fn settle(io: &mut SettingsIo, current: &mut Preferences) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(loaded) = io.poll(current) {
            *current = loaded;
        }
        if !io.has_pending_work() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "preferences worker did not finish"
        );
        std::thread::yield_now();
    }
}

#[test]
fn edit_during_initial_load_survives_and_is_saved() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("settings/preferences.json"));
    store
        .save(&store.load().unwrap(), preference("fr"))
        .unwrap();
    let mut io = SettingsIo::new(Ok(store.clone()));
    let mut current = Preferences::default();
    assert!(io.poll(&current).is_none());
    current = preference("zh");
    io.edited();
    settle(&mut io, &mut current);
    assert_eq!(current, preference("zh"));
    assert_eq!(store.load().unwrap().preferences, current);
    assert_eq!(io.status(), &Status::Saved);
}

#[test]
fn edit_during_save_is_saved_by_a_following_worker() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("settings/preferences.json"));
    let mut io = SettingsIo::new(Ok(store.clone()));
    let mut current = Preferences::default();
    settle(&mut io, &mut current);
    current = preference("zh");
    io.edited();
    io.poll(&current);
    assert!(io.has_pending_work());
    current = preference("de");
    io.edited();
    settle(&mut io, &mut current);
    assert_eq!(store.load().unwrap().preferences, current);
    assert_eq!(io.status(), &Status::Saved);
}

#[test]
fn fresh_instance_loads_saved_choice_without_rewriting_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("settings/preferences.json"));
    store
        .save(&store.load().unwrap(), preference("zh"))
        .unwrap();
    let before = std::fs::read(directory.path().join("settings/preferences.json")).unwrap();
    let mut io = SettingsIo::new(Ok(store));
    let mut current = Preferences::default();
    settle(&mut io, &mut current);
    assert_eq!(current, preference("zh"));
    assert_eq!(
        std::fs::read(directory.path().join("settings/preferences.json")).unwrap(),
        before
    );
}

#[test]
fn competing_writer_is_not_overwritten_and_reload_is_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("settings/preferences.json"));
    let mut io = SettingsIo::new(Ok(store.clone()));
    let mut current = Preferences::default();
    settle(&mut io, &mut current);
    store
        .save(&store.load().unwrap(), preference("fr"))
        .unwrap();
    current = preference("zh");
    io.edited();
    settle(&mut io, &mut current);
    assert!(matches!(io.status(), Status::SaveFailed(_)));
    assert_eq!(store.load().unwrap().preferences, preference("fr"));
    assert_eq!(current, preference("zh"));
    io.reload();
    settle(&mut io, &mut current);
    assert_eq!(current, preference("fr"));
}

#[test]
fn malformed_file_is_preserved_and_does_not_block_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preferences.json");
    std::fs::write(&path, b"not-json").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut io = SettingsIo::new(Ok(Store::new(path.clone())));
    let mut current = preference("zh");
    io.edited();
    settle(&mut io, &mut current);
    assert!(matches!(io.status(), Status::LoadFailed(_)));
    assert_eq!(current, preference("zh"));
    assert_eq!(std::fs::read(path).unwrap(), b"not-json");
}
