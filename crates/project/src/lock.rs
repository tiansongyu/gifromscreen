use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{ProjectError, atomic_file::sync_directory};

static LOCK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockPolicy {
    FailIfPresent,
    /// Explicitly preserve the previous lock as a `.stale-*` file and acquire
    /// a new one. The caller is responsible for deciding that the owner died.
    TakeOver,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LockInfo {
    pub format_version: u32,
    pub process_id: u32,
    pub owner_token: String,
    pub opened_at_unix_ms: u128,
}

#[derive(Debug)]
pub(crate) struct ProjectLock {
    path: PathBuf,
    owner_token: String,
}

impl ProjectLock {
    pub(crate) fn acquire(root: &Path, policy: LockPolicy) -> Result<Self, ProjectError> {
        fs::create_dir_all(root)
            .map_err(|error| ProjectError::io("create project directory", root, error))?;
        let path = root.join("project.lock");
        match Self::create(&path) {
            Ok(lock) => Ok(lock),
            Err(ProjectError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                match policy {
                    LockPolicy::FailIfPresent => Err(already_locked(&path)),
                    LockPolicy::TakeOver => {
                        preserve_stale_lock(&path)?;
                        Self::create(&path).map_err(|error| match error {
                            ProjectError::Io { source, .. }
                                if source.kind() == std::io::ErrorKind::AlreadyExists =>
                            {
                                already_locked(&path)
                            }
                            other => other,
                        })
                    }
                }
            }
            Err(error) => Err(error),
        }
    }

    fn create(path: &Path) -> Result<Self, ProjectError> {
        let sequence = LOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let opened_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let owner_token = format!("{}-{opened_at_unix_ms}-{sequence}", std::process::id());
        let info = LockInfo {
            format_version: 1,
            process_id: std::process::id(),
            owner_token: owner_token.clone(),
            opened_at_unix_ms,
        };
        let bytes = serde_json::to_vec(&info)
            .map_err(|error| ProjectError::json("serialize project lock", path, error))?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|error| ProjectError::io("acquire project lock", path, error))?;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(path);
            return Err(ProjectError::io("write project lock", path, error));
        }
        if let Some(parent) = path.parent()
            && let Err(error) = sync_directory(parent)
        {
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(Self {
            path: path.to_owned(),
            owner_token,
        })
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let owned = fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<LockInfo>(&bytes).ok())
            .is_some_and(|info| info.owner_token == self.owner_token);
        if owned {
            let _ = fs::remove_file(&self.path);
            if let Some(parent) = self.path.parent() {
                let _ = sync_directory(parent);
            }
        }
    }
}

fn already_locked(path: &Path) -> ProjectError {
    let owner = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LockInfo>(&bytes).ok())
        .map(|info| format!("pid {} ({})", info.process_id, info.owner_token));
    ProjectError::AlreadyLocked {
        path: path.to_owned(),
        owner,
    }
}

fn preserve_stale_lock(path: &Path) -> Result<(), ProjectError> {
    for sequence in 1..=u32::MAX {
        let candidate = path.with_file_name(format!("project.lock.stale-{sequence}"));
        if !candidate.exists() {
            fs::rename(path, &candidate)
                .map_err(|error| ProjectError::io("preserve stale project lock", path, error))?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            return Ok(());
        }
    }
    Err(ProjectError::io(
        "preserve stale project lock",
        path,
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no stale lock suffix available",
        ),
    ))
}
