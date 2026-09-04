#![forbid(unsafe_code)]

//! Pure timeline editing services built on serializable domain commands.

use std::collections::BTreeSet;

use gif_from_screen_domain::{
    DomainError, DurationUs, EditCommand, FrameDurationChange, FrameId, ProjectManifest,
};
use thiserror::Error;

/// Controls how removing frames affects the duration of the surviving timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReduceDelayMode {
    /// Keep every surviving frame's duration unchanged.
    DontAdjust,
    /// Add each removed frame's duration to the nearest preceding retained frame.
    Previous,
    /// Distribute the removed duration exactly across all retained frames in the selection.
    Evenly,
}

/// Options for reducing a consecutive frame selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReduceOptions {
    /// Retains the first selected frame and then every Nth selected frame.
    ///
    /// This value must be at least two.
    pub keep_every: usize,
    /// Controls whether and how the duration of removed frames is preserved.
    pub delay_mode: ReduceDelayMode,
}

/// A project plus bounded, command-based undo and redo stacks.
#[derive(Debug)]
pub struct EditorSession {
    project: ProjectManifest,
    undo: Vec<EditCommand>,
    redo: Vec<EditCommand>,
    history_limit: usize,
}

impl EditorSession {
    /// Creates an editor session after validating the supplied project.
    ///
    /// # Errors
    ///
    /// Returns an error when the project is invalid or `history_limit` is zero.
    pub fn new(project: ProjectManifest, history_limit: usize) -> Result<Self, EditorError> {
        project.validate()?;
        if history_limit == 0 {
            return Err(EditorError::ZeroHistoryLimit);
        }
        Ok(Self {
            project,
            undo: Vec::new(),
            redo: Vec::new(),
            history_limit,
        })
    }

    /// Returns the current immutable project snapshot.
    pub const fn project(&self) -> &ProjectManifest {
        &self.project
    }

    /// Returns true when an edit can be undone.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Returns true when an undone edit can be reapplied.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Applies one atomic edit, records its inverse, and clears redo history.
    ///
    /// # Errors
    ///
    /// Returns an error when the command violates a project invariant.
    pub fn execute(&mut self, command: &EditCommand) -> Result<(), EditorError> {
        let applied = self.project.apply_command(command)?;
        self.undo.push(applied.inverse);
        self.trim_undo();
        self.redo.clear();
        Ok(())
    }

    /// Applies the most recent inverse command.
    ///
    /// # Errors
    ///
    /// Returns an error when a previously recorded inverse can no longer be applied.
    pub fn undo(&mut self) -> Result<bool, EditorError> {
        let Some(command) = self.undo.pop() else {
            return Ok(false);
        };
        match self.project.apply_command(&command) {
            Ok(applied) => {
                self.redo.push(applied.inverse);
                Ok(true)
            }
            Err(error) => {
                self.undo.push(command);
                Err(error.into())
            }
        }
    }

    /// Reapplies the most recently undone command.
    ///
    /// # Errors
    ///
    /// Returns an error when a previously recorded command can no longer be applied.
    pub fn redo(&mut self) -> Result<bool, EditorError> {
        let Some(command) = self.redo.pop() else {
            return Ok(false);
        };
        match self.project.apply_command(&command) {
            Ok(applied) => {
                self.undo.push(applied.inverse);
                self.trim_undo();
                Ok(true)
            }
            Err(error) => {
                self.redo.push(command);
                Err(error.into())
            }
        }
    }

    fn trim_undo(&mut self) {
        let excess = self.undo.len().saturating_sub(self.history_limit);
        if excess > 0 {
            self.undo.drain(..excess);
        }
    }
}

/// Builds the command for deleting a selection.
pub fn delete_frames(frame_ids: impl IntoIterator<Item = FrameId>) -> EditCommand {
    EditCommand::RemoveFrames {
        frame_ids: frame_ids.into_iter().collect(),
    }
}

/// Builds one atomic command that reduces a consecutive selection by a fixed factor.
///
/// The first selected frame is always retained. Subsequent selected frames are retained at
/// offsets `keep_every`, `keep_every * 2`, and so on. Selection input order is ignored: stable
/// [`FrameId`] values are resolved against the current timeline order.
///
/// [`ReduceDelayMode::DontAdjust`] shortens the timeline by exactly the removed duration. The
/// other modes preserve the total timeline duration. When an even distribution has a remainder,
/// one microsecond is assigned to each retained frame in timeline order until it is exhausted.
///
/// # Errors
///
/// Returns an error when `keep_every` is less than two, the selection is empty, unknown,
/// non-consecutive, or too small to remove a frame, or when duration arithmetic overflows.
pub fn reduce_frames(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    options: ReduceOptions,
) -> Result<EditCommand, EditorError> {
    if options.keep_every < 2 {
        return Err(EditorError::InvalidKeepEvery);
    }

    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;

    let selected_frames = project
        .timeline
        .frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| selected.contains(&frame.id))
        .collect::<Vec<_>>();
    if selected_frames
        .windows(2)
        .any(|pair| pair[0].0.checked_add(1) != Some(pair[1].0))
    {
        return Err(EditorError::NonConsecutiveSelection);
    }

    let mut retained = Vec::new();
    let mut removed_ids = Vec::new();
    let mut removed_duration = 0_u64;

    for (selection_index, (_, frame)) in selected_frames.into_iter().enumerate() {
        if selection_index % options.keep_every == 0 {
            retained.push((frame.id, frame.duration.get(), frame.duration.get()));
            continue;
        }

        removed_ids.push(frame.id);
        removed_duration = removed_duration
            .checked_add(frame.duration.get())
            .ok_or(EditorError::InvalidDuration)?;
        if options.delay_mode == ReduceDelayMode::Previous {
            let (_, _, adjusted) = retained
                .last_mut()
                .ok_or(EditorError::ReductionRemovedFirstFrame)?;
            *adjusted = adjusted
                .checked_add(frame.duration.get())
                .ok_or(EditorError::InvalidDuration)?;
        }
    }

    if removed_ids.is_empty() {
        return Err(EditorError::NoFramesReduced);
    }

    if options.delay_mode == ReduceDelayMode::Evenly {
        let retained_count =
            u64::try_from(retained.len()).map_err(|_| EditorError::InvalidDuration)?;
        let share = removed_duration / retained_count;
        let mut remainder = removed_duration % retained_count;
        for (_, _, adjusted) in &mut retained {
            let extra = u64::from(remainder > 0);
            remainder = remainder.saturating_sub(extra);
            *adjusted = adjusted
                .checked_add(share)
                .and_then(|value| value.checked_add(extra))
                .ok_or(EditorError::InvalidDuration)?;
        }
    }

    let changes = retained
        .into_iter()
        .filter(|(_, original, adjusted)| original != adjusted)
        .map(|(frame_id, _, adjusted)| {
            Ok(FrameDurationChange {
                frame_id,
                duration: DurationUs::new(adjusted).ok_or(EditorError::InvalidDuration)?,
            })
        })
        .collect::<Result<Vec<_>, EditorError>>()?;

    let removed: BTreeSet<_> = removed_ids.iter().copied().collect();
    let transitions = project
        .timeline
        .transitions
        .iter()
        .filter(|transition| {
            !removed.contains(&transition.from_frame) && !removed.contains(&transition.to_frame)
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut commands = Vec::with_capacity(3);
    if !changes.is_empty() {
        commands.push(EditCommand::SetFrameDurations { changes });
    }
    if transitions.len() != project.timeline.transitions.len() {
        commands.push(EditCommand::SetTransitions { transitions });
    }
    commands.push(EditCommand::RemoveFrames {
        frame_ids: removed_ids,
    });

    Ok(EditCommand::Compound { commands })
}

/// Builds a stable reorder command that reverses only the selected positions.
///
/// # Errors
///
/// Returns an error for an empty selection or a frame that is not in the project.
pub fn reverse_selected(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
) -> Result<EditCommand, EditorError> {
    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;

    let mut reversed: Vec<_> = project
        .timeline
        .frames
        .iter()
        .filter(|frame| selected.contains(&frame.id))
        .map(|frame| frame.id)
        .collect();
    reversed.reverse();
    let mut reversed = reversed.into_iter();

    let order = project
        .timeline
        .frames
        .iter()
        .map(|frame| {
            if selected.contains(&frame.id) {
                reversed
                    .next()
                    .ok_or(EditorError::SelectionCardinalityMismatch)
            } else {
                Ok(frame.id)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(EditCommand::ReorderFrames { order })
}

/// Builds a command that moves the selected frames one slot toward the start.
/// Selected neighbors retain their relative order.
///
/// # Errors
///
/// Returns an error for an empty selection or a frame that is not in the project.
pub fn move_selected_left(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
) -> Result<EditCommand, EditorError> {
    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;
    let mut order: Vec<_> = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();

    for index in 1..order.len() {
        if selected.contains(&order[index]) && !selected.contains(&order[index - 1]) {
            order.swap(index - 1, index);
        }
    }
    Ok(EditCommand::ReorderFrames { order })
}

/// Builds a command that moves the selected frames one slot toward the end.
/// Selected neighbors retain their relative order.
///
/// # Errors
///
/// Returns an error for an empty selection or a frame that is not in the project.
pub fn move_selected_right(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
) -> Result<EditCommand, EditorError> {
    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;
    let mut order: Vec<_> = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();

    for index in (0..order.len().saturating_sub(1)).rev() {
        if selected.contains(&order[index]) && !selected.contains(&order[index + 1]) {
            order.swap(index, index + 1);
        }
    }
    Ok(EditCommand::ReorderFrames { order })
}

/// Overrides the selected frame durations with a single positive duration.
///
/// # Errors
///
/// Returns an error for an invalid selection.
pub fn override_duration(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    duration: DurationUs,
) -> Result<EditCommand, EditorError> {
    change_durations(project, frame_ids, |_| Ok(duration))
}

/// Adds a signed number of microseconds to selected frame durations.
///
/// # Errors
///
/// Returns an error when the selection is invalid or a resulting duration is not positive.
pub fn adjust_duration(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    delta_us: i64,
) -> Result<EditCommand, EditorError> {
    change_durations(project, frame_ids, |current| {
        let changed = i128::from(current.get()) + i128::from(delta_us);
        let changed = u64::try_from(changed).map_err(|_| EditorError::InvalidDuration)?;
        DurationUs::new(changed).ok_or(EditorError::InvalidDuration)
    })
}

/// Scales selected frame durations by a positive percentage.
///
/// # Errors
///
/// Returns an error when the selection is invalid, the percentage is zero, or a duration
/// overflows.
pub fn scale_duration(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    percent: u32,
) -> Result<EditCommand, EditorError> {
    if percent == 0 {
        return Err(EditorError::InvalidScale);
    }
    change_durations(project, frame_ids, |current| {
        let scaled = u128::from(current.get())
            .checked_mul(u128::from(percent))
            .ok_or(EditorError::InvalidDuration)?;
        let rounded = scaled.checked_add(50).ok_or(EditorError::InvalidDuration)? / 100;
        let rounded = u64::try_from(rounded).map_err(|_| EditorError::InvalidDuration)?;
        DurationUs::new(rounded).ok_or(EditorError::InvalidDuration)
    })
}

fn change_durations(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    mut change: impl FnMut(DurationUs) -> Result<DurationUs, EditorError>,
) -> Result<EditCommand, EditorError> {
    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;
    let changes = project
        .timeline
        .frames
        .iter()
        .filter(|frame| selected.contains(&frame.id))
        .map(|frame| {
            Ok(FrameDurationChange {
                frame_id: frame.id,
                duration: change(frame.duration)?,
            })
        })
        .collect::<Result<Vec<_>, EditorError>>()?;
    Ok(EditCommand::SetFrameDurations { changes })
}

fn ensure_known_selection(
    project: &ProjectManifest,
    selected: &BTreeSet<FrameId>,
) -> Result<(), EditorError> {
    if selected.is_empty() {
        return Err(EditorError::EmptySelection);
    }
    if let Some(frame_id) = selected.iter().find(|frame_id| {
        !project
            .timeline
            .frames
            .iter()
            .any(|frame| frame.id == **frame_id)
    }) {
        return Err(EditorError::UnknownSelectedFrame(*frame_id));
    }
    Ok(())
}

/// Errors produced while building or applying editor commands.
#[derive(Debug, Error)]
pub enum EditorError {
    /// The project or command violated a domain invariant.
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// At least one frame must be selected.
    #[error("at least one frame must be selected")]
    EmptySelection,
    /// A selection referred to a frame outside the project.
    #[error("selected frame {0} does not exist")]
    UnknownSelectedFrame(FrameId),
    /// Undo history must retain at least one command.
    #[error("history limit must be greater than zero")]
    ZeroHistoryLimit,
    /// A duration edit resulted in zero, a negative value, or overflow.
    #[error("duration edit produced an invalid value")]
    InvalidDuration,
    /// A duration scale must be positive.
    #[error("duration scale must be greater than zero")]
    InvalidScale,
    /// A reduction factor must remove at least every second selected frame.
    #[error("keep_every must be at least two")]
    InvalidKeepEvery,
    /// Reduce Frames operates on one uninterrupted timeline range.
    #[error("selected frames must be consecutive")]
    NonConsecutiveSelection,
    /// The selected range was too small for the requested reduction factor.
    #[error("the reduction options would not remove any selected frame")]
    NoFramesReduced,
    /// The reduction algorithm must always retain the first selected frame.
    #[error("frame reduction unexpectedly attempted to remove the first selected frame")]
    ReductionRemovedFirstFrame,
    /// An internal selection transformation produced a different item count.
    #[error("selection transformation changed the number of selected frames")]
    SelectionCardinalityMismatch,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, FrameClip, PhysicalSize, ProjectId, ProjectRevision,
        RasterEncoding, Timeline, UnixTimeMs,
    };

    use super::*;

    fn project() -> ProjectManifest {
        project_with_durations(&[10_000, 20_000, 30_000, 40_000])
    }

    fn project_with_durations(durations: &[u64]) -> ProjectManifest {
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
                id: FrameId::from_u128(u128::try_from(index).unwrap() + 1),
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
            app_version: "0.1.0".to_owned(),
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

    fn order(project: &ProjectManifest) -> Vec<u128> {
        project
            .timeline
            .frames
            .iter()
            .map(|frame| u128::from_be_bytes(*frame.id.as_bytes()))
            .collect()
    }

    fn durations(project: &ProjectManifest) -> Vec<u64> {
        project
            .timeline
            .frames
            .iter()
            .map(|frame| frame.duration.get())
            .collect()
    }

    fn frame_ids(project: &ProjectManifest) -> Vec<FrameId> {
        project
            .timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .collect()
    }

    fn total_duration(project: &ProjectManifest) -> u64 {
        project.timeline.total_duration().unwrap().get()
    }

    #[test]
    fn reverse_selected_preserves_unselected_slots() {
        let project = project();
        let command =
            reverse_selected(&project, [FrameId::from_u128(1), FrameId::from_u128(3)]).unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();
        assert_eq!(order(session.project()), [3, 2, 1, 4]);
    }

    #[test]
    fn move_selection_and_undo_redo_are_stable() {
        let project = project();
        let command = move_selected_left(&project, [FrameId::from_u128(3)]).unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();
        assert_eq!(order(session.project()), [1, 3, 2, 4]);
        assert!(session.undo().unwrap());
        assert_eq!(order(session.project()), [1, 2, 3, 4]);
        assert!(session.redo().unwrap());
        assert_eq!(order(session.project()), [1, 3, 2, 4]);
    }

    #[test]
    fn duration_edits_reject_non_positive_results() {
        let project = project();
        assert!(matches!(
            adjust_duration(&project, [FrameId::from_u128(1)], -10_000),
            Err(EditorError::InvalidDuration)
        ));
        assert!(matches!(
            scale_duration(&project, [FrameId::from_u128(1)], 0),
            Err(EditorError::InvalidScale)
        ));
    }

    #[test]
    fn reduce_dont_adjust_handles_both_edges_and_a_non_divisible_count() {
        let project =
            project_with_durations(&[10_000, 20_000, 30_000, 40_000, 50_000, 60_000, 70_000]);
        let mut selected = frame_ids(&project);
        selected.reverse();
        let command = reduce_frames(
            &project,
            selected,
            ReduceOptions {
                keep_every: 3,
                delay_mode: ReduceDelayMode::DontAdjust,
            },
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(order(session.project()), [1, 4, 7]);
        assert_eq!(durations(session.project()), [10_000, 40_000, 70_000]);
        assert_eq!(total_duration(session.project()), 120_000);
        assert!(
            session
                .project()
                .timeline
                .frames
                .iter()
                .all(|frame| frame.duration.get() > 0)
        );
    }

    #[test]
    fn reduce_previous_preserves_total_duration_and_is_undoable() {
        let project =
            project_with_durations(&[10_000, 20_000, 30_000, 40_000, 50_000, 60_000, 70_000]);
        let original_order = order(&project);
        let original_durations = durations(&project);
        let original_total = total_duration(&project);
        let command = reduce_frames(
            &project,
            frame_ids(&project),
            ReduceOptions {
                keep_every: 3,
                delay_mode: ReduceDelayMode::Previous,
            },
        )
        .unwrap();
        assert!(matches!(command, EditCommand::Compound { .. }));

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();
        assert_eq!(session.project().revision, ProjectRevision::new(1));
        assert_eq!(order(session.project()), [1, 4, 7]);
        assert_eq!(durations(session.project()), [60_000, 150_000, 70_000]);
        assert_eq!(total_duration(session.project()), original_total);

        assert!(session.undo().unwrap());
        assert_eq!(order(session.project()), original_order);
        assert_eq!(durations(session.project()), original_durations);
        assert_eq!(total_duration(session.project()), original_total);

        assert!(session.redo().unwrap());
        assert_eq!(order(session.project()), [1, 4, 7]);
        assert_eq!(durations(session.project()), [60_000, 150_000, 70_000]);
        assert_eq!(total_duration(session.project()), original_total);
    }

    #[test]
    fn reduce_evenly_distributes_microsecond_remainder_exactly() {
        let project = project_with_durations(&[1, 1, 2, 1]);
        let original_total = total_duration(&project);
        let command = reduce_frames(
            &project,
            frame_ids(&project),
            ReduceOptions {
                keep_every: 3,
                delay_mode: ReduceDelayMode::Evenly,
            },
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(order(session.project()), [1, 4]);
        assert_eq!(durations(session.project()), [3, 2]);
        assert_eq!(total_duration(session.project()), original_total);
    }

    #[test]
    fn reduce_entire_selection_with_extreme_factor_keeps_one_positive_frame() {
        let project = project();
        let original_total = total_duration(&project);
        let command = reduce_frames(
            &project,
            frame_ids(&project),
            ReduceOptions {
                keep_every: usize::MAX,
                delay_mode: ReduceDelayMode::Previous,
            },
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(order(session.project()), [1]);
        assert_eq!(durations(session.project()), [original_total]);
        assert!(durations(session.project())[0] > 0);
    }

    #[test]
    fn reduce_rejects_invalid_factor_non_consecutive_and_too_small_selections() {
        let project = project();
        assert!(matches!(
            reduce_frames(
                &project,
                frame_ids(&project),
                ReduceOptions {
                    keep_every: 1,
                    delay_mode: ReduceDelayMode::DontAdjust,
                },
            ),
            Err(EditorError::InvalidKeepEvery)
        ));
        assert!(matches!(
            reduce_frames(
                &project,
                [FrameId::from_u128(1), FrameId::from_u128(3)],
                ReduceOptions {
                    keep_every: 2,
                    delay_mode: ReduceDelayMode::DontAdjust,
                },
            ),
            Err(EditorError::NonConsecutiveSelection)
        ));
        assert!(matches!(
            reduce_frames(
                &project,
                [FrameId::from_u128(4)],
                ReduceOptions {
                    keep_every: 2,
                    delay_mode: ReduceDelayMode::DontAdjust,
                },
            ),
            Err(EditorError::NoFramesReduced)
        ));
    }

    #[test]
    fn history_limit_drops_oldest_inverse() {
        let mut session = EditorSession::new(project(), 1).unwrap();
        let first = move_selected_left(session.project(), [FrameId::from_u128(3)]).unwrap();
        session.execute(&first).unwrap();
        let second = move_selected_right(session.project(), [FrameId::from_u128(3)]).unwrap();
        session.execute(&second).unwrap();
        assert!(session.undo().unwrap());
        assert!(!session.undo().unwrap());
    }
}
