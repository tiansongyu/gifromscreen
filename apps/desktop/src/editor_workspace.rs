#![allow(
    dead_code,
    reason = "the persistent editor view-model precedes its egui integration"
)]

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, BlendMode, DurationUs, EditCommand, Effect, FrameId,
    OverlayContent, OverlayId, OverlayItem, OverlayTrack, PhysicalPoint, PhysicalRect,
    PhysicalSize, ProjectId, ProjectManifest, ProjectRevision, RasterEncoding, TimeUs,
    TimelineSpan, TrackId, Transition,
};
use gif_from_screen_editor::{
    ClipTransformEdit, DuplicateDelayMode, DuplicateFrameRetention, EditorError, EditorStatistics,
    EditorStatisticsError, FrameClipboardEntryId, FrameClipboardHistory,
    FrameClipboardHistoryEntry, FrameClipboardHistoryError, FrameComparison, FrameEffectEdit,
    FrameSimilarityProvider, FrameTimeRangeError, FrameTransitionSettings, ReduceDelayMode,
    ReduceOptions, RemoveDuplicateFramesOptions, TimelineSelection, TimelineSelectionError,
    YoyoOptions, YoyoScope, adjust_duration, copy_selected_frames, cut_selected_frames,
    delete_frames, delete_frames_after, delete_frames_before, edit_clip_transforms,
    edit_frame_effects, move_selected_left, move_selected_right, override_duration,
    paste_frame_clipboard, project_statistics, reduce_frames, remove_duplicate_frames,
    remove_transition_after, reverse_selected, scale_duration, select_frames_by_time_range,
    set_transition_after, yoyo_frames,
};
use gif_from_screen_project::{
    ActiveProject, AssetIssue, AssetStore, CommitReceipt, JournalRecoveryReport, LockPolicy,
    OpenedProject, ProjectError,
};
use gif_from_screen_render::RgbaSurface;
use thiserror::Error;
use uuid::Uuid;

use crate::editor_preview::{EditorPreviewError, render_frame_surface};

#[path = "editor_text.rs"]
mod text;
pub(crate) use text::{TextOverlayDraft, TitleFrameRequest};

#[path = "editor_insert.rs"]
mod insert;
pub(crate) use insert::{
    PreparedProjectInsertion, ProjectInsertionTarget, prepare_project_insertion_from_path,
};

#[path = "editor_presets.rs"]
mod presets;

const MAX_SYNCHRONOUS_DUPLICATE_SCAN_FRAMES: usize = 256;
const DUPLICATE_RENDER_SURFACE_LIMIT_BYTES: usize = 128 * 1024 * 1024;

pub(crate) struct RasterOverlayEdit {
    pub(crate) name: String,
    pub(crate) source_size: PhysicalSize,
    pub(crate) position: PhysicalPoint,
    pub(crate) display_size: PhysicalSize,
    pub(crate) item_opacity: u8,
    pub(crate) track_opacity: u8,
    pub(crate) blend_mode: BlendMode,
    pub(crate) z_index: i32,
}

/// Frozen authoring target for work that completes after the current UI event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OverlaySelectionAnchor {
    project_root: PathBuf,
    project_id: ProjectId,
    revision: ProjectRevision,
    selection: TimelineSelection,
}

impl OverlaySelectionAnchor {
    pub(crate) fn matches(&self, workspace: &EditorWorkspace) -> bool {
        self.project_root == workspace.project.layout().root
            && self.project_id == workspace.manifest().project_id
            && self.revision == workspace.manifest().revision
            && self.selection == workspace.selection
    }
}

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
    clipboard: FrameClipboardHistory,
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
            clipboard: FrameClipboardHistory::default(),
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

    /// Returns whether the opening report found a journal tail that must be repaired.
    pub(crate) fn journal_requires_repair(&self) -> bool {
        self.journal_recovery
            .as_ref()
            .is_some_and(|report| !report.is_clean())
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

    /// Returns the frame count in the selected bounded clipboard-history entry.
    pub(crate) fn clipboard_len(&self) -> usize {
        self.clipboard
            .selected_clipboard()
            .map_or(0, gif_from_screen_editor::FrameClipboard::len)
    }

    /// Returns the number of retained clipboard snapshots.
    pub(crate) fn clipboard_history_len(&self) -> usize {
        self.clipboard.len()
    }

    /// Iterates clipboard snapshots from oldest to newest.
    pub(crate) fn clipboard_history_entries(
        &self,
    ) -> impl DoubleEndedIterator<Item = &FrameClipboardHistoryEntry> {
        self.clipboard.entries()
    }

    /// Returns the stable identity of the snapshot used by Paste.
    pub(crate) const fn selected_clipboard_id(&self) -> Option<FrameClipboardEntryId> {
        self.clipboard.selected_id()
    }

    /// Selects an existing clipboard snapshot for subsequent Paste.
    pub(crate) fn select_clipboard_entry(&mut self, id: FrameClipboardEntryId) -> bool {
        self.clipboard.select(id)
    }

    /// Removes one clipboard snapshot and returns its frame count.
    pub(crate) fn remove_clipboard_entry(&mut self, id: FrameClipboardEntryId) -> Option<usize> {
        self.clipboard.remove(id).map(|clipboard| clipboard.len())
    }

    /// Clears all session-local clipboard snapshots.
    pub(crate) fn clear_clipboard_history(&mut self) {
        self.clipboard.clear();
    }

    /// Captures the project and exact selection before asynchronous authoring begins.
    pub(crate) fn overlay_selection_anchor(
        &self,
    ) -> Result<OverlaySelectionAnchor, EditorWorkspaceError> {
        self.selected_frame_ids()?;
        Ok(self.project_edit_anchor())
    }

    /// Captures an asynchronous edit target, including an empty timeline or selection.
    pub(crate) fn project_edit_anchor(&self) -> OverlaySelectionAnchor {
        OverlaySelectionAnchor {
            project_root: self.project.layout().root.clone(),
            project_id: self.manifest().project_id,
            revision: self.manifest().revision,
            selection: self.selection.clone(),
        }
    }

    /// Adds one track with an item for each uninterrupted selected frame range.
    pub(crate) fn add_overlay_for_selection(
        &mut self,
        name: String,
        content: OverlayContent,
        z_index: i32,
        track_opacity: u8,
        blend_mode: BlendMode,
    ) -> Result<TrackId, EditorWorkspaceError> {
        if name.trim().is_empty() {
            return Err(EditorWorkspaceError::EmptyOverlayName);
        }
        let spans = self.selected_timeline_spans()?;
        let track_id = TrackId::from_u128(Uuid::new_v4().as_u128());
        self.execute(EditCommand::UpsertOverlayTrack {
            track: OverlayTrack {
                id: track_id,
                name,
                visible: true,
                opacity: track_opacity,
                blend_mode,
                items: overlay_items(spans, content, z_index),
            },
        })?;
        Ok(track_id)
    }

    /// Removes one complete overlay track as a journal-backed edit.
    pub(crate) fn remove_overlay_track(
        &mut self,
        track_id: TrackId,
    ) -> Result<(), EditorWorkspaceError> {
        self.execute(EditCommand::RemoveOverlayTrack { track_id })
    }

    pub(crate) fn add_raster_overlay_for_selection(
        &mut self,
        edit: RasterOverlayEdit,
        rgba: &[u8],
    ) -> Result<TrackId, EditorWorkspaceError> {
        let position = edit.position;
        let size = edit.display_size;
        let opacity = edit.item_opacity;
        self.add_raster_content_for_selection(edit, rgba, |asset_id| OverlayContent::Raster {
            asset_id,
            position,
            size,
            opacity,
        })
    }

    fn add_raster_content_for_selection(
        &mut self,
        edit: RasterOverlayEdit,
        rgba: &[u8],
        content: impl FnOnce(AssetId) -> OverlayContent,
    ) -> Result<TrackId, EditorWorkspaceError> {
        if edit.name.trim().is_empty() {
            return Err(EditorWorkspaceError::EmptyOverlayName);
        }
        if edit.item_opacity == 0 || edit.track_opacity == 0 {
            return Err(EditorWorkspaceError::InvisibleRasterOverlay);
        }
        edit.source_size
            .validate()
            .and_then(|()| edit.display_size.validate())
            .map_err(|_| EditorWorkspaceError::EmptyRasterOverlaySize)?;
        let spans = self.selected_timeline_spans()?;
        let placement = PhysicalRect {
            origin: edit.position,
            size: edit.display_size,
        };
        if !placement.fits_within(self.project.manifest().canvas.size) {
            return Err(EditorWorkspaceError::RasterOverlayOutsideCanvas);
        }
        let asset = self.raster_asset_descriptor(edit.source_size, rgba)?;
        let track_id = TrackId::from_u128(Uuid::new_v4().as_u128());
        self.commit_with_raster_assets(
            &[(asset.clone(), rgba)],
            vec![EditCommand::UpsertOverlayTrack {
                track: OverlayTrack {
                    id: track_id,
                    name: edit.name,
                    visible: true,
                    opacity: edit.track_opacity,
                    blend_mode: edit.blend_mode,
                    items: overlay_items(spans, content(asset.id), edit.z_index),
                },
            }],
        )?;
        Ok(track_id)
    }

    fn raster_asset_descriptor(
        &self,
        size: PhysicalSize,
        rgba: &[u8],
    ) -> Result<AssetDescriptor, EditorWorkspaceError> {
        size.validate()
            .map_err(|_| EditorWorkspaceError::EmptyRasterOverlaySize)?;
        let expected = size
            .area()
            .and_then(|pixels| pixels.checked_mul(4))
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or(EditorWorkspaceError::RasterOverlayByteLengthOverflow)?;
        if rgba.len() != expected {
            return Err(EditorWorkspaceError::RasterOverlayByteLengthMismatch {
                expected,
                actual: rgba.len(),
            });
        }
        let asset_id = AssetStore::id_for_bytes(rgba);
        let existing = self.project.manifest().assets.get(&asset_id);
        if let Some(descriptor) = existing {
            let compatible = descriptor.kind.raster_descriptor()
                == Some((size, RasterEncoding::Rgba8))
                && descriptor.byte_len == u64::try_from(expected).unwrap_or(u64::MAX);
            if !compatible {
                return Err(EditorWorkspaceError::RasterOverlayAssetCollision { asset_id });
            }
            return Ok(descriptor.clone());
        }
        Ok(AssetDescriptor {
            id: asset_id,
            byte_len: u64::try_from(expected)
                .map_err(|_| EditorWorkspaceError::RasterOverlayByteLengthOverflow)?,
            kind: AssetKind::OverlayImage {
                size,
                encoding: RasterEncoding::Rgba8,
            },
        })
    }

    /// Validate the complete edit before storing bytes, then journal it as one revision.
    fn commit_with_raster_assets(
        &mut self,
        assets: &[(AssetDescriptor, &[u8])],
        commands: Vec<EditCommand>,
    ) -> Result<(), EditorWorkspaceError> {
        let mut registered = BTreeSet::new();
        let mut compound = Vec::with_capacity(assets.len() + commands.len());
        for (asset, _) in assets {
            if registered.insert(asset.id) && !self.manifest().assets.contains_key(&asset.id) {
                compound.push(EditCommand::RegisterAsset {
                    asset: asset.clone(),
                });
            }
        }
        compound.extend(commands);
        let command = EditCommand::Compound { commands: compound };
        self.manifest()
            .clone()
            .apply_command(&command)
            .map_err(EditorError::from)?;
        for (asset, rgba) in assets {
            let stored_id = self.project.assets().put(rgba)?;
            debug_assert_eq!(stored_id, asset.id);
        }
        self.execute(command)?;
        self.asset_issues
            .retain(|issue| !registered.contains(&asset_issue_id(issue)));
        Ok(())
    }

    /// Projects current timeline, selection, delay, canvas, and asset statistics.
    pub(crate) fn statistics(&self) -> Result<EditorStatistics, EditorStatisticsError> {
        project_statistics(
            self.project.manifest(),
            self.selection.selected().iter().copied(),
            self.selection.current(),
        )
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

    /// Replaces the selection with frames intersecting the half-open range `[start, end)`.
    pub(crate) fn select_time_range(
        &mut self,
        start: TimeUs,
        end: TimeUs,
    ) -> Result<(), EditorWorkspaceError> {
        let frame_ids = self.frame_ids_in_time_range(start, end)?;
        let selection = selection_for_frame_ids(self.project.manifest(), &frame_ids)?;
        self.selection = selection;
        Ok(())
    }

    /// Atomically removes every frame outside `[start, end)` and selects the retained range.
    pub(crate) fn keep_time_range(
        &mut self,
        start: TimeUs,
        end: TimeUs,
    ) -> Result<(), EditorWorkspaceError> {
        let retained = self.frame_ids_in_time_range(start, end)?;
        let retained_set: BTreeSet<_> = retained.iter().copied().collect();
        let removed = self
            .project
            .manifest()
            .timeline
            .frames
            .iter()
            .filter(|frame| !retained_set.contains(&frame.id))
            .map(|frame| frame.id)
            .collect::<Vec<_>>();
        if removed.is_empty() {
            return Err(EditorWorkspaceError::NoFramesOutsideTimeRange {
                start_us: start.get(),
                end_us: end.get(),
            });
        }
        let retained_selection = selection_for_frame_ids(self.project.manifest(), &retained)?;
        let command = delete_frames_atomically(self.project.manifest(), removed);
        self.execute(command)?;
        self.selection = retained_selection;
        Ok(())
    }

    /// Atomically removes every frame intersecting the half-open range `[start, end)`.
    pub(crate) fn delete_time_range(
        &mut self,
        start: TimeUs,
        end: TimeUs,
    ) -> Result<(), EditorWorkspaceError> {
        let removed = self.frame_ids_in_time_range(start, end)?;
        let command = delete_frames_atomically(self.project.manifest(), removed);
        self.execute(command)
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
        self.accept_commit(receipt);
        Ok(())
    }

    fn accept_commit(&mut self, receipt: CommitReceipt) {
        push_bounded(&mut self.undo, receipt.inverse, self.history_limit);
        self.redo.clear();
        self.selection.reconcile(&self.project.manifest().timeline);
        self.dirty = true;
    }

    /// Deletes the current selection and any transitions that reference it.
    pub(crate) fn delete_selection(&mut self) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = delete_frames_atomically(self.project.manifest(), selected);
        self.execute(command)
    }

    /// Copies the current selection without changing project revision or history.
    pub(crate) fn copy_selection(&mut self) -> Result<usize, EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let clipboard = copy_selected_frames(self.project.manifest(), selected)?;
        let count = clipboard.len();
        self.clipboard.push(clipboard)?;
        Ok(count)
    }

    /// Atomically cuts the selection and installs its clipboard only after commit succeeds.
    pub(crate) fn cut_selection(&mut self) -> Result<usize, EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let cut = cut_selected_frames(self.project.manifest(), selected)?;
        let count = cut.clipboard.len();
        let committed = {
            let history = &mut self.clipboard;
            let project = &mut self.project;
            history.push_after(cut.clipboard, || project.commit(cut.command))?
        };
        let (_, receipt) = committed.map_err(EditorWorkspaceError::Project)?;
        self.accept_commit(receipt);
        Ok(count)
    }

    /// Pastes fresh-ID clones after current frame, or at the end with no current frame.
    pub(crate) fn paste_after_current(&mut self) -> Result<usize, EditorWorkspaceError> {
        let clipboard = self
            .clipboard
            .selected_clipboard()
            .ok_or(EditorWorkspaceError::EmptyClipboard)?;
        let count = clipboard.len();
        let command = paste_frame_clipboard(
            self.project.manifest(),
            clipboard,
            self.selection.current(),
            || FrameId::from_u128(Uuid::new_v4().as_u128()),
        )?;
        self.execute(command)?;
        Ok(count)
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

    /// Reduces a consecutive selection using a fixed interval and explicit delay policy.
    pub(crate) fn reduce_selection(
        &mut self,
        keep_every: usize,
        delay_mode: ReduceDelayMode,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = reduce_frames(
            self.project.manifest(),
            selected,
            ReduceOptions {
                keep_every,
                delay_mode,
            },
        )?;
        self.execute(command)
    }

    /// Removes adjacent rendered duplicates from the current selection.
    ///
    /// The synchronous scan is capped at 256 selected frames. Each comparison uses the same safe
    /// final CPU-render path as editor previews with a 128 MiB per-surface limit.
    pub(crate) fn remove_duplicate_selection(
        &mut self,
        threshold: u8,
        retention: DuplicateFrameRetention,
        delay_mode: DuplicateDelayMode,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        if selected.len() > MAX_SYNCHRONOUS_DUPLICATE_SCAN_FRAMES {
            return Err(EditorWorkspaceError::DuplicateScanTooLarge {
                selected: selected.len(),
                maximum: MAX_SYNCHRONOUS_DUPLICATE_SCAN_FRAMES,
            });
        }
        let command = {
            let provider = ExactRenderedFrameProvider {
                project: &self.project,
                render_surface_limit_bytes: DUPLICATE_RENDER_SURFACE_LIMIT_BYTES,
            };
            remove_duplicate_frames(
                self.project.manifest(),
                selected,
                RemoveDuplicateFramesOptions {
                    threshold,
                    retention,
                    delay_mode,
                },
                &provider,
            )?
        };
        self.execute(command)
    }

    /// Appends a reversed clone leg for the selected range or complete timeline.
    pub(crate) fn yoyo(
        &mut self,
        scope: YoyoScope,
        repeat_endpoints: bool,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self
            .selection
            .selected()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let command = yoyo_frames(
            self.project.manifest(),
            selected,
            YoyoOptions {
                repeat_endpoints,
                scope,
            },
            || FrameId::from_u128(Uuid::new_v4().as_u128()),
        )?;
        self.execute(command)
    }

    /// Returns the transition from the current frame to its immediate successor, if present.
    pub(crate) fn current_transition(&self) -> Option<&Transition> {
        let current = self.selection.current()?;
        let index = self
            .project
            .manifest()
            .timeline
            .frames
            .iter()
            .position(|frame| frame.id == current)?;
        let next = self
            .project
            .manifest()
            .timeline
            .frames
            .get(index.checked_add(1)?)?;
        self.project
            .manifest()
            .timeline
            .transitions
            .iter()
            .find(|transition| transition.from_frame == current && transition.to_frame == next.id)
    }

    /// Creates or replaces the transition from the current frame to its immediate successor.
    pub(crate) fn set_current_transition(
        &mut self,
        settings: FrameTransitionSettings,
    ) -> Result<(), EditorWorkspaceError> {
        let command = set_transition_after(
            self.project.manifest(),
            self.selection.current(),
            settings,
            || gif_from_screen_domain::TransitionId::from_u128(Uuid::new_v4().as_u128()),
        )?;
        self.execute(command)
    }

    /// Removes the transition from the current frame to its immediate successor.
    pub(crate) fn remove_current_transition(&mut self) -> Result<(), EditorWorkspaceError> {
        let command = remove_transition_after(self.project.manifest(), self.selection.current())?;
        self.execute(command)
    }

    /// Appends one validated effect to every selected frame.
    pub(crate) fn add_selection_effect(
        &mut self,
        effect: Effect,
    ) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_effect(&FrameEffectEdit::Add(effect))
    }

    /// Replaces one zero-based effect position on every selected frame.
    pub(crate) fn replace_selection_effect(
        &mut self,
        index: usize,
        effect: Effect,
    ) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_effect(&FrameEffectEdit::Replace { index, effect })
    }

    /// Clears all effects from every selected frame.
    pub(crate) fn clear_selection_effects(&mut self) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_effect(&FrameEffectEdit::Clear)
    }

    /// Sets one source-coordinate crop on every selected frame.
    pub(crate) fn set_selection_crop(
        &mut self,
        crop: PhysicalRect,
    ) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_transform(ClipTransformEdit::SetCrop(crop))
    }

    /// Clears cropping from every selected frame.
    pub(crate) fn clear_selection_crop(&mut self) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_transform(ClipTransformEdit::ClearCrop)
    }

    /// Sets the pre-rotation output dimensions on every selected frame.
    pub(crate) fn set_selection_output_size(
        &mut self,
        size: PhysicalSize,
    ) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_transform(ClipTransformEdit::SetOutputSize(size))
    }

    /// Clears explicit output dimensions from every selected frame.
    pub(crate) fn clear_selection_output_size(&mut self) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_transform(ClipTransformEdit::ClearOutputSize)
    }

    /// Rotates every selected frame 90 degrees clockwise relative to its current transform.
    pub(crate) fn rotate_selection_clockwise(&mut self) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_transform(ClipTransformEdit::RotateClockwise)
    }

    /// Rotates every selected frame 90 degrees counterclockwise relative to its current transform.
    pub(crate) fn rotate_selection_counterclockwise(&mut self) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_transform(ClipTransformEdit::RotateCounterclockwise)
    }

    /// Toggles horizontal flipping on every selected frame.
    pub(crate) fn toggle_selection_horizontal_flip(&mut self) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_transform(ClipTransformEdit::ToggleHorizontalFlip)
    }

    /// Toggles vertical flipping on every selected frame.
    pub(crate) fn toggle_selection_vertical_flip(&mut self) -> Result<(), EditorWorkspaceError> {
        self.execute_selection_transform(ClipTransformEdit::ToggleVerticalFlip)
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
        self.dirty = self.journal_requires_repair();
        if !self.dirty {
            self.mark_recovery_clean(false);
        }
        Ok(())
    }

    /// Atomically checkpoints and compacts the fully represented journal.
    pub(crate) fn checkpoint_and_compact(&mut self) -> Result<(), EditorWorkspaceError> {
        self.project.checkpoint_and_compact()?;
        self.dirty = false;
        self.mark_recovery_clean(true);
        Ok(())
    }

    /// Preserves and replaces a rejected journal tail, returning its forensic backup path.
    ///
    /// A clean project returns `None` without changing dirty state or history. Successful repair
    /// checkpoints the recovered manifest, restores project writability, marks recovery clean, and
    /// leaves asset issues and undo/redo history untouched.
    pub(crate) fn repair_journal(&mut self) -> Result<Option<PathBuf>, EditorWorkspaceError> {
        let preserved = self.project.repair_journal()?;
        if preserved.is_some() {
            self.dirty = false;
            self.mark_recovery_clean(true);
        }
        Ok(preserved)
    }

    fn selected_frame_ids(&self) -> Result<Vec<FrameId>, EditorWorkspaceError> {
        if self.selection.is_empty() {
            return Err(EditorError::EmptySelection.into());
        }
        Ok(self.selection.selected().iter().copied().collect())
    }

    fn execute_selection_transform(
        &mut self,
        edit: ClipTransformEdit,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = edit_clip_transforms(self.project.manifest(), selected, edit)?;
        self.execute(command)
    }

    fn frame_ids_in_time_range(
        &self,
        start: TimeUs,
        end: TimeUs,
    ) -> Result<Vec<FrameId>, EditorWorkspaceError> {
        let frame_ids = select_frames_by_time_range(&self.project.manifest().timeline, start, end)?;
        if frame_ids.is_empty() {
            return Err(EditorWorkspaceError::EmptyTimeRange {
                start_us: start.get(),
                end_us: end.get(),
            });
        }
        Ok(frame_ids)
    }

    fn selected_timeline_spans(&self) -> Result<Vec<TimelineSpan>, EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let selected = selected.into_iter().collect::<BTreeSet<_>>();
        let mut cursor = 0_u64;
        let mut spans: Vec<TimelineSpan> = Vec::new();
        let mut continuing = false;
        for frame in &self.project.manifest().timeline.frames {
            let frame_end = cursor
                .checked_add(frame.duration.get())
                .ok_or(EditorWorkspaceError::OverlayTimelineDurationOverflow)?;
            if selected.contains(&frame.id) {
                if continuing {
                    let last = spans.last_mut().expect("preceding frame started a span");
                    last.duration = DurationUs::new(frame_end - last.start.get())
                        .ok_or(EditorWorkspaceError::OverlayTimelineDurationOverflow)?;
                } else {
                    spans.push(TimelineSpan {
                        start: TimeUs::new(cursor),
                        duration: frame.duration,
                    });
                }
                continuing = true;
            } else {
                continuing = false;
            }
            cursor = frame_end;
        }
        if spans.is_empty() {
            return Err(EditorWorkspaceError::EmptyOverlaySelection);
        }
        Ok(spans)
    }

    fn execute_selection_effect(
        &mut self,
        edit: &FrameEffectEdit,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = edit_frame_effects(self.project.manifest(), selected, edit)?;
        self.execute(command)
    }

    fn mark_recovery_clean(&mut self, journal_compacted: bool) {
        let revision = self.project.manifest().revision;
        if let Some(report) = &mut self.journal_recovery {
            report.snapshot_revision = revision;
            report.recovered_revision = revision;
            report.already_snapshotted_records = if journal_compacted {
                0
            } else {
                report
                    .already_snapshotted_records
                    .saturating_add(report.replayed_records)
            };
            report.replayed_records = 0;
            report.stop_reason = None;
        }
    }
}

struct ExactRenderedFrameProvider<'a> {
    project: &'a ActiveProject,
    render_surface_limit_bytes: usize,
}

impl FrameSimilarityProvider for ExactRenderedFrameProvider<'_> {
    type Error = ExactDuplicateRenderError;

    fn compare(&self, first: FrameId, second: FrameId) -> Result<FrameComparison, Self::Error> {
        let first_surface =
            render_frame_surface(self.project, first, self.render_surface_limit_bytes).map_err(
                |source| ExactDuplicateRenderError {
                    frame_id: first,
                    source,
                },
            )?;
        let second_surface =
            render_frame_surface(self.project, second, self.render_surface_limit_bytes).map_err(
                |source| ExactDuplicateRenderError {
                    frame_id: second,
                    source,
                },
            )?;
        Ok(rendered_similarity(&first_surface, &second_surface))
    }
}

#[derive(Debug, Error)]
#[error("could not render frame {frame_id} for duplicate comparison: {source}")]
struct ExactDuplicateRenderError {
    frame_id: FrameId,
    #[source]
    source: EditorPreviewError,
}

fn overlay_items(
    spans: Vec<TimelineSpan>,
    content: OverlayContent,
    z_index: i32,
) -> Vec<OverlayItem> {
    let count = spans.len();
    spans
        .into_iter()
        .zip(std::iter::repeat_n(content, count))
        .map(|(span, content)| OverlayItem {
            id: OverlayId::from_u128(Uuid::new_v4().as_u128()),
            span,
            z_index,
            content,
        })
        .collect()
}

fn rendered_similarity(first: &RgbaSurface, second: &RgbaSurface) -> FrameComparison {
    if first.size() != second.size() {
        return FrameComparison::DifferentDimensions;
    }
    let mut distance = 0_u128;
    for (first_pixel, second_pixel) in first
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(second.pixels().as_chunks::<4>().0)
    {
        let first_alpha = u32::from(first_pixel[3]);
        let second_alpha = u32::from(second_pixel[3]);
        for channel in 0..3 {
            let first_premultiplied = (u32::from(first_pixel[channel]) * first_alpha + 127) / 255;
            let second_premultiplied =
                (u32::from(second_pixel[channel]) * second_alpha + 127) / 255;
            distance += u128::from(first_premultiplied.abs_diff(second_premultiplied));
        }
        distance += u128::from(first_alpha.abs_diff(second_alpha));
    }
    let pixel_count = u128::try_from(first.pixels().len() / 4).unwrap_or(u128::MAX);
    let maximum_distance = pixel_count.saturating_mul(4 * 255);
    if maximum_distance == 0 {
        return FrameComparison::SimilarityPercent(100);
    }
    let similarity = maximum_distance
        .saturating_sub(distance)
        .saturating_mul(100)
        .saturating_add(maximum_distance / 2)
        / maximum_distance;
    FrameComparison::SimilarityPercent(u8::try_from(similarity).unwrap_or(100))
}

fn selection_for_frame_ids(
    manifest: &ProjectManifest,
    frame_ids: &[FrameId],
) -> Result<TimelineSelection, TimelineSelectionError> {
    let mut selection = TimelineSelection::new();
    let Some((first, rest)) = frame_ids.split_first() else {
        return Ok(selection);
    };
    let last = rest.last().unwrap_or(first);
    selection.select_only(&manifest.timeline, *last)?;
    selection.extend_range(&manifest.timeline, *first)?;
    Ok(selection)
}

const fn asset_issue_id(issue: &AssetIssue) -> gif_from_screen_domain::AssetId {
    match issue {
        AssetIssue::Missing { asset_id }
        | AssetIssue::LengthMismatch { asset_id, .. }
        | AssetIssue::DigestMismatch { asset_id } => *asset_id,
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
    /// Session-local clipboard history could not retain a Copy/Cut snapshot.
    #[error(transparent)]
    ClipboardHistory(#[from] FrameClipboardHistoryError),
    /// A selection or navigation request was invalid.
    #[error(transparent)]
    Selection(#[from] TimelineSelectionError),
    /// The requested time range was reversed or the timeline duration overflowed.
    #[error(transparent)]
    TimeRange(#[from] FrameTimeRangeError),
    /// The half-open range intersects no timeline frame.
    #[error("time range [{start_us}, {end_us})us contains no frames")]
    EmptyTimeRange {
        /// Inclusive project-relative start time.
        start_us: u64,
        /// Exclusive project-relative end time.
        end_us: u64,
    },
    /// Keeping this range would not remove any frame.
    #[error("all frames are already inside time range [{start_us}, {end_us})us")]
    NoFramesOutsideTimeRange {
        /// Inclusive project-relative start time.
        start_us: u64,
        /// Exclusive project-relative end time.
        end_us: u64,
    },
    /// The bounded synchronous duplicate scan would require too many rendered frames.
    #[error("exact duplicate scan selected {selected} frames; the synchronous limit is {maximum}")]
    DuplicateScanTooLarge {
        /// Number of selected frames requested by the UI.
        selected: usize,
        /// Maximum frames accepted by one synchronous scan.
        maximum: usize,
    },
    /// Paste was requested before a successful Copy or Cut.
    #[error("application frame clipboard is empty; copy or cut frames first")]
    EmptyClipboard,
    /// Overlay authoring requires visible track text.
    #[error("overlay track name must not be empty")]
    EmptyOverlayName,
    /// Overlay authoring requires at least one selected frame.
    #[error("select at least one frame before adding an overlay")]
    EmptyOverlaySelection,
    /// The selected overlay span could not be represented safely.
    #[error("selected overlay timeline span overflowed")]
    OverlayTimelineDurationOverflow,
    /// Raster overlay opacity settings would make the new track invisible.
    #[error("raster overlay item and track opacity must be greater than zero")]
    InvisibleRasterOverlay,
    /// Both source and display dimensions must be nonempty before storing pixels.
    #[error("raster overlay source and display dimensions must be greater than zero")]
    EmptyRasterOverlaySize,
    /// Caption attributes must be valid before its immutable pixels are registered.
    #[error(transparent)]
    Text(#[from] gif_from_screen_text::TextError),
    /// Pixels cannot silently use a different wrapping box than the saved attributes.
    #[error("text raster dimensions do not match the requested text box")]
    TextRasterDimensionsMismatch,
    /// Editing requires one nonempty, homogeneous track with prepared text pixels.
    #[error("overlay track {0} is missing or is not a single editable text group")]
    TextTrackNotEditable(TrackId),
    /// An asynchronous title operation must not silently use a new insertion point.
    #[error("title insertion frame {0} is no longer present")]
    UnknownTitleAnchor(FrameId),
    /// Added title duration must fit the project time representation.
    #[error("title frame would overflow the project duration")]
    TitleDurationOverflow,
    /// Display placement must stay within the project canvas.
    #[error("raster overlay placement must stay inside the project canvas")]
    RasterOverlayOutsideCanvas,
    /// Source dimensions cannot be represented as a raw RGBA byte length.
    #[error("raster overlay RGBA byte length overflowed")]
    RasterOverlayByteLengthOverflow,
    /// Decoded bytes disagree with their declared dimensions.
    #[error("raster overlay has {actual} RGBA bytes, expected {expected}")]
    RasterOverlayByteLengthMismatch {
        /// Required tightly packed RGBA byte count.
        expected: usize,
        /// Supplied decoded byte count.
        actual: usize,
    },
    /// Content addressing found the same bytes under incompatible raster metadata.
    #[error("raster overlay asset {asset_id} collides with incompatible raster metadata")]
    RasterOverlayAssetCollision {
        /// Existing content identity whose descriptor cannot represent this source.
        asset_id: gif_from_screen_domain::AssetId,
    },
    /// Session history must retain at least one entry.
    #[error("editor history limit must be greater than zero")]
    ZeroHistoryLimit,
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, io::Write};

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, BlendMode, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, FrameClip, OverlayContent, PhysicalPoint, PhysicalPx,
        PhysicalRect, PhysicalSize, ProjectId, ProjectManifest, ProjectRevision, QuarterTurn,
        RasterEncoding, Rgba, ShapeKind, SlideDirection, TextRaster, Timeline, TransitionKind,
        UnixTimeMs,
    };
    use tempfile::TempDir;

    use super::*;

    pub(super) fn frame_id(number: u128) -> FrameId {
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

    pub(super) fn create_workspace(
        directory: &TempDir,
        durations: &[u64],
        history_limit: usize,
    ) -> EditorWorkspace {
        let active = ActiveProject::create(directory.path(), manifest(durations)).unwrap();
        EditorWorkspace::from_active(active, history_limit).unwrap()
    }

    pub(super) fn create_rendered_duplicate_workspace(directory: &TempDir) -> EditorWorkspace {
        let size = PhysicalSize::new(2, 1).unwrap();
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(900),
            "duplicate-render-test",
            UnixTimeMs::new(1),
            Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        let mut active = ActiveProject::create(directory.path(), manifest).unwrap();
        let sources = [
            vec![255, 0, 0, 255, 10, 20, 30, 0],
            vec![255, 0, 0, 255, 200, 100, 50, 0],
            vec![9, 8, 7, 0, 255, 0, 0, 255],
            vec![0, 0, 0, 255, 50, 60, 70, 0],
        ];
        let mut descriptors = BTreeMap::new();
        let mut frames = Vec::new();
        for (index, pixels) in sources.into_iter().enumerate() {
            let asset_id = active.assets().put(&pixels).unwrap();
            descriptors.entry(asset_id).or_insert(AssetDescriptor {
                id: asset_id,
                byte_len: 8,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            });
            frames.push(FrameClip {
                id: frame_id(u128::try_from(index).unwrap() + 1),
                asset_id,
                duration: DurationUs::new(10 * (u64::try_from(index).unwrap() + 1)).unwrap(),
                transform: if index == 2 {
                    ClipTransform {
                        flip_horizontal: true,
                        ..ClipTransform::default()
                    }
                } else {
                    ClipTransform::default()
                },
                capture_metadata: CaptureMetadata::default(),
                effects: Vec::new(),
            });
        }
        let mut commands = descriptors
            .into_values()
            .map(|asset| EditCommand::RegisterAsset { asset })
            .collect::<Vec<_>>();
        commands.push(EditCommand::InsertFrames { index: 0, frames });
        active.commit(EditCommand::Compound { commands }).unwrap();
        active.checkpoint_and_compact().unwrap();
        EditorWorkspace::from_active(active, 16).unwrap()
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

    #[test]
    fn transform_helpers_persist_and_undo_as_workspace_edits() {
        let directory = tempfile::tempdir().unwrap();
        let crop = PhysicalRect::new(1, 1, 2, 2).unwrap();
        let output_size = PhysicalSize::new(8, 6).unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20], 16);
        workspace.select_only(frame_id(1)).unwrap();

        workspace.set_selection_crop(crop).unwrap();
        workspace.set_selection_output_size(output_size).unwrap();
        workspace.rotate_selection_clockwise().unwrap();
        workspace.toggle_selection_horizontal_flip().unwrap();
        workspace.toggle_selection_vertical_flip().unwrap();
        workspace.clear_selection_crop().unwrap();
        workspace.clear_selection_output_size().unwrap();
        workspace.rotate_selection_counterclockwise().unwrap();
        let transform = workspace.manifest().timeline.frames[0].transform;
        assert_eq!(transform.crop, None);
        assert_eq!(transform.output_size, None);
        assert_eq!(transform.rotation, QuarterTurn::Zero);
        assert!(transform.flip_horizontal);
        assert!(transform.flip_vertical);

        assert!(workspace.undo().unwrap());
        assert!(workspace.undo().unwrap());
        assert!(workspace.undo().unwrap());
        let restored = workspace.manifest().timeline.frames[0].transform;
        assert_eq!(restored.crop, Some(crop));
        assert_eq!(restored.output_size, Some(output_size));
        assert_eq!(restored.rotation, QuarterTurn::Clockwise90);
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(reopened.manifest().timeline.frames[0].transform, restored);
        assert!(reopened.is_dirty());
    }

    #[test]
    fn transform_helper_without_selection_is_a_friendly_editor_error() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10], 4);

        let error = workspace.rotate_selection_clockwise().unwrap_err();

        assert!(matches!(
            &error,
            EditorWorkspaceError::Editor(EditorError::EmptySelection)
        ));
        assert!(error.to_string().contains("at least one frame"));
        assert!(!workspace.is_dirty());
        assert!(!workspace.can_undo());
    }

    #[test]
    fn time_range_select_keep_undo_redo_and_reopen_are_consistent() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30, 40], 16);

        workspace
            .select_time_range(TimeUs::new(10), TimeUs::new(60))
            .unwrap();
        assert_eq!(selected(&workspace), [2, 3]);
        assert_eq!(workspace.selection().current(), Some(frame_id(2)));
        assert!(!workspace.is_dirty());

        workspace
            .keep_time_range(TimeUs::new(10), TimeUs::new(60))
            .unwrap();
        assert_eq!(order(&workspace), [2, 3]);
        assert_eq!(selected(&workspace), [2, 3]);
        assert!(workspace.is_dirty());
        assert!(workspace.undo().unwrap());
        assert_eq!(order(&workspace), [1, 2, 3, 4]);
        assert!(workspace.redo().unwrap());
        assert_eq!(order(&workspace), [2, 3]);
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(order(&reopened), [2, 3]);
        assert!(reopened.is_dirty());
    }

    #[test]
    fn delete_time_range_is_one_undoable_journal_edit() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30, 40], 8);

        workspace
            .delete_time_range(TimeUs::new(10), TimeUs::new(60))
            .unwrap();
        assert_eq!(order(&workspace), [1, 4]);
        assert!(workspace.undo().unwrap());
        assert_eq!(order(&workspace), [1, 2, 3, 4]);
        assert!(!workspace.undo().unwrap());
    }

    #[test]
    fn time_range_boundaries_and_empty_results_are_typed_without_state_changes() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20], 8);
        workspace.select_only(frame_id(2)).unwrap();
        let selection_before = workspace.selection().clone();
        let revision_before = workspace.manifest().revision;

        assert!(matches!(
            workspace.select_time_range(TimeUs::new(10), TimeUs::new(10)),
            Err(EditorWorkspaceError::EmptyTimeRange {
                start_us: 10,
                end_us: 10,
            })
        ));
        assert!(matches!(
            workspace.delete_time_range(TimeUs::new(31), TimeUs::new(40)),
            Err(EditorWorkspaceError::EmptyTimeRange { .. })
        ));
        assert!(matches!(
            workspace.keep_time_range(TimeUs::new(20), TimeUs::new(10)),
            Err(EditorWorkspaceError::TimeRange(
                FrameTimeRangeError::Reversed {
                    start_us: 20,
                    end_us: 10,
                }
            ))
        ));
        assert!(matches!(
            workspace.keep_time_range(TimeUs::ZERO, TimeUs::new(30)),
            Err(EditorWorkspaceError::NoFramesOutsideTimeRange { .. })
        ));
        assert_eq!(workspace.selection(), &selection_before);
        assert_eq!(workspace.manifest().revision, revision_before);
        assert!(!workspace.can_undo());
    }

    #[test]
    fn all_reduce_delay_modes_are_single_undoable_persistent_edits() {
        let cases = [
            (ReduceDelayMode::DontAdjust, vec![10, 30, 50]),
            (ReduceDelayMode::Previous, vec![30, 70, 50]),
            (ReduceDelayMode::Evenly, vec![30, 50, 70]),
        ];
        for (mode, expected_durations) in cases {
            let directory = tempfile::tempdir().unwrap();
            let mut workspace = create_workspace(&directory, &[10, 20, 30, 40, 50], 8);
            workspace.select_all();

            workspace.reduce_selection(2, mode).unwrap();
            assert_eq!(order(&workspace), [1, 3, 5]);
            assert_eq!(durations(&workspace), expected_durations);
            assert!(workspace.undo().unwrap());
            assert_eq!(order(&workspace), [1, 2, 3, 4, 5]);
            assert!(!workspace.undo().unwrap());
            assert!(workspace.redo().unwrap());
            assert_eq!(order(&workspace), [1, 3, 5]);
            drop(workspace);

            let reopened =
                EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 8).unwrap();
            assert_eq!(order(&reopened), [1, 3, 5]);
            assert_eq!(durations(&reopened), expected_durations);
        }
    }

    #[test]
    fn yoyo_selection_and_entire_timeline_are_journaled_and_reversible() {
        let selection_directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&selection_directory, &[10, 20, 30, 40], 8);
        workspace.select_only(frame_id(2)).unwrap();
        workspace.toggle_selection(frame_id(3)).unwrap();

        workspace.yoyo(YoyoScope::Selection, true).unwrap();
        assert_eq!(workspace.manifest().timeline.frames.len(), 6);
        let frames = &workspace.manifest().timeline.frames;
        assert_eq!(frames[3].asset_id, frames[2].asset_id);
        assert_eq!(frames[3].duration, frames[2].duration);
        assert_eq!(frames[4].asset_id, frames[1].asset_id);
        assert_eq!(frames[4].duration, frames[1].duration);
        assert_ne!(frames[3].id, frames[2].id);
        assert_ne!(frames[4].id, frames[1].id);
        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.manifest().timeline.frames.len(), 4);
        assert!(workspace.redo().unwrap());
        assert_eq!(workspace.manifest().timeline.frames.len(), 6);
        drop(workspace);
        let reopened =
            EditorWorkspace::open(selection_directory.path(), LockPolicy::FailIfPresent, 8)
                .unwrap();
        assert_eq!(reopened.manifest().timeline.frames.len(), 6);

        let entire_directory = tempfile::tempdir().unwrap();
        let mut entire = create_workspace(&entire_directory, &[10, 20, 30, 40], 8);
        assert!(entire.selection().is_empty());
        entire.yoyo(YoyoScope::EntireTimeline, false).unwrap();
        assert_eq!(entire.manifest().timeline.frames.len(), 6);
        assert_eq!(entire.manifest().timeline.frames[4].duration.get(), 30);
        assert_eq!(entire.manifest().timeline.frames[5].duration.get(), 20);
    }

    #[test]
    fn duration_adjust_and_scale_remain_journal_backed_with_undo() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[100], 8);
        workspace.select_only(frame_id(1)).unwrap();

        workspace.adjust_selection_duration(20).unwrap();
        workspace.scale_selection_duration(50).unwrap();
        assert_eq!(durations(&workspace), [60]);
        assert!(workspace.undo().unwrap());
        assert_eq!(durations(&workspace), [120]);
        assert!(workspace.undo().unwrap());
        assert_eq!(durations(&workspace), [100]);
    }

    #[test]
    fn exact_duplicate_scan_uses_final_rendered_transparent_pixels_and_is_reversible() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&directory);
        workspace.select_all();

        workspace
            .remove_duplicate_selection(
                100,
                DuplicateFrameRetention::First,
                DuplicateDelayMode::Sum,
            )
            .unwrap();
        assert_eq!(order(&workspace), [1, 4]);
        assert_eq!(durations(&workspace), [60, 40]);
        assert!(workspace.undo().unwrap());
        assert_eq!(order(&workspace), [1, 2, 3, 4]);
        assert_eq!(durations(&workspace), [10, 20, 30, 40]);
        assert!(workspace.redo().unwrap());
        assert_eq!(order(&workspace), [1, 4]);
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(order(&reopened), [1, 4]);
        assert_eq!(durations(&reopened), [60, 40]);
    }

    #[test]
    fn duplicate_threshold_retention_and_average_delay_are_honored() {
        let exact_directory = tempfile::tempdir().unwrap();
        let mut exact = create_rendered_duplicate_workspace(&exact_directory);
        exact.select_all();
        exact
            .remove_duplicate_selection(
                100,
                DuplicateFrameRetention::Last,
                DuplicateDelayMode::Average,
            )
            .unwrap();
        assert_eq!(order(&exact), [3, 4]);
        assert_eq!(durations(&exact), [20, 40]);

        let threshold_directory = tempfile::tempdir().unwrap();
        let mut threshold = create_rendered_duplicate_workspace(&threshold_directory);
        threshold.select_all();
        threshold
            .remove_duplicate_selection(80, DuplicateFrameRetention::Last, DuplicateDelayMode::Keep)
            .unwrap();
        assert_eq!(order(&threshold), [4]);
        assert_eq!(durations(&threshold), [40]);
    }

    #[test]
    fn synchronous_duplicate_scan_rejects_unbounded_selection_before_rendering() {
        let directory = tempfile::tempdir().unwrap();
        let durations = vec![1; MAX_SYNCHRONOUS_DUPLICATE_SCAN_FRAMES + 1];
        let mut workspace = create_workspace(&directory, &durations, 4);
        workspace.select_all();

        assert!(matches!(
            workspace.remove_duplicate_selection(
                100,
                DuplicateFrameRetention::First,
                DuplicateDelayMode::Keep,
            ),
            Err(EditorWorkspaceError::DuplicateScanTooLarge {
                selected,
                maximum,
            }) if selected == MAX_SYNCHRONOUS_DUPLICATE_SCAN_FRAMES + 1
                && maximum == MAX_SYNCHRONOUS_DUPLICATE_SCAN_FRAMES
        ));
        assert!(!workspace.is_dirty());
        assert!(!workspace.can_undo());
    }

    #[test]
    fn frame_effects_update_final_render_and_persist_with_single_step_undo() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&directory);
        let frame_id = frame_id(1);
        workspace.select_only(frame_id).unwrap();
        let before = render_frame_surface(
            workspace.active_project(),
            frame_id,
            DUPLICATE_RENDER_SURFACE_LIMIT_BYTES,
        )
        .unwrap();

        workspace
            .add_selection_effect(Effect::Darken {
                region: PhysicalRect::new(0, 0, 2, 1).unwrap(),
                amount_percent: 100,
            })
            .unwrap();
        let darkened = render_frame_surface(
            workspace.active_project(),
            frame_id,
            DUPLICATE_RENDER_SURFACE_LIMIT_BYTES,
        )
        .unwrap();
        assert_eq!(&darkened.pixels()[..4], &[0, 0, 0, 255]);
        assert!(workspace.undo().unwrap());
        let restored = render_frame_surface(
            workspace.active_project(),
            frame_id,
            DUPLICATE_RENDER_SURFACE_LIMIT_BYTES,
        )
        .unwrap();
        assert_eq!(restored, before);
        assert!(workspace.redo().unwrap());

        workspace
            .replace_selection_effect(
                0,
                Effect::Lighten {
                    region: PhysicalRect::new(0, 0, 2, 1).unwrap(),
                    amount_percent: 100,
                },
            )
            .unwrap();
        let lightened = render_frame_surface(
            workspace.active_project(),
            frame_id,
            DUPLICATE_RENDER_SURFACE_LIMIT_BYTES,
        )
        .unwrap();
        assert_eq!(&lightened.pixels()[..4], &[255, 255, 255, 255]);

        workspace.clear_selection_effects().unwrap();
        assert!(workspace.manifest().timeline.frames[0].effects.is_empty());
        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.manifest().timeline.frames[0].effects.len(), 1);
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 16).unwrap();
        assert_eq!(reopened.manifest().timeline.frames[0].effects.len(), 1);
        let reopened_render = render_frame_surface(
            reopened.active_project(),
            frame_id,
            DUPLICATE_RENDER_SURFACE_LIMIT_BYTES,
        )
        .unwrap();
        assert_eq!(&reopened_render.pixels()[..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn clean_save_and_repair_noop_preserve_undo_history() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[100], 8);
        workspace.select_only(frame_id(1)).unwrap();
        workspace.adjust_selection_duration(20).unwrap();
        assert!(workspace.is_dirty());
        assert!(workspace.can_undo());

        assert_eq!(workspace.repair_journal().unwrap(), None);
        assert!(workspace.is_dirty());
        assert!(workspace.can_undo());
        workspace.checkpoint().unwrap();
        assert!(!workspace.is_dirty());
        assert!(workspace.can_undo());
        assert!(workspace.undo().unwrap());
        assert_eq!(durations(&workspace), [100]);
        assert!(workspace.is_dirty());
        workspace.checkpoint_and_compact().unwrap();
        assert!(!workspace.is_dirty());
        assert!(!workspace.can_undo());
        assert!(workspace.can_redo());
    }

    #[test]
    fn invalid_tail_repair_preserves_forensics_assets_and_restores_editability() {
        let directory = tempfile::tempdir().unwrap();
        drop(ActiveProject::create(directory.path(), manifest(&[100])).unwrap());
        let journal = directory.path().join("journal.ndjson");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap();
        file.write_all(b"{torn-journal\n").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
        assert!(!opened.journal_recovery.is_clean());
        let mut workspace = EditorWorkspace::from_opened(opened, 8).unwrap();
        let asset_issues = workspace.asset_issues().to_vec();
        assert!(workspace.journal_requires_repair());
        assert!(workspace.is_dirty());

        let preserved = workspace.repair_journal().unwrap().unwrap();
        assert!(preserved.is_file());
        assert!(
            std::fs::read(&preserved)
                .unwrap()
                .windows(b"{torn-journal".len())
                .any(|window| window == b"{torn-journal")
        );
        assert!(!workspace.journal_requires_repair());
        assert!(!workspace.is_dirty());
        assert_eq!(workspace.asset_issues(), asset_issues);
        let recovery = workspace.journal_recovery().unwrap();
        assert!(recovery.is_clean());
        assert_eq!(recovery.snapshot_revision, workspace.manifest().revision);
        assert_eq!(recovery.replayed_records, 0);

        workspace.select_first().unwrap();
        workspace.adjust_selection_duration(20).unwrap();
        assert!(workspace.undo().unwrap());
        assert_eq!(durations(&workspace), [100]);
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 8).unwrap();
        assert!(reopened.journal_recovery().unwrap().is_clean());
        assert_eq!(durations(&reopened), [100]);
        assert_eq!(reopened.asset_issues(), asset_issues);
    }

    #[test]
    fn copy_paste_is_bounded_journaled_undoable_and_reopenable() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30, 40], 8);
        workspace.select_only(frame_id(2)).unwrap();
        workspace.toggle_selection(frame_id(3)).unwrap();
        let revision = workspace.manifest().revision;

        assert_eq!(workspace.copy_selection().unwrap(), 2);
        assert_eq!(workspace.clipboard_len(), 2);
        assert_eq!(workspace.clipboard_history_len(), 1);
        assert_eq!(workspace.manifest().revision, revision);
        assert!(!workspace.can_undo());

        workspace.select_only(frame_id(1)).unwrap();
        assert_eq!(workspace.paste_after_current().unwrap(), 2);
        assert_eq!(workspace.manifest().timeline.frames.len(), 6);
        let pasted_ids = [
            workspace.manifest().timeline.frames[1].id,
            workspace.manifest().timeline.frames[2].id,
        ];
        assert!(!pasted_ids.contains(&frame_id(2)));
        assert!(!pasted_ids.contains(&frame_id(3)));
        assert_eq!(workspace.clipboard_len(), 2);
        assert!(workspace.undo().unwrap());
        assert_eq!(order(&workspace), [1, 2, 3, 4]);
        assert!(workspace.redo().unwrap());
        assert_eq!(workspace.manifest().timeline.frames[1].id, pasted_ids[0]);
        assert_eq!(workspace.manifest().timeline.frames[2].id, pasted_ids[1]);
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 8).unwrap();
        assert_eq!(reopened.manifest().timeline.frames.len(), 6);
        assert_eq!(reopened.manifest().timeline.frames[1].id, pasted_ids[0]);
        assert_eq!(reopened.clipboard_len(), 0);
        assert_eq!(reopened.clipboard_history_len(), 0);
    }

    #[test]
    fn cut_and_failed_copy_append_clipboard_only_after_success() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30, 40], 8);
        workspace.select_only(frame_id(1)).unwrap();
        workspace.copy_selection().unwrap();
        assert_eq!(workspace.clipboard_len(), 1);
        assert_eq!(workspace.clipboard_history_len(), 1);

        workspace.select_only(frame_id(2)).unwrap();
        workspace.toggle_selection(frame_id(3)).unwrap();
        assert_eq!(workspace.cut_selection().unwrap(), 2);
        assert_eq!(workspace.clipboard_len(), 2);
        assert_eq!(workspace.clipboard_history_len(), 2);
        assert_eq!(order(&workspace), [1, 4]);
        assert!(workspace.undo().unwrap());
        assert_eq!(order(&workspace), [1, 2, 3, 4]);

        workspace.select_all();
        assert!(matches!(
            workspace.cut_selection(),
            Err(EditorWorkspaceError::Editor(
                EditorError::CutWouldEmptyTimeline { .. }
            ))
        ));
        assert_eq!(workspace.clipboard_len(), 2);
        assert_eq!(workspace.clipboard_history_len(), 2);
        workspace.clear_selection();
        assert!(workspace.copy_selection().is_err());
        assert_eq!(workspace.clipboard_len(), 2);
        assert_eq!(workspace.clipboard_history_len(), 2);
    }

    #[test]
    fn paste_without_clipboard_is_friendly_and_does_not_create_history() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10], 4);

        assert!(matches!(
            workspace.paste_after_current(),
            Err(EditorWorkspaceError::EmptyClipboard)
        ));
        assert_eq!(workspace.clipboard_history_len(), 0);
        assert!(!workspace.is_dirty());
        assert!(!workspace.can_undo());
    }

    #[test]
    fn clipboard_history_select_remove_clear_and_capacity_control_paste_source() {
        let directory = tempfile::tempdir().unwrap();
        let durations = (1..=10).map(|value| value * 10).collect::<Vec<_>>();
        let mut workspace = create_workspace(&directory, &durations, 16);
        for number in 1..=10 {
            workspace.select_only(frame_id(number)).unwrap();
            workspace.copy_selection().unwrap();
        }

        assert_eq!(workspace.clipboard_history_len(), 8);
        let retained = workspace
            .clipboard_history_entries()
            .map(|entry| (entry.id(), entry.clipboard().frames()[0].id))
            .collect::<Vec<_>>();
        assert_eq!(retained.first().unwrap().0.get(), 3);
        assert_eq!(retained.first().unwrap().1, frame_id(3));
        assert_eq!(retained.last().unwrap().1, frame_id(10));

        let oldest = retained.first().unwrap().0;
        assert!(workspace.select_clipboard_entry(oldest));
        assert_eq!(workspace.clipboard_len(), 1);
        workspace.select_only(frame_id(10)).unwrap();
        workspace.paste_after_current().unwrap();
        assert_eq!(
            workspace
                .manifest()
                .timeline
                .frames
                .last()
                .unwrap()
                .duration
                .get(),
            30
        );

        assert_eq!(workspace.remove_clipboard_entry(oldest), Some(1));
        assert_eq!(workspace.clipboard_history_len(), 7);
        assert_eq!(workspace.clipboard_len(), 1);
        assert_eq!(
            workspace
                .clipboard_history_entries()
                .find(|entry| Some(entry.id()) == workspace.selected_clipboard_id())
                .unwrap()
                .clipboard()
                .frames()[0]
                .id,
            frame_id(10)
        );

        workspace.clear_clipboard_history();
        assert_eq!(workspace.clipboard_history_len(), 0);
        assert!(matches!(
            workspace.paste_after_current(),
            Err(EditorWorkspaceError::EmptyClipboard)
        ));
    }

    #[test]
    fn failed_cut_commit_preserves_manifest_and_clipboard_history() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30], 8);
        workspace.select_only(frame_id(1)).unwrap();
        workspace.copy_selection().unwrap();
        let manifest_before = workspace.manifest().clone();
        let clipboard_before = workspace
            .clipboard_history_entries()
            .map(FrameClipboardHistoryEntry::id)
            .collect::<Vec<_>>();

        workspace.select_only(frame_id(2)).unwrap();
        let journal = workspace.project_root().join("journal.ndjson");
        std::fs::remove_file(&journal).unwrap();
        std::fs::create_dir(&journal).unwrap();
        assert!(matches!(
            workspace.cut_selection(),
            Err(EditorWorkspaceError::Project(
                ProjectError::JournalCommitFailed { .. }
            ))
        ));
        assert_eq!(workspace.manifest(), &manifest_before);
        assert_eq!(
            workspace
                .clipboard_history_entries()
                .map(FrameClipboardHistoryEntry::id)
                .collect::<Vec<_>>(),
            clipboard_before
        );
        assert_eq!(workspace.clipboard_len(), 1);
    }

    #[test]
    fn current_transition_create_replace_delete_is_journaled_undoable_and_reopenable() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30], 8);
        workspace.select_only(frame_id(1)).unwrap();
        workspace
            .set_current_transition(FrameTransitionSettings {
                duration: DurationUs::new(4).unwrap(),
                steps: 2,
                kind: TransitionKind::FadeToNext,
            })
            .unwrap();
        let transition_id = workspace.current_transition().unwrap().id;
        assert_eq!(workspace.manifest().timeline.transitions.len(), 1);

        workspace
            .set_current_transition(FrameTransitionSettings {
                duration: DurationUs::new(9).unwrap(),
                steps: 3,
                kind: TransitionKind::Slide {
                    direction: SlideDirection::Right,
                },
            })
            .unwrap();
        assert_eq!(workspace.manifest().timeline.transitions.len(), 1);
        assert_eq!(workspace.current_transition().unwrap().id, transition_id);
        assert!(matches!(
            workspace.current_transition().unwrap().kind,
            TransitionKind::Slide {
                direction: SlideDirection::Right
            }
        ));

        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.current_transition().unwrap().steps, 2);
        assert!(matches!(
            workspace.current_transition().unwrap().kind,
            TransitionKind::FadeToNext
        ));
        assert!(workspace.redo().unwrap());
        assert_eq!(workspace.current_transition().unwrap().steps, 3);

        workspace.remove_current_transition().unwrap();
        assert!(workspace.current_transition().is_none());
        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.current_transition().unwrap().id, transition_id);
        drop(workspace);

        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 8).unwrap();
        let transition = reopened.current_transition().unwrap();
        assert_eq!(transition.id, transition_id);
        assert_eq!(transition.duration, DurationUs::new(9).unwrap());
        assert_eq!(transition.steps, 3);
        assert_eq!(reopened.manifest().revision, ProjectRevision::new(6));
    }

    #[test]
    fn shape_overlay_tracks_span_selection_and_are_journaled_undoable_edits() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30], 8);
        workspace.select_only(frame_id(1)).unwrap();
        workspace.toggle_selection(frame_id(3)).unwrap();
        let content = OverlayContent::Shape {
            kind: ShapeKind::Rectangle,
            bounds: PhysicalRect::new(0, 0, 2, 2).unwrap(),
            stroke_width: 1,
            stroke: Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            fill: None,
        };

        let track_id = workspace
            .add_overlay_for_selection("Callout".to_owned(), content, 7, 200, BlendMode::Screen)
            .unwrap();
        let track = &workspace.manifest().timeline.overlay_tracks[0];
        assert_eq!(track.id, track_id);
        assert_eq!(track.name, "Callout");
        assert_eq!(track.opacity, 200);
        assert_eq!(track.blend_mode, BlendMode::Screen);
        assert_eq!(track.items[0].span.start, TimeUs::ZERO);
        assert_eq!(track.items.len(), 2);
        assert_eq!(track.items[0].span.duration, DurationUs::new(10).unwrap());
        assert_eq!(track.items[1].span.start, TimeUs::new(30));
        assert_eq!(track.items[1].span.duration, DurationUs::new(30).unwrap());
        assert_ne!(track.items[0].id, track.items[1].id);
        assert_eq!(track.items[0].content, track.items[1].content);
        assert_eq!(track.items[0].z_index, 7);

        assert!(workspace.undo().unwrap());
        assert!(workspace.manifest().timeline.overlay_tracks.is_empty());
        assert!(workspace.redo().unwrap());
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0].id, track_id);
        workspace.remove_overlay_track(track_id).unwrap();
        assert!(workspace.manifest().timeline.overlay_tracks.is_empty());
        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0].id, track_id);

        let revision = workspace.manifest().revision;
        workspace.clear_selection();
        assert!(
            workspace
                .add_overlay_for_selection(
                    "Shape".to_owned(),
                    OverlayContent::Shape {
                        kind: ShapeKind::Line,
                        bounds: PhysicalRect::new(0, 0, 1, 1).unwrap(),
                        stroke_width: 1,
                        stroke: Rgba {
                            red: 0,
                            green: 0,
                            blue: 0,
                            alpha: 255,
                        },
                        fill: None,
                    },
                    0,
                    255,
                    BlendMode::Normal,
                )
                .is_err()
        );
        assert_eq!(workspace.manifest().revision, revision);
    }

    #[test]
    fn raster_overlay_import_reuses_compatible_content_and_rejects_geometry_collisions() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20], 8);
        workspace.select_all();
        let shared_pixels = vec![7_u8; 48];
        let first = workspace
            .add_raster_overlay_for_selection(
                RasterOverlayEdit {
                    name: "First logo".to_owned(),
                    source_size: PhysicalSize::new(4, 3).unwrap(),
                    position: PhysicalPoint {
                        x: PhysicalPx::new(1),
                        y: PhysicalPx::new(1),
                    },
                    display_size: PhysicalSize::new(2, 2).unwrap(),
                    item_opacity: 220,
                    track_opacity: 200,
                    blend_mode: BlendMode::Multiply,
                    z_index: 4,
                },
                &shared_pixels,
            )
            .unwrap();
        assert_eq!(workspace.manifest().assets.len(), 2);
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0].id, first);
        let second = workspace
            .add_raster_overlay_for_selection(
                RasterOverlayEdit {
                    name: "Reused logo".to_owned(),
                    source_size: PhysicalSize::new(4, 3).unwrap(),
                    position: PhysicalPoint::default(),
                    display_size: PhysicalSize::new(1, 1).unwrap(),
                    item_opacity: 255,
                    track_opacity: 255,
                    blend_mode: BlendMode::Normal,
                    z_index: 5,
                },
                &shared_pixels,
            )
            .unwrap();
        assert_eq!(workspace.manifest().assets.len(), 2);
        assert_eq!(workspace.manifest().timeline.overlay_tracks[1].id, second);

        let revision = workspace.manifest().revision;
        assert!(matches!(
            workspace.add_raster_overlay_for_selection(
                RasterOverlayEdit {
                    name: "Collision".to_owned(),
                    source_size: PhysicalSize::new(3, 4).unwrap(),
                    position: PhysicalPoint::default(),
                    display_size: PhysicalSize::new(1, 1).unwrap(),
                    item_opacity: 255,
                    track_opacity: 255,
                    blend_mode: BlendMode::Normal,
                    z_index: 0,
                },
                &shared_pixels,
            ),
            Err(EditorWorkspaceError::RasterOverlayAssetCollision { .. })
        ));
        assert_eq!(workspace.manifest().revision, revision);

        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.manifest().assets.len(), 2);
        assert_eq!(workspace.manifest().timeline.overlay_tracks.len(), 1);
        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.manifest().assets.len(), 1);
        assert!(workspace.manifest().timeline.overlay_tracks.is_empty());

        let new_pixels = [255, 0, 0, 128];
        let added = workspace
            .add_raster_overlay_for_selection(
                RasterOverlayEdit {
                    name: "Logo".to_owned(),
                    source_size: PhysicalSize::new(1, 1).unwrap(),
                    position: PhysicalPoint::default(),
                    display_size: PhysicalSize::new(1, 1).unwrap(),
                    item_opacity: 255,
                    track_opacity: 255,
                    blend_mode: BlendMode::Normal,
                    z_index: 5,
                },
                &new_pixels,
            )
            .unwrap();
        assert_eq!(workspace.manifest().assets.len(), 2);
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0].id, added);
        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.manifest().assets.len(), 1);
        assert!(workspace.redo().unwrap());
        assert_eq!(workspace.manifest().assets.len(), 2);
    }

    #[test]
    fn overlay_preflight_rejects_empty_selection_and_sizes_before_asset_storage() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30], 8);
        let rgba = [17, 29, 41, 255];
        let asset_path = workspace
            .project
            .assets()
            .asset_path(AssetStore::id_for_bytes(&rgba));
        for invalid in 0..3 {
            workspace.select_only(frame_id(1)).unwrap();
            let mut edit = RasterOverlayEdit {
                name: "Logo".to_owned(),
                source_size: PhysicalSize::new(1, 1).unwrap(),
                position: PhysicalPoint::default(),
                display_size: PhysicalSize::new(1, 1).unwrap(),
                item_opacity: 255,
                track_opacity: 255,
                blend_mode: BlendMode::Normal,
                z_index: 0,
            };
            match invalid {
                0 => workspace.clear_selection(),
                1 => edit.source_size.width = PhysicalPx::ZERO,
                _ => edit.display_size.height = PhysicalPx::ZERO,
            }
            let before = workspace.manifest().clone();
            assert!(
                workspace
                    .add_raster_overlay_for_selection(edit, &rgba)
                    .is_err()
            );
            assert_eq!(workspace.manifest(), &before);
            assert!(!asset_path.exists());
        }
    }

    #[test]
    fn overlay_target_and_drawing_draft_reject_navigation_edits_and_other_project_paths() {
        use crate::editor_ui::{DrawingDraftPhase, DrawingOverlayDraft};

        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20, 30], 8);
        workspace.select_only(frame_id(1)).unwrap();
        let original = workspace.overlay_selection_anchor().unwrap();
        assert!(original.matches(&workspace));
        let mut draft = DrawingOverlayDraft::default();
        draft.begin_for_selection(&workspace).unwrap();
        draft.reconcile(&workspace);
        assert_eq!(draft.phase, DrawingDraftPhase::Capturing);
        workspace.select_next().unwrap();
        assert!(!original.matches(&workspace));
        draft.reconcile(&workspace);
        assert_eq!(draft.phase, DrawingDraftPhase::Idle);
        workspace.select_only(frame_id(1)).unwrap();
        assert!(original.matches(&workspace));
        workspace
            .override_selection_duration(DurationUs::new(11).unwrap())
            .unwrap();
        assert!(!original.matches(&workspace));

        let second_directory = tempfile::tempdir().unwrap();
        let mut other = create_workspace(&second_directory, &[10, 20, 30], 8);
        other.select_only(frame_id(1)).unwrap();
        assert!(!original.matches(&other));
    }

    #[test]
    fn text_overlays_persist_editable_attributes_pixels_and_disjoint_spans_through_reopen() {
        use gif_from_screen_domain::HorizontalAlignment;
        use gif_from_screen_text::{TextImage, TextRequest};

        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&directory);
        workspace.select_only(frame_id(1)).unwrap();
        workspace.toggle_selection(frame_id(3)).unwrap();
        let request = TextRequest {
            text: "A\nB".to_owned(),
            font_family: "saved test font".to_owned(),
            font_size_px: 12,
            size: PhysicalSize::new(1, 1).unwrap(),
            foreground: Rgba {
                red: 5,
                green: 6,
                blue: 7,
                alpha: 255,
            },
            background: Some(Rgba::TRANSPARENT),
            alignment: HorizontalAlignment::End,
        };
        let image = TextImage {
            size: request.size,
            rgba: vec![0, 255, 0, 255],
        };
        let position = PhysicalPoint::default();
        let original_assets = workspace.manifest().assets.len();
        let track_id = workspace
            .add_text_overlay_for_selection(&request, &image, position)
            .unwrap();
        let track = workspace.manifest().timeline.overlay_tracks[0].clone();
        assert_eq!(track.id, track_id);
        assert_eq!(track.items.len(), 2);
        assert_eq!(
            track.items[0].content,
            OverlayContent::Text {
                text: request.text.clone(),
                position,
                max_width: Some(request.size.width),
                font_family: request.font_family.clone(),
                font_size_px: request.font_size_px,
                foreground: request.foreground,
                background: request.background,
                alignment: request.alignment,
                raster: Some(TextRaster {
                    asset_id: AssetStore::id_for_bytes(&image.rgba),
                    size: image.size
                }),
            }
        );
        assert_eq!(workspace.manifest().assets.len(), original_assets + 1);
        for frame_number in [1, 3] {
            let rendered =
                render_frame_surface(workspace.active_project(), frame_id(frame_number), 1024)
                    .unwrap();
            assert_eq!(&rendered.pixels()[..4], image.rgba.as_slice());
        }
        let gap = render_frame_surface(workspace.active_project(), frame_id(2), 1024).unwrap();
        assert_eq!(&gap.pixels()[..4], &[255, 0, 0, 255]);
        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.manifest().assets.len(), original_assets);
        assert!(workspace.manifest().timeline.overlay_tracks.is_empty());
        assert!(workspace.redo().unwrap());
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0], track);
        drop(workspace);
        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 8).unwrap();
        assert!(reopened.asset_issues().is_empty());
        assert_eq!(reopened.manifest().timeline.overlay_tracks[0], track);
        let rendered = render_frame_surface(reopened.active_project(), frame_id(1), 1024).unwrap();
        assert_eq!(&rendered.pixels()[..4], image.rgba.as_slice());

        let output = directory.path().join("text.gif");
        gif_from_screen_application::export_project_snapshot_to_gif(
            &gif_from_screen_application::ProjectExportSnapshot::from_active(
                reopened.active_project(),
            ),
            &output,
            &gif_from_screen_application::ProjectGifExportOptions::default(),
            &gif_from_screen_gif::NeverCancel,
            &mut gif_from_screen_application::NoopProjectExportProgress,
        )
        .unwrap();
        let decoded = gif_from_screen_media::decode_gif(
            std::fs::File::open(output).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!(&decoded.frames()[0].rgba()[..4], image.rgba.as_slice());
        assert_eq!(&decoded.frames()[1].rgba()[..4], &[255, 0, 0, 255]);
        assert_eq!(&decoded.frames()[2].rgba()[..4], image.rgba.as_slice());
    }

    #[test]
    fn text_overlay_rejects_mismatched_box_before_asset_storage() {
        use gif_from_screen_domain::HorizontalAlignment;
        use gif_from_screen_text::{TextImage, TextRequest};

        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[100], 8);
        workspace.select_all();
        let request = TextRequest {
            text: "Caption".to_owned(),
            font_family: "sans-serif".to_owned(),
            font_size_px: 12,
            size: PhysicalSize::new(2, 1).unwrap(),
            foreground: Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            background: None,
            alignment: HorizontalAlignment::Start,
        };
        let image = TextImage {
            size: PhysicalSize::new(1, 1).unwrap(),
            rgba: vec![7; 4],
        };
        let before = workspace.manifest().clone();
        assert!(matches!(
            workspace.add_text_overlay_for_selection(&request, &image, PhysicalPoint::default()),
            Err(EditorWorkspaceError::TextRasterDimensionsMismatch)
        ));
        assert_eq!(workspace.manifest(), &before);
        assert!(
            !workspace
                .project
                .assets()
                .asset_path(AssetStore::id_for_bytes(&image.rgba))
                .exists()
        );
    }

    #[test]
    fn current_transition_requires_a_selected_nonfinal_frame_without_polluting_history() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_workspace(&directory, &[10, 20], 8);
        let settings = FrameTransitionSettings {
            duration: DurationUs::new(1).unwrap(),
            steps: 1,
            kind: TransitionKind::FadeToColor {
                color: Rgba::TRANSPARENT,
            },
        };

        assert!(matches!(
            workspace.set_current_transition(settings.clone()),
            Err(EditorWorkspaceError::Editor(
                EditorError::NoCurrentFrameForTransition
            ))
        ));
        workspace.select_last().unwrap();
        assert!(matches!(
            workspace.set_current_transition(settings),
            Err(EditorWorkspaceError::Editor(
                EditorError::NoFrameAfterTransitionAnchor(frame)
            )) if frame == frame_id(2)
        ));
        assert!(matches!(
            workspace.remove_current_transition(),
            Err(EditorWorkspaceError::Editor(
                EditorError::NoFrameAfterTransitionAnchor(frame)
            )) if frame == frame_id(2)
        ));
        assert!(!workspace.is_dirty());
        assert!(!workspace.can_undo());
        assert!(!workspace.can_redo());
    }
}
