//! Non-destructive Yoyo/Ping-pong timeline construction.

use std::collections::{BTreeMap, BTreeSet};

use gif_from_screen_domain::{EditCommand, FrameClip, FrameId, ProjectManifest};

use crate::{EditorError, ensure_known_selection};

/// Chooses the source range for a Yoyo operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum YoyoScope {
    /// Use the caller-provided consecutive frame selection.
    Selection,
    /// Use every frame in the timeline; the caller-provided selection is ignored.
    EntireTimeline,
}

/// Options for appending a reversed clone of a frame range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct YoyoOptions {
    /// Whether the first and last source frames are cloned onto the reverse leg.
    ///
    /// For `A B C D`, enabling this option produces `A B C D D' C' B' A'`. Disabling it
    /// produces `A B C D C' B'`, avoiding a repeated visual frame at both turnarounds when the
    /// entire animation loops.
    pub repeat_endpoints: bool,
    /// Whether to transform a selected range or the entire timeline.
    pub scope: YoyoScope,
}

/// Builds one atomic command that appends a reversed clone of a frame range.
///
/// Selection order is ignored and stable [`FrameId`] values are resolved against timeline order.
/// For [`YoyoScope::Selection`], the reversed leg is inserted immediately after the consecutive
/// selection. For [`YoyoScope::EntireTimeline`], it is appended to the timeline and the supplied
/// selection is ignored.
///
/// Every cloned clip receives a new identity from `generate_frame_id`; all other clip state is
/// cloned exactly, including its immutable asset reference, duration, transform, capture metadata,
/// and effects. Asset bytes and descriptors are never copied or registered.
///
/// Existing transitions are retained only when their original endpoints remain adjacent after the
/// insertion. Thus a transition crossing a selected range's insertion boundary is cleared.
/// Transitions are not synthesized for the reverse leg: transition direction can be semantic, and
/// the frame-ID generator cannot safely provide new transition identities.
///
/// # Errors
///
/// Returns an error when a selected scope is empty, unknown, or non-consecutive; when the source
/// range is too short for the endpoint behavior; when the generated timeline duration overflows;
/// or when the ID generator returns nil, an existing ID, or the same new ID more than once.
pub fn yoyo_frames<G>(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    options: YoyoOptions,
    mut generate_frame_id: G,
) -> Result<EditCommand, EditorError>
where
    G: FnMut() -> FrameId,
{
    let supplied_selection: BTreeSet<_> = frame_ids.into_iter().collect();
    let (source_start, source_end) = match options.scope {
        YoyoScope::Selection => selected_range(project, &supplied_selection)?,
        YoyoScope::EntireTimeline => (0, project.timeline.frames.len()),
    };
    let source = &project.timeline.frames[source_start..source_end];

    let minimum_frames = if options.repeat_endpoints { 2 } else { 3 };
    if source.len() < minimum_frames {
        return Err(EditorError::YoyoRangeTooShort {
            minimum_frames,
            actual_frames: source.len(),
        });
    }

    let reverse_sources = if options.repeat_endpoints {
        source
    } else {
        &source[1..source.len() - 1]
    };

    let mut occupied_ids: BTreeSet<_> = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    let mut reversed_frames = Vec::with_capacity(if options.repeat_endpoints {
        source.len()
    } else {
        source.len() - 2
    });
    let mut added_duration = 0_u64;

    for source_frame in reverse_sources.iter().rev() {
        let generated_id = generate_frame_id();
        if generated_id.is_nil() {
            return Err(EditorError::GeneratedNilFrameId);
        }
        if !occupied_ids.insert(generated_id) {
            return Err(EditorError::GeneratedFrameIdConflict(generated_id));
        }

        added_duration = added_duration
            .checked_add(source_frame.duration.get())
            .ok_or(EditorError::InvalidDuration)?;
        let mut cloned_frame = source_frame.clone();
        cloned_frame.id = generated_id;
        reversed_frames.push(cloned_frame);
    }

    project
        .timeline
        .total_duration()
        .and_then(|duration| duration.get().checked_add(added_duration))
        .ok_or(EditorError::InvalidDuration)?;

    let insertion_index = source_end;
    let retained_transitions =
        transitions_after_insertion(project, insertion_index, &reversed_frames);
    let mut commands = Vec::with_capacity(2);
    if retained_transitions.len() != project.timeline.transitions.len() {
        commands.push(EditCommand::SetTransitions {
            transitions: retained_transitions,
        });
    }
    commands.push(EditCommand::InsertFrames {
        index: insertion_index,
        frames: reversed_frames,
    });

    Ok(EditCommand::Compound { commands })
}

fn selected_range(
    project: &ProjectManifest,
    selected: &BTreeSet<FrameId>,
) -> Result<(usize, usize), EditorError> {
    ensure_known_selection(project, selected)?;
    let positions = project
        .timeline
        .frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| selected.contains(&frame.id))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    if positions
        .windows(2)
        .any(|pair| pair[0].checked_add(1) != Some(pair[1]))
    {
        return Err(EditorError::NonConsecutiveSelection);
    }

    let start = positions[0];
    Ok((start, start + positions.len()))
}

fn transitions_after_insertion(
    project: &ProjectManifest,
    insertion_index: usize,
    reversed_frames: &[FrameClip],
) -> Vec<gif_from_screen_domain::Transition> {
    let mut order = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect::<Vec<_>>();
    order.splice(
        insertion_index..insertion_index,
        reversed_frames.iter().map(|frame| frame.id),
    );
    let positions: BTreeMap<_, _> = order
        .into_iter()
        .enumerate()
        .map(|(index, frame_id)| (frame_id, index))
        .collect();

    project
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
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::BTreeMap};

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, DurationUs, EdgeWidths, Effect, FrameClip, FrameId, MouseButton,
        PhysicalPoint, PhysicalPx, PhysicalSize, ProjectId, ProjectManifest, ProjectRevision,
        RasterEncoding, Rgba, Timeline, Transition, TransitionId, TransitionKind, UnixTimeMs,
    };

    use super::*;
    use crate::EditorSession;

    fn project(frame_count: usize) -> ProjectManifest {
        let size = PhysicalSize::new(8, 6).unwrap();
        let asset_id = AssetId::from_digest([7; 32]);
        let mut assets = BTreeMap::new();
        assets.insert(
            asset_id,
            AssetDescriptor {
                id: asset_id,
                byte_len: 192,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        let frames = (0..frame_count)
            .map(|index| FrameClip {
                id: id(index + 1),
                asset_id,
                duration: DurationUs::new(
                    10_000 * u64::try_from(index + 1).expect("test index fits u64"),
                )
                .unwrap(),
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

    fn id(value: usize) -> FrameId {
        FrameId::from_u128(u128::try_from(value).expect("test ID fits u128"))
    }

    fn ids(project: &ProjectManifest) -> Vec<FrameId> {
        project
            .timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .collect()
    }

    fn transition(number: u128, from: usize, to: usize) -> Transition {
        Transition {
            id: TransitionId::from_u128(number),
            from_frame: id(from),
            to_frame: id(to),
            duration: DurationUs::new(1).unwrap(),
            steps: 1,
            kind: TransitionKind::FadeToNext,
        }
    }

    #[test]
    fn entire_timeline_without_repeated_endpoints_excludes_both_turnarounds() {
        let project = project(4);
        let next = Cell::new(10_u128);
        let command = yoyo_frames(
            &project,
            std::iter::empty(),
            YoyoOptions {
                repeat_endpoints: false,
                scope: YoyoScope::EntireTimeline,
            },
            || {
                let generated = FrameId::from_u128(next.get());
                next.set(next.get() + 1);
                generated
            },
        )
        .unwrap();
        assert!(matches!(command, EditCommand::Compound { .. }));

        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(
            ids(session.project()),
            [id(1), id(2), id(3), id(4), id(10), id(11)]
        );
        assert_eq!(
            session
                .project()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [10_000, 20_000, 30_000, 40_000, 30_000, 20_000]
        );
    }

    #[test]
    fn repeated_endpoints_clone_the_complete_reverse_range() {
        let project = project(3);
        let next = Cell::new(10_u128);
        let command = yoyo_frames(
            &project,
            [],
            YoyoOptions {
                repeat_endpoints: true,
                scope: YoyoScope::EntireTimeline,
            },
            || {
                let generated = FrameId::from_u128(next.get());
                next.set(next.get() + 1);
                generated
            },
        )
        .unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(
            ids(session.project()),
            [id(1), id(2), id(3), id(10), id(11), id(12)]
        );
        assert_eq!(
            session
                .project()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [10_000, 20_000, 30_000, 30_000, 20_000, 10_000]
        );
    }

    #[test]
    fn selection_is_resolved_in_timeline_order_and_inserted_after_its_range() {
        let project = project(5);
        let next = Cell::new(10_u128);
        let command = yoyo_frames(
            &project,
            [id(4), id(2), id(3)],
            YoyoOptions {
                repeat_endpoints: false,
                scope: YoyoScope::Selection,
            },
            || {
                let generated = FrameId::from_u128(next.get());
                next.set(next.get() + 1);
                generated
            },
        )
        .unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(
            ids(session.project()),
            [id(1), id(2), id(3), id(4), id(10), id(5)]
        );
        assert_eq!(session.project().timeline.frames[4].duration.get(), 30_000);
    }

    #[test]
    fn clones_reuse_assets_and_preserve_all_clip_state() {
        let mut project = project(3);
        let source = &mut project.timeline.frames[1];
        source.transform.flip_horizontal = true;
        source.capture_metadata.cursor_position = Some(PhysicalPoint {
            x: PhysicalPx::new(3),
            y: PhysicalPx::new(4),
        });
        source.capture_metadata.pressed_mouse_buttons = vec![MouseButton::Left];
        source.effects = vec![Effect::Border {
            widths: EdgeWidths {
                top: 1,
                right: 2,
                bottom: 3,
                left: 4,
            },
            color: Rgba {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 4,
            },
        }];
        let expected = source.clone();
        let asset_count = project.assets.len();
        let command = yoyo_frames(
            &project,
            [id(1), id(2), id(3)],
            YoyoOptions {
                repeat_endpoints: false,
                scope: YoyoScope::Selection,
            },
            || id(10),
        )
        .unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        let clone = &session.project().timeline.frames[3];
        let mut expected = expected;
        expected.id = id(10);
        assert_eq!(clone, &expected);
        assert_eq!(session.project().assets.len(), asset_count);
    }

    #[test]
    fn only_transition_broken_by_selection_insertion_is_cleared() {
        let mut project = project(5);
        project.timeline.transitions = vec![
            transition(101, 1, 2),
            transition(102, 2, 3),
            transition(103, 3, 4),
            transition(104, 4, 5),
        ];
        let command = yoyo_frames(
            &project,
            [id(2), id(3), id(4)],
            YoyoOptions {
                repeat_endpoints: false,
                scope: YoyoScope::Selection,
            },
            || id(10),
        )
        .unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(
            session
                .project()
                .timeline
                .transitions
                .iter()
                .map(|transition| transition.id)
                .collect::<Vec<_>>(),
            [
                TransitionId::from_u128(101),
                TransitionId::from_u128(102),
                TransitionId::from_u128(103),
            ]
        );
        assert!(
            session
                .project()
                .timeline
                .transitions
                .iter()
                .all(|transition| transition.from_frame != id(10))
        );
    }

    #[test]
    fn entire_timeline_keeps_all_safe_original_transitions() {
        let mut project = project(3);
        project.timeline.transitions = vec![transition(101, 1, 2), transition(102, 2, 3)];
        let expected = project.timeline.transitions.clone();
        let next = Cell::new(10_u128);
        let command = yoyo_frames(
            &project,
            [],
            YoyoOptions {
                repeat_endpoints: true,
                scope: YoyoScope::EntireTimeline,
            },
            || {
                let generated = FrameId::from_u128(next.get());
                next.set(next.get() + 1);
                generated
            },
        )
        .unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();
        session.execute(&command).unwrap();

        assert_eq!(session.project().timeline.transitions, expected);
    }

    #[test]
    fn compound_command_undo_and_redo_restore_frames_and_transitions() {
        let mut project = project(5);
        project.timeline.transitions = vec![transition(101, 1, 2), transition(104, 4, 5)];
        let original_frames = project.timeline.frames.clone();
        let original_transitions = project.timeline.transitions.clone();
        let command = yoyo_frames(
            &project,
            [id(2), id(3), id(4)],
            YoyoOptions {
                repeat_endpoints: false,
                scope: YoyoScope::Selection,
            },
            || id(10),
        )
        .unwrap();
        let mut session = EditorSession::new(project, 10).unwrap();

        session.execute(&command).unwrap();
        assert_eq!(session.project().revision, ProjectRevision::new(1));
        let applied_frames = session.project().timeline.frames.clone();
        let applied_transitions = session.project().timeline.transitions.clone();
        assert!(session.undo().unwrap());
        assert_eq!(session.project().timeline.frames, original_frames);
        assert_eq!(session.project().timeline.transitions, original_transitions);
        assert!(session.redo().unwrap());
        assert_eq!(session.project().timeline.frames, applied_frames);
        assert_eq!(session.project().timeline.transitions, applied_transitions);
    }

    #[test]
    fn invalid_selections_are_rejected_before_requesting_ids() {
        let project = project(4);
        let calls = Cell::new(0);
        let mut generator = || {
            calls.set(calls.get() + 1);
            id(10)
        };
        let options = YoyoOptions {
            repeat_endpoints: true,
            scope: YoyoScope::Selection,
        };

        assert!(matches!(
            yoyo_frames(&project, [], options, &mut generator),
            Err(EditorError::EmptySelection)
        ));
        assert!(matches!(
            yoyo_frames(&project, [id(99)], options, &mut generator),
            Err(EditorError::UnknownSelectedFrame(frame_id)) if frame_id == id(99)
        ));
        assert!(matches!(
            yoyo_frames(&project, [id(1), id(3)], options, &mut generator),
            Err(EditorError::NonConsecutiveSelection)
        ));
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn ranges_too_short_for_endpoint_mode_are_rejected() {
        let three_frame_project = project(3);
        assert!(matches!(
            yoyo_frames(
                &three_frame_project,
                [id(1)],
                YoyoOptions {
                    repeat_endpoints: true,
                    scope: YoyoScope::Selection,
                },
                || id(10),
            ),
            Err(EditorError::YoyoRangeTooShort {
                minimum_frames: 2,
                actual_frames: 1,
            })
        ));
        assert!(matches!(
            yoyo_frames(
                &three_frame_project,
                [id(1), id(2)],
                YoyoOptions {
                    repeat_endpoints: false,
                    scope: YoyoScope::Selection,
                },
                || id(10),
            ),
            Err(EditorError::YoyoRangeTooShort {
                minimum_frames: 3,
                actual_frames: 2,
            })
        ));
        assert!(matches!(
            yoyo_frames(
                &project(0),
                [],
                YoyoOptions {
                    repeat_endpoints: true,
                    scope: YoyoScope::EntireTimeline,
                },
                || id(10),
            ),
            Err(EditorError::YoyoRangeTooShort {
                minimum_frames: 2,
                actual_frames: 0,
            })
        ));
    }

    #[test]
    fn generated_nil_existing_and_repeated_ids_are_rejected() {
        let project = project(3);
        let options = YoyoOptions {
            repeat_endpoints: true,
            scope: YoyoScope::EntireTimeline,
        };

        assert!(matches!(
            yoyo_frames(&project, [], options, || FrameId::NIL),
            Err(EditorError::GeneratedNilFrameId)
        ));
        assert!(matches!(
            yoyo_frames(&project, [], options, || id(2)),
            Err(EditorError::GeneratedFrameIdConflict(frame_id)) if frame_id == id(2)
        ));
        assert!(matches!(
            yoyo_frames(&project, [], options, || id(10)),
            Err(EditorError::GeneratedFrameIdConflict(frame_id)) if frame_id == id(10)
        ));
    }

    #[test]
    fn added_duration_overflow_is_rejected() {
        let mut project = project(2);
        project.timeline.frames[0].duration = DurationUs::new(u64::MAX - 1).unwrap();
        project.timeline.frames[1].duration = DurationUs::new(1).unwrap();
        assert!(project.validate().is_ok());

        assert!(matches!(
            yoyo_frames(
                &project,
                [],
                YoyoOptions {
                    repeat_endpoints: true,
                    scope: YoyoScope::EntireTimeline,
                },
                {
                    let next = Cell::new(10_u128);
                    move || {
                        let generated = FrameId::from_u128(next.get());
                        next.set(next.get() + 1);
                        generated
                    }
                },
            ),
            Err(EditorError::InvalidDuration)
        ));
    }
}
