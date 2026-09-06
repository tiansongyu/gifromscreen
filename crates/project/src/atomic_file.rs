use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::ProjectError;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    let parent = path.parent().ok_or_else(|| {
        ProjectError::io(
            "resolve parent of",
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"),
        )
    })?;
    crate::private_fs::create_dir_all(parent)
        .map_err(|error| ProjectError::io("create directory", parent, error))?;

    let (mut file, temp_path) = create_temporary_file(path)?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| ProjectError::io("write atomic temporary file", &temp_path, error))?;
        file.sync_all()
            .map_err(|error| ProjectError::io("sync atomic temporary file", &temp_path, error))?;
        drop(file);
        fs::rename(&temp_path, path)
            .map_err(|error| ProjectError::io("commit atomic file", path, error))?;
        sync_directory(parent)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn create_temporary_file(target: &Path) -> Result<(File, PathBuf), ProjectError> {
    loop {
        let path = unique_temp_path(target);
        match crate::private_fs::file_options()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ProjectError::io(
                    "create atomic temporary file",
                    path,
                    error,
                ));
            }
        }
    }
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), ProjectError> {
    let directory = File::open(path)
        .map_err(|error| ProjectError::io("open directory for sync", path, error))?;
    directory
        .sync_all()
        .map_err(|error| ProjectError::io("sync directory", path, error))
}

fn unique_temp_path(target: &Path) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    target.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ))
}
