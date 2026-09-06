//! Compact presentation timing shared by exports and interactive playback.

use std::collections::BTreeMap;

use gif_from_screen_domain::{
    DurationUs, FrameClip, FrameId, MAX_TRANSITION_STEPS, ProjectManifest, Transition, TransitionId,
};
use gif_from_screen_render::{RenderError, TransitionProgress};

use crate::ProjectGifExportError;

/// One generated intermediate. Its endpoints are original, fully rendered frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationTransitionStep {
    /// Transition to render.
    pub transition_id: TransitionId,
    /// Outgoing original frame, also the editing selection during this step.
    pub from_frame: FrameId,
    /// Incoming original frame.
    pub to_frame: FrameId,
    /// One-based intermediate index, excluding the two original endpoints.
    pub step: u16,
}

/// One original or intermediate presentation frame, without its pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationSample {
    /// Original frame associated with this sample.
    pub frame_id: FrameId,
    /// Position of that frame within the selected originals.
    pub frame_index: usize,
    /// Start within the expanded presentation, in microseconds.
    pub start_us: u64,
    /// Exact duration before GIF centisecond quantization.
    pub duration: DurationUs,
    /// Intermediate information, absent for original frames.
    pub transition: Option<PresentationTransitionStep>,
}

#[derive(Debug)]
struct PresentationSegment {
    frame_id: FrameId,
    start_us: u64,
    original_duration: DurationUs,
}

/// Indexed original frames with optional outgoing transitions.
///
/// Construction is O(n log n); storage is O(original frames), independent of
/// transition step counts. Sampling uses binary search plus constant-time
/// integer arithmetic, including arbitrarily late looping playback.
#[derive(Debug)]
pub struct PresentationPlan {
    segments: Vec<PresentationSegment>,
    transitions: Vec<Option<Transition>>,
    duration_us: u64,
    frame_count: u64,
}

impl PresentationPlan {
    /// Builds the same forward-adjacent transition sequence used by GIF export.
    ///
    /// # Errors
    ///
    /// Rejects empty input, duplicate transition endpoints, invalid applicable
    /// timing, or an expanded presentation whose duration/count overflows.
    pub fn new(
        manifest: &ProjectManifest,
        clips: &[FrameClip],
    ) -> Result<Self, ProjectGifExportError> {
        if clips.is_empty() {
            return Err(ProjectGifExportError::EmptySelection);
        }
        let transitions = applicable_transitions(manifest, clips)?;
        let frame_count = expanded_frame_count(clips.len(), &transitions)?;
        let mut segments = Vec::with_capacity(clips.len());
        let mut duration_us = 0_u64;
        for (index, clip) in clips.iter().enumerate() {
            segments.push(PresentationSegment {
                frame_id: clip.id,
                start_us: duration_us,
                original_duration: clip.duration,
            });
            duration_us = duration_us
                .checked_add(clip.duration.get())
                .and_then(|total| {
                    total.checked_add(
                        transitions
                            .get(index)
                            .and_then(Option::as_ref)
                            .map_or(0, |transition| transition.duration.get()),
                    )
                })
                .ok_or(ProjectGifExportError::OutputDurationOverflow)?;
        }
        Ok(Self {
            segments,
            transitions,
            duration_us,
            frame_count,
        })
    }

    /// Total expanded duration, including generated intermediates.
    pub const fn duration_us(&self) -> u64 {
        self.duration_us
    }

    /// Number of original and intermediate output frames.
    pub const fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Start of an original frame within this presentation.
    pub fn frame_start_us(&self, index: usize) -> Option<u64> {
        self.segments.get(index).map(|segment| segment.start_us)
    }

    pub(crate) fn transitions(&self) -> &[Option<Transition>] {
        &self.transitions
    }

    /// Resolves a half-open presentation timestamp. The terminal timestamp
    /// returns `None`; callers choose whether to hold the last frame or repeat.
    pub fn sample_at_us(&self, time_us: u64) -> Option<PresentationSample> {
        if time_us >= self.duration_us {
            return None;
        }
        let index = self
            .segments
            .partition_point(|segment| segment.start_us <= time_us)
            - 1;
        let segment = &self.segments[index];
        let age = time_us - segment.start_us;
        if age < segment.original_duration.get() {
            return Some(PresentationSample {
                frame_id: segment.frame_id,
                frame_index: index,
                start_us: segment.start_us,
                duration: segment.original_duration,
                transition: None,
            });
        }
        let transition = self.transitions.get(index)?.as_ref()?;
        let age = age - segment.original_duration.get();
        let steps = u64::from(transition.steps);
        let base = transition.duration.get() / steps;
        let remainder = transition.duration.get() % steps;
        let long_duration = base + 1;
        let long_span = long_duration * remainder;
        let (zero_based_step, step_start) = if age < long_span {
            let step = age / long_duration;
            (step, step * long_duration)
        } else {
            let step = (age - long_span) / base;
            (remainder + step, long_span + step * base)
        };
        let step = u16::try_from(zero_based_step).ok()?;
        Some(PresentationSample {
            frame_id: segment.frame_id,
            frame_index: index,
            start_us: segment.start_us + segment.original_duration.get() + step_start,
            duration: DurationUs::new(transition_step_duration(transition, step))?,
            transition: Some(PresentationTransitionStep {
                transition_id: transition.id,
                from_frame: transition.from_frame,
                to_frame: transition.to_frame,
                step: step + 1,
            }),
        })
    }
}

fn applicable_transitions(
    manifest: &ProjectManifest,
    clips: &[FrameClip],
) -> Result<Vec<Option<Transition>>, ProjectGifExportError> {
    let positions: BTreeMap<_, _> = manifest
        .timeline
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| (frame.id, index))
        .collect();
    let mut transitions_by_endpoint: BTreeMap<_, &Transition> = BTreeMap::new();
    for transition in &manifest.timeline.transitions {
        let endpoints = (transition.from_frame, transition.to_frame);
        if let Some(first) = transitions_by_endpoint.insert(endpoints, transition) {
            return Err(ProjectGifExportError::DuplicateTransitionEndpoints {
                first_transition_id: first.id,
                duplicate_transition_id: transition.id,
                from_frame: transition.from_frame,
                to_frame: transition.to_frame,
            });
        }
    }
    clips
        .windows(2)
        .map(|pair| {
            if positions
                .get(&pair[0].id)
                .zip(positions.get(&pair[1].id))
                .is_none_or(|(from, to)| from.checked_add(1) != Some(*to))
            {
                return Ok(None);
            }
            let transition = transitions_by_endpoint
                .get(&(pair[0].id, pair[1].id))
                .map(|transition| (*transition).clone());
            if let Some(transition) = &transition {
                if transition.steps == 0 || transition.steps > MAX_TRANSITION_STEPS {
                    return Err(ProjectGifExportError::InvalidTransitionSteps {
                        transition: transition.clone(),
                        steps: transition.steps,
                    });
                }
                if transition.duration.get() < u64::from(transition.steps) {
                    return Err(ProjectGifExportError::TransitionDurationTooShort {
                        transition: transition.clone(),
                        duration_us: transition.duration.get(),
                        steps: transition.steps,
                    });
                }
            }
            Ok(transition)
        })
        .collect()
}

pub(crate) fn expanded_frame_count(
    selected_frames: usize,
    transitions: &[Option<Transition>],
) -> Result<u64, ProjectGifExportError> {
    let count = transitions
        .iter()
        .flatten()
        .try_fold(selected_frames, |total, transition| {
            total.checked_add(usize::from(transition.steps))
        })
        .ok_or(ProjectGifExportError::OutputFrameCountOverflow)?;
    u64::try_from(count).map_err(|_| ProjectGifExportError::OutputFrameCountOverflow)
}

/// Exact duration for a zero-based intermediate in a validated transition.
/// Invalid input yields zero, allowing callers to reject it without panicking.
pub fn transition_step_duration(transition: &Transition, zero_based_step: u16) -> u64 {
    if transition.steps == 0 || zero_based_step >= transition.steps {
        return 0;
    }
    let steps = u64::from(transition.steps);
    transition.duration.get() / steps
        + u64::from(u64::from(zero_based_step) < transition.duration.get() % steps)
}

/// Resolves a one-based intermediate to the renderer's exact rational progress.
///
/// # Errors
///
/// Rejects endpoint indices and invalid intermediate counts.
pub fn transition_step_progress(
    transition: &Transition,
    step: u16,
) -> Result<TransitionProgress, RenderError> {
    if step == 0 || step > transition.steps || transition.steps > MAX_TRANSITION_STEPS {
        return Err(RenderError::InvalidTransitionProgress {
            step: u32::from(step),
            steps: u32::from(transition.steps),
        });
    }
    TransitionProgress::new(u32::from(step), u32::from(transition.steps) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::{
        AssetId, Canvas, CanvasBackground, CaptureMetadata, ClipTransform, ColorSpace,
        PhysicalSize, ProjectId, TransitionKind, UnixTimeMs,
    };

    fn manifest(transition_duration: u64, steps: u16) -> ProjectManifest {
        let mut manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "presentation-test",
            UnixTimeMs::new(0),
            Canvas {
                size: PhysicalSize::new(1, 1).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        manifest.timeline.frames = [10, 20, 30]
            .into_iter()
            .enumerate()
            .map(|(index, duration)| FrameClip {
                render_steps: Vec::new(),
                capture_clock: None,
                capture_binding: gif_from_screen_domain::CaptureBinding::Original,
                id: FrameId::from_u128(index as u128 + 1),
                asset_id: AssetId::from_digest([1; 32]),
                duration: DurationUs::new(duration).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata::default(),
                effects: Vec::new(),
            })
            .collect();
        manifest.timeline.transitions.push(Transition {
            id: TransitionId::from_u128(1),
            from_frame: manifest.timeline.frames[0].id,
            to_frame: manifest.timeline.frames[1].id,
            duration: DurationUs::new(transition_duration).unwrap(),
            steps,
            kind: TransitionKind::FadeToNext,
        });
        manifest
    }

    #[test]
    fn sample_boundaries_match_export_step_remainders_without_expansion() {
        let manifest = manifest(8, 3);
        let plan = PresentationPlan::new(&manifest, &manifest.timeline.frames).unwrap();
        assert_eq!(plan.duration_us(), 68);
        assert_eq!(plan.frame_count(), 6);
        assert_eq!(plan.segments.len(), 3);
        for (time, frame, step, start, duration) in [
            (0, 1, None, 0, 10),
            (9, 1, None, 0, 10),
            (10, 1, Some(1), 10, 3),
            (12, 1, Some(1), 10, 3),
            (13, 1, Some(2), 13, 3),
            (15, 1, Some(2), 13, 3),
            (16, 1, Some(3), 16, 2),
            (17, 1, Some(3), 16, 2),
            (18, 2, None, 18, 20),
            (37, 2, None, 18, 20),
            (38, 3, None, 38, 30),
            (67, 3, None, 38, 30),
        ] {
            let sample = plan.sample_at_us(time).unwrap();
            assert_eq!(sample.frame_id, FrameId::from_u128(frame));
            assert_eq!(sample.transition.map(|value| value.step), step);
            assert_eq!(sample.start_us, start);
            assert_eq!(sample.duration.get(), duration);
        }
        assert_eq!(plan.sample_at_us(68), None);
        assert_eq!(plan.sample_at_us(u64::MAX), None);
        assert_eq!(plan.frame_start_us(1), Some(18));
    }

    #[test]
    fn selected_reverse_and_nonadjacent_clips_skip_outgoing_transitions() {
        let manifest = manifest(8, 3);
        let clips = &manifest.timeline.frames;
        for selected in [
            vec![clips[1].clone(), clips[0].clone()],
            vec![clips[0].clone(), clips[2].clone()],
        ] {
            let plan = PresentationPlan::new(&manifest, &selected).unwrap();
            assert_eq!(plan.frame_count(), 2);
            assert!(plan.transitions().iter().all(Option::is_none));
        }
    }

    #[test]
    fn a_large_transition_stays_compact_and_resolves_its_last_microsecond() {
        let manifest = manifest(u64::MAX - 60, MAX_TRANSITION_STEPS);
        let plan = PresentationPlan::new(&manifest, &manifest.timeline.frames).unwrap();
        assert_eq!(plan.segments.len(), 3);
        assert_eq!(plan.transitions.len(), 2);
        assert_eq!(plan.duration_us(), u64::MAX);
        let transition_end = 10 + manifest.timeline.transitions[0].duration.get();
        assert_eq!(
            plan.sample_at_us(transition_end - 1)
                .unwrap()
                .transition
                .unwrap()
                .step,
            MAX_TRANSITION_STEPS
        );
        assert!(
            plan.sample_at_us(transition_end)
                .unwrap()
                .transition
                .is_none()
        );
    }

    #[test]
    fn shared_step_helpers_reject_invalid_indices_and_preserve_exact_ratio() {
        let mut transition = manifest(8, 3).timeline.transitions.remove(0);
        for step in 1..=3 {
            let progress = transition_step_progress(&transition, step).unwrap();
            assert_eq!(progress.step(), u32::from(step));
            assert_eq!(progress.steps(), 4);
        }
        assert!(transition_step_progress(&transition, 0).is_err());
        assert!(transition_step_progress(&transition, 4).is_err());
        assert_eq!(transition_step_duration(&transition, 3), 0);
        transition.steps = 0;
        assert_eq!(transition_step_duration(&transition, 0), 0);
        assert!(transition_step_progress(&transition, 1).is_err());
    }

    #[test]
    fn duplicate_and_invalid_timing_use_export_validation_errors() {
        let mut manifest = manifest(2, 3);
        assert!(matches!(
            PresentationPlan::new(&manifest, &manifest.timeline.frames),
            Err(ProjectGifExportError::TransitionDurationTooShort { .. })
        ));
        manifest.timeline.transitions[0].steps = 0;
        assert!(matches!(
            PresentationPlan::new(&manifest, &manifest.timeline.frames),
            Err(ProjectGifExportError::InvalidTransitionSteps { .. })
        ));
        manifest
            .timeline
            .transitions
            .push(manifest.timeline.transitions[0].clone());
        assert!(matches!(
            PresentationPlan::new(&manifest, &manifest.timeline.frames),
            Err(ProjectGifExportError::DuplicateTransitionEndpoints { .. })
        ));
    }
}
