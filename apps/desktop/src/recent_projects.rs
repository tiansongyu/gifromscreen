//! Bounded recent-project metadata with crash-released advisory locking.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_ENTRIES: usize = 20;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_PATH_BYTES: usize = 2048;

#[derive(Clone, Debug)]
pub(crate) struct RecentProjectStore {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) enum RecentProjectEdit {
    Remember(PathBuf),
    Remove(PathBuf),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    version: u16,
    projects: Vec<PathBuf>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            version: 1,
            projects: Vec::new(),
        }
    }
}

impl RecentProjectStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn load(&self) -> Result<Vec<PathBuf>, String> {
        self.read().map(|(history, _)| history.projects)
    }

    pub(crate) fn update(&self, edits: &[RecentProjectEdit]) -> Result<Vec<PathBuf>, String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "History path has no parent directory".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let lock_path = self.path.with_extension("lock");
        if fs::symlink_metadata(&lock_path)
            .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err(
                "History lock path is not a regular file; it was left unchanged".to_owned(),
            );
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| error.to_string())?;
        lock.try_lock_exclusive().map_err(|error| {
            format!("Recent projects are busy in another process; retry later: {error}")
        })?;
        let (mut history, previous) = self.read()?;
        for edit in edits {
            match edit {
                RecentProjectEdit::Remember(path) => {
                    let canonical = fs::canonicalize(path).map_err(|error| {
                        format!("Cannot remember project {}: {error}", path.display())
                    })?;
                    validate_project_path(&canonical)?;
                    if !canonical.join("manifest.json").is_file() {
                        return Err("Recent project is missing manifest.json".to_owned());
                    }
                    history.projects.retain(|existing| existing != &canonical);
                    history.projects.insert(0, canonical);
                    history.projects.truncate(MAX_ENTRIES);
                }
                RecentProjectEdit::Remove(path) => {
                    history.projects.retain(|existing| existing != path);
                }
            }
        }
        validate_history(&history)?;
        let bytes = serde_json::to_vec_pretty(&history).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("Recent project history exceeds 64 KiB".to_owned());
        }
        let mut staged =
            tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
        staged
            .write_all(&bytes)
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|error| error.to_string())?;
        let (_, current) = self.read()?;
        if current != previous {
            return Err(
                "Recent project history changed externally; it was not overwritten".to_owned(),
            );
        }
        staged
            .persist(&self.path)
            .map_err(|error| error.to_string())?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?;
        drop(lock);
        Ok(history.projects)
    }

    fn read(&self) -> Result<(History, Option<Vec<u8>>), String> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((History::default(), None));
            }
            Err(error) => return Err(error.to_string()),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_FILE_BYTES
        {
            return Err(format!(
                "History {} is not a regular file or exceeds 64 KiB; it was left unchanged",
                self.path.display()
            ));
        }
        let mut bytes = Vec::new();
        File::open(&self.path)
            .map_err(|error| error.to_string())?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(
                "Recent project history grew beyond 64 KiB; it was left unchanged".to_owned(),
            );
        }
        let history: History = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "History {} is damaged or unsupported; it was left unchanged: {error}",
                self.path.display()
            )
        })?;
        validate_history(&history)?;
        Ok((history, Some(bytes)))
    }
}

#[cfg(not(test))]
pub(crate) fn default_history_path() -> Result<PathBuf, String> {
    state_path(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn state_path(
    xdg_state_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, String> {
    let state = xdg_state_home.map(PathBuf::from).filter(|path| path.is_absolute()).or_else(|| home.map(PathBuf::from).filter(|path| path.is_absolute()).map(|path| path.join(".local/state")))
        .ok_or_else(|| "No absolute XDG_STATE_HOME or HOME is available; recent-project persistence is disabled".to_owned())?;
    Ok(state.join("gifromscreen/recent-projects.json"))
}

fn validate_project_path(path: &Path) -> Result<(), String> {
    let text = path
        .to_str()
        .ok_or_else(|| "Recent project paths must be valid UTF-8".to_owned())?;
    if !path.is_absolute() || text.len() > MAX_PATH_BYTES || text.chars().any(char::is_control) {
        return Err("Recent project paths must be absolute UTF-8, at most 2048 bytes, without control characters".to_owned());
    }
    Ok(())
}

fn validate_history(history: &History) -> Result<(), String> {
    if history.version != 1 || history.projects.len() > MAX_ENTRIES {
        return Err(
            "Recent project history version/count is unsupported; file was left unchanged"
                .to_owned(),
        );
    }
    let mut unique = BTreeSet::new();
    for path in &history.projects {
        validate_project_path(path)?;
        if !unique.insert(path) {
            return Err(
                "Recent project history contains duplicate paths; file was left unchanged"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &Path, number: usize) -> PathBuf {
        let path = root.join(format!("project-{number}.gfsproj"));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("manifest.json"), "{}").unwrap();
        path
    }

    #[test]
    fn history_deduplicates_bounds_order_and_removes_missing_paths_without_deleting_projects() {
        let directory = tempfile::tempdir().unwrap();
        let store = RecentProjectStore::new(directory.path().join("state/recent.json"));
        assert!(store.load().unwrap().is_empty());
        assert!(!directory.path().join("state").exists());
        let paths = (0..25)
            .map(|index| project(directory.path(), index))
            .collect::<Vec<_>>();
        let edits = paths
            .iter()
            .cloned()
            .map(RecentProjectEdit::Remember)
            .collect::<Vec<_>>();
        let recent = store.update(&edits).unwrap();
        assert_eq!(recent.len(), 20);
        assert_eq!(recent[0], paths[24]);
        let recent = store
            .update(&[RecentProjectEdit::Remember(paths[20].clone())])
            .unwrap();
        assert_eq!(recent[0], paths[20]);
        assert_eq!(store.load().unwrap(), recent);
        fs::remove_file(paths[20].join("manifest.json")).unwrap();
        let recent = store
            .update(&[RecentProjectEdit::Remove(paths[20].clone())])
            .unwrap();
        assert_eq!(recent.len(), 19);
        assert!(paths[20].exists());
    }

    #[test]
    fn malformed_oversized_unknown_and_symlink_history_are_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recent.json");
        let store = RecentProjectStore::new(path.clone());
        let project = project(directory.path(), 1);
        for original in [
            b"{broken".to_vec(),
            b"{\"version\":999,\"projects\":[]}".to_vec(),
            vec![b' '; usize::try_from(MAX_FILE_BYTES).unwrap() + 1],
        ] {
            fs::write(&path, &original).unwrap();
            assert!(store.load().is_err());
            assert!(
                store
                    .update(&[RecentProjectEdit::Remember(project.clone())])
                    .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), original);
        }
        #[cfg(unix)]
        {
            fs::remove_file(&path).unwrap();
            let original = directory.path().join("original.json");
            fs::write(&original, b"{\"version\":1,\"projects\":[]}").unwrap();
            std::os::unix::fs::symlink(&original, &path).unwrap();
            assert!(
                store
                    .update(&[RecentProjectEdit::Remember(project)])
                    .is_err()
            );
            assert_eq!(
                fs::read(&original).unwrap(),
                b"{\"version\":1,\"projects\":[]}"
            );
        }
    }

    #[test]
    fn advisory_lock_releases_on_close_and_prevents_lost_cross_process_updates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recent.json");
        let store = RecentProjectStore::new(path.clone());
        let first = project(directory.path(), 1);
        let second = project(directory.path(), 2);
        store
            .update(&[RecentProjectEdit::Remember(first.clone())])
            .unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        assert!(
            store
                .update(&[RecentProjectEdit::Remember(second.clone())])
                .is_err()
        );
        assert_eq!(store.load().unwrap(), std::slice::from_ref(&first));
        drop(lock);
        assert_eq!(
            store
                .update(&[RecentProjectEdit::Remember(second.clone())])
                .unwrap(),
            [second, first]
        );
    }

    #[test]
    fn xdg_state_paths_ignore_relative_overrides_and_never_read_or_write_real_user_state() {
        use std::ffi::OsStr;
        assert_eq!(
            state_path(Some(OsStr::new("/state")), Some(OsStr::new("/users/demo"))).unwrap(),
            PathBuf::from("/state/gifromscreen/recent-projects.json")
        );
        assert_eq!(
            state_path(
                Some(OsStr::new("relative")),
                Some(OsStr::new("/users/demo"))
            )
            .unwrap(),
            PathBuf::from("/users/demo/.local/state/gifromscreen/recent-projects.json")
        );
        assert!(state_path(None, None).is_err());
    }
}
