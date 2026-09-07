use super::*;

use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureBinding,
    CaptureClockContext, CaptureClockId, CaptureMetadata, ClipTransform, ColorSpace, DurationUs,
    FrameClip, FrameLocalSpan, FrameOverlayCell, FrameOverlayMark, OverlayContent, OverlayId,
    OverlayItem, PhysicalPoint, PhysicalRect, PhysicalSize, ProjectId, RasterEncoding, Rgba,
    ShapeKind, TimeUs, TimelineSpan, UnixTimeMs,
};

fn fixture() -> ProjectManifest {
    let size = PhysicalSize::new(8, 4).unwrap();
    let asset_id = AssetId::from_digest([1; 32]);
    let mut project = ProjectManifest::new(
        ProjectId::from_u128(1),
        "paint test",
        UnixTimeMs::new(1),
        Canvas {
            size,
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    )
    .unwrap();
    project.schema_version = 4;
    project.assets.insert(
        asset_id,
        AssetDescriptor {
            id: asset_id,
            byte_len: 128,
            kind: AssetKind::Frame {
                size,
                encoding: RasterEncoding::Rgba8,
            },
        },
    );
    project.timeline.frames = (1..=3)
        .map(|id| FrameClip {
            id: FrameId::from_u128(id),
            asset_id,
            duration: DurationUs::new(100_000).unwrap(),
            transform: ClipTransform::default(),
            effects: Vec::new(),
            render_steps: Vec::new(),
            capture_metadata: CaptureMetadata {
                captured_at: Some(TimeUs::new(u64::try_from(id).unwrap() * 1000)),
                dropped_frames_before: 7,
                ..CaptureMetadata::default()
            },
            capture_binding: CaptureBinding::Original,
            capture_clock: Some(CaptureClockContext {
                id: Some(CaptureClockId::from_u128(7)),
                sampled_at: TimeUs::new(u64::try_from(id).unwrap() * 1000),
            }),
        })
        .collect();
    project
}

fn track(identity: u128, owners: &[u128]) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(identity),
        name: format!("group {identity}"),
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
        annotation: None,
        annotation_scope: None,
        frame_cells: Some(
            owners
                .iter()
                .map(|owner| {
                    FrameOverlayCell::whole(
                        FrameId::from_u128(*owner),
                        1,
                        vec![FrameOverlayMark {
                            id: OverlayId::from_u128(identity * 10 + owner),
                            z_index: 0,
                            content: OverlayContent::Shape {
                                kind: ShapeKind::Rectangle,
                                bounds: PhysicalRect::new(1, 1, 2, 2).unwrap(),
                                stroke_width: 0,
                                stroke: Rgba::TRANSPARENT,
                                fill: Some(Rgba {
                                    red: 20,
                                    green: 40,
                                    blue: 80,
                                    alpha: 128,
                                }),
                            },
                        }],
                    )
                })
                .collect(),
        ),
    }
}

fn apply(project: &mut ProjectManifest, track: OverlayTrack) -> EditCommand {
    let commands = author_frame_owned_track(project, track).unwrap();
    assert!(matches!(
        commands.last(),
        Some(EditCommand::UpsertOverlayTrack { .. })
    ));
    project
        .apply_command(&EditCommand::Compound { commands })
        .unwrap()
        .inverse
}

fn stage(track: &OverlayTrack, frame: u128) -> Option<u32> {
    track
        .frame_cells
        .as_ref()
        .unwrap()
        .iter()
        .find(|cell| cell.frame_id == FrameId::from_u128(frame))
        .unwrap()
        .stage
}

#[test]
fn new_author_seals_every_legacy_tail_and_preserves_gaps_clocks_assets_and_exact_undo() {
    let mut project = fixture();
    let mut hidden = track(2, &[1, 2, 3]);
    hidden.visible = false;
    hidden.opacity = 0;
    hidden.frame_cells.as_mut().unwrap()[0].scopes[0].span =
        FrameLocalSpan::new(20_000, 40_000, DurationUs::new(100_000).unwrap()).unwrap();
    let mut empty = track(3, &[1]);
    empty.frame_cells.as_mut().unwrap()[0].marks.clear();
    let mut timed = track(4, &[]);
    timed.frame_cells = None;
    timed.items.push(OverlayItem {
        id: OverlayId::from_u128(400),
        z_index: -1,
        span: TimelineSpan {
            start: TimeUs::ZERO,
            duration: DurationUs::new(300_000).unwrap(),
        },
        content: OverlayContent::KeyStroke {
            text: "legacy".into(),
            position: PhysicalPoint::default(),
            raster: None,
        },
    });
    project.timeline.overlay_tracks = vec![hidden, empty, timed];
    let before = project.clone();
    let mut new = track(10, &[1, 3]);
    new.frame_cells.as_mut().unwrap()[1].scopes[0].run_id = 2;
    let scopes = new.frame_cells.clone();
    let inverse = apply(&mut project, new);
    assert_eq!(project.schema_version, 5);
    assert_eq!(project.canvas, before.canvas);
    assert_eq!(project.assets, before.assets);
    assert_eq!(
        project.timeline.overlay_tracks[2],
        before.timeline.overlay_tracks[2]
    );
    assert_eq!(project.timeline.frames[1], before.timeline.frames[1]);
    for position in [0, 2] {
        let frame = &project.timeline.frames[position];
        let mut restored = frame.clone();
        restored.render_steps.clear();
        assert_eq!(restored, before.timeline.frames[position]);
        assert_eq!(
            frame.render_steps,
            [
                FrameRenderStep::composite(1),
                FrameRenderStep::Composite {
                    stage_id: 2,
                    precision: CompositePrecision::WpfPbgra8PngV1
                }
            ]
        );
    }
    assert_eq!(stage(&project.timeline.overlay_tracks[0], 1), Some(1));
    assert_eq!(stage(&project.timeline.overlay_tracks[0], 2), None);
    assert_eq!(stage(&project.timeline.overlay_tracks[0], 3), Some(1));
    assert_eq!(stage(&project.timeline.overlay_tracks[1], 1), Some(1));
    let mut restored_new = project
        .timeline
        .overlay_tracks
        .last()
        .unwrap()
        .frame_cells
        .clone();
    for cell in restored_new.iter_mut().flatten() {
        cell.stage = None;
    }
    assert_eq!(restored_new, scopes);
    project.apply_command(&inverse).unwrap();
    let mut expected = before;
    expected.schema_version = 5;
    expected.revision = expected.revision.next().unwrap().next().unwrap();
    assert_eq!(project, expected);
}

#[test]
fn consecutive_authors_have_distinct_boundaries_and_clipboard_keeps_them() {
    let mut project = fixture();
    apply(&mut project, track(10, &[1]));
    apply(&mut project, track(11, &[1]));
    assert_eq!(project.timeline.frames[0].render_steps.len(), 3);
    assert_eq!(stage(&project.timeline.overlay_tracks[0], 1), Some(2));
    assert_eq!(stage(&project.timeline.overlay_tracks[1], 1), Some(3));
    let original = project.timeline.frames[0].clone();
    let clipboard = crate::copy_selected_frames(&project, [original.id]).unwrap();
    let mut identity = 100;
    let command = crate::paste_frame_clipboard(&project, &clipboard, Some(original.id), || {
        identity += 1;
        FrameId::from_u128(identity)
    })
    .unwrap();
    project.apply_command(&command).unwrap();
    let copied = &project.timeline.frames[1];
    assert_ne!(copied.id, original.id);
    assert_eq!(copied.render_steps, original.render_steps);
    assert_eq!(copied.capture_clock, original.capture_clock);
    assert_eq!(copied.capture_metadata, original.capture_metadata);
    let stages: BTreeSet<_> = project
        .timeline
        .overlay_tracks
        .iter()
        .flat_map(|track| track.frame_cells.iter().flatten())
        .filter(|cell| cell.frame_id == copied.id)
        .filter_map(|cell| cell.stage)
        .collect();
    assert_eq!(stages, BTreeSet::from([2, 3]));
}

#[test]
fn enhanced_blending_has_a_frame_owned_legacy_boundary_not_an_implicit_wpf_conversion() {
    for mode in [BlendMode::Multiply, BlendMode::Screen] {
        let mut project = fixture();
        let mut authored = track(10, &[1]);
        authored.blend_mode = mode;
        apply(&mut project, authored);
        assert_eq!(project.schema_version, 4);
        assert_eq!(
            project.timeline.frames[0].render_steps,
            [FrameRenderStep::composite(1), FrameRenderStep::composite(2)]
        );
        assert_eq!(stage(&project.timeline.overlay_tracks[0], 1), Some(2));
        assert_eq!(project.timeline.overlay_tracks[0].blend_mode, mode);
    }
}

#[test]
fn new_group_validation_rejects_existing_ids_owners_and_preanchored_cells_without_mutation() {
    let mut project = fixture();
    project.timeline.overlay_tracks.push(track(10, &[1]));
    let before = project.clone();
    let mut preanchored = track(11, &[1]);
    preanchored.frame_cells.as_mut().unwrap()[0].stage = Some(1);
    let mut duplicate_marks = track(11, &[1, 2]);
    duplicate_marks.frame_cells.as_mut().unwrap()[1].marks[0].id = OverlayId::from_u128(111);
    let mut reused_mark = track(12, &[1]);
    reused_mark.frame_cells.as_mut().unwrap()[0].marks[0].id = OverlayId::from_u128(101);
    let mut timed = track(12, &[]);
    timed.frame_cells = None;
    for invalid in [
        track(10, &[1]),
        track(11, &[9]),
        track(11, &[1, 1]),
        preanchored,
        duplicate_marks,
        reused_mark,
        timed,
    ] {
        assert!(author_frame_owned_track(&project, invalid).is_err());
        assert_eq!(project, before);
    }
}

#[test]
fn pending_rasters_are_checked_by_the_final_compound_without_pixel_io() {
    let mut project = fixture();
    let pending = AssetId::from_digest([9; 32]);
    let mut authored = track(10, &[1]);
    authored.frame_cells.as_mut().unwrap()[0].marks[0].content = OverlayContent::Raster {
        asset_id: pending,
        position: PhysicalPoint::default(),
        size: PhysicalSize::new(1, 1).unwrap(),
        opacity: 255,
    };
    let commands = author_frame_owned_track(&project, authored).unwrap();
    let before = project.clone();
    assert!(
        project
            .apply_command(&EditCommand::Compound {
                commands: commands.clone()
            })
            .is_err()
    );
    assert_eq!(project, before);
    let mut complete = vec![EditCommand::RegisterAsset {
        asset: AssetDescriptor {
            id: pending,
            byte_len: 4,
            kind: AssetKind::Frame {
                size: PhysicalSize::new(1, 1).unwrap(),
                encoding: RasterEncoding::Rgba8,
            },
        },
    }];
    complete.extend(commands);
    project
        .apply_command(&EditCommand::Compound { commands: complete })
        .unwrap();
    assert_eq!(project.schema_version, 5);
    assert!(project.assets.contains_key(&pending));
}

#[test]
fn stage_allocation_reuses_bounded_gaps_and_rejects_overflow_without_partial_edits() {
    let mut project = fixture();
    let steps = &mut project.timeline.frames[0].render_steps;
    steps.push(FrameRenderStep::composite(u32::MAX));
    steps.extend(std::iter::repeat_n(
        FrameRenderStep::FlipHorizontal,
        MAX_FRAME_RENDER_STEPS - 2,
    ));
    apply(&mut project, track(10, &[1]));
    assert_eq!(
        project.timeline.frames[0].render_steps.len(),
        MAX_FRAME_RENDER_STEPS
    );
    assert_eq!(stage(&project.timeline.overlay_tracks[0], 1), Some(1));
    let before = project.clone();
    assert!(author_frame_owned_track(&project, track(11, &[1])).is_err());
    assert_eq!(project, before);
}

#[test]
fn clone_budget_ignores_unrelated_tracks_but_bounds_tracks_that_must_be_sealed() {
    let mut project = fixture();
    let mut large = track(10, &[2]);
    large.visible = false;
    large.frame_cells.as_mut().unwrap()[0].marks[0].content = OverlayContent::KeyStroke {
        text: "a".repeat(MAX_FRAME_BUNDLE_METADATA_BYTES + 1),
        position: PhysicalPoint::default(),
        raster: None,
    };
    project.timeline.overlay_tracks.push(large);
    let commands = author_frame_owned_track(&project, track(11, &[1])).unwrap();
    assert_eq!(commands.len(), 2);
    project.timeline.overlay_tracks[0]
        .frame_cells
        .as_mut()
        .unwrap()[0]
        .frame_id = FrameId::from_u128(1);
    assert!(matches!(
        author_frame_owned_track(&project, track(11, &[1])),
        Err(EditorError::RenderPipelineMetadataLimit)
    ));
    let mut fresh = track(11, &[3]);
    fresh.frame_cells.as_mut().unwrap()[0].marks[0].content = OverlayContent::KeyStroke {
        text: "b".repeat(MAX_FRAME_BUNDLE_METADATA_BYTES + 1),
        position: PhysicalPoint::default(),
        raster: None,
    };
    assert!(matches!(
        author_frame_owned_track(&project, fresh),
        Err(EditorError::RenderPipelineMetadataLimit)
    ));
}
