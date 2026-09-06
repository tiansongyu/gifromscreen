use std::{
    collections::HashMap,
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

use gif_from_screen_domain::{
    AppliedEdit, AssetDescriptor, AssetId, DomainError, EditCommand, FrameClip,
    FrameDurationChange, FrameId, ProjectManifest, ProjectRevision, RasterEncoding,
};

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
            crate::private_fs::create_dir_all(directory).map_err(|error| {
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
    frame_positions: HashMap<FrameId, usize>,
    timeline_duration_us: u64,
    _lock: ProjectLock,
}

struct TimelineIndex {
    frame_positions: HashMap<FrameId, usize>,
    duration_us: u64,
}

impl TimelineIndex {
    fn build(manifest: &ProjectManifest) -> Result<Self, ProjectError> {
        let requested = manifest.timeline.frames.len();
        let mut frame_positions = HashMap::new();
        frame_positions
            .try_reserve(requested)
            .map_err(|_| ProjectError::RecordingIndexAllocationFailed { requested })?;
        let mut duration_us = 0_u64;
        for (index, frame) in manifest.timeline.frames.iter().enumerate() {
            frame_positions.insert(frame.id, index);
            duration_us = duration_us
                .checked_add(frame.duration.get())
                .ok_or_else(|| {
                    ProjectError::InvalidRecordingMutation(
                        "validated timeline duration unexpectedly overflowed".to_owned(),
                    )
                })?;
        }
        Ok(Self {
            frame_positions,
            duration_us,
        })
    }
}

impl ActiveProject {
    pub fn create(root: impl AsRef<Path>, manifest: ProjectManifest) -> Result<Self, ProjectError> {
        manifest.validate()?;
        let timeline_index = TimelineIndex::build(&manifest)?;
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
            frame_positions: timeline_index.frame_positions,
            timeline_duration_us: timeline_index.duration_us,
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
        let timeline_index = TimelineIndex::build(&recovered.manifest)?;
        let assets = AssetStore::open(&layout.root)?;
        let project = Self {
            layout,
            manifest: recovered.manifest,
            assets,
            journal_requires_repair,
            write_requires_recovery: false,
            frame_positions: timeline_index.frame_positions,
            timeline_duration_us: timeline_index.duration_us,
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

    /// Appends one raw, canvas-sized RGBA recording frame without cloning or
    /// revalidating the complete existing timeline.
    ///
    /// The emitted journal command is byte-for-byte compatible with the normal
    /// `RegisterAsset + InsertFrames` command path and remains recoverable by
    /// the generic journal replayer. All fallible validation, cache growth,
    /// record serialization, and durable journal append finish before the
    /// in-memory manifest changes.
    ///
    /// # Errors
    ///
    /// Returns an error for a read-only/recovery-required project, duplicate or
    /// nil frame identity, incompatible asset descriptor, duration/revision
    /// overflow, cache allocation failure, or journal failure.
    pub fn commit_recording_append(
        &mut self,
        new_asset: Option<AssetDescriptor>,
        frame: FrameClip,
    ) -> Result<CommitReceipt, ProjectError> {
        self.commit_recording_append_with_cursor(new_asset, None, frame)
    }

    /// Atomically registers a changed cursor shape alongside a newly captured frame.
    /// This retains the indexed recording fast path even for animated cursor shapes.
    ///
    /// # Errors
    /// Returns recording validation, descriptor, allocation, or journal errors.
    pub fn commit_recording_append_with_cursor(
        &mut self,
        new_asset: Option<AssetDescriptor>,
        new_cursor: Option<AssetDescriptor>,
        frame: FrameClip,
    ) -> Result<CommitReceipt, ProjectError> {
        self.validate_recording_append(new_asset.as_ref(), new_cursor.as_ref(), &frame)?;
        let next_duration = self
            .timeline_duration_us
            .checked_add(frame.duration.get())
            .ok_or_else(|| {
                ProjectError::InvalidRecordingMutation(
                    "recording timeline duration overflowed u64".to_owned(),
                )
            })?;
        let from_revision = self.manifest.revision;
        let to_revision = from_revision.next().ok_or(DomainError::RevisionOverflow)?;
        let insertion_index = self.manifest.timeline.frames.len();
        self.frame_positions.try_reserve(1).map_err(|_| {
            ProjectError::RecordingIndexAllocationFailed {
                requested: insertion_index.saturating_add(1),
            }
        })?;
        self.manifest.timeline.frames.try_reserve(1).map_err(|_| {
            ProjectError::RecordingIndexAllocationFailed {
                requested: insertion_index.saturating_add(1),
            }
        })?;

        let assets = [new_asset, new_cursor];
        let mut commands = Vec::with_capacity(3);
        for asset in assets.iter().flatten() {
            commands.push(EditCommand::RegisterAsset {
                asset: asset.clone(),
            });
        }
        commands.push(EditCommand::InsertFrames {
            index: insertion_index,
            frames: vec![frame.clone()],
        });
        let command = EditCommand::Compound { commands };
        let mut inverse_commands = vec![EditCommand::RemoveFrames {
            frame_ids: vec![frame.id],
        }];
        for asset in assets.iter().flatten() {
            inverse_commands.push(EditCommand::UnregisterAsset { asset_id: asset.id });
        }
        let inverse = EditCommand::Compound {
            commands: inverse_commands,
        };
        let record = JournalRecord::new(from_revision, to_revision, command)?;
        self.append_record(&record, to_revision)?;

        for asset in assets.into_iter().flatten() {
            self.manifest.assets.insert(asset.id, asset);
        }
        self.frame_positions.insert(frame.id, insertion_index);
        self.timeline_duration_us = next_duration;
        self.manifest.timeline.frames.push(frame);
        self.manifest.revision = to_revision;
        Ok(CommitReceipt {
            from_revision,
            to_revision,
            inverse,
        })
    }

    fn validate_recording_append(
        &self,
        new_asset: Option<&AssetDescriptor>,
        new_cursor: Option<&AssetDescriptor>,
        frame: &FrameClip,
    ) -> Result<(), ProjectError> {
        self.ensure_writable()?;
        if frame.id.is_nil() {
            return Err(ProjectError::InvalidRecordingMutation(
                "recording frame identity must not be nil".to_owned(),
            ));
        }
        if self.frame_positions.contains_key(&frame.id) {
            return Err(DomainError::DuplicateFrameId(frame.id).into());
        }
        if frame.transform != Default::default() || !frame.effects.is_empty() {
            return Err(ProjectError::InvalidRecordingMutation(
                "fast recording append requires an untransformed frame without effects".to_owned(),
            ));
        }
        let descriptor = match new_asset {
            Some(descriptor) => {
                if descriptor.id != frame.asset_id {
                    return Err(ProjectError::InvalidRecordingMutation(
                        "new asset identity differs from the appended frame asset".to_owned(),
                    ));
                }
                if self.manifest.assets.contains_key(&descriptor.id) {
                    return Err(DomainError::DuplicateAssetId(descriptor.id).into());
                }
                descriptor
            }
            None => self
                .manifest
                .assets
                .get(&frame.asset_id)
                .ok_or(DomainError::UnknownAsset(frame.asset_id))?,
        };
        validate_raw_recording_asset(&self.manifest, descriptor)?;
        if let Some(cursor) = new_cursor {
            let valid_raster = cursor
                .kind
                .raster_descriptor()
                .is_some_and(|(size, encoding)| {
                    encoding == RasterEncoding::Rgba8
                        && size.validate().is_ok()
                        && size.area().and_then(|area| area.checked_mul(4)) == Some(cursor.byte_len)
                });
            if frame.capture_metadata.cursor_asset != Some(cursor.id)
                || cursor.id == frame.asset_id
                || !valid_raster
            {
                return Err(ProjectError::InvalidRecordingMutation(
                    "new cursor must be a distinct valid RGBA asset referenced by the frame"
                        .to_owned(),
                ));
            }
            if self.manifest.assets.contains_key(&cursor.id) {
                return Err(DomainError::DuplicateAssetId(cursor.id).into());
            }
        }
        if let Some(cursor_id) = frame.capture_metadata.cursor_asset {
            let cursor = if let Some(cursor) = new_cursor {
                cursor
            } else if cursor_id == descriptor.id {
                descriptor
            } else {
                self.manifest
                    .assets
                    .get(&cursor_id)
                    .ok_or(DomainError::UnknownAsset(cursor_id))?
            };
            if cursor.kind.raster_descriptor().is_none() {
                return Err(ProjectError::InvalidRecordingMutation(
                    "recording cursor asset must be a raster".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Updates one recording-frame duration through an indexed journal fast path.
    ///
    /// Projects with overlays or transitions fall back to the fully validating
    /// generic commit because changing total time can affect their invariants.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown frame, revision/duration overflow,
    /// recovery-required state, or journal failure.
    pub fn commit_recording_duration(
        &mut self,
        frame_id: FrameId,
        duration: gif_from_screen_domain::DurationUs,
    ) -> Result<CommitReceipt, ProjectError> {
        self.ensure_writable()?;
        if !self.manifest.timeline.transitions.is_empty()
            || !self.manifest.timeline.overlay_tracks.is_empty()
        {
            return self.commit(EditCommand::SetFrameDurations {
                changes: vec![FrameDurationChange { frame_id, duration }],
            });
        }
        let index = *self
            .frame_positions
            .get(&frame_id)
            .ok_or(DomainError::UnknownFrame(frame_id))?;
        let previous = self.manifest.timeline.frames[index].duration;
        let next_duration = self
            .timeline_duration_us
            .checked_sub(previous.get())
            .and_then(|total| total.checked_add(duration.get()))
            .ok_or_else(|| {
                ProjectError::InvalidRecordingMutation(
                    "recording duration update overflowed the timeline".to_owned(),
                )
            })?;
        let from_revision = self.manifest.revision;
        let to_revision = from_revision.next().ok_or(DomainError::RevisionOverflow)?;
        let command = EditCommand::SetFrameDurations {
            changes: vec![FrameDurationChange { frame_id, duration }],
        };
        let inverse = EditCommand::SetFrameDurations {
            changes: vec![FrameDurationChange {
                frame_id,
                duration: previous,
            }],
        };
        let record = JournalRecord::new(from_revision, to_revision, command)?;
        self.append_record(&record, to_revision)?;

        self.manifest.timeline.frames[index].duration = duration;
        self.timeline_duration_us = next_duration;
        self.manifest.revision = to_revision;
        Ok(CommitReceipt {
            from_revision,
            to_revision,
            inverse,
        })
    }

    pub fn commit(&mut self, command: EditCommand) -> Result<CommitReceipt, ProjectError> {
        self.ensure_writable()?;

        let mut candidate = self.manifest.clone();
        let applied = candidate.apply_command(&command)?;
        let timeline_index = TimelineIndex::build(&candidate)?;
        let record = JournalRecord::new(applied.from_revision, applied.to_revision, command)?;
        self.append_record(&record, applied.to_revision)?;
        self.manifest = candidate;
        self.frame_positions = timeline_index.frame_positions;
        self.timeline_duration_us = timeline_index.duration_us;
        Ok(applied.into())
    }

    fn ensure_writable(&self) -> Result<(), ProjectError> {
        if self.write_requires_recovery {
            return Err(ProjectError::RequiresRecovery);
        }
        if self.journal_requires_repair {
            return Err(ProjectError::RequiresJournalRepair);
        }
        Ok(())
    }

    fn append_record(
        &mut self,
        record: &JournalRecord,
        revision: ProjectRevision,
    ) -> Result<(), ProjectError> {
        if let Err(source) = journal::append(&self.layout.journal, record) {
            self.write_requires_recovery = true;
            return Err(ProjectError::JournalCommitFailed {
                revision,
                source: Box::new(source),
            });
        }
        Ok(())
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

fn validate_raw_recording_asset(
    manifest: &ProjectManifest,
    descriptor: &AssetDescriptor,
) -> Result<(), ProjectError> {
    let expected_byte_len = manifest
        .canvas
        .size
        .area()
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            ProjectError::InvalidRecordingMutation(
                "recording canvas RGBA byte length overflowed".to_owned(),
            )
        })?;
    let Some((size, encoding)) = descriptor.kind.raster_descriptor() else {
        return Err(ProjectError::InvalidRecordingMutation(
            "recording asset must be a raster".to_owned(),
        ));
    };
    if size != manifest.canvas.size || encoding != RasterEncoding::Rgba8 {
        return Err(ProjectError::InvalidRecordingMutation(
            "recording asset must be canvas-sized raw RGBA8".to_owned(),
        ));
    }
    if descriptor.byte_len != expected_byte_len {
        return Err(ProjectError::InvalidRecordingMutation(format!(
            "recording asset declares {} bytes, expected {expected_byte_len}",
            descriptor.byte_len
        )));
    }
    Ok(())
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
        AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
        ColorSpace, DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize, ProjectId,
        ProjectManifest, RasterEncoding, UnixTimeMs,
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

    fn recording_asset(id: AssetId) -> AssetDescriptor {
        AssetDescriptor {
            id,
            byte_len: 16,
            kind: AssetKind::Frame {
                size: PhysicalSize::new(2, 2).unwrap(),
                encoding: RasterEncoding::Rgba8,
            },
        }
    }

    fn recording_frame(id: u128, asset_id: AssetId, duration_us: u64) -> FrameClip {
        FrameClip {
            id: FrameId::from_u128(id),
            asset_id,
            duration: DurationUs::new(duration_us).unwrap(),
            transform: ClipTransform::default(),
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
        }
    }

    #[test]
    #[cfg(unix)]
    fn new_project_pixels_snapshots_and_event_journals_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("private.gfsproj");
        let mut project = ActiveProject::create(&root, manifest()).unwrap();
        let bytes = b"private recorded metadata";
        let asset_id = project.assets().put(bytes).unwrap();
        project
            .commit(register(asset_id, bytes.len() as u64))
            .unwrap();
        let layout = project.layout();
        for path in [
            &layout.root,
            &layout.assets,
            &layout.manifest,
            &layout.journal,
            &layout.lock,
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o077,
                0,
                "{}",
                path.display()
            );
        }
        for file in fs::read_dir(&layout.assets).unwrap() {
            assert_eq!(
                file.unwrap().metadata().unwrap().permissions().mode() & 0o077,
                0
            );
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

    #[test]
    fn indexed_recording_commits_recover_and_survive_generic_edits() {
        let directory = tempdir().unwrap();
        let mut active = ActiveProject::create(directory.path(), manifest()).unwrap();
        let pixels = [7_u8; 16];
        let asset_id = active.assets().put(&pixels).unwrap();

        let first = active
            .commit_recording_append(
                Some(recording_asset(asset_id)),
                recording_frame(1, asset_id, 10),
            )
            .unwrap();
        assert_eq!(first.from_revision, ProjectRevision::ZERO);
        assert_eq!(first.to_revision, ProjectRevision::new(1));
        active
            .commit_recording_append(None, recording_frame(2, asset_id, 20))
            .unwrap();
        active
            .commit_recording_duration(FrameId::from_u128(1), DurationUs::new(15).unwrap())
            .unwrap();
        active
            .commit(EditCommand::RemoveFrames {
                frame_ids: vec![FrameId::from_u128(1)],
            })
            .unwrap();
        active
            .commit_recording_append(None, recording_frame(3, asset_id, 30))
            .unwrap();
        assert_eq!(active.manifest().revision, ProjectRevision::new(5));
        assert_eq!(
            active
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| (frame.id, frame.duration.get()))
                .collect::<Vec<_>>(),
            [(FrameId::from_u128(2), 20), (FrameId::from_u128(3), 30)]
        );
        drop(active);

        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(opened.journal_recovery.replayed_records, 5);
        assert_eq!(opened.project.manifest().revision, ProjectRevision::new(5));
        assert_eq!(opened.project.manifest().timeline.frames.len(), 2);
    }

    #[test]
    fn cursor_recording_append_is_atomic_validated_and_undoable() {
        let directory = tempdir().unwrap();
        let mut active = ActiveProject::create(directory.path(), manifest()).unwrap();
        let asset_id = active.assets().put(&[7_u8; 16]).unwrap();
        let cursor_id = active.assets().put(&[255_u8; 4]).unwrap();
        let cursor = AssetDescriptor {
            id: cursor_id,
            byte_len: 4,
            kind: gif_from_screen_domain::AssetKind::OverlayImage {
                size: gif_from_screen_domain::PhysicalSize::new(1, 1).unwrap(),
                encoding: RasterEncoding::Rgba8,
            },
        };
        let mut frame = recording_frame(1, asset_id, 10);
        frame.capture_metadata.cursor_asset = Some(cursor_id);
        assert!(matches!(
            active.commit_recording_append(Some(recording_asset(asset_id)), frame.clone()),
            Err(ProjectError::Domain(DomainError::UnknownAsset(_)))
        ));
        let mut invalid = cursor.clone();
        invalid.byte_len = 3;
        assert!(
            active
                .commit_recording_append_with_cursor(
                    Some(recording_asset(asset_id)),
                    Some(invalid),
                    frame.clone()
                )
                .is_err()
        );
        assert_eq!(active.manifest().revision, ProjectRevision::ZERO);
        let receipt = active
            .commit_recording_append_with_cursor(
                Some(recording_asset(asset_id)),
                Some(cursor),
                frame,
            )
            .unwrap();
        assert_eq!(active.manifest().assets.len(), 2);
        active.commit(receipt.inverse).unwrap();
        assert!(active.manifest().assets.is_empty());
        assert!(active.manifest().timeline.frames.is_empty());
    }

    #[test]
    fn recording_fast_path_reuses_existing_overlay_raster_without_relabeling() {
        let directory = tempdir().unwrap();
        let mut active = ActiveProject::create(directory.path(), manifest()).unwrap();
        let asset_id = active.assets().put(&[7_u8; 16]).unwrap();
        let mut descriptor = recording_asset(asset_id);
        descriptor.kind = AssetKind::OverlayImage {
            size: PhysicalSize::new(2, 2).unwrap(),
            encoding: RasterEncoding::Rgba8,
        };
        active
            .commit(EditCommand::RegisterAsset {
                asset: descriptor.clone(),
            })
            .unwrap();
        active
            .commit_recording_append(None, recording_frame(1, asset_id, 10))
            .unwrap();
        drop(active);
        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert_eq!(opened.project.manifest().assets[&asset_id], descriptor);
        assert_eq!(
            opened.project.manifest().timeline.frames[0].asset_id,
            asset_id
        );
    }

    #[test]
    fn recording_fast_path_rejects_invalid_input_and_journal_failure_atomically() {
        let directory = tempdir().unwrap();
        let mut active = ActiveProject::create(directory.path(), manifest()).unwrap();
        let pixels = [9_u8; 16];
        let asset_id = active.assets().put(&pixels).unwrap();
        let mut wrong = recording_asset(asset_id);
        wrong.byte_len = 15;
        assert!(matches!(
            active.commit_recording_append(Some(wrong), recording_frame(1, asset_id, 10)),
            Err(ProjectError::InvalidRecordingMutation(_))
        ));
        assert_eq!(active.manifest().revision, ProjectRevision::ZERO);
        assert!(active.manifest().timeline.frames.is_empty());

        fs::remove_file(&active.layout().journal).unwrap();
        fs::create_dir(&active.layout().journal).unwrap();
        assert!(matches!(
            active.commit_recording_append(
                Some(recording_asset(asset_id)),
                recording_frame(1, asset_id, 10)
            ),
            Err(ProjectError::JournalCommitFailed { .. })
        ));
        assert_eq!(active.manifest().revision, ProjectRevision::ZERO);
        assert!(active.manifest().timeline.frames.is_empty());
        assert!(matches!(
            active.commit_recording_append(
                Some(recording_asset(asset_id)),
                recording_frame(1, asset_id, 10)
            ),
            Err(ProjectError::RequiresRecovery)
        ));
    }
}
