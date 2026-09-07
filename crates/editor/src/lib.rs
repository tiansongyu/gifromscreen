#![forbid(unsafe_code)]

//! Pure timeline editing services built on serializable domain commands.

use std::{collections::BTreeSet, error::Error};

use gif_from_screen_domain::{
    DomainError, DurationUs, EditCommand, FrameDurationChange, FrameId, ProjectManifest,
};
use thiserror::Error;

mod clip_transform;
mod composed_frame;
mod duplicates;
mod frame_bundle;
mod frame_clipboard;
mod frame_effect;
mod frame_selection;
mod frame_transition;
mod paint_stage;
mod selection;
mod statistics;
mod virtual_filmstrip;
mod yoyo;

pub use clip_transform::{ClipTransformEdit, edit_clip_transforms};
pub use composed_frame::{
    ComposedEffectEdit, ComposedFrameEdit, ComposedImageEffect, MAX_COMPOSED_IMAGE_BYTES,
    edit_composed_frames, frame_effect_count,
};
pub use duplicates::{
    DuplicateDelayMode, DuplicateFrameRetention, FrameComparison, FrameSimilarityProvider,
    RemoveDuplicateFramesOptions, remove_duplicate_frames,
};
pub use frame_bundle::{
    FrameBundle, FrameBundleError, FrameBundleIdentities, MAX_FRAME_BUNDLE_METADATA_BYTES,
    MAX_FRAME_BUNDLE_TRACKS,
};
pub use frame_clipboard::{
    CutFrameSelection, DEFAULT_FRAME_CLIPBOARD_HISTORY_CAPACITY, FrameClipboard,
    FrameClipboardEntryId, FrameClipboardHistory, FrameClipboardHistoryEntry,
    FrameClipboardHistoryError, MAX_FRAME_CLIPBOARD_FRAMES, MAX_FRAME_CLIPBOARD_HISTORY_CAPACITY,
    copy_selected_frames, cut_selected_frames, paste_frame_clipboard,
};
pub use frame_effect::{FrameEffectEdit, MAX_FRAME_EFFECT_BLUR_RADIUS, edit_frame_effects};
pub use frame_selection::{
    FrameExpressionError, FrameExpressionErrorReason, FrameTimeRangeError, parse_frame_expression,
    select_frames_by_time_range,
};
pub use frame_transition::{
    FrameTransitionSettings, remove_transition_after, set_transition_after,
};
pub use paint_stage::author_frame_owned_track;
pub use selection::{TimelineSelection, TimelineSelectionError};
pub use statistics::{
    CurrentFrameStatistics, EditorStatistics, EditorStatisticsError, project_statistics,
};
pub use virtual_filmstrip::{
    MAX_VIRTUAL_FILMSTRIP_FRAMES, VirtualFilmstripError, VirtualFilmstripLayout,
};
pub use yoyo::{YoyoOptions, YoyoScope, yoyo_frames};

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

/// Builds one atomic command that deletes every frame before the earliest selected frame.
///
/// Input order is ignored and duplicate identities are treated as one selection. Transitions
/// referencing a deleted frame are removed in the same command so that applying it cannot leave
/// the project in an invalid intermediate state.
///
/// # Errors
///
/// Returns an error for an empty or unknown selection, or when the earliest selected frame is
/// already the first frame in the timeline.
pub fn delete_frames_before(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
) -> Result<EditCommand, EditorError> {
    delete_frames_relative(project, frame_ids, RelativeDelete::Before)
}

/// Builds one atomic command that deletes every frame after the latest selected frame.
///
/// Input order is ignored and duplicate identities are treated as one selection. Transitions
/// referencing a deleted frame are removed in the same command so that applying it cannot leave
/// the project in an invalid intermediate state.
///
/// # Errors
///
/// Returns an error for an empty or unknown selection, or when the latest selected frame is
/// already the final frame in the timeline.
pub fn delete_frames_after(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
) -> Result<EditCommand, EditorError> {
    delete_frames_relative(project, frame_ids, RelativeDelete::After)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelativeDelete {
    Before,
    After,
}

fn delete_frames_relative(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    direction: RelativeDelete,
) -> Result<EditCommand, EditorError> {
    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;

    let first_selected = project
        .timeline
        .frames
        .iter()
        .position(|frame| selected.contains(&frame.id))
        .ok_or(EditorError::SelectionCardinalityMismatch)?;
    let last_selected = project
        .timeline
        .frames
        .iter()
        .rposition(|frame| selected.contains(&frame.id))
        .ok_or(EditorError::SelectionCardinalityMismatch)?;

    let removed_ids = match direction {
        RelativeDelete::Before => project.timeline.frames[..first_selected]
            .iter()
            .map(|frame| frame.id)
            .collect::<Vec<_>>(),
        RelativeDelete::After => project.timeline.frames[last_selected + 1..]
            .iter()
            .map(|frame| frame.id)
            .collect::<Vec<_>>(),
    };
    if removed_ids.is_empty() {
        return Err(match direction {
            RelativeDelete::Before => EditorError::NoFramesBeforeSelection,
            RelativeDelete::After => EditorError::NoFramesAfterSelection,
        });
    }

    Ok(remove_frames_atomically(project, removed_ids))
}

fn remove_frames_atomically(project: &ProjectManifest, removed_ids: Vec<FrameId>) -> EditCommand {
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
    let remove = EditCommand::RemoveFrames {
        frame_ids: removed_ids,
    };

    if transitions.len() == project.timeline.transitions.len() {
        remove
    } else {
        EditCommand::Compound {
            commands: vec![EditCommand::SetTransitions { transitions }, remove],
        }
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
    Ok(reorder_frames_atomically(project, order))
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
    Ok(reorder_frames_atomically(project, order))
}

fn reorder_frames_atomically(project: &ProjectManifest, order: Vec<FrameId>) -> EditCommand {
    let positions = order
        .iter()
        .copied()
        .enumerate()
        .map(|(index, frame_id)| (frame_id, index))
        .collect::<std::collections::BTreeMap<_, _>>();
    let transitions = project
        .timeline
        .transitions
        .iter()
        .filter(|transition| {
            positions
                .get(&transition.from_frame)
                .zip(positions.get(&transition.to_frame))
                .is_some_and(|(from, to)| from.checked_add(1) == Some(*to))
        })
        .cloned()
        .collect::<Vec<_>>();
    let reorder = EditCommand::ReorderFrames { order };

    if transitions.len() == project.timeline.transitions.len() {
        reorder
    } else {
        EditCommand::Compound {
            commands: vec![EditCommand::SetTransitions { transitions }, reorder],
        }
    }
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
/// Successful results are rounded to the nearest microsecond and clamped to at least one
/// microsecond. Returns an error when the selection is invalid, the percentage is zero, or a
/// duration exceeds the representable `u64` range.
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
        let rounded = u64::try_from(rounded.max(1)).map_err(|_| EditorError::InvalidDuration)?;
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
    /// A new group cannot be authored without changing existing paint semantics.
    #[error("group {track_id}: {reason}")]
    InvalidPaintTrack {
        /// Identity of the proposed frame-owned group.
        track_id: gif_from_screen_domain::TrackId,
        /// Specific identity, authoring-scope or paint-stage failure.
        reason: String,
    },
    /// An ordered edit cannot be represented safely for this frame.
    #[error("frame {frame_id}: {reason}")]
    InvalidRenderPipeline {
        /// Owner of the invalid operation chain.
        frame_id: FrameId,
        /// Specific geometry, stage or resource failure.
        reason: String,
    },
    /// One atomic geometry edit would exceed the command metadata budget.
    #[error("This edit exceeds the 16 MiB metadata budget. Use fewer frames or layers.")]
    RenderPipelineMetadataLimit,
    /// Frozen frame-owned annotation metadata could not be copied safely.
    #[error(transparent)]
    FrameBundle(#[from] FrameBundleError),
    /// The project or command violated a domain invariant.
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// At least one frame must be selected.
    #[error("at least one frame must be selected")]
    EmptySelection,
    /// A selection referred to a frame outside the project.
    #[error("selected frame {0} does not exist")]
    UnknownSelectedFrame(FrameId),
    /// A selected frame refers to an asset absent from the manifest.
    #[error("selected frame {frame_id} refers to missing asset {asset_id}")]
    MissingFrameAsset {
        /// Selected frame whose source cannot be resolved.
        frame_id: FrameId,
        /// Missing immutable source asset.
        asset_id: gif_from_screen_domain::AssetId,
    },
    /// A selected frame's asset kind cannot provide original frame pixels.
    #[error("selected frame {frame_id} asset {asset_id} is not a frame raster")]
    UnsupportedFrameAsset {
        /// Selected frame whose source kind is incompatible.
        frame_id: FrameId,
        /// Incompatible source asset.
        asset_id: gif_from_screen_domain::AssetId,
    },
    /// A crop rectangle is empty or its coordinates overflow.
    #[error("crop rectangle {0:?} is empty or has overflowing coordinates")]
    InvalidCrop(gif_from_screen_domain::PhysicalRect),
    /// A crop rectangle lies outside one selected frame's immutable source asset.
    #[error(
        "crop {crop:?} for frame {frame_id} does not fit source asset {asset_id} size {source_size:?}"
    )]
    CropOutsideFrameAsset {
        /// Selected frame rejected by the shared crop.
        frame_id: FrameId,
        /// Immutable source asset for that frame.
        asset_id: gif_from_screen_domain::AssetId,
        /// Requested source-coordinate crop.
        crop: gif_from_screen_domain::PhysicalRect,
        /// Original source-asset dimensions.
        source_size: gif_from_screen_domain::PhysicalSize,
    },
    /// A requested pre-rotation output size is empty or invalid.
    #[error("clip output size {0:?} is invalid")]
    InvalidOutputSize(gif_from_screen_domain::PhysicalSize),
    /// A frame effect region is empty, overflowing, or outside the project canvas.
    #[error("{effect} region {region:?} does not fit project canvas {canvas:?}")]
    InvalidFrameEffectRegion {
        /// Stable effect family name.
        effect: &'static str,
        /// Rejected canvas-coordinate region.
        region: gif_from_screen_domain::PhysicalRect,
        /// Current project canvas dimensions.
        canvas: gif_from_screen_domain::PhysicalSize,
    },
    /// A numeric frame-effect parameter is outside its supported range.
    #[error("invalid {effect} {parameter}={value}; maximum is {maximum}")]
    InvalidFrameEffectParameter {
        /// Stable effect family name.
        effect: &'static str,
        /// Stable parameter name.
        parameter: &'static str,
        /// Rejected value.
        value: u64,
        /// Largest supported value.
        maximum: u64,
    },
    /// Border edge widths are empty or overlap beyond the project canvas.
    #[error("border widths {widths:?} are invalid for project canvas {canvas:?}")]
    InvalidBorderWidths {
        /// Rejected edge widths.
        widths: gif_from_screen_domain::EdgeWidths,
        /// Current project canvas dimensions.
        canvas: gif_from_screen_domain::PhysicalSize,
    },
    /// A visible effect was given a fully transparent color.
    #[error("{effect} color {color:?} is fully transparent")]
    InvisibleFrameEffectColor {
        /// Stable effect family name.
        effect: &'static str,
        /// Rejected color.
        color: gif_from_screen_domain::Rgba,
    },
    /// One selected frame has no effect at the requested replacement index.
    #[error(
        "frame {frame_id} has {effect_count} effects, so effect index {index} cannot be replaced"
    )]
    EffectIndexOutOfBounds {
        /// Selected frame with a shorter effect list.
        frame_id: FrameId,
        /// Requested zero-based effect index.
        index: usize,
        /// Number of effects currently on that frame.
        effect_count: usize,
    },
    /// The effect is persisted by the domain but not supported by the current renderer/editor.
    #[error("frame effect {0} is not supported by the editor")]
    UnsupportedFrameEffect(&'static str),
    /// There are no frames before the selected range.
    #[error("there are no frames before the selection")]
    NoFramesBeforeSelection,
    /// There are no frames after the selected range.
    #[error("there are no frames after the selection")]
    NoFramesAfterSelection,
    /// A similarity provider failed while comparing two adjacent selected frames.
    #[error("failed to compare adjacent frames {first} and {second}")]
    FrameComparisonFailed {
        /// The earlier frame in timeline order.
        first: FrameId,
        /// The later frame in timeline order.
        second: FrameId,
        /// The provider-specific failure.
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// A similarity provider returned a percentage outside the inclusive 0..=100 range.
    #[error(
        "similarity provider returned invalid percentage {percent} for frames {first} and {second}"
    )]
    InvalidSimilarityPercent {
        /// The earlier frame in timeline order.
        first: FrameId,
        /// The later frame in timeline order.
        second: FrameId,
        /// The invalid percentage.
        percent: u8,
    },
    /// The requested inclusive similarity threshold exceeded 100 percent.
    #[error("duplicate-frame similarity threshold must be between 0 and 100, got {0}")]
    InvalidSimilarityThreshold(u8),
    /// No adjacent frames in the selection met the requested similarity threshold.
    #[error("the selected frames contain no removable duplicate group")]
    NoDuplicateFrames,
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
    /// The operation requires one uninterrupted timeline range.
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
    /// A single application clipboard snapshot exceeded its explicit frame limit.
    #[error("frame clipboard selected {selected} frames; maximum is {maximum}")]
    FrameClipboardTooLarge {
        /// Number of selected frames requested for Copy/Cut.
        selected: usize,
        /// Maximum clips retained by one clipboard snapshot.
        maximum: usize,
    },
    /// Cutting the selection would remove every frame from the timeline.
    #[error("cannot cut all {frame_count} timeline frames")]
    CutWouldEmptyTimeline {
        /// Current timeline frame count, all of which were selected.
        frame_count: usize,
    },
    /// Paste requires a previously copied non-empty frame snapshot.
    #[error("frame clipboard is empty")]
    EmptyFrameClipboard,
    /// The requested current-frame insertion anchor is stale.
    #[error("paste anchor frame {0} does not exist")]
    UnknownPasteAnchor(FrameId),
    /// A Yoyo range needs enough frames to produce a meaningful reverse leg.
    #[error(
        "yoyo requires at least {minimum_frames} frame(s) for these endpoint settings, got {actual_frames}"
    )]
    YoyoRangeTooShort {
        /// Minimum source range length for the requested endpoint behavior.
        minimum_frames: usize,
        /// Actual source range length.
        actual_frames: usize,
    },
    /// A frame identity generator returned the reserved nil identity.
    #[error("the frame id generator returned the reserved nil identity")]
    GeneratedNilFrameId,
    /// A generated frame identity was already present or returned earlier in this edit.
    #[error("the generated frame id {0} conflicts with another timeline frame")]
    GeneratedFrameIdConflict(FrameId),
    /// A frame transition operation requires a current frame.
    #[error("select a current frame before editing its outgoing transition")]
    NoCurrentFrameForTransition,
    /// The current frame is the final timeline frame and therefore has no outgoing pair.
    #[error("frame {0} is the final timeline frame and has no next-frame transition")]
    NoFrameAfterTransitionAnchor(FrameId),
    /// A transition step count must be within the domain's bounded positive range.
    #[error("transition steps must be between 1 and {maximum}, got {steps}")]
    InvalidTransitionSteps {
        /// Rejected intermediate-frame count.
        steps: u16,
        /// Largest supported intermediate-frame count.
        maximum: u16,
    },
    /// The total added duration cannot give every transition step a positive duration.
    #[error("transition duration {duration_us}us is shorter than its {steps} steps")]
    TransitionDurationTooShort {
        /// Rejected total added duration.
        duration_us: u64,
        /// Requested intermediate-frame count.
        steps: u16,
    },
    /// A transition identity generator returned the reserved nil identity.
    #[error("the transition id generator returned the reserved nil identity")]
    GeneratedNilTransitionId,
    /// A generated transition identity conflicts with an existing transition.
    #[error("the generated transition id {0} conflicts with an existing transition")]
    GeneratedTransitionIdConflict(gif_from_screen_domain::TransitionId),
    /// Corrupt input contains multiple transitions for one ordered endpoint pair.
    #[error("multiple transitions target frames {from_frame} -> {to_frame}")]
    AmbiguousTransitionPair {
        /// Outgoing endpoint.
        from_frame: FrameId,
        /// Incoming endpoint.
        to_frame: FrameId,
    },
    /// Removing a transition was requested for an endpoint pair without one.
    #[error("frames {from_frame} -> {to_frame} have no transition to remove")]
    MissingTransitionPair {
        /// Outgoing endpoint.
        from_frame: FrameId,
        /// Incoming endpoint.
        to_frame: FrameId,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, FrameClip, PhysicalSize, ProjectId, ProjectRevision,
        RasterEncoding, Timeline, Transition, TransitionId, TransitionKind, UnixTimeMs,
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
                render_steps: Vec::new(),
                capture_clock: None,
                capture_binding: gif_from_screen_domain::CaptureBinding::Original,
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
            task_runs: Vec::new(),
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

    fn add_adjacent_transitions(project: &mut ProjectManifest) {
        project.timeline.transitions = project
            .timeline
            .frames
            .windows(2)
            .enumerate()
            .map(|(index, pair)| Transition {
                id: TransitionId::from_u128(u128::try_from(index).unwrap() + 1),
                from_frame: pair[0].id,
                to_frame: pair[1].id,
                duration: DurationUs::new(1).unwrap(),
                steps: 1,
                kind: TransitionKind::FadeToNext,
            })
            .collect();
        project.validate().unwrap();
    }

    fn transition_pairs(project: &ProjectManifest) -> Vec<(u128, u128)> {
        project
            .timeline
            .transitions
            .iter()
            .map(|transition| {
                (
                    u128::from_be_bytes(*transition.from_frame.as_bytes()),
                    u128::from_be_bytes(*transition.to_frame.as_bytes()),
                )
            })
            .collect()
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
    fn delete_before_uses_earliest_selected_frame_and_is_reversible() {
        let mut project = project_with_durations(&[10, 20, 30, 40, 50]);
        add_adjacent_transitions(&mut project);
        let command = delete_frames_before(
            &project,
            [
                FrameId::from_u128(4),
                FrameId::from_u128(3),
                FrameId::from_u128(4),
            ],
        )
        .unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();

        session.execute(&command).unwrap();
        assert_eq!(order(session.project()), [3, 4, 5]);
        assert_eq!(durations(session.project()), [30, 40, 50]);
        assert_eq!(transition_pairs(session.project()), [(3, 4), (4, 5)]);
        assert!(session.undo().unwrap());
        assert_eq!(order(session.project()), [1, 2, 3, 4, 5]);
        assert_eq!(
            transition_pairs(session.project()),
            [(1, 2), (2, 3), (3, 4), (4, 5)]
        );
        assert!(session.redo().unwrap());
        assert_eq!(order(session.project()), [3, 4, 5]);
        assert_eq!(transition_pairs(session.project()), [(3, 4), (4, 5)]);
    }

    #[test]
    fn delete_after_uses_latest_selected_frame_and_is_reversible() {
        let project = project_with_durations(&[10, 20, 30, 40, 50]);
        let command = delete_frames_after(
            &project,
            [
                FrameId::from_u128(2),
                FrameId::from_u128(3),
                FrameId::from_u128(2),
            ],
        )
        .unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();

        session.execute(&command).unwrap();
        assert_eq!(order(session.project()), [1, 2, 3]);
        assert_eq!(durations(session.project()), [10, 20, 30]);
        assert!(session.undo().unwrap());
        assert_eq!(order(session.project()), [1, 2, 3, 4, 5]);
        assert!(session.redo().unwrap());
        assert_eq!(order(session.project()), [1, 2, 3]);
    }

    #[test]
    fn relative_deletes_reject_invalid_or_exhausted_ranges() {
        let project = project();
        assert!(matches!(
            delete_frames_before(&project, std::iter::empty()),
            Err(EditorError::EmptySelection)
        ));
        assert!(matches!(
            delete_frames_after(&project, [FrameId::from_u128(99)]),
            Err(EditorError::UnknownSelectedFrame(frame_id))
                if frame_id == FrameId::from_u128(99)
        ));
        assert!(matches!(
            delete_frames_before(&project, [FrameId::from_u128(1)]),
            Err(EditorError::NoFramesBeforeSelection)
        ));
        assert!(matches!(
            delete_frames_after(&project, [FrameId::from_u128(4)]),
            Err(EditorError::NoFramesAfterSelection)
        ));
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
    fn move_disjoint_and_contiguous_selections_preserves_relative_order() {
        let project = project_with_durations(&[1, 2, 3, 4, 5, 6]);
        let selection = [
            FrameId::from_u128(5),
            FrameId::from_u128(3),
            FrameId::from_u128(2),
            FrameId::from_u128(3),
        ];
        let left = move_selected_left(&project, selection).unwrap();
        let right = move_selected_right(&project, selection).unwrap();

        let mut left_session = EditorSession::new(project.clone(), 10).unwrap();
        left_session.execute(&left).unwrap();
        assert_eq!(order(left_session.project()), [2, 3, 1, 5, 4, 6]);

        let mut right_session = EditorSession::new(project, 10).unwrap();
        right_session.execute(&right).unwrap();
        assert_eq!(order(right_session.project()), [1, 4, 2, 3, 6, 5]);
    }

    #[test]
    fn move_removes_only_transitions_invalidated_by_new_order_and_undo_restores_them() {
        let mut project = project_with_durations(&[10, 20, 30, 40, 50]);
        add_adjacent_transitions(&mut project);
        let command =
            move_selected_left(&project, [FrameId::from_u128(2), FrameId::from_u128(3)]).unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();

        session.execute(&command).unwrap();
        assert_eq!(order(session.project()), [2, 3, 1, 4, 5]);
        assert_eq!(transition_pairs(session.project()), [(2, 3), (4, 5)]);
        assert!(session.undo().unwrap());
        assert_eq!(order(session.project()), [1, 2, 3, 4, 5]);
        assert_eq!(
            transition_pairs(session.project()),
            [(1, 2), (2, 3), (3, 4), (4, 5)]
        );
        assert!(session.redo().unwrap());
        assert_eq!(order(session.project()), [2, 3, 1, 4, 5]);
        assert_eq!(transition_pairs(session.project()), [(2, 3), (4, 5)]);
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
    fn duration_scaling_clamps_rounding_to_one_and_supports_undo_redo() {
        let project = project_with_durations(&[1, 149, 150]);
        let command = scale_duration(&project, frame_ids(&project), 1).unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();

        session.execute(&command).unwrap();
        assert_eq!(durations(session.project()), [1, 1, 2]);
        assert!(
            session
                .project()
                .timeline
                .frames
                .iter()
                .all(|frame| frame.duration.get() > 0)
        );
        assert!(session.undo().unwrap());
        assert_eq!(durations(session.project()), [1, 149, 150]);
        assert!(session.redo().unwrap());
        assert_eq!(durations(session.project()), [1, 1, 2]);
    }

    #[test]
    fn duration_scaling_accepts_maximum_identity_and_rejects_overflow() {
        let project = project_with_durations(&[u64::MAX]);
        let command = scale_duration(&project, frame_ids(&project), 100).unwrap();
        let mut session = EditorSession::new(project.clone(), 10).unwrap();
        session.execute(&command).unwrap();
        assert_eq!(durations(session.project()), [u64::MAX]);

        assert!(matches!(
            scale_duration(&project, frame_ids(&project), 101),
            Err(EditorError::InvalidDuration)
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

    #[test]
    fn reducing_annotated_frames_retains_valid_spans_and_undo_restores_exact_tracks() {
        use gif_from_screen_domain::{
            BlendMode, OverlayContent, OverlayId, OverlayItem, OverlayTrack, PhysicalPoint, TimeUs,
            TimelineSpan, TrackId,
        };

        for delay_mode in [
            ReduceDelayMode::DontAdjust,
            ReduceDelayMode::Previous,
            ReduceDelayMode::Evenly,
        ] {
            let mut project = project_with_durations(&[100, 100, 100, 100]);
            let asset_id = project.timeline.frames[0].asset_id;
            project.timeline.overlay_tracks.push(OverlayTrack {
                frame_cells: None,
                annotation: None,
                annotation_scope: None,
                id: TrackId::from_u128(1),
                name: "Selected frame watermarks".to_owned(),
                visible: true,
                opacity: 255,
                blend_mode: BlendMode::Normal,
                items: [(0, 400), (100, 200), (300, 400)]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (start, end))| OverlayItem {
                        id: OverlayId::from_u128(index as u128 + 1),
                        span: TimelineSpan {
                            start: TimeUs::new(start),
                            duration: DurationUs::new(end - start).unwrap(),
                        },
                        z_index: 0,
                        content: OverlayContent::Raster {
                            asset_id,
                            position: PhysicalPoint::default(),
                            size: PhysicalSize::new(2, 2).unwrap(),
                            opacity: 255,
                        },
                    })
                    .collect(),
            });
            let original = project.timeline.clone();
            let command = reduce_frames(
                &project,
                frame_ids(&project),
                ReduceOptions {
                    keep_every: 2,
                    delay_mode,
                },
            )
            .unwrap();
            let mut session = EditorSession::new(project, 10).unwrap();
            session.execute(&command).unwrap();
            let edited = session.project().timeline.clone();
            assert_eq!(edited.overlay_tracks[0].items.len(), 1);
            assert_eq!(
                edited.overlay_tracks[0].items[0].span.end(),
                edited.total_duration()
            );
            assert!(session.undo().unwrap());
            assert_eq!(session.project().timeline, original);
            assert!(session.redo().unwrap());
            assert_eq!(session.project().timeline, edited);
        }
    }
}
