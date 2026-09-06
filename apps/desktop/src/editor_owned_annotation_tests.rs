use std::sync::atomic::AtomicBool;

use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, EditCommand, FrameId, KeyStroke, OverlayContent,
    OverlayTrack, Rgba, TimeUs, TrackId,
};
use gif_from_screen_project::LockPolicy;

use super::{
    EditorWorkspace,
    tests::{record_first_key, workspace},
};
use crate::editor_preview::render_frame_surface;

fn update(workspace: &mut EditorWorkspace, id: TrackId, request: &AnnotationRequest) {
    workspace
        .apply_annotation_group(
            &workspace.project_edit_anchor(),
            request,
            Some(id),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
}

fn create(workspace: &mut EditorWorkspace, request: &AnnotationRequest) -> TrackId {
    workspace
        .apply_annotation_edit(
            &workspace.project_edit_anchor(),
            request,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    workspace
        .manifest()
        .timeline
        .overlay_tracks
        .last()
        .unwrap()
        .id
}

fn track(workspace: &EditorWorkspace, id: TrackId) -> &OverlayTrack {
    workspace
        .manifest()
        .timeline
        .overlay_tracks
        .iter()
        .find(|track| track.id == id)
        .unwrap()
}

fn text(track: &OverlayTrack) -> Vec<String> {
    track
        .all_mark_contents()
        .filter_map(|(_, content)| {
            if let OverlayContent::KeyStroke { text, .. } = content {
                Some(text.clone())
            } else {
                None
            }
        })
        .collect()
}

fn append_future_key(workspace: &mut EditorWorkspace) {
    let mut replacement = workspace.manifest().timeline.frames[2].clone();
    replacement.capture_metadata.key_strokes.push(KeyStroke {
        physical_key: "KeyB".to_owned(),
        display_text: Some("B".to_owned()),
        pressed: true,
        at: TimeUs::new(200_000),
        repeat: false,
        modifiers: 0,
    });
    workspace
        .execute(EditCommand::ReplaceFrame {
            frame_id: replacement.id,
            replacement: Box::new(replacement),
        })
        .unwrap();
}

#[test]
fn copied_held_empty_owner_reedits_after_source_deletion_without_replaying_future_input() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("held-copy.gfsproj");
    let mut workspace = workspace(&root);
    record_first_key(&mut workspace);
    append_future_key(&mut workspace);
    let mut request = AnnotationRequest {
        mode: AnnotationMode::RecordedKeys,
        hold_ms: 500,
        ..AnnotationRequest::default()
    };
    let original_id = create(&mut workspace, &request);
    assert_eq!(text(track(&workspace, original_id)), ["A", "A", "A  B"]);
    let initial = render_frame_surface(
        workspace.active_project(),
        FrameId::from_u128(2),
        1024 * 1024,
    )
    .unwrap();
    assert!(
        workspace.manifest().timeline.frames[1]
            .capture_metadata
            .key_strokes
            .is_empty()
    );
    workspace.select_only(FrameId::from_u128(2)).unwrap();
    workspace.copy_selection().unwrap();
    workspace.select_all();
    workspace.delete_selection().unwrap();
    workspace.paste_after_current().unwrap();
    let owner = workspace.manifest().timeline.frames[0].id;
    let copied = workspace
        .manifest()
        .timeline
        .overlay_tracks
        .iter()
        .find(|track| {
            track.id != original_id
                && track
                    .frame_cells
                    .as_ref()
                    .is_some_and(|cells| cells.iter().any(|cell| cell.frame_id == owner))
        })
        .unwrap()
        .id;
    assert_eq!(
        render_frame_surface(workspace.active_project(), owner, 1024 * 1024).unwrap(),
        initial
    );
    request.hold_ms = 1;
    update(&mut workspace, copied, &request);
    assert!(text(track(&workspace, copied)).is_empty());
    let seed = track(&workspace, copied).frame_cells.as_ref().unwrap()[0]
        .input_replay
        .clone();
    request.hold_ms = 500;
    request.foreground = Rgba {
        red: 255,
        green: 30,
        blue: 60,
        alpha: 255,
    };
    update(&mut workspace, copied, &request);
    assert_eq!(text(track(&workspace, copied)), ["A"]);
    assert_eq!(
        track(&workspace, copied).frame_cells.as_ref().unwrap()[0].input_replay,
        seed
    );
    let changed = render_frame_surface(workspace.active_project(), owner, 1024 * 1024).unwrap();
    assert_ne!(initial, changed);
    workspace.undo().unwrap();
    assert!(text(track(&workspace, copied)).is_empty());
    workspace.redo().unwrap();
    assert_eq!(text(track(&workspace, copied)), ["A"]);
    workspace.checkpoint().unwrap();
    drop(workspace);
    let mut reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 16).unwrap();
    assert_eq!(
        render_frame_surface(reopened.active_project(), owner, 1024 * 1024).unwrap(),
        changed
    );
    update(&mut reopened, copied, &request);
    assert_eq!(text(track(&reopened, copied)), ["A"]);
    assert_eq!(
        render_frame_surface(reopened.active_project(), owner, 1024 * 1024).unwrap(),
        changed
    );
}

#[test]
fn owned_input_replay_uses_source_order_after_reverse_and_new_hold_keeps_run_gaps() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("reverse.gfsproj"));
    record_first_key(&mut workspace);
    let mut request = AnnotationRequest {
        mode: AnnotationMode::RecordedKeys,
        hold_ms: 1,
        ..AnnotationRequest::default()
    };
    let id = create(&mut workspace, &request);
    let before = track(&workspace, id).frame_cells.clone();
    workspace
        .execute(EditCommand::ReorderFrames {
            order: vec![
                FrameId::from_u128(3),
                FrameId::from_u128(2),
                FrameId::from_u128(1),
            ],
        })
        .unwrap();
    request.hold_ms = 500;
    update(&mut workspace, id, &request);
    assert_eq!(text(track(&workspace, id)), ["A", "A", "A"]);
    for (old, new) in before
        .as_ref()
        .unwrap()
        .iter()
        .zip(track(&workspace, id).frame_cells.as_ref().unwrap())
    {
        assert_eq!(old.scopes, new.scopes);
        assert_eq!(old.input_replay, new.input_replay);
    }
    workspace.select_only(FrameId::from_u128(1)).unwrap();
    workspace.toggle_selection(FrameId::from_u128(3)).unwrap();
    let gap_id = create(&mut workspace, &request);
    update(&mut workspace, gap_id, &request);
    assert_eq!(text(track(&workspace, gap_id)), ["A"]);
}

#[test]
fn missing_or_corrupt_pool_and_archived_owner_reject_whole_group_without_journal_write() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("guard.gfsproj");
    let mut workspace = workspace(&root);
    record_first_key(&mut workspace);
    let request = AnnotationRequest {
        mode: AnnotationMode::RecordedKeys,
        ..AnnotationRequest::default()
    };
    let id = create(&mut workspace, &request);
    let pool = track(&workspace, id)
        .referenced_assets()
        .find(|id| {
            matches!(
                &workspace.manifest().assets[id].kind,
                gif_from_screen_domain::AssetKind::ImportedSource { .. }
            )
        })
        .unwrap();
    let path = workspace.active_project().assets().asset_path(pool);
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, vec![0; original.len()]).unwrap();
    let before = workspace.manifest().clone();
    let journal = std::fs::read(root.join("journal.ndjson")).unwrap();
    assert!(
        workspace
            .apply_annotation_group(
                &workspace.project_edit_anchor(),
                &request,
                Some(id),
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap_err()
            .contains("digest")
    );
    assert_eq!(workspace.manifest(), &before);
    assert_eq!(std::fs::read(root.join("journal.ndjson")).unwrap(), journal);
    std::fs::write(path, original).unwrap();
    let mut replacement = workspace.manifest().timeline.frames[1].clone();
    replacement.capture_binding = gif_from_screen_domain::CaptureBinding::ArchivedAfterComposite;
    workspace
        .execute(EditCommand::ReplaceFrame {
            frame_id: replacement.id,
            replacement: Box::new(replacement),
        })
        .unwrap();
    let before = workspace.manifest().clone();
    assert!(
        workspace
            .apply_annotation_group(
                &workspace.project_edit_anchor(),
                &request,
                Some(id),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
}

#[test]
fn copied_click_history_keeps_its_delivery_origin_after_the_source_frame_is_deleted() {
    use gif_from_screen_domain::{
        CaptureOrigin, MouseButton, MouseInputEvent, PhysicalPoint, PhysicalPx,
    };
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("click-copy.gfsproj"));
    record_first_key(&mut workspace);
    for index in 0..3 {
        let mut frame = workspace.manifest().timeline.frames[index].clone();
        frame.capture_metadata.key_strokes.clear();
        frame.capture_metadata.capture_origin = Some(CaptureOrigin {
            x: 100 + i32::try_from(index).unwrap() * 10,
            y: 50,
        });
        if index == 0 {
            frame.capture_metadata.mouse_events.push(MouseInputEvent {
                at: TimeUs::ZERO,
                button: MouseButton::Left,
                pressed: true,
                position: Some(PhysicalPoint {
                    x: PhysicalPx::new(20),
                    y: PhysicalPx::new(10),
                }),
            });
        }
        workspace
            .execute(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame),
            })
            .unwrap();
    }
    let mut request = AnnotationRequest {
        mode: AnnotationMode::RecordedClicks,
        hold_ms: 500,
        click_radius: 2,
        ..AnnotationRequest::default()
    };
    create(&mut workspace, &request);
    let initial = render_frame_surface(
        workspace.active_project(),
        FrameId::from_u128(2),
        1024 * 1024,
    )
    .unwrap();
    workspace.select_only(FrameId::from_u128(2)).unwrap();
    workspace.copy_selection().unwrap();
    workspace.select_all();
    workspace.delete_selection().unwrap();
    workspace.paste_after_current().unwrap();
    let copied = workspace
        .manifest()
        .timeline
        .overlay_tracks
        .iter()
        .find(|track| {
            track
                .frame_cells
                .as_ref()
                .is_some_and(|cells| !cells.is_empty())
        })
        .unwrap()
        .id;
    let owner = workspace.manifest().timeline.frames[0].id;
    update(&mut workspace, copied, &request);
    assert_eq!(
        render_frame_surface(workspace.active_project(), owner, 1024 * 1024).unwrap(),
        initial
    );
    request.click_radius = 3;
    update(&mut workspace, copied, &request);
    assert!(
        matches!(track(&workspace, copied).all_mark_contents().next().unwrap().1,
        OverlayContent::MouseClick { position, radius: 3, .. } if position.x.get() == 10 && position.y.get() == 10)
    );
}

#[test]
fn independent_and_unknown_source_clocks_never_gain_held_labels_during_owned_reedit() {
    use gif_from_screen_domain::CaptureClockId;
    let directory = tempfile::tempdir().unwrap();
    for (name, identity) in [
        ("different", Some(CaptureClockId::from_u128(99))),
        ("unknown", None),
    ] {
        let mut workspace = workspace(&directory.path().join(name));
        record_first_key(&mut workspace);
        for index in 1..3 {
            let mut frame = workspace.manifest().timeline.frames[index].clone();
            frame.capture_clock.as_mut().unwrap().id = identity;
            workspace
                .execute(EditCommand::ReplaceFrame {
                    frame_id: frame.id,
                    replacement: Box::new(frame),
                })
                .unwrap();
        }
        let request = AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            hold_ms: 500,
            ..AnnotationRequest::default()
        };
        let id = create(&mut workspace, &request);
        assert_eq!(text(track(&workspace, id)), ["A"]);
        update(&mut workspace, id, &request);
        assert_eq!(text(track(&workspace, id)), ["A"]);
        let cells = track(&workspace, id).frame_cells.as_ref().unwrap();
        assert_eq!(cells.len(), 3);
        assert!(cells[1..].iter().all(|cell| cell.marks.is_empty()));
    }
}

fn repeat_first_sample_scope(workspace: &mut EditorWorkspace, id: TrackId) {
    use gif_from_screen_domain::{DurationUs, FrameAuthoringSpan, FrameLocalSpan};
    let mut replacement = track(workspace, id).clone();
    let cell = &mut replacement.frame_cells.as_mut().unwrap()[0];
    cell.scopes = vec![
        FrameAuthoringSpan {
            run_id: 1,
            span: FrameLocalSpan::new(0, 1, DurationUs::new(2).unwrap()).unwrap(),
        },
        FrameAuthoringSpan {
            run_id: 2,
            span: FrameLocalSpan::new(1, 2, DurationUs::new(2).unwrap()).unwrap(),
        },
    ];
    let replay = cell.input_replay.as_mut().unwrap();
    let mut repeated = replay.runs[0];
    repeated.run_id = 2;
    replay.runs.push(repeated);
    workspace
        .execute(EditCommand::UpsertOverlayTrack { track: replacement })
        .unwrap();
}

#[test]
fn multiple_scope_runs_share_one_key_contribution_without_double_painting() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("same-source-keys.gfsproj"));
    record_first_key(&mut workspace);
    let request = AnnotationRequest {
        mode: AnnotationMode::RecordedKeys,
        ..AnnotationRequest::default()
    };
    let id = create(&mut workspace, &request);
    let before = render_frame_surface(
        workspace.active_project(),
        FrameId::from_u128(1),
        1024 * 1024,
    )
    .unwrap();
    repeat_first_sample_scope(&mut workspace, id);
    update(&mut workspace, id, &request);
    assert_eq!(text(track(&workspace, id)), ["A", "A", "A"]);
    assert_eq!(
        render_frame_surface(
            workspace.active_project(),
            FrameId::from_u128(1),
            1024 * 1024
        )
        .unwrap(),
        before
    );
}

#[test]
fn multiple_scope_runs_do_not_repeat_real_clicks_or_merge_distinct_same_position_clicks() {
    use gif_from_screen_domain::{MouseButton, MouseInputEvent, PhysicalPoint, PhysicalPx};
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("same-source-clicks.gfsproj"));
    record_first_key(&mut workspace);
    let mut frame = workspace.manifest().timeline.frames[0].clone();
    frame.capture_metadata.mouse_events = vec![
        MouseInputEvent {
            at: TimeUs::ZERO,
            button: MouseButton::Left,
            pressed: true,
            position: Some(PhysicalPoint {
                x: PhysicalPx::new(10),
                y: PhysicalPx::new(10)
            }),
        };
        2
    ];
    workspace
        .execute(EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame),
        })
        .unwrap();
    let request = AnnotationRequest {
        mode: AnnotationMode::RecordedClicks,
        foreground: Rgba {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 128,
        },
        ..AnnotationRequest::default()
    };
    let id = create(&mut workspace, &request);
    let before = render_frame_surface(
        workspace.active_project(),
        FrameId::from_u128(1),
        1024 * 1024,
    )
    .unwrap();
    assert_eq!(
        track(&workspace, id).frame_cells.as_ref().unwrap()[0]
            .marks
            .len(),
        2
    );
    repeat_first_sample_scope(&mut workspace, id);
    update(&mut workspace, id, &request);
    assert_eq!(
        track(&workspace, id).frame_cells.as_ref().unwrap()[0]
            .marks
            .len(),
        2
    );
    assert_eq!(
        render_frame_surface(
            workspace.active_project(),
            FrameId::from_u128(1),
            1024 * 1024
        )
        .unwrap(),
        before
    );
}

#[test]
fn authoring_freezes_only_missing_legacy_sample_context_before_retime_and_copy() {
    use gif_from_screen_domain::{DurationUs, FrameDurationChange};
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("legacy-sample.gfsproj"));
    record_first_key(&mut workspace);
    for index in 0..3 {
        let mut frame = workspace.manifest().timeline.frames[index].clone();
        frame.capture_clock = None;
        frame.capture_metadata.captured_at = None;
        frame.capture_metadata.key_strokes.clear();
        if index == 1 {
            frame.capture_metadata.key_strokes.push(KeyStroke {
                physical_key: "A".to_owned(),
                display_text: Some("A".to_owned()),
                pressed: true,
                at: TimeUs::new(100_000),
                repeat: false,
                modifiers: 0,
            });
        }
        workspace
            .execute(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame),
            })
            .unwrap();
    }
    let before = workspace.manifest().clone();
    let request = AnnotationRequest {
        mode: AnnotationMode::RecordedKeys,
        ..AnnotationRequest::default()
    };
    create(&mut workspace, &request);
    for (index, frame) in workspace.manifest().timeline.frames.iter().enumerate() {
        assert_eq!(frame.capture_clock.unwrap().id, None);
        assert_eq!(
            frame.capture_clock.unwrap().sampled_at,
            TimeUs::new(index as u64 * 100_000)
        );
        assert_eq!(
            frame.capture_metadata,
            before.timeline.frames[index].capture_metadata
        );
    }
    workspace.undo().unwrap();
    assert_eq!(workspace.manifest().timeline, before.timeline);
    workspace.redo().unwrap();
    workspace
        .execute(EditCommand::ReorderFrames {
            order: vec![
                FrameId::from_u128(2),
                FrameId::from_u128(1),
                FrameId::from_u128(3),
            ],
        })
        .unwrap();
    workspace
        .execute(EditCommand::SetFrameDurations {
            changes: vec![FrameDurationChange {
                frame_id: FrameId::from_u128(2),
                duration: DurationUs::new(300_000).unwrap(),
            }],
        })
        .unwrap();
    workspace.select_only(FrameId::from_u128(2)).unwrap();
    workspace.copy_selection().unwrap();
    workspace.select_all();
    workspace.delete_selection().unwrap();
    workspace.paste_after_current().unwrap();
    let copied = workspace
        .manifest()
        .timeline
        .overlay_tracks
        .iter()
        .find(|track| {
            track
                .frame_cells
                .as_ref()
                .is_some_and(|cells| !cells.is_empty())
        })
        .unwrap()
        .id;
    assert_eq!(
        workspace.manifest().timeline.frames[0]
            .capture_clock
            .unwrap()
            .sampled_at,
        TimeUs::new(100_000)
    );
    update(&mut workspace, copied, &request);
    assert_eq!(text(track(&workspace, copied)), ["A"]);
}
