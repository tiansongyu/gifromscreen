use super::super::tests::{create_rendered_duplicate_workspace, frame_id};
use super::*;
use gif_from_screen_domain::{
    DurationUs, PhysicalPoint, PhysicalRect, Rgba, ShapeKind, TimelineSpan,
};
use gif_from_screen_project::LockPolicy;
use std::collections::BTreeSet;

fn span(start: u64, end: u64) -> TimelineSpan {
    TimelineSpan {
        start: TimeUs::new(start),
        duration: DurationUs::new(end - start).unwrap(),
    }
}

fn item(id: u128, start: u64, end: u64) -> OverlayItem {
    OverlayItem {
        id: OverlayId::from_u128(id),
        span: span(start, end),
        z_index: 4,
        content: OverlayContent::Progress {
            bounds: PhysicalRect::new(0, 0, 10, 1).unwrap(),
            foreground: Rgba {
                red: 0,
                green: 255,
                blue: 30,
                alpha: 170,
            },
            background: Rgba {
                red: 90,
                green: 5,
                blue: 170,
                alpha: 100,
            },
            show_frame_number: false,
            style: None,
        },
    }
}

fn track(id: u128, items: Vec<OverlayItem>) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(id),
        frame_cells: None,
        annotation: None,
        annotation_scope: None,
        name: format!("Legacy \"{id}\" Ω"),
        visible: true,
        opacity: 135,
        blend_mode: BlendMode::Screen,
        items,
    }
}

fn shape(id: u128) -> OverlayItem {
    let mut item = item(id, 0, 100);
    item.content = OverlayContent::Shape {
        kind: ShapeKind::Rectangle,
        bounds: PhysicalRect::new(0, 0, 2, 1).unwrap(),
        stroke_width: 0,
        stroke: Rgba::TRANSPARENT,
        fill: Some(Rgba {
            red: 90,
            green: 200,
            blue: 70,
            alpha: 130,
        }),
    };
    item
}

fn paint(workspace: &EditorWorkspace, id: FrameId) -> Vec<u8> {
    crate::editor_preview::render_frame_surface(workspace.active_project(), id, 1024 * 1024)
        .unwrap()
        .pixels()
        .to_vec()
}

fn convert(workspace: &mut EditorWorkspace, track: TrackId) -> AnnotationEditReport {
    let anchor = workspace.project_edit_anchor();
    workspace
        .convert_overlay_to_frames(&anchor, track, &AtomicBool::new(false), |_| {})
        .unwrap()
}

#[test]
fn entire_track_conversion_preserves_pixels_order_frames_assets_undo_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = create_rendered_duplicate_workspace(&directory);
    let target = TrackId::from_u128(11);
    for track in [
        track(10, vec![shape(100)]),
        track(11, vec![item(101, 0, 100), shape(102)]),
        track(12, vec![shape(103)]),
    ] {
        workspace
            .execute(EditCommand::UpsertOverlayTrack { track })
            .unwrap();
    }
    workspace.select_only(frame_id(3)).unwrap(); // conversion is deliberately not selection-local.
    let before = workspace.manifest().clone();
    let pixels: Vec<_> = before
        .timeline
        .frames
        .iter()
        .map(|frame| paint(&workspace, frame.id))
        .collect();
    let assets_before = std::fs::read_dir(workspace.active_project().assets().directory())
        .unwrap()
        .count();
    let report = convert(&mut workspace, target);
    assert_eq!(report.frames, 4);
    assert_eq!(workspace.manifest().timeline.frames, before.timeline.frames);
    assert_eq!(workspace.manifest().assets, before.assets);
    assert_eq!(
        std::fs::read_dir(workspace.active_project().assets().directory())
            .unwrap()
            .count(),
        assets_before
    );
    assert_eq!(
        workspace.manifest().timeline.overlay_tracks[0],
        before.timeline.overlay_tracks[0]
    );
    assert_eq!(
        workspace.manifest().timeline.overlay_tracks[2],
        before.timeline.overlay_tracks[2]
    );
    let frozen = workspace.manifest().timeline.overlay_tracks[1].clone();
    assert_eq!(frozen.id, target);
    assert_eq!(frozen.name, before.timeline.overlay_tracks[1].name);
    assert_eq!(frozen.opacity, 135);
    assert_eq!(frozen.blend_mode, BlendMode::Screen);
    assert!(frozen.items.is_empty());
    let mut identities = BTreeSet::new();
    for cell in frozen.frame_cells.as_ref().unwrap() {
        assert!(cell.scopes.is_empty() && cell.input_replay.is_none());
        for mark in &cell.marks {
            assert!(!mark.id.is_nil() && identities.insert(mark.id));
            assert_eq!(mark.z_index, 4);
        }
    }
    assert_eq!(identities.len(), 8);
    for (frame, expected) in before.timeline.frames.iter().zip(&pixels) {
        assert_eq!(&paint(&workspace, frame.id), expected);
    }
    assert!(workspace.undo().unwrap());
    assert_eq!(
        workspace.manifest().timeline.overlay_tracks,
        before.timeline.overlay_tracks
    );
    assert!(workspace.redo().unwrap());
    assert_eq!(workspace.manifest().timeline.overlay_tracks[1], frozen);
    workspace.checkpoint().unwrap();
    drop(workspace);
    let reopened = EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 16).unwrap();
    assert_eq!(reopened.manifest().timeline.overlay_tracks[1], frozen);
    for (frame, expected) in before.timeline.frames.iter().zip(&pixels) {
        assert_eq!(&paint(&reopened, frame.id), expected);
    }
}

#[test]
fn converted_progress_follows_its_owner_after_reverse_and_retime() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = create_rendered_duplicate_workspace(&directory);
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: track(10, vec![item(100, 0, 100)]),
        })
        .unwrap();
    convert(&mut workspace, TrackId::from_u128(10));
    let pixels: BTreeMap<_, _> = workspace
        .manifest()
        .timeline
        .frames
        .iter()
        .map(|frame| (frame.id, paint(&workspace, frame.id)))
        .collect();
    workspace.select_all();
    workspace.reverse_selection().unwrap();
    workspace
        .override_selection_duration(DurationUs::new(7).unwrap())
        .unwrap();
    for (&id, expected) in &pixels {
        assert_eq!(&paint(&workspace, id), expected);
    }
}

#[test]
fn known_partial_scopes_keep_exact_fractions_positive_gaps_empty_cells_and_unknown_replay() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = create_rendered_duplicate_workspace(&directory);
    let mut source = track(10, vec![item(100, 10, 40)]);
    source.annotation = Some(AnnotationRequest::default());
    source.annotation_scope = Some(vec![span(5, 15), span(18, 35), span(35, 40)]);
    let recipe = source.annotation.clone();
    workspace
        .execute(EditCommand::UpsertOverlayTrack { track: source })
        .unwrap();
    assert_eq!(convert(&mut workspace, TrackId::from_u128(10)).frames, 3);
    let frozen = &workspace.manifest().timeline.overlay_tracks[0];
    assert_eq!(frozen.annotation, recipe);
    assert!(frozen.annotation_scope.is_none());
    let cells = frozen.frame_cells.as_ref().unwrap();
    assert!(cells[0].marks.is_empty());
    assert_eq!(
        cells[0].scopes,
        [FrameAuthoringSpan {
            run_id: 1,
            span: FrameLocalSpan::new(5, 10, DurationUs::new(10).unwrap()).unwrap()
        }]
    );
    assert_eq!(
        cells[1].scopes,
        [
            FrameAuthoringSpan {
                run_id: 1,
                span: FrameLocalSpan::new(0, 5, DurationUs::new(20).unwrap()).unwrap()
            },
            FrameAuthoringSpan {
                run_id: 2,
                span: FrameLocalSpan::new(8, 20, DurationUs::new(20).unwrap()).unwrap()
            },
        ]
    );
    assert_eq!(
        cells[2].scopes,
        [
            FrameAuthoringSpan {
                run_id: 2,
                span: FrameLocalSpan::new(0, 5, DurationUs::new(30).unwrap()).unwrap()
            },
            FrameAuthoringSpan {
                run_id: 2,
                span: FrameLocalSpan::new(5, 10, DurationUs::new(30).unwrap()).unwrap()
            },
        ]
    );
    assert!(cells.iter().all(|cell| cell.input_replay.is_none()));
}

#[test]
fn hidden_zero_opacity_and_explicit_empty_groups_are_not_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = create_rendered_duplicate_workspace(&directory);
    let mut hidden = track(10, vec![item(100, 0, 100)]);
    hidden.visible = false;
    hidden.opacity = 0;
    workspace
        .execute(EditCommand::UpsertOverlayTrack { track: hidden })
        .unwrap();
    let mut empty = track(11, Vec::new());
    empty.annotation = Some(AnnotationRequest::default());
    empty.annotation_scope = Some(Vec::new());
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: empty.clone(),
        })
        .unwrap();
    assert_eq!(convert(&mut workspace, TrackId::from_u128(10)).frames, 4);
    let hidden = &workspace.manifest().timeline.overlay_tracks[0];
    assert!(!hidden.visible && hidden.opacity == 0);
    assert_eq!(
        hidden
            .frame_cells
            .as_ref()
            .unwrap()
            .iter()
            .map(|cell| cell.marks.len())
            .sum::<usize>(),
        4
    );
    assert_eq!(convert(&mut workspace, TrackId::from_u128(11)).frames, 0);
    let converted = &workspace.manifest().timeline.overlay_tracks[1];
    assert_eq!(converted.id, empty.id);
    assert_eq!(converted.annotation, empty.annotation);
    assert_eq!(converted.frame_cells, Some(Vec::new()));
    assert!(workspace.undo().unwrap());
    assert_eq!(workspace.manifest().timeline.overlay_tracks[1], empty);
}

#[test]
fn unknown_authoring_scope_and_uncovered_marks_are_rejected_without_a_commit() {
    for coverage in [None, Some(vec![span(50, 55)])] {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&directory);
        let mut source = track(10, vec![item(100, 0, 10)]);
        source.annotation = Some(AnnotationRequest::default());
        source.annotation_scope = coverage;
        workspace
            .execute(EditCommand::UpsertOverlayTrack { track: source })
            .unwrap();
        let before = workspace.manifest().clone();
        let anchor = workspace.project_edit_anchor();
        assert!(
            workspace
                .convert_overlay_to_frames(
                    &anchor,
                    TrackId::from_u128(10),
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
        );
        assert_eq!(workspace.manifest(), &before);
    }
}

#[test]
fn stale_already_owned_cancelled_and_unsupported_requests_leave_the_track_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = create_rendered_duplicate_workspace(&directory);
    let stale = workspace.project_edit_anchor();
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: track(10, vec![item(100, 0, 100)]),
        })
        .unwrap();
    let before = workspace.manifest().clone();
    assert!(
        workspace
            .convert_overlay_to_frames(
                &stale,
                TrackId::from_u128(10),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    let anchor = workspace.project_edit_anchor();
    let cancel = AtomicBool::new(false);
    let error = workspace
        .convert_overlay_to_frames(&anchor, TrackId::from_u128(10), &cancel, |_| {
            cancel.store(true, Ordering::Relaxed);
        })
        .unwrap_err();
    assert!(error.contains("cancelled"));
    assert_eq!(workspace.manifest(), &before);
    convert(&mut workspace, TrackId::from_u128(10));
    let owned = workspace.manifest().clone();
    let anchor = workspace.project_edit_anchor();
    assert!(
        workspace
            .convert_overlay_to_frames(
                &anchor,
                TrackId::from_u128(10),
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap_err()
            .contains("already")
    );
    assert_eq!(workspace.manifest(), &owned);
    workspace.undo().unwrap();
    let mut unsupported = workspace.manifest().timeline.overlay_tracks[0].clone();
    if let OverlayContent::Progress {
        show_frame_number, ..
    } = &mut unsupported.items[0].content
    {
        *show_frame_number = true;
    }
    workspace
        .execute(EditCommand::UpsertOverlayTrack { track: unsupported })
        .unwrap();
    let before = workspace.manifest().clone();
    let anchor = workspace.project_edit_anchor();
    assert!(
        workspace
            .convert_overlay_to_frames(
                &anchor,
                TrackId::from_u128(10),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
    );
    assert_eq!(workspace.manifest(), &before);
}

#[test]
fn index_counts_output_cells_and_marks_before_cloning_any_contents() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = create_rendered_duplicate_workspace(&directory);
    let source = track(
        10,
        (0..25_001).map(|index| item(100 + index, 0, 100)).collect(),
    );
    assert!(
        ConversionPlan::new(workspace.manifest(), &source, &AtomicBool::new(false))
            .err()
            .unwrap()
            .contains("100,000")
    );
    let mut manifest = workspace.manifest().clone();
    let template = manifest.timeline.frames[0].clone();
    manifest.timeline.frames = (0..40_001)
        .map(|index| {
            let mut frame = template.clone();
            frame.id = FrameId::from_u128(index + 1);
            frame.duration = DurationUs::new(1).unwrap();
            frame
        })
        .collect();
    let source = track(10, vec![item(100, 0, 40_001)]);
    assert!(
        ConversionPlan::new(&manifest, &source, &AtomicBool::new(false))
            .err()
            .unwrap()
            .contains("40,000")
    );
}

#[test]
fn actual_serialized_metadata_budget_rejects_large_repeated_text_before_materialization() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = create_rendered_duplicate_workspace(&directory);
    let mut text = item(100, 0, 100);
    text.content = OverlayContent::KeyStroke {
        text: "x".repeat(5 * 1024 * 1024),
        position: PhysicalPoint::default(),
        raster: None,
    };
    let source = track(10, vec![text]);
    let cancellation = AtomicBool::new(false);
    assert!(metadata_bytes(&source, &cancellation).unwrap() < MAX_FRAME_BUNDLE_METADATA_BYTES);
    let plan = ConversionPlan::new(workspace.manifest(), &source, &cancellation).unwrap();
    let error = metadata_bytes(
        &PlannedCommand {
            kind: "upsert_overlay_track",
            track: TrackView(&plan),
        },
        &cancellation,
    )
    .unwrap_err();
    assert!(error.contains("16 MiB"));
}

#[test]
fn cancellation_during_or_after_mark_materialization_never_commits_a_partial_track() {
    for completed in [1, 4] {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = create_rendered_duplicate_workspace(&directory);
        workspace
            .execute(EditCommand::UpsertOverlayTrack {
                track: track(10, vec![item(100, 0, 100)]),
            })
            .unwrap();
        let before = workspace.manifest().clone();
        let anchor = workspace.project_edit_anchor();
        let cancellation = AtomicBool::new(false);
        let result = workspace.convert_overlay_to_frames(
            &anchor,
            TrackId::from_u128(10),
            &cancellation,
            |progress| {
                if progress.completed == completed {
                    cancellation.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(workspace.manifest(), &before);
    }
}

#[test]
fn items_active_only_inside_a_frame_do_not_create_subframes_or_invent_marks() {
    let directory = tempfile::tempdir().unwrap();
    let mut workspace = create_rendered_duplicate_workspace(&directory);
    workspace
        .execute(EditCommand::UpsertOverlayTrack {
            track: track(10, vec![item(100, 5, 9)]),
        })
        .unwrap();
    let before = workspace.manifest().timeline.frames.clone();
    let pixels = paint(&workspace, frame_id(1));
    assert_eq!(convert(&mut workspace, TrackId::from_u128(10)).frames, 0);
    assert_eq!(workspace.manifest().timeline.frames, before);
    assert_eq!(paint(&workspace, frame_id(1)), pixels);
    assert_eq!(
        workspace.manifest().timeline.overlay_tracks[0].frame_cells,
        Some(Vec::new())
    );
}
