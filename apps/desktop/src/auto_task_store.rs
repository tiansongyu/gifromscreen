//! Atomic, optimistic-concurrency protected application editing presets.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};

use fs2::FileExt;
use gif_from_screen_domain::{EditingTaskSettings, MAX_EDITING_SETTINGS_BYTES};

#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub config: EditingTaskSettings,
    bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub(super) struct AutoTaskStore {
    path: PathBuf,
}

impl AutoTaskStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<Snapshot, String> {
        if !self.path.is_absolute() {
            return Err(
                "Editing settings require an absolute path; no files were accessed.".to_owned(),
            );
        }
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Snapshot {
                    config: EditingTaskSettings::default(),
                    bytes: None,
                });
            }
            Err(error) => return Err(error.to_string()),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_EDITING_SETTINGS_BYTES as u64
        {
            return Err("Editing settings are not a regular file or exceed 256 KiB; the file was left unchanged.".to_owned());
        }
        let mut bytes = Vec::new();
        File::open(&self.path)
            .map_err(|e| e.to_string())?
            .take(MAX_EDITING_SETTINGS_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_EDITING_SETTINGS_BYTES {
            return Err("Editing settings exceed 256 KiB; the file was left unchanged.".to_owned());
        }
        let config: EditingTaskSettings = serde_json::from_slice(&bytes).map_err(|error| {
            format!("Invalid editing settings; the file was left unchanged: {error}")
        })?;
        config.validate()?;
        Ok(Snapshot {
            config,
            bytes: Some(bytes),
        })
    }

    pub fn save(
        &self,
        previous: &Snapshot,
        config: EditingTaskSettings,
    ) -> Result<Snapshot, String> {
        if !self.path.is_absolute() {
            return Err(
                "Editing settings require an absolute path; no files were accessed.".to_owned(),
            );
        }
        config.validate()?;
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or("Editing settings path needs a parent directory.")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let lock_path = self.path.with_extension("lock");
        if fs::symlink_metadata(&lock_path)
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
        {
            return Err(
                "Editing settings lock is not a regular file; it was left unchanged.".to_owned(),
            );
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|e| e.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|e| format!("Editing presets are being saved in another process: {e}"))?;
        if self.load()?.bytes != previous.bytes {
            return Err("Editing presets changed in another process. Reload before saving; no changes were overwritten.".to_owned());
        }
        let bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_EDITING_SETTINGS_BYTES {
            return Err("Formatted editing settings exceed 256 KiB.".to_owned());
        }
        let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        staged
            .write_all(&bytes)
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        if self.load()?.bytes != previous.bytes {
            return Err(
                "Editing presets changed externally; no changes were overwritten.".to_owned(),
            );
        }
        staged.persist(&self.path).map_err(|e| e.to_string())?;
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(Snapshot {
            config,
            bytes: Some(bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::{EditingTaskPreset, EditingTaskSources};

    #[test]
    fn presets_restart_and_stale_writers_do_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let store = AutoTaskStore::new(dir.path().join("settings/tasks.json"));
        let before = store.load().unwrap();
        let mut config = before.config.clone();
        config.presets.push(EditingTaskPreset {
            name: "Demo".into(),
            sources: EditingTaskSources::default(),
            tasks: vec![],
        });
        config.active_preset = Some("Demo".into());
        let saved = store.save(&before, config.clone()).unwrap();
        assert_eq!(store.load().unwrap().config, config);
        assert!(store.save(&before, EditingTaskSettings::default()).is_err());
        store.save(&saved, EditingTaskSettings::default()).unwrap();
        assert!(store.load().unwrap().config.presets.is_empty());
    }

    #[test]
    fn malformed_unknown_version_oversized_and_symlink_files_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let store = AutoTaskStore::new(path.clone());
        let before = store.load().unwrap();
        for bytes in [
            b"broken".to_vec(),
            br#"{"version":99,"enabled":false,"active_preset":null,"presets":[]}"#.to_vec(),
            vec![b' '; MAX_EDITING_SETTINGS_BYTES + 1],
        ] {
            fs::write(&path, &bytes).unwrap();
            assert!(store.load().is_err());
            assert!(store.save(&before, EditingTaskSettings::default()).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        let linked = dir.path().join("linked.json");
        std::os::unix::fs::symlink(&path, &linked).unwrap();
        assert!(AutoTaskStore::new(linked).load().is_err());
    }
}
