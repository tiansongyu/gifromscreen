//! Duplicate-frame detection and command construction.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

use gif_from_screen_domain::{
    DurationUs, EditCommand, FrameClip, FrameDurationChange, FrameId, ProjectManifest,
};

use crate::{EditorError, ensure_known_selection};

/// The result of comparing two rendered frames.
///
/// Providers must compare the final rendered pixels represented by the supplied stable
/// [`FrameId`] values. A dimension mismatch is a normal result, rather than an I/O failure, and
/// always separates duplicate groups.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameComparison {
    /// The rendered frames have different pixel dimensions and cannot be duplicates.
    DifferentDimensions,
    /// The rendered frames have equal dimensions and this is their inclusive 0..=100 similarity.
    SimilarityPercent(u8),
}

/// Supplies rendered-frame similarity without coupling the editor to asset storage or image I/O.
pub trait FrameSimilarityProvider {
    /// The provider-specific decode, render, or comparison error.
    type Error: Error + Send + Sync + 'static;

    /// Compares two adjacent frames identified by their stable timeline identities.
    ///
    /// Implementations must return [`FrameComparison::DifferentDimensions`] when their rendered
    /// dimensions differ. Similarity percentages greater than 100 are rejected by the editor.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific error when either frame cannot be loaded, rendered, or
    /// compared.
    fn compare(&self, first: FrameId, second: FrameId) -> Result<FrameComparison, Self::Error>;
}

/// Chooses which stable frame identity survives each adjacent duplicate group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateFrameRetention {
    /// Retain the first frame in timeline order.
    First,
    /// Retain the last frame in timeline order.
    Last,
}

/// Controls the surviving delay of each adjacent duplicate group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateDelayMode {
    /// Keep the surviving frame's original delay and discard every removed frame's delay.
    Keep,
    /// Assign the sum of all group delays to the survivor, preserving the group's total duration.
    Sum,
    /// Assign the rounded arithmetic mean of all group delays to the survivor.
    ///
    /// The calculation includes the survivor and removed frames. It rounds to the nearest
    /// microsecond, with an exact half rounded upward.
    Average,
}

/// Options for removing adjacent duplicate frames from a selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoveDuplicateFramesOptions {
    /// Inclusive similarity percentage required to join two adjacent selected frames.
    ///
    /// Valid values are 0 through 100. A value of 100 removes only frames the provider reports as
    /// identical; 0 joins every same-sized adjacent pair in a selected run.
    pub threshold: u8,
    /// Which frame identity survives each duplicate group.
    pub retention: DuplicateFrameRetention,
    /// How the survivor's delay is calculated.
    pub delay_mode: DuplicateDelayMode,
}

/// Builds one reversible atomic command that removes adjacent duplicate frames.
///
/// The input is treated as a set of stable [`FrameId`] values and resolved in timeline order.
/// Only frames that are both selected and adjacent in the original timeline are compared;
/// unselected frames split groups, so disjoint selections are supported without bridging gaps.
/// A group is formed transitively from qualifying adjacent comparisons: when A resembles B and B
/// resembles C, A/B/C form one group even if A and C were not directly compared.
///
/// Transitions that reference removed frames, cease to connect adjacent survivors, or exceed an
/// adjusted survivor delay are removed in the same command. The returned
/// [`EditCommand::Compound`] is therefore one project revision and its generated inverse restores
/// frames, delays, and transitions together.
///
/// # Errors
///
/// Returns an error for an empty or unknown selection, an invalid threshold or provider result, a
/// provider failure, no qualifying duplicate group, or duration overflow in
/// [`DuplicateDelayMode::Sum`].
pub fn remove_duplicate_frames<P>(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    options: RemoveDuplicateFramesOptions,
    provider: &P,
) -> Result<EditCommand, EditorError>
where
    P: FrameSimilarityProvider + ?Sized,
{
    if options.threshold > 100 {
        return Err(EditorError::InvalidSimilarityThreshold(options.threshold));
    }

    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;
    let duplicate_groups = find_duplicate_groups(project, &selected, options.threshold, provider)?;

    if duplicate_groups.is_empty() {
        return Err(EditorError::NoDuplicateFrames);
    }

    let mut removed_ids = Vec::new();
    let mut changes = Vec::new();
    for group in duplicate_groups {
        let survivor_index = match options.retention {
            DuplicateFrameRetention::First => 0,
            DuplicateFrameRetention::Last => group.len() - 1,
        };
        let survivor = group[survivor_index];
        let adjusted_duration = duplicate_group_duration(&group, survivor, options.delay_mode)?;

        if adjusted_duration != survivor.duration {
            changes.push(FrameDurationChange {
                frame_id: survivor.id,
                duration: adjusted_duration,
            });
        }
        removed_ids.extend(
            group
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != survivor_index)
                .map(|(_, frame)| frame.id),
        );
    }

    let removed: BTreeSet<_> = removed_ids.iter().copied().collect();
    let adjusted: BTreeMap<_, _> = changes
        .iter()
        .map(|change| (change.frame_id, change.duration))
        .collect();
    let remaining: BTreeMap<_, _> = project
        .timeline
        .frames
        .iter()
        .filter(|frame| !removed.contains(&frame.id))
        .enumerate()
        .map(|(index, frame)| {
            (
                frame.id,
                (
                    index,
                    adjusted.get(&frame.id).copied().unwrap_or(frame.duration),
                ),
            )
        })
        .collect();
    let transitions = project
        .timeline
        .transitions
        .iter()
        .filter(|transition| {
            let Some((from_index, from_duration)) = remaining.get(&transition.from_frame) else {
                return false;
            };
            let Some((to_index, to_duration)) = remaining.get(&transition.to_frame) else {
                return false;
            };
            from_index.checked_add(1) == Some(*to_index)
                && transition.duration <= *from_duration
                && transition.duration <= *to_duration
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

fn find_duplicate_groups<'a, P>(
    project: &'a ProjectManifest,
    selected: &BTreeSet<FrameId>,
    threshold: u8,
    provider: &P,
) -> Result<Vec<Vec<&'a FrameClip>>, EditorError>
where
    P: FrameSimilarityProvider + ?Sized,
{
    let mut groups = Vec::new();
    let mut candidate = Vec::new();

    for frame in &project.timeline.frames {
        if !selected.contains(&frame.id) {
            finish_candidate(&mut candidate, &mut groups);
            continue;
        }

        let Some(previous) = candidate.last().copied() else {
            candidate.push(frame);
            continue;
        };
        let comparison = provider.compare(previous.id, frame.id).map_err(|source| {
            EditorError::FrameComparisonFailed {
                first: previous.id,
                second: frame.id,
                source: Box::new(source),
            }
        })?;
        let matches = match comparison {
            FrameComparison::DifferentDimensions => false,
            FrameComparison::SimilarityPercent(percent) => {
                if percent > 100 {
                    return Err(EditorError::InvalidSimilarityPercent {
                        first: previous.id,
                        second: frame.id,
                        percent,
                    });
                }
                percent >= threshold
            }
        };

        if matches {
            candidate.push(frame);
        } else {
            finish_candidate(&mut candidate, &mut groups);
            candidate.push(frame);
        }
    }
    finish_candidate(&mut candidate, &mut groups);
    Ok(groups)
}

fn finish_candidate<'a>(candidate: &mut Vec<&'a FrameClip>, groups: &mut Vec<Vec<&'a FrameClip>>) {
    if candidate.len() > 1 {
        groups.push(std::mem::take(candidate));
    } else {
        candidate.clear();
    }
}

fn duplicate_group_duration(
    group: &[&FrameClip],
    survivor: &FrameClip,
    mode: DuplicateDelayMode,
) -> Result<DurationUs, EditorError> {
    match mode {
        DuplicateDelayMode::Keep => Ok(survivor.duration),
        DuplicateDelayMode::Sum => {
            let sum = group.iter().try_fold(0_u64, |sum, frame| {
                sum.checked_add(frame.duration.get())
                    .ok_or(EditorError::InvalidDuration)
            })?;
            DurationUs::new(sum).ok_or(EditorError::InvalidDuration)
        }
        DuplicateDelayMode::Average => {
            let sum = group
                .iter()
                .map(|frame| u128::from(frame.duration.get()))
                .sum::<u128>();
            let count = u128::try_from(group.len()).map_err(|_| EditorError::InvalidDuration)?;
            let rounded = sum
                .checked_add(count / 2)
                .ok_or(EditorError::InvalidDuration)?
                / count;
            let rounded = u64::try_from(rounded).map_err(|_| EditorError::InvalidDuration)?;
            DurationUs::new(rounded).ok_or(EditorError::InvalidDuration)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::BTreeMap};

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, FrameClip, PhysicalSize, ProjectId, ProjectRevision,
        RasterEncoding, Timeline, Transition, TransitionId, TransitionKind, UnixTimeMs,
    };
    use thiserror::Error;

    use super::*;
    use crate::EditorSession;

    #[derive(Clone, Debug)]
    enum ComparisonOutcome {
        Comparison(FrameComparison),
        Failure,
    }

    #[derive(Clone, Copy, Debug, Error)]
    #[error("test comparison failed")]
    struct TestComparisonError;

    #[derive(Debug, Default)]
    struct TestProvider {
        outcomes: BTreeMap<(FrameId, FrameId), ComparisonOutcome>,
        calls: RefCell<Vec<(FrameId, FrameId)>>,
    }

    impl TestProvider {
        fn with(mut self, first: u128, second: u128, comparison: FrameComparison) -> Self {
            self.outcomes.insert(
                (FrameId::from_u128(first), FrameId::from_u128(second)),
                ComparisonOutcome::Comparison(comparison),
            );
            self
        }

        fn failing(mut self, first: u128, second: u128) -> Self {
            self.outcomes.insert(
                (FrameId::from_u128(first), FrameId::from_u128(second)),
                ComparisonOutcome::Failure,
            );
            self
        }
    }

    impl FrameSimilarityProvider for TestProvider {
        type Error = TestComparisonError;

        fn compare(&self, first: FrameId, second: FrameId) -> Result<FrameComparison, Self::Error> {
            self.calls.borrow_mut().push((first, second));
            match self
                .outcomes
                .get(&(first, second))
                .unwrap_or_else(|| panic!("unexpected comparison between {first} and {second}"))
            {
                ComparisonOutcome::Comparison(comparison) => Ok(*comparison),
                ComparisonOutcome::Failure => Err(TestComparisonError),
            }
        }
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

    fn ids(project: &ProjectManifest) -> Vec<FrameId> {
        project
            .timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .collect()
    }

    fn numeric_ids(project: &ProjectManifest) -> Vec<u128> {
        ids(project)
            .into_iter()
            .map(|id| u128::from_be_bytes(*id.as_bytes()))
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

    fn options(
        retention: DuplicateFrameRetention,
        delay_mode: DuplicateDelayMode,
    ) -> RemoveDuplicateFramesOptions {
        RemoveDuplicateFramesOptions {
            threshold: 90,
            retention,
            delay_mode,
        }
    }

    fn transition(number: u128, from: u128, to: u128, duration: u64) -> Transition {
        Transition {
            id: TransitionId::from_u128(number),
            from_frame: FrameId::from_u128(from),
            to_frame: FrameId::from_u128(to),
            duration: DurationUs::new(duration).unwrap(),
            steps: 1,
            kind: TransitionKind::FadeToNext,
        }
    }

    #[test]
    fn adjacent_chains_form_groups_and_threshold_is_inclusive() {
        let project = project_with_durations(&[10, 20, 30, 40, 50, 60, 70]);
        let provider = TestProvider::default()
            .with(1, 2, FrameComparison::SimilarityPercent(90))
            .with(2, 3, FrameComparison::SimilarityPercent(99))
            .with(3, 4, FrameComparison::DifferentDimensions)
            .with(4, 5, FrameComparison::SimilarityPercent(89))
            .with(5, 6, FrameComparison::SimilarityPercent(90))
            .with(6, 7, FrameComparison::DifferentDimensions);
        let command = remove_duplicate_frames(
            &project,
            ids(&project),
            options(DuplicateFrameRetention::First, DuplicateDelayMode::Keep),
            &provider,
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(numeric_ids(session.project()), [1, 4, 5, 7]);
        assert_eq!(durations(session.project()), [10, 40, 50, 70]);
    }

    #[test]
    fn disjoint_selection_does_not_compare_across_unselected_gap() {
        let project = project_with_durations(&[10, 20, 30, 40, 50]);
        let provider = TestProvider::default()
            .with(1, 2, FrameComparison::SimilarityPercent(100))
            .with(4, 5, FrameComparison::SimilarityPercent(100));
        let selected = [1, 2, 4, 5].map(FrameId::from_u128);
        let command = remove_duplicate_frames(
            &project,
            selected,
            options(DuplicateFrameRetention::First, DuplicateDelayMode::Keep),
            &provider,
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(numeric_ids(session.project()), [1, 3, 4]);
        assert_eq!(
            provider.calls.into_inner(),
            [
                (FrameId::from_u128(1), FrameId::from_u128(2)),
                (FrameId::from_u128(4), FrameId::from_u128(5)),
            ]
        );
    }

    #[test]
    fn retain_last_sum_handles_entire_identical_timeline_and_undo_redo() {
        let mut project = project_with_durations(&[10, 20, 30, 40]);
        project.timeline.transitions = vec![
            transition(1, 1, 2, 5),
            transition(2, 2, 3, 5),
            transition(3, 3, 4, 5),
        ];
        let original_transitions = project.timeline.transitions.clone();
        let provider = TestProvider::default()
            .with(1, 2, FrameComparison::SimilarityPercent(100))
            .with(2, 3, FrameComparison::SimilarityPercent(100))
            .with(3, 4, FrameComparison::SimilarityPercent(100));
        let command = remove_duplicate_frames(
            &project,
            ids(&project),
            options(DuplicateFrameRetention::Last, DuplicateDelayMode::Sum),
            &provider,
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();
        assert_eq!(session.project().revision, ProjectRevision::new(1));
        assert_eq!(numeric_ids(session.project()), [4]);
        assert_eq!(durations(session.project()), [100]);
        assert!(session.project().timeline.transitions.is_empty());

        assert!(session.undo().unwrap());
        assert_eq!(numeric_ids(session.project()), [1, 2, 3, 4]);
        assert_eq!(durations(session.project()), [10, 20, 30, 40]);
        assert_eq!(session.project().timeline.transitions, original_transitions);

        assert!(session.redo().unwrap());
        assert_eq!(numeric_ids(session.project()), [4]);
        assert_eq!(durations(session.project()), [100]);
        assert!(session.project().timeline.transitions.is_empty());
    }

    #[test]
    fn average_rounds_half_up_and_keeps_unselected_edges() {
        let project = project_with_durations(&[50, 1, 2, 60]);
        let provider = TestProvider::default().with(2, 3, FrameComparison::SimilarityPercent(100));
        let command = remove_duplicate_frames(
            &project,
            [FrameId::from_u128(2), FrameId::from_u128(3)],
            options(DuplicateFrameRetention::First, DuplicateDelayMode::Average),
            &provider,
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(numeric_ids(session.project()), [1, 2, 4]);
        assert_eq!(durations(session.project()), [50, 2, 60]);
    }

    #[test]
    fn average_removes_transition_that_exceeds_adjusted_survivor() {
        let mut project = project_with_durations(&[100, 100, 1]);
        project.timeline.transitions = vec![transition(1, 1, 2, 80)];
        let provider = TestProvider::default().with(2, 3, FrameComparison::SimilarityPercent(100));
        let command = remove_duplicate_frames(
            &project,
            [FrameId::from_u128(2), FrameId::from_u128(3)],
            options(DuplicateFrameRetention::First, DuplicateDelayMode::Average),
            &provider,
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(numeric_ids(session.project()), [1, 2]);
        assert_eq!(durations(session.project()), [100, 51]);
        assert!(session.project().timeline.transitions.is_empty());
    }

    #[test]
    fn dimension_mismatch_splits_groups() {
        let project = project_with_durations(&[10, 20, 30]);
        let provider = TestProvider::default()
            .with(1, 2, FrameComparison::DifferentDimensions)
            .with(2, 3, FrameComparison::SimilarityPercent(100));
        let command = remove_duplicate_frames(
            &project,
            ids(&project),
            options(DuplicateFrameRetention::First, DuplicateDelayMode::Keep),
            &provider,
        )
        .unwrap();

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(numeric_ids(session.project()), [1, 2]);
    }

    #[test]
    fn comparison_failure_reports_the_adjacent_frame_ids_and_source() {
        let project = project_with_durations(&[10, 20]);
        let provider = TestProvider::default().failing(1, 2);
        let error = remove_duplicate_frames(
            &project,
            ids(&project),
            options(DuplicateFrameRetention::First, DuplicateDelayMode::Keep),
            &provider,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            EditorError::FrameComparisonFailed { first, second, .. }
                if first == FrameId::from_u128(1) && second == FrameId::from_u128(2)
        ));
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some("test comparison failed".to_owned())
        );
    }

    #[test]
    fn invalid_provider_percentage_is_rejected_with_frame_context() {
        let project = project_with_durations(&[10, 20]);
        let provider = TestProvider::default().with(1, 2, FrameComparison::SimilarityPercent(101));
        let error = remove_duplicate_frames(
            &project,
            ids(&project),
            options(DuplicateFrameRetention::First, DuplicateDelayMode::Keep),
            &provider,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            EditorError::InvalidSimilarityPercent {
                first,
                second,
                percent: 101
            } if first == FrameId::from_u128(1) && second == FrameId::from_u128(2)
        ));
    }

    #[test]
    fn invalid_threshold_empty_unknown_and_no_duplicates_are_distinct() {
        let project = project_with_durations(&[10, 20]);
        let provider = TestProvider::default().with(1, 2, FrameComparison::SimilarityPercent(50));

        assert!(matches!(
            remove_duplicate_frames(
                &project,
                ids(&project),
                RemoveDuplicateFramesOptions {
                    threshold: 101,
                    retention: DuplicateFrameRetention::First,
                    delay_mode: DuplicateDelayMode::Keep,
                },
                &provider,
            ),
            Err(EditorError::InvalidSimilarityThreshold(101))
        ));
        assert!(matches!(
            remove_duplicate_frames(
                &project,
                [],
                options(DuplicateFrameRetention::First, DuplicateDelayMode::Keep),
                &provider,
            ),
            Err(EditorError::EmptySelection)
        ));
        assert!(matches!(
            remove_duplicate_frames(
                &project,
                [FrameId::from_u128(99)],
                options(
                    DuplicateFrameRetention::First,
                    DuplicateDelayMode::Keep
                ),
                &provider,
            ),
            Err(EditorError::UnknownSelectedFrame(frame_id))
                if frame_id == FrameId::from_u128(99)
        ));
        assert!(matches!(
            remove_duplicate_frames(
                &project,
                ids(&project),
                options(DuplicateFrameRetention::First, DuplicateDelayMode::Keep),
                &provider,
            ),
            Err(EditorError::NoDuplicateFrames)
        ));
    }

    #[test]
    fn sum_reports_duration_overflow_before_building_a_command() {
        let project = project_with_durations(&[u64::MAX, 1]);
        let provider = TestProvider::default().with(1, 2, FrameComparison::SimilarityPercent(100));
        let error = remove_duplicate_frames(
            &project,
            ids(&project),
            options(DuplicateFrameRetention::First, DuplicateDelayMode::Sum),
            &provider,
        )
        .unwrap_err();

        assert!(matches!(error, EditorError::InvalidDuration));
    }
}
