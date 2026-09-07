//! Small private, atomic settings snapshots with stale-writer protection.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::{Settings, ShortcutAction, ShortcutBinding, ShortcutKey, ShortcutTrigger};

const MAX_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub settings: Settings,
    bytes: Option<Vec<u8>>,
}

#[derive(Clone)]
pub(super) struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(not(test))]
    pub fn from_environment() -> Result<Self, String> {
        configuration_path(
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
        .map(Self::new)
    }

    pub fn load(&self) -> Result<Snapshot, String> {
        self.validate_path()?;
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Snapshot {
                    settings: Settings::default(),
                    bytes: None,
                });
            }
            Err(error) => return Err(error.to_string()),
        };
        check_file(&metadata)?;
        if metadata.len() > MAX_BYTES as u64 {
            return Err("Shortcut settings exceed 4 KiB; the file was left unchanged.".into());
        }
        let mut file = File::open(&self.path).map_err(|error| error.to_string())?;
        check_file(&file.metadata().map_err(|error| error.to_string())?)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("Shortcut settings exceed 4 KiB; the file was left unchanged.".into());
        }
        let wire: WireSettings = serde_json::from_slice(&bytes).map_err(|error| {
            format!("Invalid shortcut settings; the file was left unchanged: {error}")
        })?;
        let settings = wire.decode()?;
        Ok(Snapshot {
            settings,
            bytes: Some(bytes),
        })
    }

    pub fn save(&self, previous: &Snapshot, settings: Settings) -> Result<Snapshot, String> {
        self.validate_path()?;
        settings.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or("Shortcut settings need a parent directory.")?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent).map_err(|error| error.to_string())?;
        let metadata = fs::symlink_metadata(parent).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Shortcut settings directory must not be a symbolic link.".into());
        }
        let lock_path = self.path.with_extension("lock");
        match fs::symlink_metadata(&lock_path) {
            Ok(metadata) => check_file(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&lock_path)
            .map_err(|error| error.to_string())?;
        check_file(&lock.metadata().map_err(|error| error.to_string())?)?;
        lock.try_lock_exclusive().map_err(|error| {
            format!("Shortcut settings are being saved in another process: {error}")
        })?;
        self.require_unchanged(previous)?;
        let bytes = serde_json::to_vec_pretty(&WireSettings::encode(&settings))
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("Formatted shortcut settings exceed 4 KiB.".into());
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| error.to_string())?;
        self.require_unchanged(previous)?;
        temporary
            .persist(&self.path)
            .map_err(|error| error.to_string())?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?;
        Ok(Snapshot {
            settings,
            bytes: Some(bytes),
        })
    }

    fn require_unchanged(&self, previous: &Snapshot) -> Result<(), String> {
        if self.load()?.bytes != previous.bytes {
            return Err("Shortcut settings changed externally. Reload before saving; nothing was overwritten.".into());
        }
        Ok(())
    }

    fn validate_path(&self) -> Result<(), String> {
        if !self.path.is_absolute() || self.path.file_name().is_none() {
            return Err(
                "Shortcut settings require an absolute file path; no file was accessed.".into(),
            );
        }
        Ok(())
    }
}

fn check_file(metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Shortcut settings and lock must be regular files, not symbolic links.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Shortcut settings and lock must be owner-only (0600). Existing permissions were not changed.".into());
        }
    }
    Ok(())
}

fn configuration_path(xdg: Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf, String> {
    let base = match xdg {
        Some(path) if path.as_os_str().is_empty() => None,
        Some(path) if path.is_absolute() => Some(path),
        Some(_) => {
            return Err(
                "XDG_CONFIG_HOME must be absolute; shortcut settings remain in memory.".into(),
            );
        }
        None => None,
    }
    .or_else(|| {
        home.filter(|path| path.is_absolute())
            .map(|path| path.join(".config"))
    })
    .ok_or(
        "No absolute configuration directory is available; shortcut settings remain in memory.",
    )?;
    Ok(base.join("gifromscreen/shortcuts.json"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSettings {
    format_version: u8,
    enabled: bool,
    bindings: Vec<WireBinding>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireBinding {
    action: String,
    key: String,
    modifiers: Vec<String>,
}

impl WireSettings {
    fn encode(settings: &Settings) -> Self {
        Self {
            format_version: 1,
            enabled: settings.enabled,
            bindings: settings
                .bindings
                .iter()
                .map(|binding| {
                    let trigger = binding.trigger;
                    let modifiers = [
                        (trigger.control, "control"),
                        (trigger.shift, "shift"),
                        (trigger.alt, "alt"),
                        (trigger.super_key, "super"),
                    ]
                    .into_iter()
                    .filter(|(enabled, _)| *enabled)
                    .map(|(_, name)| name.to_owned())
                    .collect();
                    WireBinding {
                        action: binding.action.id().into(),
                        key: match trigger.key {
                            ShortcutKey::Function(number) => format!("F{number}"),
                            ShortcutKey::Character(key) => key.to_string(),
                        },
                        modifiers,
                    }
                })
                .collect(),
        }
    }

    fn decode(self) -> Result<Settings, String> {
        if self.format_version != 1 || self.bindings.len() != 3 {
            return Err(
                "Shortcut settings need format version 1 and exactly three bindings.".into(),
            );
        }
        let mut bindings = self
            .bindings
            .into_iter()
            .map(WireBinding::decode)
            .collect::<Result<Vec<_>, _>>()?;
        bindings.sort_by_key(|binding| binding.action);
        let settings = Settings {
            enabled: self.enabled,
            bindings,
        };
        settings.validate()?;
        Ok(settings)
    }
}

impl WireBinding {
    fn decode(self) -> Result<ShortcutBinding, String> {
        let action = match self.action.as_str() {
            "start-pause" => ShortcutAction::StartPause,
            "stop" => ShortcutAction::Stop,
            "snapshot" => ShortcutAction::Snapshot,
            _ => return Err("Unknown recorder shortcut action.".into()),
        };
        let key = if let Some(number) = self
            .key
            .strip_prefix('F')
            .filter(|_| self.key.len() <= 3 && self.key.len() > 1)
        {
            ShortcutKey::Function(
                number
                    .parse()
                    .map_err(|_| "Invalid shortcut function key.")?,
            )
        } else if self.key.len() == 1 {
            ShortcutKey::Character(self.key.chars().next().ok_or("Missing shortcut key.")?)
        } else {
            return Err("Invalid recorder shortcut key.".into());
        };
        let mut trigger = ShortcutTrigger {
            key,
            control: false,
            shift: false,
            alt: false,
            super_key: false,
        };
        for modifier in self.modifiers {
            let flag = match modifier.as_str() {
                "control" => &mut trigger.control,
                "shift" => &mut trigger.shift,
                "alt" => &mut trigger.alt,
                "super" => &mut trigger.super_key,
                _ => return Err("Unknown shortcut modifier.".into()),
            };
            if *flag {
                return Err("Duplicate shortcut modifier.".into());
            }
            *flag = true;
        }
        trigger.validate()?;
        Ok(ShortcutBinding { action, trigger })
    }
}

#[cfg(test)]
mod tests;
