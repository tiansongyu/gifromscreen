use gif_from_screen_domain::{
    DurationUs, EditCommand, FrameId, MAX_TRANSITION_STEPS, ProjectManifest, Transition,
    TransitionId, TransitionKind,
};

use crate::EditorError;

/// Validated settings for the generated intermediate frames between two timeline clips.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameTransitionSettings {
    /// Total added duration distributed exactly across all intermediate frames.
    pub duration: DurationUs,
    /// Number of intermediate frames, excluding the two original endpoints.
    pub steps: u16,
    /// Pixel interpolation or slide behavior.
    pub kind: TransitionKind,
}

/// Builds one reversible command that creates or replaces the current frame's outgoing transition.
///
/// The ordered endpoint pair is always the current frame followed by its immediate timeline
/// successor. If that pair already has a transition, its stable identity and vector position are
/// preserved and the ID generator is not called. Otherwise exactly one new identity is requested.
/// No asset, clip, duration, effect, transform, or metadata field is changed.
///
/// # Errors
///
/// Returns [`EditorError`] when there is no current frame, the current identity is stale or is the
/// final frame, `steps` is outside `1..=MAX_TRANSITION_STEPS`, the total duration is shorter than
/// the step count, the generated identity is nil or already used, or corrupt input contains more
/// than one transition for the same ordered endpoint pair. Validation completes before a command
/// is returned and the generator is called only when insertion is required.
pub fn set_transition_after(
    project: &ProjectManifest,
    current_frame: Option<FrameId>,
    settings: FrameTransitionSettings,
    generate_id: impl FnOnce() -> TransitionId,
) -> Result<EditCommand, EditorError> {
    validate_settings(&settings)?;
    let (from_frame, to_frame) = transition_pair(project, current_frame)?;
    let mut transitions = project.timeline.transitions.clone();
    let matching = matching_transition_index(&transitions, from_frame, to_frame)?;

    if let Some(index) = matching {
        let id = transitions[index].id;
        transitions[index] = Transition {
            id,
            from_frame,
            to_frame,
            duration: settings.duration,
            steps: settings.steps,
            kind: settings.kind,
        };
    } else {
        let id = generate_id();
        if id.is_nil() {
            return Err(EditorError::GeneratedNilTransitionId);
        }
        if transitions.iter().any(|transition| transition.id == id) {
            return Err(EditorError::GeneratedTransitionIdConflict(id));
        }
        transitions.push(Transition {
            id,
            from_frame,
            to_frame,
            duration: settings.duration,
            steps: settings.steps,
            kind: settings.kind,
        });
    }

    Ok(EditCommand::SetTransitions { transitions })
}

/// Builds one reversible command that removes the current frame's outgoing transition.
///
/// The target is the ordered pair formed by the current frame and its immediate timeline
/// successor. Other transitions retain their order and complete contents.
///
/// # Errors
///
/// Returns [`EditorError`] when there is no current frame, the current identity is stale or final,
/// the pair has no transition, or corrupt input contains more than one matching transition.
pub fn remove_transition_after(
    project: &ProjectManifest,
    current_frame: Option<FrameId>,
) -> Result<EditCommand, EditorError> {
    let (from_frame, to_frame) = transition_pair(project, current_frame)?;
    let mut transitions = project.timeline.transitions.clone();
    let Some(index) = matching_transition_index(&transitions, from_frame, to_frame)? else {
        return Err(EditorError::MissingTransitionPair {
            from_frame,
            to_frame,
        });
    };
    transitions.remove(index);
    Ok(EditCommand::SetTransitions { transitions })
}

fn validate_settings(settings: &FrameTransitionSettings) -> Result<(), EditorError> {
    if settings.steps == 0 || settings.steps > MAX_TRANSITION_STEPS {
        return Err(EditorError::InvalidTransitionSteps {
            steps: settings.steps,
            maximum: MAX_TRANSITION_STEPS,
        });
    }
    if settings.duration.get() < u64::from(settings.steps) {
        return Err(EditorError::TransitionDurationTooShort {
            duration_us: settings.duration.get(),
            steps: settings.steps,
        });
    }
    Ok(())
}

fn transition_pair(
    project: &ProjectManifest,
    current_frame: Option<FrameId>,
) -> Result<(FrameId, FrameId), EditorError> {
    let current_frame = current_frame.ok_or(EditorError::NoCurrentFrameForTransition)?;
    let index = project
        .timeline
        .frames
        .iter()
        .position(|frame| frame.id == current_frame)
        .ok_or(EditorError::UnknownSelectedFrame(current_frame))?;
    let next = project
        .timeline
        .frames
        .get(index + 1)
        .ok_or(EditorError::NoFrameAfterTransitionAnchor(current_frame))?;
    Ok((current_frame, next.id))
}

fn matching_transition_index(
    transitions: &[Transition],
    from_frame: FrameId,
    to_frame: FrameId,
) -> Result<Option<usize>, EditorError> {
    let mut matches = transitions
        .iter()
        .enumerate()
        .filter(|(_, transition)| {
            transition.from_frame == from_frame && transition.to_frame == to_frame
        })
        .map(|(index, _)| index);
    let first = matches.next();
    if matches.next().is_some() {
        Err(EditorError::AmbiguousTransitionPair {
            from_frame,
            to_frame,
        })
    } else {
        Ok(first)
    }
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

    fn id(number: u128) -> FrameId {
        FrameId::from_u128(number)
    }

    fn manifest() -> ProjectManifest {
        let size = PhysicalSize::new(2, 1).unwrap();
        let asset_id = AssetId::from_digest([7; 32]);
        let frames = [1, 2, 3]
            .into_iter()
            .map(|number| FrameClip {
                id: id(number),
                asset_id,
                duration: DurationUs::new(10).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata::default(),
                effects: Vec::new(),
            })
            .collect();
        ProjectManifest {
            schema_version: gif_from_screen_domain::CURRENT_SCHEMA_VERSION,
            project_id: ProjectId::from_u128(1),
            revision: ProjectRevision::ZERO,
            app_version: "test".into(),
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
            assets: BTreeMap::from([(
                asset_id,
                AssetDescriptor {
                    id: asset_id,
                    byte_len: 8,
                    kind: AssetKind::Frame {
                        size,
                        encoding: RasterEncoding::Rgba8,
                    },
                },
            )]),
            export_presets: BTreeMap::new(),
            source_provenance: Vec::new(),
        }
    }

    fn settings(steps: u16) -> FrameTransitionSettings {
        FrameTransitionSettings {
            duration: DurationUs::new(u64::from(steps).max(1)).unwrap(),
            steps,
            kind: TransitionKind::FadeToNext,
        }
    }

    #[test]
    fn create_replace_remove_and_inverse_preserve_identity_and_other_project_state() {
        let mut project = manifest();
        let original = project.clone();
        let create = set_transition_after(&project, Some(id(1)), settings(3), || {
            TransitionId::from_u128(10)
        })
        .unwrap();
        let create_inverse = project.apply_command(&create).unwrap().inverse;
        assert_eq!(project.timeline.transitions.len(), 1);
        assert_eq!(
            project.timeline.transitions[0].id,
            TransitionId::from_u128(10)
        );
        assert_eq!(project.timeline.transitions[0].steps, 3);
        assert_eq!(project.timeline.frames, original.timeline.frames);
        assert_eq!(project.assets, original.assets);

        let replace = set_transition_after(
            &project,
            Some(id(1)),
            FrameTransitionSettings {
                duration: DurationUs::new(12).unwrap(),
                steps: 4,
                kind: TransitionKind::FadeToColor {
                    color: gif_from_screen_domain::Rgba {
                        red: 1,
                        green: 2,
                        blue: 3,
                        alpha: 4,
                    },
                },
            },
            || panic!("replacement must preserve the existing id"),
        )
        .unwrap();
        let replace_inverse = project.apply_command(&replace).unwrap().inverse;
        assert_eq!(project.timeline.transitions.len(), 1);
        assert_eq!(
            project.timeline.transitions[0].id,
            TransitionId::from_u128(10)
        );
        assert_eq!(project.timeline.transitions[0].steps, 4);
        project.apply_command(&replace_inverse).unwrap();
        assert_eq!(project.timeline.transitions[0].steps, 3);

        let remove = remove_transition_after(&project, Some(id(1))).unwrap();
        let remove_inverse = project.apply_command(&remove).unwrap().inverse;
        assert!(project.timeline.transitions.is_empty());
        project.apply_command(&remove_inverse).unwrap();
        assert_eq!(
            project.timeline.transitions[0].id,
            TransitionId::from_u128(10)
        );

        project.apply_command(&create_inverse).unwrap();
        assert!(project.timeline.transitions.is_empty());
    }

    #[test]
    fn pair_and_settings_failures_are_typed_before_identity_generation() {
        let project = manifest();
        assert!(matches!(
            set_transition_after(&project, None, settings(1), || unreachable!()),
            Err(EditorError::NoCurrentFrameForTransition)
        ));
        assert!(matches!(
            set_transition_after(&project, Some(id(99)), settings(1), || unreachable!()),
            Err(EditorError::UnknownSelectedFrame(frame)) if frame == id(99)
        ));
        assert!(matches!(
            set_transition_after(&project, Some(id(3)), settings(1), || unreachable!()),
            Err(EditorError::NoFrameAfterTransitionAnchor(frame)) if frame == id(3)
        ));
        for steps in [0, MAX_TRANSITION_STEPS + 1] {
            assert!(matches!(
                set_transition_after(&project, Some(id(1)), settings(steps), || unreachable!()),
                Err(EditorError::InvalidTransitionSteps { steps: actual, maximum })
                    if actual == steps && maximum == MAX_TRANSITION_STEPS
            ));
        }
        let too_short = FrameTransitionSettings {
            duration: DurationUs::new(2).unwrap(),
            steps: 3,
            kind: TransitionKind::FadeToNext,
        };
        assert!(matches!(
            set_transition_after(&project, Some(id(1)), too_short, || unreachable!()),
            Err(EditorError::TransitionDurationTooShort {
                duration_us: 2,
                steps: 3
            })
        ));
    }

    #[test]
    fn insertion_identity_and_removal_failures_do_not_build_commands() {
        let project = manifest();
        assert!(matches!(
            set_transition_after(&project, Some(id(1)), settings(1), || TransitionId::NIL),
            Err(EditorError::GeneratedNilTransitionId)
        ));

        let mut project = project;
        project.timeline.transitions.push(Transition {
            id: TransitionId::from_u128(7),
            from_frame: id(2),
            to_frame: id(3),
            duration: DurationUs::new(1).unwrap(),
            steps: 1,
            kind: TransitionKind::FadeToNext,
        });
        assert!(matches!(
            set_transition_after(&project, Some(id(1)), settings(1), || TransitionId::from_u128(7)),
            Err(EditorError::GeneratedTransitionIdConflict(value))
                if value == TransitionId::from_u128(7)
        ));
        assert!(matches!(
            remove_transition_after(&project, Some(id(1))),
            Err(EditorError::MissingTransitionPair { from_frame, to_frame })
                if from_frame == id(1) && to_frame == id(2)
        ));

        let duplicate = Transition {
            id: TransitionId::from_u128(8),
            from_frame: id(2),
            to_frame: id(3),
            duration: DurationUs::new(1).unwrap(),
            steps: 1,
            kind: TransitionKind::FadeToNext,
        };
        project.timeline.transitions.push(duplicate);
        assert!(matches!(
            remove_transition_after(&project, Some(id(2))),
            Err(EditorError::AmbiguousTransitionPair { from_frame, to_frame })
                if from_frame == id(2) && to_frame == id(3)
        ));
    }
}
