use std::collections::BTreeSet;

use gif_from_screen_domain::{FrameId, TimeUs, Timeline};
use thiserror::Error;

/// Selection and keyboard-navigation state for a frame timeline.
///
/// Frame identities, rather than indices, are retained so selection survives
/// edits that reorder the timeline. Whenever the selection is non-empty,
/// `current` is one of its members. The range anchor is either a surviving
/// frame or is repaired to the current frame by [`Self::reconcile`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TimelineSelection {
    selected: BTreeSet<FrameId>,
    current: Option<FrameId>,
    anchor: Option<FrameId>,
}

impl TimelineSelection {
    /// Creates an empty selection.
    pub const fn new() -> Self {
        Self {
            selected: BTreeSet::new(),
            current: None,
            anchor: None,
        }
    }

    /// Returns all selected identities in stable identity order.
    pub const fn selected(&self) -> &BTreeSet<FrameId> {
        &self.selected
    }

    /// Returns the frame used for preview and navigation.
    pub const fn current(&self) -> Option<FrameId> {
        self.current
    }

    /// Returns the fixed end of the next range selection.
    pub const fn anchor(&self) -> Option<FrameId> {
        self.anchor
    }

    /// Returns `true` when no frame is selected.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Returns the number of selected frames.
    pub fn len(&self) -> usize {
        self.selected.len()
    }

    /// Returns whether `frame_id` is selected.
    pub fn contains(&self, frame_id: FrameId) -> bool {
        self.selected.contains(&frame_id)
    }

    /// Replaces the selection and range anchor with one frame.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineSelectionError::UnknownFrame`] without changing state
    /// when `frame_id` is not present.
    pub fn select_only(
        &mut self,
        timeline: &Timeline,
        frame_id: FrameId,
    ) -> Result<(), TimelineSelectionError> {
        ensure_frame(timeline, frame_id)?;
        self.selected.clear();
        self.selected.insert(frame_id);
        self.current = Some(frame_id);
        self.anchor = Some(frame_id);
        Ok(())
    }

    /// Toggles one frame, matching a conventional Ctrl-click selection.
    ///
    /// Adding a frame makes it current and establishes a new range anchor.
    /// Removing the current frame selects the nearest remaining frame,
    /// preferring the following frame when distances are equal.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineSelectionError::UnknownFrame`] without changing state
    /// when `frame_id` is not present.
    pub fn toggle(
        &mut self,
        timeline: &Timeline,
        frame_id: FrameId,
    ) -> Result<(), TimelineSelectionError> {
        let toggled_index = frame_index(timeline, frame_id)?;
        if self.selected.remove(&frame_id) {
            if self.selected.is_empty() {
                self.current = None;
                self.anchor = None;
            } else {
                if self.current == Some(frame_id) {
                    self.current = nearest_selected(timeline, &self.selected, toggled_index);
                }
                if self.anchor == Some(frame_id) {
                    self.anchor = self.current;
                }
            }
        } else {
            self.selected.insert(frame_id);
            self.current = Some(frame_id);
            self.anchor = Some(frame_id);
        }
        Ok(())
    }

    /// Replaces the selection with the inclusive range from the anchor to
    /// `frame_id`, matching a conventional Shift-click selection.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineSelectionError::UnknownFrame`] without changing state
    /// when the target or retained anchor is not present.
    pub fn extend_range(
        &mut self,
        timeline: &Timeline,
        frame_id: FrameId,
    ) -> Result<(), TimelineSelectionError> {
        let target_index = frame_index(timeline, frame_id)?;
        let anchor = self.anchor.or(self.current).unwrap_or(frame_id);
        let anchor_index = frame_index(timeline, anchor)?;
        let (start, end) = if anchor_index <= target_index {
            (anchor_index, target_index)
        } else {
            (target_index, anchor_index)
        };
        self.selected = timeline.frames[start..=end]
            .iter()
            .map(|frame| frame.id)
            .collect();
        self.current = Some(frame_id);
        self.anchor = Some(anchor);
        Ok(())
    }

    /// Selects every frame, preserving a valid current frame when possible.
    pub fn select_all(&mut self, timeline: &Timeline) {
        self.selected = timeline.frames.iter().map(|frame| frame.id).collect();
        self.current = self
            .current
            .filter(|frame_id| self.selected.contains(frame_id))
            .or_else(|| timeline.frames.first().map(|frame| frame.id));
        self.anchor = self.current;
    }

    /// Selects every currently unselected frame.
    pub fn invert(&mut self, timeline: &Timeline) {
        self.selected = timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .filter(|frame_id| !self.selected.contains(frame_id))
            .collect();
        self.current = timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .find(|frame_id| self.selected.contains(frame_id));
        self.anchor = self.current;
    }

    /// Clears selection, preview, and range-anchor state.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.current = None;
        self.anchor = None;
    }

    /// Selects the first frame.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineSelectionError::EmptyTimeline`] for an empty timeline.
    pub fn first(&mut self, timeline: &Timeline) -> Result<FrameId, TimelineSelectionError> {
        self.select_index(timeline, 0)
    }

    /// Selects the frame immediately before the current frame, clamped at the
    /// first frame. With no current frame, selects the first frame.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty timeline or a stale current frame.
    pub fn previous(&mut self, timeline: &Timeline) -> Result<FrameId, TimelineSelectionError> {
        let index = match self.current {
            Some(frame_id) => frame_index(timeline, frame_id)?.saturating_sub(1),
            None => 0,
        };
        self.select_index(timeline, index)
    }

    /// Selects the frame immediately after the current frame, clamped at the
    /// final frame. With no current frame, selects the first frame.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty timeline or a stale current frame.
    pub fn next(&mut self, timeline: &Timeline) -> Result<FrameId, TimelineSelectionError> {
        let last = timeline
            .frames
            .len()
            .checked_sub(1)
            .ok_or(TimelineSelectionError::EmptyTimeline)?;
        let index = match self.current {
            Some(frame_id) => frame_index(timeline, frame_id)?
                .checked_add(1)
                .unwrap_or(last)
                .min(last),
            None => 0,
        };
        self.select_index(timeline, index)
    }

    /// Selects the final frame.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineSelectionError::EmptyTimeline`] for an empty timeline.
    pub fn last(&mut self, timeline: &Timeline) -> Result<FrameId, TimelineSelectionError> {
        let index = timeline
            .frames
            .len()
            .checked_sub(1)
            .ok_or(TimelineSelectionError::EmptyTimeline)?;
        self.select_index(timeline, index)
    }

    /// Selects a frame by its one-based number.
    ///
    /// # Errors
    ///
    /// Returns [`TimelineSelectionError::FrameNumberOutOfRange`] when the
    /// number is zero or greater than the frame count.
    pub fn select_frame_number(
        &mut self,
        timeline: &Timeline,
        frame_number: usize,
    ) -> Result<FrameId, TimelineSelectionError> {
        let index = frame_number
            .checked_sub(1)
            .filter(|index| *index < timeline.frames.len())
            .ok_or(TimelineSelectionError::FrameNumberOutOfRange {
                requested: frame_number,
                frame_count: timeline.frames.len(),
            })?;
        self.select_index(timeline, index)
    }

    /// Selects the frame visible at project-relative `time`.
    ///
    /// Intervals are start-inclusive and end-exclusive. The exact total
    /// duration selects the final frame, which makes a time input positioned
    /// at the end of the animation useful rather than surprising.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeline is empty, its total duration
    /// overflows, or `time` lies beyond the animation end.
    pub fn select_time(
        &mut self,
        timeline: &Timeline,
        time: TimeUs,
    ) -> Result<FrameId, TimelineSelectionError> {
        let total = timeline
            .total_duration()
            .ok_or(TimelineSelectionError::DurationOverflow)?;
        if timeline.frames.is_empty() {
            return Err(TimelineSelectionError::EmptyTimeline);
        }
        if time > total {
            return Err(TimelineSelectionError::TimeOutOfRange {
                requested_us: time.get(),
                total_us: total.get(),
            });
        }
        if time == total {
            return self.last(timeline);
        }

        let mut end = 0_u64;
        for (index, frame) in timeline.frames.iter().enumerate() {
            end = end
                .checked_add(frame.duration.get())
                .ok_or(TimelineSelectionError::DurationOverflow)?;
            if time.get() < end {
                return self.select_index(timeline, index);
            }
        }
        Err(TimelineSelectionError::DurationOverflow)
    }

    /// Repairs identities after frames are removed or reordered.
    ///
    /// Surviving selections remain selected. If all selected frames were
    /// removed, the first remaining frame becomes current and selected. An
    /// intentionally empty selection remains empty.
    pub fn reconcile(&mut self, timeline: &Timeline) {
        let was_empty = self.selected.is_empty() && self.current.is_none();
        let existing: BTreeSet<_> = timeline.frames.iter().map(|frame| frame.id).collect();
        self.selected.retain(|frame_id| existing.contains(frame_id));

        if timeline.frames.is_empty() || was_empty {
            self.clear();
            return;
        }
        if self.selected.is_empty()
            && let Some(first) = timeline.frames.first().map(|frame| frame.id)
        {
            self.selected.insert(first);
        }
        self.current = self
            .current
            .filter(|frame_id| self.selected.contains(frame_id))
            .or_else(|| {
                timeline
                    .frames
                    .iter()
                    .map(|frame| frame.id)
                    .find(|frame_id| self.selected.contains(frame_id))
            });
        self.anchor = self
            .anchor
            .filter(|frame_id| existing.contains(frame_id))
            .or(self.current);
    }

    fn select_index(
        &mut self,
        timeline: &Timeline,
        index: usize,
    ) -> Result<FrameId, TimelineSelectionError> {
        let frame_id = timeline
            .frames
            .get(index)
            .map(|frame| frame.id)
            .ok_or(TimelineSelectionError::EmptyTimeline)?;
        self.select_only(timeline, frame_id)?;
        Ok(frame_id)
    }
}

fn ensure_frame(timeline: &Timeline, frame_id: FrameId) -> Result<(), TimelineSelectionError> {
    frame_index(timeline, frame_id).map(|_| ())
}

fn frame_index(timeline: &Timeline, frame_id: FrameId) -> Result<usize, TimelineSelectionError> {
    timeline
        .frames
        .iter()
        .position(|frame| frame.id == frame_id)
        .ok_or(TimelineSelectionError::UnknownFrame(frame_id))
}

fn nearest_selected(
    timeline: &Timeline,
    selected: &BTreeSet<FrameId>,
    removed_index: usize,
) -> Option<FrameId> {
    timeline
        .frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| selected.contains(&frame.id))
        .min_by_key(|(index, _)| (index.abs_diff(removed_index), *index < removed_index))
        .map(|(_, frame)| frame.id)
}

/// Errors produced by timeline navigation and selection operations.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TimelineSelectionError {
    /// An operation needs at least one frame.
    #[error("the timeline has no frames")]
    EmptyTimeline,
    /// A frame identity is not present in the timeline.
    #[error("frame {0} does not exist in the timeline")]
    UnknownFrame(FrameId),
    /// A one-based frame number lies outside the timeline.
    #[error("frame number {requested} is outside a timeline with {frame_count} frames")]
    FrameNumberOutOfRange {
        /// Requested one-based frame number.
        requested: usize,
        /// Number of frames currently in the timeline.
        frame_count: usize,
    },
    /// A project-relative time lies beyond the animation end.
    #[error("time {requested_us}us exceeds the timeline duration of {total_us}us")]
    TimeOutOfRange {
        /// Requested project-relative time.
        requested_us: u64,
        /// Current animation duration.
        total_us: u64,
    },
    /// Adding the individual frame durations overflowed.
    #[error("the timeline duration exceeds the supported microsecond range")]
    DurationOverflow,
}

#[cfg(test)]
mod tests {
    use gif_from_screen_domain::{
        AssetId, CaptureMetadata, ClipTransform, DurationUs, FrameClip, FrameId, TimeUs, Timeline,
    };

    use super::*;

    fn frame(id: u128, duration: u64) -> FrameClip {
        FrameClip {
            capture_clock: None,
            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
            id: FrameId::from_u128(id),
            asset_id: AssetId::from_digest([u8::try_from(id).unwrap(); 32]),
            duration: DurationUs::new(duration).unwrap(),
            transform: ClipTransform::default(),
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
        }
    }

    fn timeline() -> Timeline {
        Timeline {
            frames: vec![frame(1, 10), frame(2, 20), frame(3, 30), frame(4, 40)],
            ..Timeline::default()
        }
    }

    fn ids(selection: &TimelineSelection) -> Vec<u128> {
        selection
            .selected()
            .iter()
            .map(|id| u128::from_be_bytes(*id.as_bytes()))
            .collect()
    }

    #[test]
    fn toggle_and_range_selection_use_timeline_order_and_stable_anchor() {
        let timeline = timeline();
        let mut selection = TimelineSelection::new();
        selection
            .select_only(&timeline, FrameId::from_u128(3))
            .unwrap();
        selection
            .extend_range(&timeline, FrameId::from_u128(1))
            .unwrap();
        assert_eq!(ids(&selection), [1, 2, 3]);
        assert_eq!(selection.current(), Some(FrameId::from_u128(1)));
        assert_eq!(selection.anchor(), Some(FrameId::from_u128(3)));

        selection.toggle(&timeline, FrameId::from_u128(2)).unwrap();
        assert_eq!(ids(&selection), [1, 3]);
        selection.toggle(&timeline, FrameId::from_u128(4)).unwrap();
        assert_eq!(ids(&selection), [1, 3, 4]);
        assert_eq!(selection.current(), Some(FrameId::from_u128(4)));
        assert_eq!(selection.anchor(), Some(FrameId::from_u128(4)));
    }

    #[test]
    fn removing_current_prefers_equally_distant_following_frame() {
        let timeline = timeline();
        let mut selection = TimelineSelection::new();
        selection
            .select_only(&timeline, FrameId::from_u128(2))
            .unwrap();
        selection.select_all(&timeline);
        selection.toggle(&timeline, FrameId::from_u128(2)).unwrap();

        assert_eq!(ids(&selection), [1, 3, 4]);
        assert_eq!(selection.current(), Some(FrameId::from_u128(3)));
        assert_eq!(selection.anchor(), Some(FrameId::from_u128(3)));
    }

    #[test]
    fn all_invert_and_clear_preserve_selection_invariants() {
        let timeline = timeline();
        let mut selection = TimelineSelection::new();
        selection
            .select_only(&timeline, FrameId::from_u128(2))
            .unwrap();
        selection.select_all(&timeline);
        assert_eq!(ids(&selection), [1, 2, 3, 4]);
        assert_eq!(selection.current(), Some(FrameId::from_u128(2)));

        selection.toggle(&timeline, FrameId::from_u128(4)).unwrap();
        selection.invert(&timeline);
        assert_eq!(ids(&selection), [4]);
        assert_eq!(selection.current(), Some(FrameId::from_u128(4)));
        selection.invert(&timeline);
        assert_eq!(ids(&selection), [1, 2, 3]);
        selection.clear();
        assert!(selection.is_empty());
        assert_eq!(selection.current(), None);
        assert_eq!(selection.anchor(), None);
    }

    #[test]
    fn navigation_clamps_and_uses_first_frame_without_current() {
        let timeline = timeline();
        let mut selection = TimelineSelection::new();
        assert_eq!(
            selection.previous(&timeline).unwrap(),
            FrameId::from_u128(1)
        );
        assert_eq!(
            selection.previous(&timeline).unwrap(),
            FrameId::from_u128(1)
        );
        assert_eq!(selection.next(&timeline).unwrap(), FrameId::from_u128(2));
        assert_eq!(selection.last(&timeline).unwrap(), FrameId::from_u128(4));
        assert_eq!(selection.next(&timeline).unwrap(), FrameId::from_u128(4));
        assert_eq!(selection.first(&timeline).unwrap(), FrameId::from_u128(1));
    }

    #[test]
    fn frame_number_and_time_navigation_are_precise_at_boundaries() {
        let timeline = timeline();
        let mut selection = TimelineSelection::new();
        assert_eq!(
            selection.select_frame_number(&timeline, 3).unwrap(),
            FrameId::from_u128(3)
        );
        assert_eq!(
            selection.select_time(&timeline, TimeUs::ZERO).unwrap(),
            FrameId::from_u128(1)
        );
        assert_eq!(
            selection.select_time(&timeline, TimeUs::new(9)).unwrap(),
            FrameId::from_u128(1)
        );
        assert_eq!(
            selection.select_time(&timeline, TimeUs::new(10)).unwrap(),
            FrameId::from_u128(2)
        );
        assert_eq!(
            selection.select_time(&timeline, TimeUs::new(30)).unwrap(),
            FrameId::from_u128(3)
        );
        assert_eq!(
            selection.select_time(&timeline, TimeUs::new(100)).unwrap(),
            FrameId::from_u128(4)
        );
        assert!(matches!(
            selection.select_time(&timeline, TimeUs::new(101)),
            Err(TimelineSelectionError::TimeOutOfRange { .. })
        ));
    }

    #[test]
    fn invalid_navigation_does_not_mutate_selection() {
        let timeline = timeline();
        let mut selection = TimelineSelection::new();
        selection
            .select_only(&timeline, FrameId::from_u128(2))
            .unwrap();
        let before = selection.clone();

        assert!(matches!(
            selection.select_only(&timeline, FrameId::from_u128(99)),
            Err(TimelineSelectionError::UnknownFrame(_))
        ));
        assert_eq!(selection, before);
        assert!(matches!(
            selection.select_frame_number(&timeline, 0),
            Err(TimelineSelectionError::FrameNumberOutOfRange { .. })
        ));
        assert_eq!(selection, before);
    }

    #[test]
    fn reconcile_keeps_survivors_and_repairs_current_anchor() {
        let timeline = timeline();
        let mut selection = TimelineSelection::new();
        selection
            .select_only(&timeline, FrameId::from_u128(2))
            .unwrap();
        selection.toggle(&timeline, FrameId::from_u128(3)).unwrap();
        let edited = Timeline {
            frames: vec![frame(4, 40), frame(2, 20), frame(1, 10)],
            ..Timeline::default()
        };

        selection.reconcile(&edited);
        assert_eq!(ids(&selection), [2]);
        assert_eq!(selection.current(), Some(FrameId::from_u128(2)));
        assert_eq!(selection.anchor(), Some(FrameId::from_u128(2)));
    }

    #[test]
    fn reconcile_selects_first_after_all_selected_frames_are_deleted() {
        let timeline = timeline();
        let mut selection = TimelineSelection::new();
        selection
            .select_only(&timeline, FrameId::from_u128(3))
            .unwrap();
        let edited = Timeline {
            frames: vec![frame(4, 40), frame(1, 10)],
            ..Timeline::default()
        };

        selection.reconcile(&edited);
        assert_eq!(ids(&selection), [4]);
        assert_eq!(selection.current(), Some(FrameId::from_u128(4)));
        assert_eq!(selection.anchor(), Some(FrameId::from_u128(4)));
    }

    #[test]
    fn empty_timeline_and_duration_overflow_are_typed() {
        let mut selection = TimelineSelection::new();
        assert_eq!(
            selection.first(&Timeline::default()),
            Err(TimelineSelectionError::EmptyTimeline)
        );
        let overflow = Timeline {
            frames: vec![frame(1, u64::MAX), frame(2, 1)],
            ..Timeline::default()
        };
        assert_eq!(
            selection.select_time(&overflow, TimeUs::ZERO),
            Err(TimelineSelectionError::DurationOverflow)
        );
    }
}
