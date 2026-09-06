use super::*;
use gif_from_screen_application::{
    IncrementalRecordingProject, IncrementalRecordingProjectOptions,
};
use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, BlendMode, CaptureBinding, CaptureMetadata,
    FrameAuthoringSpan, FrameLocalSpan, FrameOverlayCell, FrameOverlayMark, KeyStroke, MouseButton,
    MouseInputEvent, OverlayContent, OverlayTrack, PhysicalPoint, PhysicalPx, ProjectId,
    ProjectManifest, Rgba, ShapeKind, TrackId, UnixTimeMs,
};
use gif_from_screen_gif::RgbaFrame;
use gif_from_screen_project::LockPolicy;

fn workspace(root: &std::path::Path) -> EditorWorkspace {
    let mut writer = IncrementalRecordingProject::create(
        root,
        PhysicalSize::new(2, 1).unwrap(),
        IncrementalRecordingProjectOptions {
            project_id: ProjectId::from_u128(100),
            app_version: "motion-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            source_label: None,
        },
    )
    .unwrap();
    for (index, pixels) in [
        vec![255, 0, 0, 0, 0, 255, 0, 255],
        vec![0, 0, 255, 255, 255, 255, 0, 255],
        vec![255, 255, 255, 255, 0, 0, 255, 255],
    ]
    .into_iter()
    .enumerate()
    {
        writer
            .append_frame(
                FrameId::from_u128(index as u128 + 1),
                &RgbaFrame::new(2, 1, pixels, 10_000).unwrap(),
            )
            .unwrap();
    }
    let mut workspace = EditorWorkspace::from_active(writer.finish().unwrap(), 32).unwrap();
    workspace.select_first().unwrap();
    workspace
}

fn add_raw_input(workspace: &mut EditorWorkspace) {
    let rgba = [200, 200, 200, 255];
    let id = workspace.active_project().assets().put(&rgba).unwrap();
    let mut replacement = workspace.manifest().timeline.frames[0].clone();
    replacement.capture_binding = CaptureBinding::Original;
    replacement.transform = ClipTransform {
        crop: Some(PhysicalRect::new(1, 0, 1, 1).unwrap()),
        output_size: Some(PhysicalSize::new(2, 1).unwrap()),
        ..ClipTransform::default()
    };
    replacement.capture_metadata = CaptureMetadata {
        captured_at: Some(TimeUs::ZERO),
        cursor_position: Some(PhysicalPoint {
            x: PhysicalPx::new(1),
            y: PhysicalPx::ZERO,
        }),
        cursor_asset: Some(id),
        cursor_visible: true,
        cursor_embedded: false,
        key_strokes: vec![KeyStroke {
            physical_key: "C".to_owned(),
            display_text: Some("Ctrl+C".to_owned()),
            pressed: true,
            at: TimeUs::ZERO,
            repeat: false,
            modifiers: 2,
        }],
        mouse_events: vec![MouseInputEvent {
            at: TimeUs::ZERO,
            button: MouseButton::Left,
            pressed: true,
            position: Some(PhysicalPoint {
                x: PhysicalPx::new(1),
                y: PhysicalPx::ZERO,
            }),
        }],
        ..CaptureMetadata::default()
    };
    workspace
        .execute(EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset {
                    asset: AssetDescriptor {
                        id,
                        byte_len: 4,
                        kind: AssetKind::OverlayImage {
                            size: PhysicalSize::new(1, 1).unwrap(),
                            encoding: RasterEncoding::Rgba8,
                        },
                    },
                },
                EditCommand::ReplaceFrame {
                    frame_id: replacement.id,
                    replacement: Box::new(replacement),
                },
            ],
        })
        .unwrap();
}

#[test]
fn transformed_cinemagraph_archives_input_without_duplicate_replay_and_undo_restores_it() {
    use crate::annotation_engine::{load_annotation_asset, prepare_annotations_with_assets};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("bound.gfsproj");
    let mut workspace = workspace(&root);
    add_raw_input(&mut workspace);
    let request = AnnotationRequest {
        mode: AnnotationMode::RecordedCursor,
        ..AnnotationRequest::default()
    };
    workspace
        .apply_annotation_edit(
            &workspace.project_edit_anchor(),
            &request,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    let before = workspace.manifest().clone();
    let raw = serde_json::to_vec(&before.timeline.frames[0].capture_metadata).unwrap();
    let visible = pixels(&workspace, 1);
    workspace
        .apply_motion_edit(
            &workspace.project_edit_anchor(),
            MotionOperation::Cinemagraph {
                region: rect(),
                invert: false,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    let frame = &workspace.manifest().timeline.frames[0];
    assert_eq!(
        frame.capture_binding,
        CaptureBinding::ArchivedAfterComposite
    );
    assert_eq!(serde_json::to_vec(&frame.capture_metadata).unwrap(), raw);
    assert!(!frame.capture_metadata.cursor_embedded);
    assert_eq!(pixels(&workspace, 1), visible);
    assert_archived_replay_is_blocked(&workspace);
    let manual = prepare_annotations_with_assets(
        workspace.manifest(),
        workspace.selection().selected(),
        &AnnotationRequest {
            mode: AnnotationMode::BuiltinCursor,
            ..AnnotationRequest::default()
        },
        &AtomicBool::new(false),
        |_| {},
        &|_| panic!("manual pointer uses no source assets"),
    )
    .unwrap();
    assert!(!manual.commands.is_empty());
    workspace.undo().unwrap();
    equal_except_revision(workspace.manifest(), &before);
    let restored = prepare_annotations_with_assets(
        workspace.manifest(),
        workspace.selection().selected(),
        &request,
        &AtomicBool::new(false),
        |_| {},
        &|id| {
            load_annotation_asset(
                workspace.manifest(),
                workspace.active_project().assets(),
                id,
            )
        },
    )
    .unwrap();
    assert!(!restored.commands.is_empty());
    assert!(restored.replay_skips.is_empty());
    workspace.redo().unwrap();
    workspace.checkpoint().unwrap();
    drop(workspace);
    let reopened = EditorWorkspace::open(root, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(
        reopened.manifest().timeline.frames[0].capture_binding,
        CaptureBinding::ArchivedAfterComposite
    );
    assert_eq!(
        serde_json::to_vec(&reopened.manifest().timeline.frames[0].capture_metadata).unwrap(),
        raw
    );
    assert_eq!(pixels(&reopened, 1), visible);
}

fn assert_archived_replay_is_blocked(workspace: &EditorWorkspace) {
    use crate::annotation_engine::prepare_annotations_with_assets;
    for mode in [
        AnnotationMode::RecordedCursor,
        AnnotationMode::RecordedClicks,
        AnnotationMode::RecordedKeys,
    ] {
        let prepared = prepare_annotations_with_assets(
            workspace.manifest(),
            workspace.selection().selected(),
            &AnnotationRequest {
                mode,
                ..AnnotationRequest::default()
            },
            &AtomicBool::new(false),
            |_| {},
            &|_| panic!("archived coordinates must not be replayed"),
        )
        .unwrap();
        assert!(prepared.commands.is_empty());
        assert_eq!(prepared.replay_skips.archived_after_composite, 1);
    }
}

#[test]
fn baked_binding_and_raw_input_survive_clipboard_and_project_insertion() {
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("source.gfsproj");
    let mut source = workspace(&source_root);
    add_raw_input(&mut source);
    source
        .apply_motion_edit(
            &source.project_edit_anchor(),
            MotionOperation::Cinemagraph {
                region: rect(),
                invert: false,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    let original = source.manifest().timeline.frames[0].clone();
    source.copy_selection().unwrap();
    source.paste_after_current().unwrap();
    let copy = &source.manifest().timeline.frames[1];
    assert_ne!(copy.id, original.id);
    assert_eq!(copy.capture_metadata, original.capture_metadata);
    assert_eq!(copy.capture_binding, original.capture_binding);
    let expected: Vec<_> = source
        .manifest()
        .timeline
        .frames
        .iter()
        .map(|frame| (frame.capture_binding, frame.capture_metadata.clone()))
        .collect();
    let target_manifest = ProjectManifest::new(
        ProjectId::from_u128(999),
        "binding-test",
        UnixTimeMs::new(0),
        source.manifest().canvas.clone(),
    )
    .unwrap();
    source.checkpoint().unwrap();
    drop(source);
    let mut target = EditorWorkspace::from_active(
        gif_from_screen_project::ActiveProject::create(
            dir.path().join("target.gfsproj"),
            target_manifest,
        )
        .unwrap(),
        32,
    )
    .unwrap();
    let prepared = crate::editor_workspace::prepare_project_insertion_from_path(
        target.project_insertion_target(None).unwrap(),
        &source_root,
        &gif_from_screen_gif::NeverCancel,
    )
    .unwrap();
    target.insert_prepared_project(prepared).unwrap();
    assert_eq!(
        target
            .manifest()
            .timeline
            .frames
            .iter()
            .map(|frame| (frame.capture_binding, frame.capture_metadata.clone()))
            .collect::<Vec<_>>(),
        expected
    );
    target.undo().unwrap();
    assert!(target.manifest().timeline.frames.is_empty());
}

#[test]
fn cinemagraph_preserves_hidden_zero_opacity_and_remaining_authoring_scope() {
    let dir = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&dir.path().join("hidden.gfsproj"));
    add_overlay(&mut workspace);
    let mut hidden = workspace.manifest().timeline.overlay_tracks[0].clone();
    hidden.visible = false;
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: hidden.clone(),
        })
        .unwrap();
    let mut zero = hidden.clone();
    zero.id = gif_from_screen_domain::TrackId::from_u128(901);
    zero.visible = true;
    zero.opacity = 0;
    for item in &mut zero.items {
        item.id = OverlayId::from_u128(uuid::Uuid::new_v4().as_u128());
    }
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: zero.clone(),
        })
        .unwrap();
    let mut authored = hidden.clone();
    authored.id = gif_from_screen_domain::TrackId::from_u128(902);
    authored.visible = true;
    authored.items.clear();
    authored.annotation = Some(AnnotationRequest::default());
    authored.annotation_scope = Some(vec![
        TimelineSpan {
            start: TimeUs::ZERO,
            duration: DurationUs::new(10_000).unwrap(),
        },
        TimelineSpan {
            start: TimeUs::new(20_000),
            duration: DurationUs::new(10_000).unwrap(),
        },
    ]);
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: authored.clone(),
        })
        .unwrap();
    let mut zero_item = hidden.clone();
    zero_item.id = gif_from_screen_domain::TrackId::from_u128(903);
    zero_item.visible = true;
    for item in &mut zero_item.items {
        item.id = OverlayId::from_u128(uuid::Uuid::new_v4().as_u128());
        item.content = OverlayContent::Raster {
            asset_id: workspace.manifest().timeline.frames[0].asset_id,
            position: PhysicalPoint::default(),
            size: PhysicalSize::new(2, 1).unwrap(),
            opacity: 0,
        };
    }
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: zero_item.clone(),
        })
        .unwrap();
    workspace.select_only(FrameId::from_u128(1)).unwrap();
    let untouched_binding = workspace.manifest().timeline.frames[1].capture_binding;
    workspace
        .apply_motion_edit(
            &workspace.project_edit_anchor(),
            MotionOperation::Cinemagraph {
                region: rect(),
                invert: false,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    let tracks = &workspace.manifest().timeline.overlay_tracks;
    assert_eq!(
        tracks.iter().find(|track| track.id == hidden.id).unwrap(),
        &hidden
    );
    assert_eq!(
        tracks.iter().find(|track| track.id == zero.id).unwrap(),
        &zero
    );
    assert_eq!(
        tracks
            .iter()
            .find(|track| track.id == zero_item.id)
            .unwrap(),
        &zero_item
    );
    let kept = tracks.iter().find(|track| track.id == authored.id).unwrap();
    assert!(kept.items.is_empty());
    assert_eq!(
        kept.annotation_scope.as_ref().unwrap(),
        &authored.annotation_scope.unwrap()[1..]
    );
    assert_eq!(
        workspace.manifest().timeline.frames[1].capture_binding,
        untouched_binding
    );
}

fn rect() -> PhysicalRect {
    PhysicalRect {
        origin: PhysicalPoint {
            x: PhysicalPx::new(1),
            y: PhysicalPx::ZERO,
        },
        size: PhysicalSize::new(1, 1).unwrap(),
    }
}

fn pixels(workspace: &EditorWorkspace, id: u128) -> Vec<u8> {
    render(
        workspace.active_project(),
        FrameId::from_u128(id),
        workspace.manifest().canvas.size,
        &AtomicBool::new(false),
    )
    .unwrap()
    .into_pixels()
}

fn equal_except_revision(actual: &ProjectManifest, expected: &ProjectManifest) {
    let mut expected = expected.clone();
    expected.revision = actual.revision;
    assert_eq!(*actual, expected);
}

fn loop_workspace(root: &std::path::Path, levels: &[u8]) -> EditorWorkspace {
    let mut writer = IncrementalRecordingProject::create(
        root,
        PhysicalSize::new(4, 1).unwrap(),
        IncrementalRecordingProjectOptions {
            project_id: ProjectId::from_u128(908),
            app_version: "loop-search-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            source_label: None,
        },
    )
    .unwrap();
    for (index, level) in levels.iter().enumerate() {
        let mut rgba = [20, 30, 40, 255].repeat(4);
        rgba[0] = *level;
        writer
            .append_frame(
                FrameId::from_u128(index as u128 + 1),
                &RgbaFrame::new(4, 1, rgba, (index as u64 + 1) * 10_000).unwrap(),
            )
            .unwrap();
    }
    let mut workspace = EditorWorkspace::from_active(writer.finish().unwrap(), 32).unwrap();
    workspace.select_first().unwrap();
    workspace
}

#[test]
fn loop_search_obeys_direction_skip_and_exact_pixel_percentage() {
    let directory = tempfile::tempdir().unwrap();
    for (name, from_end, expected_removed, expected_len) in [
        ("forward.gfsproj", false, 3, 3),
        ("reverse.gfsproj", true, 1, 5),
    ] {
        let mut workspace = loop_workspace(&directory.path().join(name), &[20, 21, 20, 22, 20, 23]);
        let before = workspace.manifest().clone();
        let result = workspace
            .apply_motion_edit(
                &workspace.project_edit_anchor(),
                MotionOperation::FindSmoothLoop {
                    skip_first: 1,
                    similarity_tenths: 1000,
                    from_end,
                },
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
        assert_eq!(result, MotionOutcome::TrimmedTail(expected_removed));
        assert_eq!(workspace.manifest().timeline.frames.len(), expected_len);
        assert_eq!(workspace.manifest().assets, before.assets);
        workspace.undo().unwrap();
        equal_except_revision(workspace.manifest(), &before);
        workspace.redo().unwrap();
        assert_eq!(workspace.manifest().timeline.frames.len(), expected_len);
    }
    let mut workspace = loop_workspace(&directory.path().join("threshold.gfsproj"), &[20, 21, 22]);
    let anchor = workspace.project_edit_anchor();
    // Only one channel of one pixel differs. Mean-color rounding is nearly 100%, but
    // the reference behavior is exactly 75% equal pixels, inclusive at that boundary.
    assert_eq!(
        workspace
            .apply_motion_edit(
                &anchor,
                MotionOperation::FindSmoothLoop {
                    skip_first: 1,
                    similarity_tenths: 751,
                    from_end: false
                },
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap(),
        MotionOutcome::NoMatchingEnd
    );
    assert_eq!(
        workspace
            .apply_motion_edit(
                &anchor,
                MotionOperation::FindSmoothLoop {
                    skip_first: 1,
                    similarity_tenths: 750,
                    from_end: false
                },
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap(),
        MotionOutcome::TrimmedTail(1)
    );
}

#[test]
fn loop_search_noop_invalid_and_cancelled_runs_do_not_write_a_revision() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = loop_workspace(&directory.path().join("noop.gfsproj"), &[20, 21, 20]);
    let before = workspace.manifest().clone();
    let operation = MotionOperation::FindSmoothLoop {
        skip_first: 1,
        similarity_tenths: 1000,
        from_end: true,
    };
    assert_eq!(
        workspace
            .apply_motion_edit(
                &workspace.project_edit_anchor(),
                operation,
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap(),
        MotionOutcome::AlreadySmooth
    );
    assert_eq!(workspace.manifest(), &before);
    let cancelled = AtomicBool::new(false);
    assert!(
        workspace
            .apply_motion_edit(
                &workspace.project_edit_anchor(),
                operation,
                &cancelled,
                |_| cancelled.store(true, Ordering::Release)
            )
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
    for (skip_first, similarity_tenths) in [(0, 1000), (3, 1000), (1, 0), (1, 1001)] {
        assert!(
            workspace
                .apply_motion_edit(
                    &workspace.project_edit_anchor(),
                    MotionOperation::FindSmoothLoop {
                        skip_first,
                        similarity_tenths,
                        from_end: true
                    },
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
        );
        assert_eq!(workspace.manifest(), &before);
    }
}

fn add_overlay(workspace: &mut EditorWorkspace) {
    workspace.select_all();
    workspace
        .add_overlay_for_selection(
            "Before baking".to_owned(),
            OverlayContent::Shape {
                kind: ShapeKind::Rectangle,
                bounds: rect(),
                stroke_width: 0,
                stroke: Rgba::TRANSPARENT,
                fill: Some(Rgba {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 128,
                }),
            },
            4,
            255,
            BlendMode::Normal,
        )
        .unwrap();
}

fn add_owned_overlay(workspace: &mut EditorWorkspace) -> OverlayTrack {
    add_overlay(workspace);
    let mut track = workspace.manifest().timeline.overlay_tracks[0].clone();
    let content = track.items[0].content.clone();
    track.items.clear();
    track.annotation = Some(AnnotationRequest::default());
    track.annotation_scope = None;
    track.opacity = 177;
    track.blend_mode = BlendMode::Multiply;
    track.frame_cells = Some(
        workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| FrameOverlayCell {
                frame_id: frame.id,
                scopes: vec![FrameAuthoringSpan {
                    run_id: 1,
                    span: FrameLocalSpan::new(1, 2, DurationUs::new(3).unwrap()).unwrap(),
                }],
                marks: vec![FrameOverlayMark {
                    id: OverlayId::from_u128(10_000 + index as u128),
                    z_index: 4,
                    content: content.clone(),
                }],
            })
            .collect(),
    );
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: track.clone(),
        })
        .unwrap();
    track
}

fn bake_whole_selected(workspace: &mut EditorWorkspace) {
    workspace
        .apply_motion_edit(
            &workspace.project_edit_anchor(),
            MotionOperation::Cinemagraph {
                region: PhysicalRect::new(0, 0, 2, 1).unwrap(),
                invert: false,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
}

#[test]
fn frame_owned_cinemagraph_bakes_once_preserving_unselected_owners_and_raw_input() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("owned-cinemagraph.gfsproj");
    let mut workspace = workspace(&root);
    add_raw_input(&mut workspace);
    let original_track = add_owned_overlay(&mut workspace);
    workspace.select_only(FrameId::from_u128(1)).unwrap();
    workspace.toggle_selection(FrameId::from_u128(3)).unwrap();
    let before = workspace.manifest().clone();
    let visible = (1..=3).map(|id| pixels(&workspace, id)).collect::<Vec<_>>();
    let raw = serde_json::to_vec(
        &before
            .timeline
            .frames
            .iter()
            .map(|frame| &frame.capture_metadata)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    bake_whole_selected(&mut workspace);
    assert_eq!(
        (1..=3).map(|id| pixels(&workspace, id)).collect::<Vec<_>>(),
        visible
    );
    let kept = &workspace.manifest().timeline.overlay_tracks[0];
    assert_eq!(
        kept.frame_cells.as_ref().unwrap(),
        &original_track.frame_cells.as_ref().unwrap()[1..2]
    );
    assert_eq!(
        serde_json::to_vec(
            &workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| &frame.capture_metadata)
                .collect::<Vec<_>>()
        )
        .unwrap(),
        raw
    );
    assert_eq!(
        workspace.manifest().timeline.frames[1],
        before.timeline.frames[1]
    );
    let baked = workspace.manifest().clone();
    workspace.undo().unwrap();
    equal_except_revision(workspace.manifest(), &before);
    assert_eq!(
        (1..=3).map(|id| pixels(&workspace, id)).collect::<Vec<_>>(),
        visible
    );
    workspace.redo().unwrap();
    equal_except_revision(workspace.manifest(), &baked);
    workspace.checkpoint().unwrap();
    drop(workspace);
    let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(
        (1..=3).map(|id| pixels(&reopened, id)).collect::<Vec<_>>(),
        visible
    );
    assert_eq!(
        reopened.manifest().timeline.overlay_tracks[0],
        *kept_from(&baked, original_track.id)
    );
}

fn kept_from(manifest: &ProjectManifest, id: TrackId) -> &OverlayTrack {
    manifest
        .timeline
        .overlay_tracks
        .iter()
        .find(|track| track.id == id)
        .unwrap()
}

fn renamed_track(original: &OverlayTrack, id: u128) -> OverlayTrack {
    let mut track = original.clone();
    track.id = TrackId::from_u128(id);
    for (index, mark) in track
        .frame_cells
        .as_mut()
        .unwrap()
        .iter_mut()
        .flat_map(|cell| &mut cell.marks)
        .enumerate()
    {
        mark.id = OverlayId::from_u128(id * 100 + index as u128);
    }
    track
}

#[test]
fn frame_owned_bake_keeps_hidden_zero_tracks_and_zero_marks_with_their_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&dir.path().join("invisible-owned.gfsproj"));
    let original = add_owned_overlay(&mut workspace);
    let mut hidden = renamed_track(&original, 91);
    hidden.visible = false;
    let mut zero = renamed_track(&original, 92);
    zero.opacity = 0;
    let mut zero_marks = renamed_track(&original, 93);
    for cell in zero_marks.frame_cells.as_mut().unwrap() {
        cell.marks[0].content = OverlayContent::Raster {
            asset_id: workspace.manifest().timeline.frames[0].asset_id,
            position: PhysicalPoint::default(),
            size: PhysicalSize::new(2, 1).unwrap(),
            opacity: 0,
        };
    }
    let mut mixed = original.clone();
    let cells = mixed.frame_cells.as_mut().unwrap();
    let mut retained_zero = zero_marks.frame_cells.as_ref().unwrap()[0].marks[0].clone();
    retained_zero.id = OverlayId::from_u128(94_000);
    cells[0].marks.push(retained_zero.clone());
    // Selected empty coverage is consumed; unselected empty coverage survives.
    cells[1].marks.clear();
    cells[2].marks.clear();
    for track in [&hidden, &zero, &zero_marks, &mixed] {
        workspace
            .execute(EditCommand::UpsertOverlayTrack {
                track: track.clone(),
            })
            .unwrap();
    }
    workspace.select_only(FrameId::from_u128(1)).unwrap();
    workspace.toggle_selection(FrameId::from_u128(3)).unwrap();
    let visible = (1..=3).map(|id| pixels(&workspace, id)).collect::<Vec<_>>();
    bake_whole_selected(&mut workspace);
    assert_eq!(
        (1..=3).map(|id| pixels(&workspace, id)).collect::<Vec<_>>(),
        visible
    );
    for track in [&hidden, &zero, &zero_marks] {
        assert_eq!(kept_from(workspace.manifest(), track.id), track);
    }
    let remaining = kept_from(workspace.manifest(), original.id)
        .frame_cells
        .as_ref()
        .unwrap();
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0].marks, vec![retained_zero]);
    assert_eq!(
        remaining[0].scopes,
        mixed.frame_cells.as_ref().unwrap()[0].scopes
    );
    assert_eq!(remaining[1], mixed.frame_cells.as_ref().unwrap()[1]);
}

#[test]
fn frame_owned_bake_removes_exhausted_track_but_loop_preserves_source_owners() {
    let dir = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&dir.path().join("whole-owned.gfsproj"));
    let original = add_owned_overlay(&mut workspace);
    let visible = (1..=3).map(|id| pixels(&workspace, id)).collect::<Vec<_>>();
    bake_whole_selected(&mut workspace);
    assert!(workspace.manifest().timeline.overlay_tracks.is_empty());
    assert_eq!(
        (1..=3).map(|id| pixels(&workspace, id)).collect::<Vec<_>>(),
        visible
    );
    workspace.undo().unwrap();
    workspace
        .apply_motion_edit(
            &workspace.project_edit_anchor(),
            MotionOperation::LoopCrossfade {
                frames: 1,
                duration_us: 10_000,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(workspace.manifest().timeline.overlay_tracks, vec![original]);
    let appended = workspace.manifest().timeline.frames[3].id;
    assert_eq!(
        render(
            workspace.active_project(),
            appended,
            PhysicalSize::new(2, 1).unwrap(),
            &AtomicBool::new(false)
        )
        .unwrap()
        .pixels(),
        visible[0]
    );
}

#[test]
fn cinemagraph_freezes_transparent_pixels_and_does_not_touch_selection_gaps() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    add_overlay(&mut workspace);
    workspace.select_only(FrameId::from_u128(3)).unwrap();
    workspace.toggle_selection(FrameId::from_u128(1)).unwrap();
    let before = workspace.manifest().clone();
    let frozen = pixels(&workspace, 1);
    let gap = pixels(&workspace, 2);
    let animated = pixels(&workspace, 3);
    let anchor = workspace.project_edit_anchor();
    workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::Cinemagraph {
                region: rect(),
                invert: false,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(pixels(&workspace, 1), frozen);
    assert_eq!(pixels(&workspace, 2), gap);
    assert_eq!(
        pixels(&workspace, 3),
        [frozen[..4].to_vec(), animated[4..].to_vec()].concat()
    );
    assert_eq!(
        pixels(&workspace, 3)[3],
        0,
        "transparent frozen pixels must erase moving pixels"
    );
    let overlay = &workspace.manifest().timeline.overlay_tracks[0].items[0];
    assert_eq!(overlay.span.start.get(), 10_000);
    assert_eq!(overlay.span.duration.get(), 10_000);
    assert!(workspace.undo().unwrap());
    equal_except_revision(workspace.manifest(), &before);
    assert!(workspace.redo().unwrap());
    let saved = pixels(&workspace, 3);
    let root = workspace.project_root().to_owned();
    drop(workspace);
    let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(pixels(&reopened, 3), saved);
}

#[test]
fn inverted_cinemagraph_only_freezes_inside_the_rectangle() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    workspace.select_all();
    let frozen = pixels(&workspace, 1);
    let animated = pixels(&workspace, 3);
    let anchor = workspace.project_edit_anchor();
    workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::Cinemagraph {
                region: rect(),
                invert: true,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(
        pixels(&workspace, 3),
        [animated[..4].to_vec(), frozen[4..].to_vec()].concat()
    );
}

#[test]
fn smooth_loop_appends_exact_duration_and_first_endpoint_without_overlay_double_composition() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    add_overlay(&mut workspace);
    let before = workspace.manifest().clone();
    let first = pixels(&workspace, 1);
    let anchor = workspace.project_edit_anchor();
    let count = workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::LoopCrossfade {
                frames: 3,
                duration_us: 33_334,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    assert_eq!(count, MotionOutcome::Edited(3));
    assert_eq!(
        &workspace.manifest().timeline.frames[..3],
        &before.timeline.frames
    );
    assert_eq!(
        workspace.manifest().timeline.overlay_tracks,
        before.timeline.overlay_tracks
    );
    let appended = &workspace.manifest().timeline.frames[3..];
    assert_mixed_loop_frames_cannot_replay_input(appended);
    assert_eq!(
        appended
            .iter()
            .map(|frame| frame.duration.get())
            .collect::<Vec<_>>(),
        [11_111, 11_111, 11_112]
    );
    let last_id = appended[2].id;
    let last = render(
        workspace.active_project(),
        last_id,
        PhysicalSize::new(2, 1).unwrap(),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(last.pixels(), first);
    let after = workspace.manifest().clone();
    assert!(workspace.undo().unwrap());
    equal_except_revision(workspace.manifest(), &before);
    assert!(workspace.redo().unwrap());
    equal_except_revision(workspace.manifest(), &after);
    let root = workspace.project_root().to_owned();
    drop(workspace);
    let reopened = EditorWorkspace::open(&root, LockPolicy::FailIfPresent, 32).unwrap();
    assert_eq!(reopened.manifest().timeline.frames.len(), 6);
    assert_eq!(
        render(
            reopened.active_project(),
            last_id,
            PhysicalSize::new(2, 1).unwrap(),
            &AtomicBool::new(false)
        )
        .unwrap()
        .pixels(),
        first
    );
}

fn assert_mixed_loop_frames_cannot_replay_input(frames: &[gif_from_screen_domain::FrameClip]) {
    for frame in frames {
        assert_eq!(
            frame.capture_binding,
            CaptureBinding::ArchivedAfterComposite
        );
        assert!(!frame.has_recorded_annotation_input());
        assert!(gif_from_screen_domain::recorded_annotation_barrier(
            frame,
            &AnnotationMode::RecordedKeys
        ));
    }
}

#[test]
fn motion_rejects_stale_selection_invalid_rectangles_canvas_mismatch_and_limits() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    let stale = workspace.project_edit_anchor();
    workspace.select_last().unwrap();
    let before = workspace.manifest().clone();
    assert!(
        workspace
            .apply_motion_edit(
                &stale,
                MotionOperation::LoopCrossfade {
                    frames: 3,
                    duration_us: 30_000
                },
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    equal_except_revision(workspace.manifest(), &before);
    for operation in [
        MotionOperation::LoopCrossfade {
            frames: 121,
            duration_us: 30_000,
        },
        MotionOperation::LoopCrossfade {
            frames: 3,
            duration_us: 2,
        },
        MotionOperation::Cinemagraph {
            region: PhysicalRect {
                origin: PhysicalPoint::default(),
                size: PhysicalSize::new(3, 1).unwrap(),
            },
            invert: false,
        },
        MotionOperation::Cinemagraph {
            region: PhysicalRect {
                origin: PhysicalPoint::default(),
                size: PhysicalSize {
                    width: PhysicalPx::ZERO,
                    height: PhysicalPx::new(1),
                },
            },
            invert: false,
        },
    ] {
        let anchor = workspace.project_edit_anchor();
        assert!(
            workspace
                .apply_motion_edit(&anchor, operation, &AtomicBool::new(false), |_| {})
                .is_err()
        );
        equal_except_revision(workspace.manifest(), &before);
    }
    assert!(validate_budget(PhysicalSize::new(4096, 4096).unwrap(), 5).is_err());
    workspace
        .set_selection_output_size(PhysicalSize::new(1, 1).unwrap())
        .unwrap();
    let mismatch = workspace.manifest().clone();
    let anchor = workspace.project_edit_anchor();
    assert!(
        workspace
            .apply_motion_edit(
                &anchor,
                MotionOperation::LoopCrossfade {
                    frames: 3,
                    duration_us: 30_000
                },
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    equal_except_revision(workspace.manifest(), &mismatch);
}

#[test]
fn cancelling_after_pixel_preparation_preserves_timeline_and_undo_history() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    let before = workspace.manifest().clone();
    let anchor = workspace.project_edit_anchor();
    let cancel = AtomicBool::new(false);
    let error = workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::LoopCrossfade {
                frames: 8,
                duration_us: 400_000,
            },
            &cancel,
            |update| {
                if update.completed == 1 {
                    cancel.store(true, Ordering::Release);
                }
            },
        )
        .unwrap_err();
    assert!(error.contains("cancelled"));
    equal_except_revision(workspace.manifest(), &before);
    assert!(!workspace.can_undo());
}

#[test]
fn removing_selected_overlay_intervals_retains_every_gap_exactly() {
    let span = |start, duration| TimelineSpan {
        start: TimeUs::new(start),
        duration: DurationUs::new(duration).unwrap(),
    };
    assert_eq!(
        subtract_spans(span(0, 100), &[span(10, 20), span(50, 10), span(80, 20)]).unwrap(),
        [span(0, 10), span(30, 20), span(60, 20)]
    );
}

#[test]
fn smooth_loop_duration_overflow_is_rejected_before_storing_any_new_pixels() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = workspace(&directory.path().join("project"));
    workspace.select_last().unwrap();
    workspace
        .override_selection_duration(DurationUs::new(u64::MAX - 20_000).unwrap())
        .unwrap();
    let before = workspace.manifest().clone();
    let before_files = std::fs::read_dir(workspace.active_project().assets().directory())
        .unwrap()
        .count();
    let anchor = workspace.project_edit_anchor();
    let error = workspace
        .apply_motion_edit(
            &anchor,
            MotionOperation::LoopCrossfade {
                frames: 3,
                duration_us: 30_000,
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
    assert!(error.contains("overflow"));
    equal_except_revision(workspace.manifest(), &before);
    assert_eq!(
        std::fs::read_dir(workspace.active_project().assets().directory())
            .unwrap()
            .count(),
        before_files
    );
}
