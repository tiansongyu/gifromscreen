use std::{
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

use gif_from_screen_domain::{AppliedEdit, AssetId, EditCommand, ProjectManifest, ProjectRevision};

use crate::{
    AssetStore, JournalRecord, JournalRecoveryReport, LockPolicy, ProjectError,
    atomic_file::{atomic_write, sync_directory},
    journal,
    lock::ProjectLock,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectLayout {
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub journal: PathBuf,
    pub lock: PathBuf,
    pub assets: PathBuf,
    pub thumbnails: PathBuf,
    pub previews: PathBuf,
}

impl ProjectLayout {
    pub fn at(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_owned();
        Self {
            manifest: root.join("manifest.json"),
            journal: root.join("journal.ndjson"),
            lock: root.join("project.lock"),
            assets: root.join("assets"),
            thumbnails: root.join("cache/thumbnails"),
            previews: root.join("cache/previews"),
            root,
        }
    }

    fn create_directories(&self) -> Result<(), ProjectError> {
        for directory in [&self.root, &self.assets, &self.thumbnails, &self.previews] {
            fs::create_dir_all(directory).map_err(|error| {
                ProjectError::io("create project layout directory", directory, error)
            })?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetCheck {
    PresenceAndLength,
    FullDigest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetIssue {
    Missing {
        asset_id: AssetId,
    },
    LengthMismatch {
        asset_id: AssetId,
        expected: u64,
        actual: u64,
    },
    DigestMismatch {
        asset_id: AssetId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitReceipt {
    pub from_revision: ProjectRevision,
    pub to_revision: ProjectRevision,
    pub inverse: EditCommand,
}

impl From<AppliedEdit> for CommitReceipt {
    fn from(value: AppliedEdit) -> Self {
        Self {
            from_revision: value.from_revision,
            to_revision: value.to_revision,
            inverse: value.inverse,
        }
    }
}

#[derive(Debug)]
pub struct OpenedProject {
    pub project: ActiveProject,
    pub journal_recovery: JournalRecoveryReport,
    pub asset_issues: Vec<AssetIssue>,
}

#[derive(Debug)]
pub struct ActiveProject {
    layout: ProjectLayout,
    manifest: ProjectManifest,
    assets: AssetStore,
    journal_requires_repair: bool,
    write_requires_recovery: bool,
    _lock: ProjectLock,
}

impl ActiveProject {
    pub fn create(root: impl AsRef<Path>, manifest: ProjectManifest) -> Result<Self, ProjectError> {
        manifest.validate()?;
        let layout = ProjectLayout::at(root);
        layout.create_directories()?;
        let lock = ProjectLock::acquire(&layout.root, LockPolicy::FailIfPresent)?;
        if layout.manifest.exists() {
            return Err(ProjectError::ManifestAlreadyExists(layout.manifest));
        }
        let assets = AssetStore::open(&layout.root)?;
        write_manifest(&layout.manifest, &manifest)?;
        atomic_write(&layout.journal, b"")?;
        Ok(Self {
            layout,
            manifest,
            assets,
            journal_requires_repair: false,
            write_requires_recovery: false,
            _lock: lock,
        })
    }

    pub fn open(
        root: impl AsRef<Path>,
        lock_policy: LockPolicy,
    ) -> Result<OpenedProject, ProjectError> {
        let layout = ProjectLayout::at(root);
        layout.create_directories()?;
        let lock = ProjectLock::acquire(&layout.root, lock_policy)?;
        if !layout.manifest.is_file() {
            return Err(ProjectError::MissingManifest(layout.manifest));
        }
        let snapshot = read_manifest(&layout.manifest)?;
        let recovered = journal::recover(snapshot, &layout.journal)?;
        let journal_requires_repair = !recovered.report.is_clean();
        let assets = AssetStore::open(&layout.root)?;
        let project = Self {
            layout,
            manifest: recovered.manifest,
            assets,
            journal_requires_repair,
            write_requires_recovery: false,
            _lock: lock,
        };
        let asset_issues = project.validate_assets(AssetCheck::PresenceAndLength)?;
        Ok(OpenedProject {
            project,
            journal_recovery: recovered.report,
            asset_issues,
        })
    }

    pub fn layout(&self) -> &ProjectLayout {
        &self.layout
    }

    pub fn manifest(&self) -> &ProjectManifest {
        &self.manifest
    }

    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }

    pub fn commit(&mut self, command: EditCommand) -> Result<CommitReceipt, ProjectError> {
        if self.write_requires_recovery {
            return Err(ProjectError::RequiresRecovery);
        }
        if self.journal_requires_repair {
            return Err(ProjectError::RequiresJournalRepair);
        }

        let mut candidate = self.manifest.clone();
        let applied = candidate.apply_command(&command)?;
        let record = JournalRecord::new(applied.from_revision, applied.to_revision, command)?;
        if let Err(source) = journal::append(&self.layout.journal, &record) {
            self.write_requires_recovery = true;
            return Err(ProjectError::JournalCommitFailed {
                revision: applied.to_revision,
                source: Box::new(source),
            });
        }
        self.manifest = candidate;
        Ok(applied.into())
    }

    /// Atomically persists the current in-memory revision. The journal remains
    /// intact, so reopening is safe if the process dies before later compaction.
    pub fn checkpoint(&self) -> Result<(), ProjectError> {
        if self.write_requires_recovery {
            return Err(ProjectError::RequiresRecovery);
        }
        self.manifest.validate()?;
        write_manifest(&self.layout.manifest, &self.manifest)
    }

    /// Checkpoints first and then atomically replaces the fully represented
    /// journal with an empty one.
    pub fn checkpoint_and_compact(&mut self) -> Result<(), ProjectError> {
        if self.journal_requires_repair {
            return Err(ProjectError::RequiresJournalRepair);
        }
        self.checkpoint()?;
        atomic_write(&self.layout.journal, b"")
    }

    /// Makes a recovered project writable without destroying forensic data.
    /// The invalid journal is moved aside after the recovered manifest has been
    /// checkpointed, and the returned path identifies that preserved copy.
    pub fn repair_journal(&mut self) -> Result<Option<PathBuf>, ProjectError> {
        if self.write_requires_recovery {
            return Err(ProjectError::RequiresRecovery);
        }
        if !self.journal_requires_repair {
            return Ok(None);
        }
        self.checkpoint()?;
        let preserved = next_rejected_journal_path(&self.layout, self.manifest.revision);
        if self.layout.journal.exists() {
            fs::rename(&self.layout.journal, &preserved).map_err(|error| {
                ProjectError::io("preserve rejected journal", &self.layout.journal, error)
            })?;
            sync_directory(&self.layout.root)?;
        }
        atomic_write(&self.layout.journal, b"")?;
        self.journal_requires_repair = false;
        Ok(Some(preserved))
    }

    pub fn validate_assets(&self, check: AssetCheck) -> Result<Vec<AssetIssue>, ProjectError> {
        let mut issues = Vec::new();
        for (asset_id, descriptor) in &self.manifest.assets {
            let path = self.assets.asset_path(*asset_id);
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    issues.push(AssetIssue::Missing {
                        asset_id: *asset_id,
                    });
                    continue;
                }
                Err(error) => return Err(ProjectError::io("inspect asset", path, error)),
            };
            if metadata.len() != descriptor.byte_len {
                issues.push(AssetIssue::LengthMismatch {
                    asset_id: *asset_id,
                    expected: descriptor.byte_len,
                    actual: metadata.len(),
                });
                continue;
            }
            if check == AssetCheck::FullDigest {
                match self.assets.verify(*asset_id) {
                    Ok(_) => {}
                    Err(ProjectError::CorruptAsset { .. }) => {
                        issues.push(AssetIssue::DigestMismatch {
                            asset_id: *asset_id,
                        });
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(issues)
    }
}

fn read_manifest(path: &Path) -> Result<ProjectManifest, ProjectError> {
    let file =
        File::open(path).map_err(|error| ProjectError::io("open project manifest", path, error))?;
    let manifest: ProjectManifest = serde_json::from_reader(BufReader::new(file))
        .map_err(|error| ProjectError::json("parse project manifest", path, error))?;
    manifest.validate()?;
    Ok(manifest)
}

fn write_manifest(path: &Path, manifest: &ProjectManifest) -> Result<(), ProjectError> {
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| ProjectError::json("serialize project manifest", path, error))?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn next_rejected_journal_path(layout: &ProjectLayout, revision: ProjectRevision) -> PathBuf {
    for sequence in 1..=u32::MAX {
        let path = layout.root.join(format!(
            "journal.rejected-r{}-{sequence}.ndjson",
            revision.get()
        ));
        if !path.exists() {
            return path;
        }
    }
    layout.root.join(format!(
        "journal.rejected-r{}-overflow.ndjson",
        revision.get()
    ))
}

#[cfg(test)]
mod tests {
    use std::{fs::OpenOptions, io::Write};

    use gif_from_screen_domain::{
        AssetDescriptor, AssetKind, Canvas, CanvasBackground, ColorSpace, EditCommand,
        PhysicalSize, ProjectId, ProjectManifest, RasterEncoding, UnixTimeMs,
    };
    use tempfile::tempdir;

    use super::*;

    fn manifest() -> ProjectManifest {
        ProjectManifest::new(
            ProjectId::from_u128(123),
            "test",
            UnixTimeMs::new(1),
            Canvas {
                size: PhysicalSize::new(2, 2).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap()
    }

    fn register(id: AssetId, len: u64) -> EditCommand {
        EditCommand::RegisterAsset {
            asset: AssetDescriptor {
                id,
                byte_len: len,
                kind: AssetKind::Frame {
                    size: PhysicalSize::new(1, 1).unwrap(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        }
    }

    #[test]
    fn journal_recovers_commit_newer_than_snapshot() {
        let directory = tempdir().unwrap();
        let mut active = ActiveProject::create(directory.path(), manifest()).unwrap();
        let bytes = b"four";
        let id = active.assets().put(bytes).unwrap();
        active.commit(register(id, bytes.len() as u64)).unwrap();
        assert_eq!(active.manifest().revision, ProjectRevision::new(1));
        drop(active);

        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(opened.project.manifest().revision, ProjectRevision::new(1));
        assert_eq!(opened.journal_recovery.replayed_records, 1);
        assert!(opened.asset_issues.is_empty());
    }

    #[test]
    fn invalid_tail_blocks_append_until_preserved_and_repaired() {
        let directory = tempdir().unwrap();
        let active = ActiveProject::create(directory.path(), manifest()).unwrap();
        let journal = active.layout().journal.clone();
        drop(active);
        let mut file = OpenOptions::new().append(true).open(&journal).unwrap();
        file.write_all(b"{torn").unwrap();
        file.sync_all().unwrap();

        let mut opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent)
            .unwrap()
            .project;
        let fake_id = AssetId::from_digest([9; 32]);
        assert!(matches!(
            opened.commit(register(fake_id, 4)),
            Err(ProjectError::RequiresJournalRepair)
        ));
        let preserved = opened.repair_journal().unwrap().unwrap();
        assert!(preserved.is_file());
        opened.commit(register(fake_id, 4)).unwrap();
    }

    #[test]
    fn checkpoint_compaction_does_not_reapply_commands() {
        let directory = tempdir().unwrap();
        let mut active = ActiveProject::create(directory.path(), manifest()).unwrap();
        let bytes = b"asset";
        let id = active.assets().put(bytes).unwrap();
        active.commit(register(id, bytes.len() as u64)).unwrap();
        active.checkpoint_and_compact().unwrap();
        drop(active);

        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(opened.project.manifest().revision, ProjectRevision::new(1));
        assert_eq!(opened.journal_recovery.replayed_records, 0);
        assert_eq!(
            fs::metadata(opened.project.layout().journal.clone())
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn second_writer_is_rejected() {
        let directory = tempdir().unwrap();
        let _active = ActiveProject::create(directory.path(), manifest()).unwrap();
        assert!(matches!(
            ActiveProject::open(directory.path(), LockPolicy::FailIfPresent),
            Err(ProjectError::AlreadyLocked { .. })
        ));
    }
}
