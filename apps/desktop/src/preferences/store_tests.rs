use std::{
    fs::{self as stdfs, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
};

use gif_from_screen_localization::LanguagePreference;

use super::*;

fn explicit(tag: &str) -> Preferences {
    Preferences {
        language: LanguagePreference::explicit(tag).unwrap(),
    }
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn fixture() -> (tempfile::TempDir, Store) {
    let directory = private_directory();
    let store = Store::new(directory.path().join("preferences.json"));
    (directory, store)
}

fn private_directory() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    stdfs::set_permissions(directory.path(), stdfs::Permissions::from_mode(0o700)).unwrap();
    directory
}

#[test]
fn missing_load_is_read_only_and_save_creates_private_directories() {
    let directory = private_directory();
    let parent = directory.path().join("config/gifromscreen");
    let store = Store::new(parent.join("preferences.json"));
    let missing = store.load().unwrap();
    assert_eq!(missing.preferences, Preferences::default());
    assert!(missing.bytes.is_none());
    assert!(!parent.exists());
    let saved = store.save(&missing, explicit("zh-CN")).unwrap();
    assert_eq!(store.load().unwrap().preferences, saved.preferences);
    for path in [&parent, &store.path, &store.path.with_extension("lock")] {
        let metadata = stdfs::metadata(path).unwrap();
        assert_eq!(metadata.mode() & 0o077, 0);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    }
}

#[test]
fn roundtrip_preserves_unavailable_tag_and_system_choice() {
    let (_directory, store) = fixture();
    let first = store.load().unwrap();
    let desired = explicit("qaa-Qaaa-x-Custom");
    let saved = store.save(&first, desired.clone()).unwrap();
    assert_eq!(store.load().unwrap().preferences, desired);
    assert_eq!(
        serde_json::from_slice::<Preferences>(&stdfs::read(&store.path).unwrap()).unwrap(),
        desired
    );
    let system = store.save(&saved, Preferences::default()).unwrap();
    assert_eq!(store.load().unwrap().preferences, system.preferences);
    assert_eq!(system.preferences, Preferences::default());
}

#[test]
fn stale_writers_and_external_reformatting_cannot_overwrite_new_bytes() {
    let (_directory, store) = fixture();
    let first = store.load().unwrap();
    let second = store.load().unwrap();
    let saved = store.save(&first, explicit("zh-CN")).unwrap();
    let original = stdfs::read(&store.path).unwrap();
    assert!(
        store
            .save(&second, explicit("en"))
            .unwrap_err()
            .contains("changed")
    );
    assert_eq!(stdfs::read(&store.path).unwrap(), original);
    let reformatted = serde_json::to_vec(&saved.preferences).unwrap();
    assert_ne!(reformatted, original);
    stdfs::write(&store.path, &reformatted).unwrap();
    assert!(store.save(&saved, explicit("en")).is_err());
    assert_eq!(stdfs::read(&store.path).unwrap(), reformatted);
}

#[test]
fn invalid_and_oversized_revisions_are_never_replaced() {
    let cases: Vec<Vec<u8>> = vec![
        b"not json".to_vec(),
        br#"{"format_version":2,"language":{"mode":"system"}}"#.to_vec(),
        br#"{"format_version":1,"language":{"mode":"system"},"future":true}"#.to_vec(),
        br#"{"format_version":1,"language":{"mode":"explicit","tag":"bad tag"}}"#.to_vec(),
        vec![b' '; MAX_BYTES + 1],
    ];
    for bytes in cases {
        let (_directory, store) = fixture();
        let absent = store.load().unwrap();
        write_private(&store.path, &bytes);
        assert!(store.load().is_err());
        assert!(store.save(&absent, explicit("en")).is_err());
        assert_eq!(stdfs::read(&store.path).unwrap(), bytes);
    }
}

#[test]
fn exact_four_kib_valid_json_is_allowed_but_one_more_byte_is_not() {
    let (_directory, store) = fixture();
    let mut bytes = serde_json::to_vec(&Preferences::default()).unwrap();
    bytes.resize(MAX_BYTES, b' ');
    write_private(&store.path, &bytes);
    assert_eq!(store.load().unwrap().preferences, Preferences::default());
    bytes.push(b' ');
    stdfs::write(&store.path, &bytes).unwrap();
    assert!(store.load().is_err());
    assert_eq!(stdfs::read(&store.path).unwrap(), bytes);
}

#[test]
fn settings_and_lock_symlinks_never_touch_the_target() {
    for lock in [false, true] {
        let (directory, store) = fixture();
        let absent = store.load().unwrap();
        let target = directory.path().join("unrelated");
        let bytes = serde_json::to_vec(&Preferences::default()).unwrap();
        write_private(&target, &bytes);
        let link = if lock {
            store.path.with_extension("lock")
        } else {
            store.path.clone()
        };
        symlink(&target, &link).unwrap();
        if !lock {
            assert!(store.load().is_err());
        }
        assert!(store.save(&absent, explicit("en")).is_err());
        assert_eq!(stdfs::read(&target).unwrap(), bytes);
        assert!(
            stdfs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn parent_and_ancestor_symlinks_are_rejected_without_creating_files() {
    for nested in [false, true] {
        let directory = private_directory();
        let real = directory.path().join("real");
        stdfs::create_dir(&real).unwrap();
        stdfs::set_permissions(&real, stdfs::Permissions::from_mode(0o700)).unwrap();
        let link = directory.path().join("link");
        symlink(&real, &link).unwrap();
        let path = if nested {
            link.join("nested/preferences.json")
        } else {
            link.join("preferences.json")
        };
        let store = Store::new(path);
        assert!(store.load().is_err());
        assert!(store.save(&empty_snapshot(), explicit("en")).is_err());
        assert_eq!(stdfs::read_dir(&real).unwrap().count(), 0);
    }
}

#[test]
fn hard_linked_settings_and_lock_are_rejected() {
    for lock in [false, true] {
        let (directory, store) = fixture();
        let absent = store.load().unwrap();
        let original = directory.path().join("other");
        let bytes = serde_json::to_vec(&Preferences::default()).unwrap();
        write_private(&original, &bytes);
        let link = if lock {
            store.path.with_extension("lock")
        } else {
            store.path.clone()
        };
        stdfs::hard_link(&original, &link).unwrap();
        if !lock {
            assert!(store.load().is_err());
        }
        assert!(store.save(&absent, explicit("en")).is_err());
        assert_eq!(stdfs::read(&original).unwrap(), bytes);
        assert_eq!(stdfs::metadata(&original).unwrap().nlink(), 2);
    }
}

#[test]
fn public_permissions_and_foreign_owners_are_rejected_without_chmod() {
    let (directory, store) = fixture();
    let saved = store.save(&store.load().unwrap(), explicit("en")).unwrap();
    let bytes = stdfs::read(&store.path).unwrap();
    for path in [
        &store.path,
        &store.path.with_extension("lock"),
        directory.path(),
    ] {
        let original_mode = stdfs::metadata(path).unwrap().mode() & 0o777;
        stdfs::set_permissions(path, stdfs::Permissions::from_mode(original_mode | 0o040)).unwrap();
        assert!(store.save(&saved, explicit("zh-CN")).is_err());
        assert_eq!(
            stdfs::metadata(path).unwrap().mode() & 0o777,
            original_mode | 0o040
        );
        assert_eq!(stdfs::read(&store.path).unwrap(), bytes);
        stdfs::set_permissions(path, stdfs::Permissions::from_mode(original_mode)).unwrap();
    }
    // Exercise the actual ownership predicate without requiring chown privileges.
    let file = stdfs::metadata(&store.path).unwrap();
    assert!(
        check_private(&file, false, file.uid() ^ 1)
            .unwrap_err()
            .contains("another user")
    );
    let parent = stdfs::metadata(directory.path()).unwrap();
    assert!(check_private(&parent, true, parent.uid() ^ 1).is_err());
}

#[test]
fn lock_contention_is_try_only_and_preserves_current_revision() {
    let (_directory, store) = fixture();
    let saved = store.save(&store.load().unwrap(), explicit("en")).unwrap();
    let bytes = stdfs::read(&store.path).unwrap();
    let lock = File::open(store.path.with_extension("lock")).unwrap();
    lock.try_lock_exclusive().unwrap();
    assert!(
        store
            .save(&saved, explicit("zh-CN"))
            .unwrap_err()
            .contains("locked")
    );
    assert_eq!(stdfs::read(&store.path).unwrap(), bytes);
    drop(lock);
    assert_eq!(
        store.save(&saved, explicit("zh-CN")).unwrap().preferences,
        explicit("zh-CN")
    );
}

#[test]
fn replaced_lock_identity_is_rejected() {
    let (directory, store) = fixture();
    let handle = open_directory(directory.path(), false).unwrap().unwrap();
    let name = OsStr::new("preferences.lock");
    let lock = acquire_lock(&handle, name).unwrap();
    stdfs::rename(
        store.path.with_extension("lock"),
        directory.path().join("old.lock"),
    )
    .unwrap();
    write_private(&store.path.with_extension("lock"), &[]);
    assert!(check_link(&handle, name, &lock).is_err());
}

#[test]
fn fifo_settings_and_lock_fail_without_waiting_for_another_process() {
    for lock in [false, true] {
        let (_directory, store) = fixture();
        let absent = store.load().unwrap();
        let path = if lock {
            store.path.with_extension("lock")
        } else {
            store.path.clone()
        };
        fs::mknodat(fs::CWD, &path, fs::FileType::Fifo, PRIVATE_FILE, 0).unwrap();
        if !lock {
            assert!(store.load().is_err());
        }
        assert!(store.save(&absent, explicit("en")).is_err());
        assert!(!stdfs::metadata(path).unwrap().is_file());
    }
}

#[test]
fn aborted_temporary_and_no_replace_conflict_leave_original_bytes() {
    let (directory, store) = fixture();
    let handle = open_directory(directory.path(), false).unwrap().unwrap();
    let bytes = serde_json::to_vec(&Preferences::default()).unwrap();
    {
        let mut temporary = PendingFile::create(&handle).unwrap();
        temporary.file.write_all(b"new bytes").unwrap();
        write_private(&store.path, &bytes);
        assert!(
            temporary
                .publish(OsStr::new("preferences.json"), true)
                .is_err()
        );
    }
    assert_eq!(stdfs::read(&store.path).unwrap(), bytes);
    assert_eq!(stdfs::read_dir(directory.path()).unwrap().count(), 1);
    {
        let _cancelled_before_commit = PendingFile::create(&handle).unwrap();
    }
    assert_eq!(stdfs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn injected_config_paths_obey_xdg_without_using_host_environment() {
    assert_eq!(
        configuration_path(Some("/config".into()), Some("/home/test".into())).unwrap(),
        PathBuf::from("/config/gifromscreen/preferences.json")
    );
    assert_eq!(
        configuration_path(Some(PathBuf::new()), Some("/home/test".into())).unwrap(),
        PathBuf::from("/home/test/.config/gifromscreen/preferences.json")
    );
    assert_eq!(
        configuration_path(None, Some("/home/test".into())).unwrap(),
        PathBuf::from("/home/test/.config/gifromscreen/preferences.json")
    );
    assert!(configuration_path(Some("relative".into()), Some("/home/test".into())).is_err());
    assert!(configuration_path(None, None).is_err());
    assert!(configuration_path(None, Some("relative".into())).is_err());
    for path in ["relative/preferences.json", "/tmp/../preferences.json", "/"] {
        let store = Store::new(path.into());
        assert!(store.load().is_err());
        assert!(
            store
                .save(&empty_snapshot(), Preferences::default())
                .is_err()
        );
    }
}
