#![allow(
    dead_code,
    reason = "the persistent editor view-model precedes its egui integration"
)]

use std::{collections::BTreeSet, path::Path};

use gif_from_screen_domain::{
    DurationUs, EditCommand, Effect, FrameId, PhysicalRect, PhysicalSize, ProjectManifest, TimeUs,
};
use gif_from_screen_editor::{
    ClipTransformEdit, DuplicateDelayMode, DuplicateFrameRetention, EditorError, FrameComparison,
    FrameEffectEdit, FrameSimilarityProvider, FrameTimeRangeError, ReduceDelayMode, ReduceOptions,
    RemoveDuplicateFramesOptions, TimelineSelection, TimelineSelectionError, YoyoOptions,
    YoyoScope, adjust_duration, delete_frames, delete_frames_after, delete_frames_before,
    edit_clip_transforms, edit_frame_effects, move_selected_left, move_selected_right,
    override_duration, reduce_frames, remove_duplicate_frames, reverse_selected, scale_duration,
    select_frames_by_time_range, yoyo_frames,
};
use gif_from_screen_project::{
    ActiveProject, AssetIssue, JournalRecoveryReport, LockPolicy, OpenedProject, ProjectError,
};
use gif_from_screen_render::RgbaSurface;
use thiserror::Error;
use uuid::Uuid;

use crate::editor_preview::{EditorPreviewError, render_frame_surface};

const MAX_SYNCHRONOUS_DUPLICATE_SCAN_FRAMES: usize = 256;
const DUPLICATE_RENDER_SURFACE_LIMIT_BYTES: usize = 128 * 1024 * 1024;

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

    fn execute_selection_effect(
        &mut self,
        edit: &FrameEffectEdit,
    ) -> Result<(), EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        let command = edit_frame_effects(self.project.manifest(), selected, edit)?;
        self.execute(command)
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
        ProjectRevision, QuarterTurn, RasterEncoding, Timeline, UnixTimeMs,
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

    fn create_rendered_duplicate_workspace(directory: &TempDir) -> EditorWorkspace {
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
}
