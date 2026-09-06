use super::tests::{manifest, track};
use super::*;
// Persisted timed scopes keep their original ripple and gap semantics.
use super::prepare_legacy_annotations_with_assets as prepare_annotations_with_assets;
use gif_from_screen_domain::{CaptureBinding, DurationUs, FrameDurationChange, KeyStroke};

fn span(start: u64, end: u64) -> TimelineSpan {
    TimelineSpan {
        start: TimeUs::new(start),
        duration: DurationUs::new(end - start).unwrap(),
    }
}
fn first_key_project() -> ProjectManifest {
    let mut project = manifest();
    project.timeline.frames[0]
        .capture_metadata
        .key_strokes
        .push(KeyStroke {
            physical_key: "KeyA".to_owned(),
            display_text: Some("A".to_owned()),
            pressed: true,
            at: TimeUs::ZERO,
            repeat: false,
            modifiers: 0,
        });
    project
}
fn keys(hold_ms: u32) -> AnnotationRequest {
    AnnotationRequest {
        mode: AnnotationMode::RecordedKeys,
        hold_ms,
        ..AnnotationRequest::default()
    }
}
fn prepare(
    project: &ProjectManifest,
    selected: &[u128],
    request: &AnnotationRequest,
) -> PreparedAnnotations {
    prepare_annotations_with_assets(
        project,
        &selected.iter().copied().map(FrameId::from_u128).collect(),
        request,
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!("keys and progress do not read cursor assets"),
    )
    .unwrap()
}

#[test]
fn authoring_scope_includes_unmarked_frames_and_hold_can_grow_into_them() {
    let mut project = first_key_project();
    let initial = prepare(&project, &[1, 2, 3], &keys(1));
    assert_eq!(initial.frames, 1);
    let authored = track(&initial).annotation_scope.clone().unwrap();
    assert_eq!(
        authored,
        [
            span(0, 100_000),
            span(100_000, 300_000),
            span(300_000, 600_000)
        ]
    );
    project
        .apply_command(&EditCommand::Compound {
            commands: initial.commands,
        })
        .unwrap();
    let expanded = prepare_annotations_in_scope(
        &project,
        &authored,
        &keys(500),
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap();
    assert_eq!(expanded.frames, 3);
    assert_eq!(track(&expanded).annotation_scope.as_ref(), Some(&authored));
}

#[test]
fn positive_partial_frame_gaps_break_carry_and_progress_keeps_full_frame_end_values() {
    let project = first_key_project();
    let scope = [span(25_000, 75_000), span(125_000, 175_000)];
    let prepared = prepare_annotations_in_scope(
        &project,
        &scope,
        &keys(500),
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap();
    assert_eq!(track(&prepared).items.len(), 1);
    assert_eq!(track(&prepared).items[0].span, scope[0]);
    let progress = AnnotationRequest {
        mode: AnnotationMode::Progress(ProgressOptions {
            format: "{elapsed}".to_owned(),
            ..ProgressOptions::default()
        }),
        ..AnnotationRequest::default()
    };
    let prepared = prepare_annotations_in_scope(
        &project,
        &scope,
        &progress,
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap();
    let labels: Vec<_> = track(&prepared)
        .items
        .iter()
        .map(|item| match &item.content {
            OverlayContent::Progress {
                style: Some(style), ..
            } => (item.span, style.label_text.as_str()),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        labels,
        [(scope[0], "00:00:00.100"), (scope[1], "00:00:00.300")]
    );
}

#[test]
fn scope_ripples_duration_changes_but_event_holds_keep_the_original_capture_clock() {
    let mut project = first_key_project();
    let initial = prepare(&project, &[1, 2, 3], &keys(1));
    let track_id = track(&initial).id;
    project
        .apply_command(&EditCommand::Compound {
            commands: initial.commands,
        })
        .unwrap();
    project
        .apply_command(&EditCommand::SetFrameDurations {
            changes: vec![FrameDurationChange {
                frame_id: FrameId::from_u128(1),
                duration: DurationUs::new(10_000_000).unwrap(),
            }],
        })
        .unwrap();
    let scope = project
        .timeline
        .overlay_tracks
        .iter()
        .find(|track| track.id == track_id)
        .unwrap()
        .annotation_scope
        .as_ref()
        .unwrap();
    assert_eq!(scope[1].start.get(), 10_000_000);
    let expanded = prepare_annotations_in_scope(
        &project,
        scope,
        &keys(500),
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap();
    assert_eq!(
        expanded.frames, 3,
        "capture clock is 0,100ms,200ms even though edited timeline spans >10s"
    );
}

#[test]
fn new_groups_skip_untrusted_frames_and_exclude_them_from_persistent_authoring_scope() {
    let mut project = first_key_project();
    let event = project.timeline.frames[0].capture_metadata.key_strokes[0].clone();
    for frame in &mut project.timeline.frames[1..] {
        frame.capture_metadata.key_strokes.push(event.clone());
    }
    project.timeline.frames[1].capture_binding = CaptureBinding::LegacyUnknown;
    project.timeline.frames[2].capture_binding = CaptureBinding::ArchivedAfterComposite;
    let original = project.clone();
    let prepared = prepare(&project, &[1, 2, 3], &keys(500));
    assert_eq!(prepared.frames, 1);
    assert_eq!(
        prepared.replay_skips,
        AnnotationReplaySkips {
            legacy_unknown: 1,
            archived_after_composite: 1,
            not_recorded: 0,
        }
    );
    assert_eq!(
        track(&prepared).annotation_scope,
        Some(vec![span(0, 100_000)])
    );
    assert_eq!(project, original);
    prepare_annotations_in_scope(
        &project,
        track(&prepared).annotation_scope.as_ref().unwrap(),
        &keys(1000),
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap();
}

#[test]
fn archived_frames_without_own_events_are_real_scope_gaps_and_carry_barriers() {
    let mut project = first_key_project();
    project.timeline.frames[1].capture_binding = CaptureBinding::ArchivedAfterComposite;
    let prepared = prepare(&project, &[1, 2, 3], &keys(500));
    assert_eq!(prepared.frames, 1);
    assert_eq!(
        track(&prepared).annotation_scope,
        Some(vec![span(0, 100_000), span(300_000, 600_000)])
    );
    assert!(
        prepared.replay_skips.is_empty(),
        "empty raw metadata does not masquerade as a recoverable input record"
    );
}

#[test]
fn imported_or_generated_frames_without_capture_metadata_interrupt_carried_keys() {
    let mut project = first_key_project();
    project.timeline.frames[1].capture_binding = CaptureBinding::NotRecorded;
    project.timeline.frames[1].capture_metadata =
        gif_from_screen_domain::CaptureMetadata::default();
    let prepared = prepare(&project, &[1, 2, 3], &keys(500));
    assert_eq!(prepared.frames, 1);
    assert_eq!(
        track(&prepared).annotation_scope,
        Some(vec![span(0, 100_000), span(300_000, 600_000)])
    );
    assert!(prepared.replay_skips.is_empty());
}

#[test]
fn existing_group_replay_is_atomically_rejected_if_any_authored_frame_becomes_untrusted() {
    let mut project = first_key_project();
    project.timeline.frames[1].capture_binding = CaptureBinding::LegacyUnknown;
    let error = prepare_annotations_in_scope(
        &project,
        &[span(0, 600_000)],
        &keys(500),
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap_err();
    assert!(error.contains("whole group is unchanged"));
    let manual = AnnotationRequest {
        mode: AnnotationMode::ManualKeys {
            text: "Safe manual label".to_owned(),
        },
        ..AnnotationRequest::default()
    };
    prepare_annotations_in_scope(
        &project,
        &[span(0, 600_000)],
        &manual,
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap();
}

#[test]
fn deleting_an_unselected_gap_makes_adjacent_authoring_time_contiguous_without_retiming_input() {
    let mut project = first_key_project();
    let initial = prepare(&project, &[1, 3], &keys(500));
    assert_eq!(initial.frames, 1);
    project
        .apply_command(&EditCommand::Compound {
            commands: initial.commands,
        })
        .unwrap();
    project
        .apply_command(&EditCommand::RemoveFrames {
            frame_ids: vec![FrameId::from_u128(2)],
        })
        .unwrap();
    assert_eq!(
        project.timeline.frames[1].capture_metadata.captured_at,
        Some(TimeUs::new(200_000))
    );
    let scope = project.timeline.overlay_tracks[0]
        .annotation_scope
        .as_ref()
        .unwrap();
    assert_eq!(scope, &[span(0, 100_000), span(100_000, 400_000)]);
    let expanded = prepare_annotations_in_scope(
        &project,
        scope,
        &keys(500),
        &AtomicBool::new(false),
        |_| {},
        &|_| unreachable!(),
    )
    .unwrap();
    assert_eq!(expanded.frames, 2);
}
