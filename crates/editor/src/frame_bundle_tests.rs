use super::*;
use crate::{
    EditorError, YoyoOptions, YoyoScope, copy_selected_frames, paste_frame_clipboard, yoyo_frames,
};
use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, AssetDescriptor, AssetKind, BlendMode, Canvas,
    CanvasBackground, CaptureBinding, CaptureClockContext, CaptureClockId, CaptureMetadata,
    ClipTransform, ColorSpace, DurationUs, EditCommand, FrameAuthoringSpan, FrameClip,
    FrameLocalSpan, FrameOverlayCell, FrameOverlayMark, OverlayContent, OverlayItem, PhysicalPoint,
    PhysicalSize, ProjectId, RasterEncoding, TimeUs, TimelineSpan, UnixTimeMs,
};

fn frame_id(value: u128) -> FrameId {
    FrameId::from_u128(value)
}

fn cell(owner: u128, text: Option<&str>) -> FrameOverlayCell {
    FrameOverlayCell {
        frame_id: frame_id(owner),
        scopes: vec![
            FrameAuthoringSpan {
                run_id: 1,
                span: FrameLocalSpan::new(0, 1, DurationUs::new(3).unwrap()).unwrap(),
            },
            FrameAuthoringSpan {
                run_id: 2,
                span: FrameLocalSpan::new(2, 3, DurationUs::new(3).unwrap()).unwrap(),
            },
        ],
        marks: text
            .map(|text| {
                vec![FrameOverlayMark {
                    id: OverlayId::from_u128(owner + 10),
                    z_index: 7,
                    content: OverlayContent::KeyStroke {
                        text: text.to_owned(),
                        position: PhysicalPoint::default(),
                        raster: None,
                    },
                }]
            })
            .unwrap_or_default(),
    }
}

fn project() -> ProjectManifest {
    let size = PhysicalSize::new(1, 1).unwrap();
    let mut project = ProjectManifest::new(
        ProjectId::from_u128(1),
        "bundle-test",
        UnixTimeMs::new(1),
        Canvas {
            size,
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    )
    .unwrap();
    for byte in [7, 8, 9] {
        let id = AssetId::from_digest([byte; 32]);
        project.assets.insert(
            id,
            AssetDescriptor {
                id,
                byte_len: 4,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
    }
    project.timeline.frames = (1..=3)
        .map(|id| {
            let sampled_at = TimeUs::new(u64::try_from((id - 1) * 100_000).unwrap());
            FrameClip {
                id: frame_id(id),
                asset_id: AssetId::from_digest([7; 32]),
                duration: DurationUs::new(100_000).unwrap(),
                transform: ClipTransform::default(),
                capture_metadata: CaptureMetadata {
                    captured_at: Some(sampled_at),
                    ..CaptureMetadata::default()
                },
                capture_binding: CaptureBinding::Original,
                capture_clock: Some(CaptureClockContext {
                    id: Some(CaptureClockId::from_u128(1)),
                    sampled_at,
                }),
                effects: Vec::new(),
            }
        })
        .collect();
    project.timeline.overlay_tracks.push(OverlayTrack {
        id: TrackId::from_u128(10),
        name: "Recorded keys".to_owned(),
        annotation: Some(AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            ..AnnotationRequest::default()
        }),
        annotation_scope: None,
        visible: true,
        opacity: 213,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
        frame_cells: Some(vec![
            cell(1, Some("Ctrl+C")),
            cell(2, Some("held Ctrl+C")),
            cell(3, None),
        ]),
    });
    project.timeline.overlay_tracks.push(OverlayTrack {
        id: TrackId::from_u128(20),
        name: "Hidden raster".to_owned(),
        annotation: None,
        annotation_scope: None,
        visible: false,
        opacity: 0,
        blend_mode: BlendMode::Multiply,
        items: Vec::new(),
        frame_cells: Some(vec![FrameOverlayCell {
            frame_id: frame_id(2),
            scopes: Vec::new(),
            marks: vec![FrameOverlayMark {
                id: OverlayId::from_u128(21),
                z_index: -4,
                content: OverlayContent::Raster {
                    asset_id: AssetId::from_digest([8; 32]),
                    position: PhysicalPoint::default(),
                    size,
                    opacity: 0,
                },
            }],
        }]),
    });
    project.timeline.overlay_tracks.push(legacy_track(size));
    project.validate().unwrap();
    project
}

fn legacy_track(size: PhysicalSize) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(30),
        name: "Legacy time anchor".to_owned(),
        annotation: None,
        annotation_scope: None,
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        frame_cells: None,
        items: vec![OverlayItem {
            id: OverlayId::from_u128(31),
            z_index: 0,
            span: TimelineSpan {
                start: TimeUs::ZERO,
                duration: DurationUs::new(300_000).unwrap(),
            },
            content: OverlayContent::Raster {
                asset_id: AssetId::from_digest([9; 32]),
                position: PhysicalPoint::default(),
                size,
                opacity: 255,
            },
        }],
    }
}

fn generator() -> impl FnMut() -> FrameId {
    let mut next = 99;
    move || {
        next += 1;
        frame_id(next)
    }
}

fn assert_frozen_copy(
    source: &OverlayTrack,
    copied: &OverlayTrack,
    owners: &BTreeMap<FrameId, FrameId>,
) {
    assert_ne!(copied.id, source.id);
    let mut expected = source.clone();
    expected.id = copied.id;
    let cells = expected.frame_cells.as_mut().unwrap();
    cells.retain(|cell| owners.contains_key(&cell.frame_id));
    for (cell, actual) in cells.iter_mut().zip(copied.frame_cells.as_ref().unwrap()) {
        cell.frame_id = owners[&cell.frame_id];
        for (mark, actual) in cell.marks.iter_mut().zip(&actual.marks) {
            assert_ne!(mark.id, actual.id);
            mark.id = actual.id;
        }
    }
    assert_eq!(copied, &expected);
}

#[test]
fn selected_clipboard_preserves_input_empty_held_marks_after_source_frame_is_deleted() {
    let mut project = project();
    let source = project.clone();
    assert!(
        source.timeline.frames[1]
            .capture_metadata
            .key_strokes
            .is_empty()
    );
    let clipboard = copy_selected_frames(&project, [frame_id(2)]).unwrap();
    assert_eq!(clipboard.frame_bundle().tracks().len(), 2);
    assert_eq!(
        clipboard
            .frame_bundle()
            .referenced_assets()
            .collect::<Vec<_>>(),
        [AssetId::from_digest([8; 32])]
    );
    project
        .apply_command(&EditCommand::RemoveFrames {
            frame_ids: vec![frame_id(2)],
        })
        .unwrap();
    let before_paste = project.clone();
    let command =
        paste_frame_clipboard(&project, &clipboard, Some(frame_id(1)), generator()).unwrap();
    let undo = project.apply_command(&command).unwrap().inverse;
    let inserted = &project.timeline.frames[1];
    let mut expected = source.timeline.frames[1].clone();
    expected.id = frame_id(100);
    assert_eq!(inserted, &expected);
    let owners = BTreeMap::from([(frame_id(2), frame_id(100))]);
    for (original, copied) in source.timeline.overlay_tracks[..2]
        .iter()
        .zip(&project.timeline.overlay_tracks[before_paste.timeline.overlay_tracks.len()..])
    {
        assert_frozen_copy(original, copied, &owners);
    }
    let frozen = project.clone();
    let redo = project.apply_command(&undo).unwrap().inverse;
    project.revision = before_paste.revision;
    assert_eq!(project, before_paste);
    project.apply_command(&redo).unwrap();
    project.revision = frozen.revision;
    assert_eq!(project, frozen);
    let reopened: ProjectManifest =
        serde_json::from_slice(&serde_json::to_vec(&project).unwrap()).unwrap();
    reopened.validate().unwrap();
    assert_eq!(reopened, project);
}

#[test]
fn full_copy_keeps_unmarked_scope_but_never_copies_legacy_time_tracks() {
    let project = project();
    let selected = project
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    let bundle = FrameBundle::capture(&project, &selected).unwrap();
    assert_eq!(bundle.tracks(), &project.timeline.overlay_tracks[..2]);
    assert!(
        bundle.tracks()[0].frame_cells.as_ref().unwrap()[2]
            .marks
            .is_empty()
    );
    assert!(
        !bundle
            .referenced_assets()
            .any(|id| id == AssetId::from_digest([9; 32]))
    );
    let mut legacy_only = project.clone();
    legacy_only
        .timeline
        .overlay_tracks
        .retain(|track| track.frame_cells.is_none());
    let clipboard = copy_selected_frames(&legacy_only, [frame_id(2)]).unwrap();
    let mut calls = 0;
    paste_frame_clipboard(&legacy_only, &clipboard, None, || {
        calls += 1;
        frame_id(100)
    })
    .unwrap();
    assert_eq!(
        calls, 1,
        "legacy copy keeps the original frame-ID factory contract"
    );
}

#[test]
fn large_unselected_cell_does_not_consume_copy_budget_but_selected_data_does() {
    let mut project = project();
    let cells = project.timeline.overlay_tracks[0]
        .frame_cells
        .as_mut()
        .unwrap();
    if let OverlayContent::KeyStroke { text, .. } = &mut cells[0].marks[0].content {
        *text = "x".repeat(MAX_FRAME_BUNDLE_METADATA_BYTES + 1);
    }
    let tiny = copy_selected_frames(&project, [frame_id(2)]).unwrap();
    assert_eq!(
        tiny.frame_bundle().tracks()[0]
            .frame_cells
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        copy_selected_frames(&project, [frame_id(1)]),
        Err(EditorError::FrameBundle(FrameBundleError::Budget {
            kind: "serialized metadata bytes"
        }))
    ));
}

#[test]
fn hidden_mark_assets_are_required_before_any_paste_ids_are_requested() {
    let mut project = project();
    let clipboard = copy_selected_frames(&project, [frame_id(2)]).unwrap();
    project.assets.remove(&AssetId::from_digest([8; 32]));
    assert!(matches!(
        paste_frame_clipboard(&project, &clipboard, None, || panic!(
            "must validate assets first"
        )),
        Err(EditorError::FrameBundle(
            FrameBundleError::MissingAsset { .. }
        ))
    ));
}

#[test]
fn yoyo_full_and_selected_ranges_clone_frozen_cells_without_replaying_events() {
    for (scope, repeat_endpoints, expected_sources) in [
        (YoyoScope::EntireTimeline, true, vec![3, 2, 1]),
        (YoyoScope::EntireTimeline, false, vec![2]),
        (YoyoScope::Selection, true, vec![2, 1]),
    ] {
        let mut project = project();
        let before = project.clone();
        let command = yoyo_frames(
            &project,
            [frame_id(1), frame_id(2)],
            YoyoOptions {
                repeat_endpoints,
                scope,
            },
            generator(),
        )
        .unwrap();
        let undo = project.apply_command(&command).unwrap().inverse;
        let owners: BTreeMap<_, _> = expected_sources
            .iter()
            .enumerate()
            .map(|(index, source)| (frame_id(*source), frame_id(100 + index as u128)))
            .collect();
        for frame in &project.timeline.frames {
            if let Some((source, _)) = owners
                .iter()
                .find(|(_, destination)| **destination == frame.id)
            {
                let mut expected = before
                    .timeline
                    .frames
                    .iter()
                    .find(|frame| frame.id == *source)
                    .unwrap()
                    .clone();
                expected.id = frame.id;
                assert_eq!(frame, &expected);
            }
        }
        for (original, copied) in before.timeline.overlay_tracks[..2]
            .iter()
            .zip(&project.timeline.overlay_tracks[before.timeline.overlay_tracks.len()..])
        {
            assert_frozen_copy(original, copied, &owners);
        }
        assert_eq!(
            &project.timeline.overlay_tracks[..2],
            &before.timeline.overlay_tracks[..2]
        );
        project.apply_command(&undo).unwrap();
        project.revision = before.revision;
        assert_eq!(project, before);
    }
}

#[test]
fn identity_factories_are_bounded_and_reserve_source_destination_and_new_namespaces() {
    let project = project();
    let mut ids = FrameBundleIdentities::new(&project.timeline.overlay_tracks).unwrap();
    let mut candidates = [FrameId::NIL, frame_id(10), frame_id(20), frame_id(100)].into_iter();
    assert_eq!(
        ids.track(&mut || candidates.next().unwrap()).unwrap(),
        TrackId::from_u128(100)
    );
    let mut calls = 0;
    assert!(matches!(
        ids.track(&mut || {
            calls += 1;
            frame_id(100)
        }),
        Err(FrameBundleError::IdentityExhausted { kind: "track" })
    ));
    assert_eq!(calls, MAX_ID_ATTEMPTS);
    let mut candidates = [
        frame_id(11),
        frame_id(12),
        frame_id(21),
        frame_id(31),
        frame_id(100),
    ]
    .into_iter();
    assert_eq!(
        ids.mark(&mut || candidates.next().unwrap()).unwrap(),
        OverlayId::from_u128(100)
    );
    let mut nil_calls = 0;
    assert!(
        ids.mark(&mut || {
            nil_calls += 1;
            FrameId::NIL
        })
        .is_err()
    );
    assert_eq!(nil_calls, MAX_ID_ATTEMPTS);
}

#[test]
fn malformed_mappings_and_selection_fail_without_changing_the_snapshot() {
    let project = project();
    assert!(matches!(
        FrameBundle::capture(&project, &BTreeSet::from([frame_id(99)])),
        Err(FrameBundleError::UnknownFrame { .. })
    ));
    let bundle =
        FrameBundle::capture(&project, &BTreeSet::from([frame_id(1), frame_id(2)])).unwrap();
    let before = bundle.clone();
    for mapping in [
        BTreeMap::new(),
        BTreeMap::from([(frame_id(1), FrameId::NIL), (frame_id(2), frame_id(101))]),
        BTreeMap::from([(frame_id(1), frame_id(100)), (frame_id(2), frame_id(100))]),
    ] {
        let mut identities = FrameBundleIdentities::new(&project.timeline.overlay_tracks).unwrap();
        assert!(
            bundle
                .remap(&mapping, &mut identities, &mut generator())
                .is_err()
        );
        assert_eq!(bundle, before);
    }
    let mut count = usize::MAX;
    assert!(add(&mut count, 1, usize::MAX, "test").is_err());
}
