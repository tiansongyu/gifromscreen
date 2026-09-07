use super::*;
use std::path::Path;

#[test]
fn private_settings_roundtrip_and_stale_writer_preserve_external_changes() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("private/shortcuts.json"));
    let before = store.load().unwrap();
    assert!(!before.settings.enabled);
    assert!(!store.path.exists());
    let mut settings = Settings {
        enabled: true,
        ..Settings::default()
    };
    settings.bindings[0].trigger.key = ShortcutKey::Function(12);
    let saved = store.save(&before, settings.clone()).unwrap();
    assert_eq!(store.load().unwrap().settings, settings);
    assert!(fs::metadata(&store.path).unwrap().len() <= MAX_BYTES as u64);
    assert!(store.save(&before, Settings::default()).is_err());
    assert_eq!(store.load().unwrap().settings, settings);
    store.save(&saved, Settings::default()).unwrap();
    assert!(!store.load().unwrap().settings.enabled);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&store.path, &store.path.with_extension("lock")] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            fs::metadata(store.path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn corrupt_oversized_unknown_and_duplicate_settings_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("shortcuts.json"));
    let empty = store.load().unwrap();
    let before = store.save(&empty, Settings::default()).unwrap();
    let valid = serde_json::to_value(WireSettings::encode(&Settings::default())).unwrap();
    let mut future = valid.clone();
    future["format_version"] = serde_json::json!(99);
    let mut unknown = valid.clone();
    unknown["unknown"] = serde_json::json!(true);
    let mut duplicate = valid.clone();
    duplicate["bindings"][1] = duplicate["bindings"][0].clone();
    let mut typing = valid.clone();
    typing["bindings"][0]["key"] = serde_json::json!("A");
    typing["bindings"][0]["modifiers"] = serde_json::json!(["shift"]);
    let mut modifiers = valid.clone();
    modifiers["bindings"][0]["modifiers"] = serde_json::json!(["control", "control"]);
    let mut oversized_list = valid;
    oversized_list["bindings"] = serde_json::json!([]);
    let mut bad = vec![b"broken".to_vec(), vec![b' '; MAX_BYTES + 1]];
    bad.extend(
        [
            future,
            unknown,
            duplicate,
            typing,
            modifiers,
            oversized_list,
        ]
        .iter()
        .map(|value| serde_json::to_vec(value).unwrap()),
    );
    for bytes in bad {
        fs::write(&store.path, &bytes).unwrap(); // Retains the existing private file mode.
        assert!(store.load().is_err());
        assert!(store.save(&before, Settings::default()).is_err());
        assert_eq!(fs::read(&store.path).unwrap(), bytes);
    }
}

#[cfg(unix)]
#[test]
fn symlinks_and_public_files_are_rejected_without_chmod_or_overwrite() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let store = Store::new(directory.path().join("private.json"));
    store
        .save(&store.load().unwrap(), Settings::default())
        .unwrap();
    let linked = directory.path().join("linked.json");
    symlink(&store.path, &linked).unwrap();
    assert!(Store::new(linked).load().is_err());
    let bytes = fs::read(&store.path).unwrap();
    fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load().unwrap_err().contains("owner-only"));
    assert_eq!(
        fs::metadata(&store.path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(fs::read(&store.path).unwrap(), bytes);
}

#[test]
fn configuration_paths_do_not_depend_on_cwd_or_accept_relative_environment_roots() {
    assert_eq!(
        configuration_path(Some("/config".into()), Some("/user".into())).unwrap(),
        Path::new("/config/gifromscreen/shortcuts.json")
    );
    assert_eq!(
        configuration_path(None, Some("/user".into())).unwrap(),
        Path::new("/user/.config/gifromscreen/shortcuts.json")
    );
    assert!(configuration_path(Some("relative".into()), Some("/user".into())).is_err());
    assert!(configuration_path(None, Some("relative".into())).is_err());
    assert!(configuration_path(None, None).is_err());
    assert!(Store::new("relative.json".into()).load().is_err());
}
