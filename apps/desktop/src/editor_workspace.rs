#![allow(
    dead_code,
    reason = "the persistent editor view-model precedes its egui integration"
)]

use std::{collections::BTreeSet, path::Path};

use gif_from_screen_domain::{DurationUs, EditCommand, FrameId, ProjectManifest, TimeUs};
use gif_from_screen_editor::{
    EditorError, TimelineSelection, TimelineSelectionError, adjust_duration, delete_frames,
    delete_frames_after, delete_frames_before, move_selected_left, move_selected_right,
    override_duration, reverse_selected, scale_duration,
};
use gif_from_screen_project::{
    ActiveProject, AssetIssue, JournalRecoveryReport, LockPolicy, OpenedProject, ProjectError,
};
use thiserror::Error;

/// Durable editor state owned by the desktop application.
///
/// Every edit, undo, and redo is committed through the active project's append-only journal before
/// this view-model mutates its history or reconciles its selection. Undo and redo history is
/// intentionally session-local and bounded; the resulting project state itself is durable and is
/// recovered when the project is reopened.
#[derive(Debug)]
pub(crate) struct EditorWorkspace {
    project: ActiveProject,
    selection: TimelineSelection,
    undo: Vec<EditCommand>,
    redo: Vec<EditCommand>,
    history_limit: usize,
    dirty: bool,
    journal_recovery: Option<JournalRecoveryReport>,
    asset_issues: Vec<AssetIssue>,
}

impl EditorWorkspace {
    /// Opens a durable project and creates an editor workspace with empty session history.
    ///
    /// # Errors
    ///
    /// Returns an error when the project cannot be opened or `history_limit` is zero.
    pub(crate) fn open(
        root: impl AsRef<Path>,
        lock_policy: LockPolicy,
        history_limit: usize,
    ) -> Result<Self, EditorWorkspaceError> {
        let opened = ActiveProject::open(root, lock_policy)?;
        Self::from_opened(opened, history_limit)
    }

    /// Consumes an already-opened project without reacquiring its lock or repeating recovery.
    ///
    /// The opening recovery report and asset issues are retained for the UI. A non-empty timeline
    /// initially selects its first frame.
    ///
    /// # Errors
    ///
    /// Returns [`EditorWorkspaceError::ZeroHistoryLimit`] when `history_limit` is zero, or a
    /// selection error if the recovered timeline cannot select its reported first frame.
    pub(crate) fn from_opened(
        opened: OpenedProject,
        history_limit: usize,
    ) -> Result<Self, EditorWorkspaceError> {
        let dirty = opened.journal_recovery.replayed_records > 0
            || opened.journal_recovery.snapshot_revision
                != opened.journal_recovery.recovered_revision
            || !opened.journal_recovery.is_clean();
        let mut workspace = Self::from_active(opened.project, history_limit)?;
        workspace.dirty = dirty;
        workspace.journal_recovery = Some(opened.journal_recovery);
        workspace.asset_issues = opened.asset_issues;
        if !workspace.manifest().timeline.frames.is_empty() {
            workspace.select_first()?;
        }
        Ok(workspace)
    }

    /// Wraps an already-open active project with empty selection and session history.
    ///
    /// # Errors
    ///
    /// Returns [`EditorWorkspaceError::ZeroHistoryLimit`] when `history_limit` is zero.
    pub(crate) fn from_active(
        project: ActiveProject,
        history_limit: usize,
    ) -> Result<Self, EditorWorkspaceError> {
        if history_limit == 0 {
            return Err(EditorWorkspaceError::ZeroHistoryLimit);
        }
        Ok(Self {
            project,
            selection: TimelineSelection::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            history_limit,
            dirty: false,
            journal_recovery: None,
            asset_issues: Vec::new(),
        })
    }

    /// Returns the current recovered in-memory manifest.
    pub(crate) fn manifest(&self) -> &ProjectManifest {
        self.project.manifest()
    }

    /// Returns the active project for read-only asset access and export snapshots.
    ///
    /// No mutable accessor is exposed: project changes must pass through [`Self::execute`] so the
    /// durable journal, bounded history, selection reconciliation, and dirty state stay coherent.
    pub(crate) const fn active_project(&self) -> &ActiveProject {
        &self.project
    }

    /// Returns the durable project's root directory.
    pub(crate) fn project_root(&self) -> &Path {
        &self.project.layout().root
    }

    /// Returns the current stable-identity selection.
    pub(crate) const fn selection(&self) -> &TimelineSelection {
        &self.selection
    }

    /// Returns whether this workspace has committed changes since it opened or last checkpointed.
    ///
    /// Journaled changes are already crash durable while this flag is true. The flag specifically
    /// indicates that the manifest snapshot has not been refreshed by this workspace.
    pub(crate) const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Returns journal recovery details captured while opening this project.
    ///
    /// A workspace created with [`Self::from_active`] has no opening report.
    pub(crate) const fn journal_recovery(&self) -> Option<&JournalRecoveryReport> {
        self.journal_recovery.as_ref()
    }

    /// Returns missing or length-mismatched assets discovered while opening.
    ///
    /// The desktop UI can use these retained issues to block rendering or offer repair without
    /// losing access to otherwise recoverable timeline metadata.
    pub(crate) fn asset_issues(&self) -> &[AssetIssue] {
        &self.asset_issues
    }

    /// Returns whether a session edit can be undone.
    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Returns whether a session edit can be redone.
    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Returns the configured maximum number of undo and redo entries.
    pub(crate) const fn history_limit(&self) -> usize {
        self.history_limit
    }

    /// Selects only `frame_id`.
    pub(crate) fn select_only(&mut self, frame_id: FrameId) -> Result<(), EditorWorkspaceError> {
        self.selection
            .select_only(&self.project.manifest().timeline, frame_id)?;
        Ok(())
    }

    /// Toggles `frame_id` in the current selection.
    pub(crate) fn toggle_selection(
        &mut self,
        frame_id: FrameId,
    ) -> Result<(), EditorWorkspaceError> {
        self.selection
            .toggle(&self.project.manifest().timeline, frame_id)?;
        Ok(())
    }

    /// Extends selection from its stable anchor through `frame_id` in timeline order.
    pub(crate) fn extend_selection_range(
        &mut self,
        frame_id: FrameId,
    ) -> Result<(), EditorWorkspaceError> {
        self.selection
            .extend_range(&self.project.manifest().timeline, frame_id)?;
        Ok(())
    }

    /// Selects every timeline frame.
    pub(crate) fn select_all(&mut self) {
        self.selection.select_all(&self.project.manifest().timeline);
    }

    /// Selects every currently unselected timeline frame.
    pub(crate) fn invert_selection(&mut self) {
        self.selection.invert(&self.project.manifest().timeline);
    }

    /// Clears the selection and navigation anchor.
    pub(crate) fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Selects and returns the first frame.
    pub(crate) fn select_first(&mut self) -> Result<FrameId, EditorWorkspaceError> {
        Ok(self.selection.first(&self.project.manifest().timeline)?)
    }

    /// Selects and returns the frame before the current frame, clamped at the beginning.
    pub(crate) fn select_previous(&mut self) -> Result<FrameId, EditorWorkspaceError> {
        Ok(self.selection.previous(&self.project.manifest().timeline)?)
    }

    /// Selects and returns the frame after the current frame, clamped at the end.
    pub(crate) fn select_next(&mut self) -> Result<FrameId, EditorWorkspaceError> {
        Ok(self.selection.next(&self.project.manifest().timeline)?)
    }

    /// Selects and returns the final frame.
    pub(crate) fn select_last(&mut self) -> Result<FrameId, EditorWorkspaceError> {
        Ok(self.selection.last(&self.project.manifest().timeline)?)
    }

    /// Selects a frame by its one-based number.
    pub(crate) fn select_frame_number(
        &mut self,
        frame_number: usize,
    ) -> Result<FrameId, EditorWorkspaceError> {
        Ok(self
            .selection
            .select_frame_number(&self.project.manifest().timeline, frame_number)?)
    }

    /// Selects the frame visible at project-relative `time`.
    pub(crate) fn select_time(&mut self, time: TimeUs) -> Result<FrameId, EditorWorkspaceError> {
        Ok(self
            .selection
            .select_time(&self.project.manifest().timeline, time)?)
    }

    /// Commits one command and records its inverse in bounded session history.
    ///
    /// Selection and both history stacks remain unchanged when the commit fails.
    ///
    /// # Errors
    ///
    /// Returns an error when the command violates a project invariant or cannot be journaled.
    pub(crate) fn execute(&mut self, command: EditCommand) -> Result<(), EditorWorkspaceError> {
        let receipt = self.project.commit(command)?;
        push_bounded(&mut self.undo, receipt.inverse, self.history_limit);
        self.redo.clear();
        self.selection.reconcile(&self.project.manifest().timeline);
        self.dirty = true;
        Ok(())
    }

    /// Deletes the current selection and any transitions that reference it.
    pub(crate) fn delete_selection(&mut self) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = delete_frames_atomically(self.project.manifest(), selected);
        self.execute(command)
    }

    /// Deletes every frame before the earliest selected frame.
    pub(crate) fn delete_before_selection(&mut self) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = delete_frames_before(self.project.manifest(), selected)?;
        self.execute(command)
    }

    /// Deletes every frame after the latest selected frame.
    pub(crate) fn delete_after_selection(&mut self) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = delete_frames_after(self.project.manifest(), selected)?;
        self.execute(command)
    }

    /// Reverses the frames occupying selected timeline positions.
    pub(crate) fn reverse_selection(&mut self) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = reverse_selected(self.project.manifest(), selected)?;
        self.execute(command)
    }

    /// Moves selected frames one slot toward the start of the timeline.
    pub(crate) fn move_selection_left(&mut self) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = move_selected_left(self.project.manifest(), selected)?;
        self.execute(command)
    }

    /// Moves selected frames one slot toward the end of the timeline.
    pub(crate) fn move_selection_right(&mut self) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = move_selected_right(self.project.manifest(), selected)?;
        self.execute(command)
    }

    /// Replaces every selected frame duration with `duration`.
    pub(crate) fn override_selection_duration(
        &mut self,
        duration: DurationUs,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = override_duration(self.project.manifest(), selected, duration)?;
        self.execute(command)
    }

    /// Adds `delta_us` to every selected frame duration.
    pub(crate) fn adjust_selection_duration(
        &mut self,
        delta_us: i64,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = adjust_duration(self.project.manifest(), selected, delta_us)?;
        self.execute(command)
    }

    /// Scales every selected frame duration by `percent`.
    pub(crate) fn scale_selection_duration(
        &mut self,
        percent: u32,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = scale_duration(self.project.manifest(), selected, percent)?;
        self.execute(command)
    }

    /// Commits the newest inverse command through the project journal.
    ///
    /// Returns `false` without writing when no undo entry exists. A failed commit preserves the
    /// undo entry, redo stack, selection, and dirty state.
    pub(crate) fn undo(&mut self) -> Result<bool, EditorWorkspaceError> {
        let Some(command) = self.undo.last().cloned() else {
            return Ok(false);
        };
        let receipt = self.project.commit(command)?;
        self.undo.pop();
        push_bounded(&mut self.redo, receipt.inverse, self.history_limit);
        self.selection.reconcile(&self.project.manifest().timeline);
        self.dirty = true;
        Ok(true)
    }

    /// Commits the newest redo command through the project journal.
    ///
    /// Returns `false` without writing when no redo entry exists. A failed commit preserves both
    /// history stacks, selection, and dirty state.
    pub(crate) fn redo(&mut self) -> Result<bool, EditorWorkspaceError> {
        let Some(command) = self.redo.last().cloned() else {
            return Ok(false);
        };
        let receipt = self.project.commit(command)?;
        self.redo.pop();
        push_bounded(&mut self.undo, receipt.inverse, self.history_limit);
        self.selection.reconcile(&self.project.manifest().timeline);
        self.dirty = true;
        Ok(true)
    }

    /// Atomically refreshes the manifest snapshot while retaining the journal and history.
    pub(crate) fn checkpoint(&mut self) -> Result<(), EditorWorkspaceError> {
        self.project.checkpoint()?;
        self.dirty = self
            .journal_recovery
            .as_ref()
            .is_some_and(|report| !report.is_clean());
        Ok(())
    }

    /// Atomically checkpoints and compacts the fully represented journal.
    pub(crate) fn checkpoint_and_compact(&mut self) -> Result<(), EditorWorkspaceError> {
        self.project.checkpoint_and_compact()?;
        self.dirty = false;
        Ok(())
    }

    fn selected_frame_ids(&self) -> Result<Vec<FrameId>, EditorWorkspaceError> {
        if self.selection.is_empty() {
            return Err(EditorError::EmptySelection.into());
        }
        Ok(self.selection.selected().iter().copied().collect())
    }
}

fn push_bounded(history: &mut Vec<EditCommand>, command: EditCommand, limit: usize) {
    history.push(command);
    let excess = history.len().saturating_sub(limit);
    if excess > 0 {
        history.drain(..excess);
    }
}

fn delete_frames_atomically(manifest: &ProjectManifest, selected: Vec<FrameId>) -> EditCommand {
    let removed: BTreeSet<_> = selected.iter().copied().collect();
    let transitions = manifest
        .timeline
        .transitions
        .iter()
        .filter(|transition| {
            !removed.contains(&transition.from_frame) && !removed.contains(&transition.to_frame)
        })
        .cloned()
        .collect::<Vec<_>>();
    let remove = delete_frames(selected);
    if transitions.len() == manifest.timeline.transitions.len() {
        remove
    } else {
        EditCommand::Compound {
            commands: vec![EditCommand::SetTransitions { transitions }, remove],
        }
    }
}

/// Errors produced by the desktop editor workspace.
#[derive(Debug, Error)]
pub(crate) enum EditorWorkspaceError {
    /// The durable project could not be opened, edited, or checkpointed.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// An editor command could not be built for the current selection.
    #[error(transparent)]
    Editor(#[from] EditorError),
    /// A selection or navigation request was invalid.
    #[error(transparent)]
    Selection(#[from] TimelineSelectionError),
    /// Session history must retain at least one entry.
    #[error("editor history limit must be greater than zero")]
    ZeroHistoryLimit,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, FrameClip, PhysicalSize, ProjectId, ProjectManifest,
        ProjectRevision, RasterEncoding, Timeline, UnixTimeMs,
    };
    use tempfile::TempDir;

    use super::*;

    fn frame_id(number: u128) -> FrameId {
        FrameId::from_u128(number)
    }

    fn manifest(durations: &[u64]) -> ProjectManifest {
        let size = PhysicalSize::new(4, 3).unwrap();
        let asset_id = AssetId::from_digest([7; 32]);
        let mut assets = BTreeMap::new();
        assets.insert(
            asset_id,
            AssetDescriptor {
                id: asset_id,
                byte_len: 48,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        let frames = durations
            .iter()
            .copied()
            .enumerate()
            .map(|(index, duration)| FrameClip {
                id: frame_id(u128::try_from(index).unwrap() + 1),
                asset_id,
                duration: DurationUs::new(duration).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata::default(),
                effects: Vec::new(),
            })
            .collect();
        ProjectManifest {
            schema_version: gif_from_screen_domain::CURRENT_SCHEMA_VERSION,
            project_id: ProjectId::from_u128(1),
            revision: ProjectRevision::ZERO,
            app_version: "0.1.0".into(),
            created_at: UnixTimeMs::new(0),
            canvas: Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
            timeline: Timeline {
                frames,
                ..Timeline::default()
            },
            assets,
            export_presets: BTreeMap::new(),
            source_provenance: Vec::new(),
        }
    }

    fn create_workspace(
        directory: &TempDir,
        durations: &[u64],
        history_limit: usize,
    ) -> EditorWorkspace {
        let active = ActiveProject::create(directory.path(), manifest(durations)).unwrap();
        EditorWorkspace::from_active(active, history_limit).unwrap()
    }

    fn order(workspace: &EditorWorkspace) -> Vec<u128> {
        workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .map(|frame| u128::from_be_bytes(*frame.id.as_bytes()))
            .collect()
    }

    fn durations(workspace: &EditorWorkspace) -> Vec<u64> {
        workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .map(|frame| frame.duration.get())
            .collect()
    }

    fn selected(workspace: &EditorWorkspace) -> Vec<u128> {
        workspace
            .selection()
            .selected()
            .iter()
            .map(|frame_id| u128::from_be_bytes(*frame_id.as_bytes()))
            .collect()
    }

    #[test]
    fn journaled_edit_is_recovered_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        {
            let mut workspace = create_workspace(&directory, &[10, 20, 30], 8);
            workspace.select_only(frame_id(2)).unwrap();
            workspace
                .override_selection_duration(DurationUs::new(99).unwrap())
                .unwrap();
            assert!(workspace.is_dirty());
        }

        let mut reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 8).unwrap();
        assert_eq!(durations(&reopened), [10, 99, 30]);
        assert_eq!(reopened.manifest().revision, ProjectRevision::new(1));
        assert!(reopened.is_dirty());
        let recovery = reopened.journal_recovery().unwrap();
        assert_eq!(recovery.snapshot_revision, ProjectRevision::ZERO);
        assert_eq!(recovery.recovered_revision, ProjectRevision::new(1));
        assert_eq!(recovery.replayed_records, 1);
        assert!(recovery.is_clean());
        assert!(matches!(
            reopened.asset_issues(),
            [AssetIssue::Missing { .. }]
        ));
        assert!(!reopened.can_undo());
        reopened.checkpoint().unwrap();
        assert!(!reopened.is_dirty());
    }

    #[test]
    fn from_opened_keeps_recovery_issues_lock_and_selects_first_frame() {
        let directory = tempfile::tempdir().unwrap();
        let mut active = ActiveProject::create(directory.path(), manifest(&[10, 20])).unwrap();
        let canvas = active.manifest().canvas.clone();
        active.commit(EditCommand::SetCanvas { canvas }).unwrap();
        drop(active);
        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();

        let workspace = EditorWorkspace::from_opened(opened, 8).unwrap();

        assert!(workspace.is_dirty());
        assert_eq!(workspace.journal_recovery().unwrap().replayed_records, 1);
        assert!(matches!(
            workspace.asset_issues(),
            [AssetIssue::Missing { .. }]
        ));
        assert_eq!(workspace.selection().current(), Some(frame_id(1)));
        assert!(directory.path().join("project.lock").exists());
        assert!(matches!(
            ActiveProject::open(directory.path(), LockPolicy::FailIfPresent),
            Err(ProjectError::AlreadyLocked { .. })
        ));
    }

    #[test]
    fn undo_and_redo_are_journaled_and_reconcile_selection() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30], 8);
        workspace.select_only(frame_id(2)).unwrap();
        workspace
            .override_selection_duration(DurationUs::new(99).unwrap())
            .unwrap();

        assert!(workspace.undo().unwrap());
        assert_eq!(durations(&workspace), [10, 20, 30]);
        assert_eq!(selected(&workspace), [2]);
        assert!(workspace.can_redo());
        assert!(workspace.redo().unwrap());
        assert_eq!(durations(&workspace), [10, 99, 30]);
        assert_eq!(selected(&workspace), [2]);
        assert!(!workspace.can_redo());
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 8).unwrap();
        assert_eq!(durations(&reopened), [10, 99, 30]);
        assert_eq!(reopened.manifest().revision, ProjectRevision::new(3));
    }

    #[test]
    fn deleting_the_entire_selection_falls_back_to_a_valid_frame() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30, 40], 8);
        workspace.select_only(frame_id(3)).unwrap();

        workspace.delete_selection().unwrap();
        assert_eq!(order(&workspace), [1, 2, 4]);
        assert_eq!(selected(&workspace), [1]);
        assert_eq!(workspace.selection().current(), Some(frame_id(1)));

        assert!(workspace.undo().unwrap());
        assert_eq!(order(&workspace), [1, 2, 3, 4]);
        assert_eq!(selected(&workspace), [1]);
    }

    #[test]
    fn failed_commit_does_not_change_manifest_selection_or_history() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30], 8);
        workspace.select_only(frame_id(2)).unwrap();
        workspace.adjust_selection_duration(5).unwrap();
        let manifest_before = workspace.manifest().clone();
        let selection_before = workspace.selection().clone();
        let dirty_before = workspace.is_dirty();

        let error = workspace
            .execute(EditCommand::RemoveFrames {
                frame_ids: vec![frame_id(99)],
            })
            .unwrap_err();
        assert!(matches!(error, EditorWorkspaceError::Project(_)));
        assert_eq!(workspace.manifest(), &manifest_before);
        assert_eq!(workspace.selection(), &selection_before);
        assert_eq!(workspace.is_dirty(), dirty_before);
        assert!(workspace.can_undo());
        assert!(!workspace.can_redo());

        assert!(workspace.undo().unwrap());
        assert_eq!(durations(&workspace), [10, 20, 30]);
        assert!(!workspace.undo().unwrap());
    }

    #[test]
    fn failed_journal_write_during_undo_preserves_view_model_state() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20], 8);
        workspace.select_only(frame_id(2)).unwrap();
        workspace.adjust_selection_duration(5).unwrap();
        let manifest_before = workspace.manifest().clone();
        let selection_before = workspace.selection().clone();
        let dirty_before = workspace.is_dirty();

        let journal = workspace.project_root().join("journal.ndjson");
        std::fs::remove_file(&journal).unwrap();
        std::fs::create_dir(&journal).unwrap();

        assert!(matches!(
            workspace.undo(),
            Err(EditorWorkspaceError::Project(
                ProjectError::JournalCommitFailed { .. }
            ))
        ));
        assert_eq!(workspace.manifest(), &manifest_before);
        assert_eq!(workspace.selection(), &selection_before);
        assert_eq!(workspace.is_dirty(), dirty_before);
        assert!(workspace.can_undo());
        assert!(!workspace.can_redo());
    }

    #[test]
    fn history_limit_keeps_only_the_newest_inverse_commands() {
        let directory = tempfile::tempdir().unwrap();
        let active = ActiveProject::create(directory.path(), manifest(&[10])).unwrap();
        assert!(matches!(
            EditorWorkspace::from_active(active, 0),
            Err(EditorWorkspaceError::ZeroHistoryLimit)
        ));

        let mut workspace =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 2).unwrap();
        assert_eq!(workspace.history_limit(), 2);
        workspace.select_only(frame_id(1)).unwrap();
        for _ in 0..3 {
            workspace.adjust_selection_duration(1).unwrap();
        }
        assert_eq!(durations(&workspace), [13]);

        assert!(workspace.undo().unwrap());
        assert!(workspace.undo().unwrap());
        assert!(!workspace.undo().unwrap());
        assert_eq!(durations(&workspace), [11]);
        assert!(workspace.redo().unwrap());
        assert!(workspace.redo().unwrap());
        assert!(!workspace.redo().unwrap());
        assert_eq!(durations(&workspace), [13]);
    }

    #[test]
    fn open_retains_asset_length_issues_for_the_ui() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = create_workspace(&directory, &[10], 8);
        let asset_id = workspace.manifest().timeline.frames[0].asset_id;
        let asset_path = workspace.project.assets().asset_path(asset_id);
        std::fs::write(asset_path, b"short").unwrap();
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 8).unwrap();
        assert!(matches!(
            reopened.asset_issues(),
            [AssetIssue::LengthMismatch {
                expected: 48,
                actual: 5,
                ..
            }]
        ));
        assert!(!reopened.is_dirty());
        assert_eq!(reopened.journal_recovery().unwrap().replayed_records, 0);
    }

    #[test]
    fn selection_navigation_covers_identity_range_number_and_time() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30, 40], 8);
        assert_eq!(workspace.select_first().unwrap(), frame_id(1));
        assert_eq!(workspace.select_next().unwrap(), frame_id(2));
        assert_eq!(workspace.select_previous().unwrap(), frame_id(1));
        assert_eq!(workspace.select_last().unwrap(), frame_id(4));
        assert_eq!(workspace.select_frame_number(3).unwrap(), frame_id(3));
        assert_eq!(workspace.select_time(TimeUs::new(10)).unwrap(), frame_id(2));

        workspace.toggle_selection(frame_id(4)).unwrap();
        assert_eq!(selected(&workspace), [2, 4]);
        workspace.extend_selection_range(frame_id(1)).unwrap();
        assert_eq!(selected(&workspace), [1, 2, 3, 4]);
        workspace.invert_selection();
        assert!(workspace.selection().is_empty());
        workspace.select_all();
        assert_eq!(selected(&workspace), [1, 2, 3, 4]);
        workspace.clear_selection();
        assert!(workspace.selection().is_empty());
    }

    #[test]
    fn editing_helpers_all_commit_and_checkpoint_updates_dirty_state() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30, 40], 16);
        assert_eq!(workspace.project_root(), directory.path());
        assert_eq!(workspace.active_project().manifest(), workspace.manifest());
        assert_eq!(
            workspace.active_project().assets().directory(),
            directory.path().join("assets")
        );
        workspace.select_only(frame_id(2)).unwrap();
        workspace.toggle_selection(frame_id(3)).unwrap();

        workspace.move_selection_right().unwrap();
        assert_eq!(order(&workspace), [1, 4, 2, 3]);
        workspace.move_selection_left().unwrap();
        assert_eq!(order(&workspace), [1, 2, 3, 4]);
        workspace.reverse_selection().unwrap();
        assert_eq!(order(&workspace), [1, 3, 2, 4]);
        workspace
            .override_selection_duration(DurationUs::new(50).unwrap())
            .unwrap();
        workspace.adjust_selection_duration(-10).unwrap();
        workspace.scale_selection_duration(50).unwrap();
        assert_eq!(durations(&workspace), [10, 20, 20, 40]);

        workspace.delete_before_selection().unwrap();
        assert_eq!(order(&workspace), [3, 2, 4]);
        workspace.delete_after_selection().unwrap();
        assert_eq!(order(&workspace), [3, 2]);
        assert!(workspace.is_dirty());
        workspace.checkpoint().unwrap();
        assert!(!workspace.is_dirty());

        workspace.delete_selection().unwrap();
        assert!(workspace.manifest().timeline.frames.is_empty());
        assert!(workspace.is_dirty());
        workspace.checkpoint_and_compact().unwrap();
        assert!(!workspace.is_dirty());
    }
}
