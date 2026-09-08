//! Private language preferences; all I/O runs on the caller's background worker.

use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use fs2::FileExt;
use gif_from_screen_localization::Preferences;
use rustix::fs::{self, AtFlags, Mode, OFlags};

const MAX_BYTES: usize = 4 * 1024;
const PRIVATE_FILE: Mode = Mode::from_raw_mode(0o600);
const PRIVATE_DIR: Mode = Mode::from_raw_mode(0o700);
const OPEN_FILE: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const OPEN_DIR: OFlags = OPEN_FILE.union(OFlags::DIRECTORY);

#[derive(Clone, Debug)]
pub(super) struct Snapshot {
    pub preferences: Preferences,
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

    pub fn from_environment() -> Result<Self, String> {
        configuration_path(
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
        .map(Self::new)
    }

    pub fn load(&self) -> Result<Snapshot, String> {
        let (parent, name) = self.location()?;
        let Some(directory) = open_directory(parent, false)? else {
            return Ok(empty_snapshot());
        };
        read_snapshot(&directory, name)
    }

    pub fn save(&self, previous: &Snapshot, preferences: Preferences) -> Result<Snapshot, String> {
        let bytes = serde_json::to_vec_pretty(&preferences).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BYTES {
            return Err("Language preferences exceed 4 KiB.".into());
        }
        let (parent, name) = self.location()?;
        let directory =
            open_directory(parent, true)?.ok_or("Language preferences directory disappeared.")?;
        let lock_name = self.path.with_extension("lock");
        let lock_name = lock_name
            .file_name()
            .ok_or("Invalid preferences lock name.")?;
        if lock_name == name {
            return Err("Preferences and lock paths must be different.".into());
        }
        let lock = acquire_lock(&directory, lock_name)?;
        check_previous(&directory, name, previous)?;
        let mut temporary = PendingFile::create(&directory)?;
        temporary
            .file
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        temporary
            .file
            .sync_all()
            .map_err(|error| error.to_string())?;

        // The lock serializes cooperating writers. Compare raw bytes, including
        // formatting, once more before commit; never replace a corrupt revision.
        check_previous(&directory, name, previous)?;
        check_link(&directory, lock_name, &lock)?;
        let current = open_directory(parent, false)?
            .ok_or("Language preferences directory disappeared before commit.")?;
        if !same_file(&metadata(&directory)?, &metadata(&current)?) {
            return Err("Language preferences directory changed before commit.".into());
        }
        temporary.publish(name, previous.bytes.is_none())?;
        // A failure here is after the atomic commit. Rolling back could overwrite
        // a newer writer, so report this durability failure without a false claim
        // that the old bytes remain on disk.
        directory.sync_all().map_err(|error| {
            format!("Preferences were replaced, but directory synchronization failed: {error}")
        })?;
        Ok(Snapshot {
            preferences,
            bytes: Some(bytes),
        })
    }

    fn location(&self) -> Result<(&Path, &OsStr), String> {
        if !self.path.is_absolute()
            || self
                .path
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        {
            return Err(
                "Language preferences require an absolute path without parent components.".into(),
            );
        }
        let name = self
            .path
            .file_name()
            .ok_or("Language preferences require a file name.")?;
        let parent = self
            .path
            .parent()
            .ok_or("Language preferences require a parent directory.")?;
        Ok((parent, name))
    }
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        preferences: Preferences::default(),
        bytes: None,
    }
}

fn configuration_path(xdg: Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf, String> {
    let base = if let Some(path) = xdg.filter(|path| !path.as_os_str().is_empty()) {
        if !path.is_absolute() {
            return Err("XDG_CONFIG_HOME must be an absolute directory.".into());
        }
        path
    } else {
        let home = home
            .filter(|path| path.is_absolute())
            .ok_or("An absolute XDG_CONFIG_HOME or HOME is required for language preferences.")?;
        home.join(".config")
    };
    Ok(base.join("gifromscreen/preferences.json"))
}

/// Pin every ancestor without following symlinks. Only the final application
/// directory must be private: system ancestors such as /home may be shared.
fn open_directory(path: &Path, create: bool) -> Result<Option<File>, String> {
    let mut directory =
        File::from(fs::open("/", OPEN_DIR, Mode::empty()).map_err(|error| error.to_string())?);
    for part in path.components() {
        let Component::Normal(name) = part else {
            if part == Component::RootDir {
                continue;
            }
            return Err("Invalid language preferences directory component.".into());
        };
        let next = match fs::openat(&directory, name, OPEN_DIR, Mode::empty()) {
            Ok(next) => next,
            Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
            Err(rustix::io::Errno::NOENT) => {
                match fs::mkdirat(&directory, name, PRIVATE_DIR) {
                    Ok(()) => directory.sync_all().map_err(|error| error.to_string())?,
                    Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.to_string()),
                }
                fs::openat(&directory, name, OPEN_DIR, Mode::empty())
                    .map_err(|error| error.to_string())?
            }
            Err(error) => return Err(error.to_string()),
        };
        directory = File::from(next);
    }
    check_private(
        &metadata(&directory)?,
        true,
        rustix::process::geteuid().as_raw(),
    )?;
    Ok(Some(directory))
}

fn metadata(file: &File) -> Result<Metadata, String> {
    file.metadata().map_err(|error| error.to_string())
}

fn check_private(metadata: &Metadata, directory: bool, owner: u32) -> Result<(), String> {
    if if directory {
        !metadata.is_dir()
    } else {
        !metadata.is_file()
    } {
        return Err("Preferences storage must use regular files and directories.".into());
    }
    if metadata.uid() != owner {
        return Err("Preferences storage belongs to another user.".into());
    }
    if metadata.mode() & 0o077 != 0 {
        return Err("Preferences storage must be accessible only to its owner.".into());
    }
    if !directory && metadata.nlink() != 1 {
        return Err("Preferences files must not have hard links.".into());
    }
    Ok(())
}

fn check_file(file: &File) -> Result<Metadata, String> {
    let value = metadata(file)?;
    check_private(&value, false, rustix::process::geteuid().as_raw())?;
    if value.len() > MAX_BYTES as u64 {
        return Err("Language preferences exceed 4 KiB.".into());
    }
    Ok(value)
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn check_link(directory: &File, name: &OsStr, file: &File) -> Result<(), String> {
    let linked = fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| error.to_string())?;
    let opened = check_file(file)?;
    if linked.st_dev != opened.dev() || linked.st_ino != opened.ino() || linked.st_nlink != 1 {
        return Err("Preferences file identity changed during access.".into());
    }
    Ok(())
}

fn read_snapshot(directory: &File, name: &OsStr) -> Result<Snapshot, String> {
    let mut file = match fs::openat(directory, name, OPEN_FILE, Mode::empty()) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(empty_snapshot()),
        Err(error) => return Err(error.to_string()),
    };
    check_file(&file)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("Language preferences exceed 4 KiB.".into());
    }
    check_link(directory, name, &file)?;
    let preferences = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid language preferences: {error}"))?;
    Ok(Snapshot {
        preferences,
        bytes: Some(bytes),
    })
}

fn acquire_lock(directory: &File, name: &OsStr) -> Result<File, String> {
    let flags =
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let lock = File::from(
        fs::openat(directory, name, flags, PRIVATE_FILE).map_err(|error| error.to_string())?,
    );
    check_file(&lock)?;
    lock.try_lock_exclusive()
        .map_err(|error| format!("Preferences are locked: {error}"))?;
    check_link(directory, name, &lock)?;
    Ok(lock)
}

fn check_previous(directory: &File, name: &OsStr, previous: &Snapshot) -> Result<(), String> {
    if read_snapshot(directory, name)?.bytes != previous.bytes {
        return Err("Language preferences changed on disk; reload before saving.".into());
    }
    Ok(())
}

/// Created and removed relative to the pinned directory, never via an ancestor
/// path that another process could redirect between validation and publication.
struct PendingFile<'a> {
    directory: &'a File,
    name: OsString,
    file: File,
    published: bool,
}

impl<'a> PendingFile<'a> {
    fn create(directory: &'a File) -> Result<Self, String> {
        let name = OsString::from(format!(".preferences-{}.tmp", uuid::Uuid::new_v4()));
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = File::from(
            fs::openat(directory, &name, flags, PRIVATE_FILE).map_err(|error| error.to_string())?,
        );
        Ok(Self {
            directory,
            name,
            file,
            published: false,
        })
    }

    fn publish(&mut self, name: &OsStr, absent: bool) -> Result<(), String> {
        check_link(self.directory, &self.name, &self.file)?;
        if absent {
            fs::renameat_with(
                self.directory,
                &self.name,
                self.directory,
                name,
                fs::RenameFlags::NOREPLACE,
            )
        } else {
            fs::renameat(self.directory, &self.name, self.directory, name)
        }
        .map_err(|error| error.to_string())?;
        self.published = true;
        Ok(())
    }
}

impl Drop for PendingFile<'_> {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::unlinkat(self.directory, &self.name, AtFlags::empty());
        }
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
