use super::*;
use crate::{EditorSession, copy_selected_frames, paste_frame_clipboard};
use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, BlendMode, Canvas, CanvasBackground, CaptureBinding,
    CaptureMetadata, ClipTransform, ColorSpace, DurationUs, Effect, FrameOverlayCell,
    FrameOverlayMark, OverlayContent, OverlayId, OverlayTrack, ProjectId, ProjectRevision,
    RasterEncoding, Rgba, ShapeKind, Timeline, TrackId, UnixTimeMs,
};

fn fixture() -> ProjectManifest {
    let size = PhysicalSize::new(8, 4).unwrap();
    let asset_id = AssetId::from_digest([1; 32]);
    ProjectManifest {
        schema_version: 2,
        project_id: ProjectId::from_u128(1),
        revision: ProjectRevision::ZERO,
        app_version: "test".into(),
        created_at: UnixTimeMs::new(0),
        canvas: Canvas {
            size,
            background: CanvasBackground::Transparent,
            color_space: ColorSpace::Srgb,
        },
        timeline: Timeline {
            frames: (1..=2)
                .map(|id| FrameClip {
                    id: FrameId::from_u128(id),
                    asset_id,
                    duration: DurationUs::new(10).unwrap(),
                    transform: ClipTransform::default(),
                    capture_metadata: CaptureMetadata::default(),
                    capture_binding: CaptureBinding::Original,
                    capture_clock: None,
                    effects: vec![],
                    render_steps: vec![],
                })
                .collect(),
            ..Timeline::default()
        },
        assets: BTreeMap::from([(
            asset_id,
            AssetDescriptor {
                id: asset_id,
                byte_len: 128,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        )]),
        export_presets: BTreeMap::new(),
        task_runs: vec![],
        source_provenance: vec![],
    }
}

fn owned(project: &mut ProjectManifest, track_id: u128, visible: bool, opacity: u8) {
    project.timeline.overlay_tracks.push(OverlayTrack {
        id: TrackId::from_u128(track_id),
        name: "artwork".into(),
        visible,
        opacity,
        blend_mode: BlendMode::Normal,
        items: vec![],
        annotation: None,
        annotation_scope: None,
        frame_cells: Some(
            project
                .timeline
                .frames
                .iter()
                .map(|frame| {
                    FrameOverlayCell::whole(
                        frame.id,
                        1,
                        vec![FrameOverlayMark {
                            id: OverlayId::from_u128(
                                track_id * 10 + u128::from_be_bytes(*frame.id.as_bytes()),
                            ),
                            z_index: 0,
                            content: OverlayContent::Shape {
                                kind: ShapeKind::Rectangle,
                                bounds: PhysicalRect::new(1, 1, 2, 2).unwrap(),
                                stroke_width: 1,
                                stroke: Rgba {
                                    red: 0,
                                    green: 0,
                                    blue: 0,
                                    alpha: 255,
                                },
                                fill: None,
                            },
                        }],
                    )
                })
                .collect(),
        ),
    });
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "test helper consumes a temporary edit specification"
)]
fn apply(project: &mut ProjectManifest, edit: ComposedFrameEdit) -> EditCommand {
    let command = edit_composed_frames(project, [FrameId::from_u128(1)], &edit).unwrap();
    project.apply_command(&command).unwrap().inverse
}

#[test]
fn resize_rotate_crop_change_all_frames_and_canvas_in_one_undo() {
    for edit in [
        ComposedFrameEdit::Resize(PhysicalSize::new(3, 2).unwrap()),
        ComposedFrameEdit::Rotate(QuarterTurn::Clockwise90),
        ComposedFrameEdit::Crop(PhysicalRect::new(2, 1, 4, 2).unwrap()),
    ] {
        let mut project = fixture();
        owned(&mut project, 20, true, 255);
        let original = project.clone();
        let inverse = apply(&mut project, edit);
        assert_eq!(project.schema_version, 3);
        for frame in &project.timeline.frames {
            assert!(!frame.render_steps.is_empty());
            assert_eq!(
                geometry(frame, original.canvas.size).unwrap().output_size(),
                project.canvas.size
            );
            assert_eq!(frame.transform, ClipTransform::default());
        }
        assert!(
            project.timeline.overlay_tracks[0]
                .frame_cells
                .as_ref()
                .unwrap()
                .iter()
                .all(|cell| cell.stage == Some(1))
        );
        project.apply_command(&inverse).unwrap();
        assert_eq!(project.timeline, original.timeline);
        assert_eq!(project.canvas, original.canvas);
        assert_eq!(project.schema_version, 3);
    }
}

#[test]
fn flip_and_effects_seal_only_selected_cells_including_hidden_empty_and_zero_opacity() {
    let mut project = fixture();
    owned(&mut project, 20, false, 255);
    owned(&mut project, 30, true, 0);
    owned(&mut project, 40, true, 255);
    project.timeline.overlay_tracks[2]
        .frame_cells
        .as_mut()
        .unwrap()[0]
        .marks
        .clear();
    apply(&mut project, ComposedFrameEdit::FlipHorizontal);
    assert!(project.timeline.frames[1].render_steps.is_empty());
    for track in &project.timeline.overlay_tracks {
        let cells = track.frame_cells.as_ref().unwrap();
        assert_eq!(cells[0].stage, Some(1));
        assert_eq!(cells[1].stage, None);
    }
    owned(&mut project, 50, true, 255);
    apply(
        &mut project,
        ComposedFrameEdit::Effect(FrameEffectEdit::Add(Effect::Darken {
            region: PhysicalRect::new(0, 0, 8, 4).unwrap(),
            amount_percent: 50,
        })),
    );
    assert_eq!(
        project.timeline.overlay_tracks[3]
            .frame_cells
            .as_ref()
            .unwrap()[0]
            .stage,
        Some(2)
    );
    assert_eq!(
        project.timeline.overlay_tracks[0]
            .frame_cells
            .as_ref()
            .unwrap()[0]
            .stage,
        Some(1)
    );
    assert_eq!(frame_effect_count(&project.timeline.frames[0]), 1);
    assert_eq!(frame_effect_count(&project.timeline.frames[1]), 0);
}

#[test]
fn imported_maximum_stage_identity_does_not_exhaust_available_ids() {
    let mut project = fixture();
    project.schema_version = 3;
    project.timeline.frames[0].render_steps = vec![
        FrameRenderStep::composite(u32::MAX),
        FrameRenderStep::FlipHorizontal,
    ];
    owned(&mut project, 20, true, 255);
    project.validate().unwrap();
    apply(&mut project, ComposedFrameEdit::FlipVertical);
    assert_eq!(
        project.timeline.overlay_tracks[0]
            .frame_cells
            .as_ref()
            .unwrap()[0]
            .stage,
        Some(1)
    );
    assert!(
        project.timeline.frames[0]
            .render_steps
            .contains(&FrameRenderStep::composite(u32::MAX))
    );
}

fn image_edit(effect: ComposedImageEffect) -> ComposedFrameEdit {
    ComposedFrameEdit::ImageEffect(ComposedEffectEdit::Add(effect))
}

fn shadow() -> gif_from_screen_domain::ImageShadowStyle {
    gif_from_screen_domain::ImageShadowStyle {
        blur_radius_hundredths: 400,
        depth_hundredths: 200,
        direction_hundredths: 0,
        ..gif_from_screen_domain::ImageShadowStyle::default()
    }
}

#[test]
fn inner_border_is_selected_but_outer_border_and_shadow_change_the_entire_canvas() {
    use gif_from_screen_domain::{ImageBorderStyle, SignedEdgeWidths};
    let inner = ImageBorderStyle {
        widths: SignedEdgeWidths {
            top_milli: 1000,
            ..SignedEdgeWidths::default()
        },
        ..ImageBorderStyle::default()
    };
    let outer = ImageBorderStyle {
        widths: SignedEdgeWidths {
            left_milli: -2000,
            bottom_milli: -1000,
            top_milli: 1000,
            right_milli: 0,
        },
        ..ImageBorderStyle::default()
    };
    for (effect, expected, all) in [
        (
            ComposedImageEffect::Border(inner),
            PhysicalSize::new(8, 4).unwrap(),
            false,
        ),
        (
            ComposedImageEffect::Border(outer),
            PhysicalSize::new(10, 5).unwrap(),
            true,
        ),
        (
            ComposedImageEffect::Shadow(shadow()),
            PhysicalSize::new(14, 8).unwrap(),
            true,
        ),
    ] {
        let mut project = fixture();
        owned(&mut project, 20, false, 0);
        let before = project.clone();
        let inverse = apply(&mut project, image_edit(effect));
        assert_eq!(project.schema_version, 4);
        assert_eq!(project.canvas.size, expected);
        assert_eq!(!project.timeline.frames[1].render_steps.is_empty(), all);
        let cells = project.timeline.overlay_tracks[0]
            .frame_cells
            .as_ref()
            .unwrap();
        assert_eq!(cells[0].stage, Some(1));
        assert_eq!(cells[1].stage, all.then_some(1));
        for (frame, original) in project.timeline.frames.iter().zip(&before.timeline.frames) {
            assert_eq!(frame.capture_metadata, original.capture_metadata);
            assert_eq!(frame.capture_binding, original.capture_binding);
        }
        project.apply_command(&inverse).unwrap();
        assert_eq!(project.timeline, before.timeline);
        assert_eq!(project.canvas, before.canvas);
        assert_eq!(project.schema_version, 4);
    }
}

#[test]
fn replacing_and_clearing_canvas_effects_keep_every_frame_size_consistent() {
    let mut project = fixture();
    owned(&mut project, 20, true, 255);
    apply(
        &mut project,
        image_edit(ComposedImageEffect::Shadow(shadow())),
    );
    apply(
        &mut project,
        ComposedFrameEdit::Rotate(QuarterTurn::Clockwise90),
    );
    let before = project.clone();
    apply(
        &mut project,
        ComposedFrameEdit::ImageEffect(ComposedEffectEdit::Replace {
            index: 0,
            effect: ComposedImageEffect::Shadow(gif_from_screen_domain::ImageShadowStyle {
                depth_hundredths: 0,
                ..shadow()
            }),
        }),
    );
    assert_eq!(project.canvas.size, PhysicalSize::new(8, 12).unwrap());
    assert!(
        project
            .timeline
            .frames
            .iter()
            .all(|frame| frame_effect_count(frame) == 1)
    );
    assert_eq!(
        project.timeline.overlay_tracks,
        before.timeline.overlay_tracks
    );
    let inverse = apply(
        &mut project,
        ComposedFrameEdit::Effect(FrameEffectEdit::Clear),
    );
    assert_eq!(project.canvas.size, PhysicalSize::new(4, 8).unwrap());
    assert!(
        project
            .timeline
            .frames
            .iter()
            .all(|frame| frame_effect_count(frame) == 0)
    );
    project.apply_command(&inverse).unwrap();
    assert_eq!(project.canvas.size, PhysicalSize::new(8, 12).unwrap());
    project.validate().unwrap();
}

#[test]
fn replacing_earlier_canvas_effect_cannot_strand_later_crop_or_effect_regions() {
    let mut project = fixture();
    apply(
        &mut project,
        image_edit(ComposedImageEffect::Shadow(shadow())),
    );
    apply(
        &mut project,
        ComposedFrameEdit::Crop(PhysicalRect::new(10, 0, 3, 2).unwrap()),
    );
    let before = project.clone();
    let edit = ComposedFrameEdit::ImageEffect(ComposedEffectEdit::Replace {
        index: 0,
        effect: ComposedImageEffect::Border(gif_from_screen_domain::ImageBorderStyle::default()),
    });
    assert!(edit_composed_frames(&project, [FrameId::from_u128(1)], &edit).is_err());
    assert_eq!(project, before);
}

#[test]
fn expanded_effect_limits_reject_before_command_and_prefix_replacement_is_explicit() {
    use gif_from_screen_domain::{ImageBorderStyle, SignedEdgeWidths};
    let mut project = fixture();
    for widths in [
        SignedEdgeWidths {
            left_milli: i32::MIN,
            ..SignedEdgeWidths::default()
        },
        SignedEdgeWidths {
            left_milli: -32_000_000,
            top_milli: -1_000_000,
            ..SignedEdgeWidths::default()
        },
    ] {
        let edit = image_edit(ComposedImageEffect::Border(ImageBorderStyle {
            widths,
            ..ImageBorderStyle::default()
        }));
        let error = edit_composed_frames(&project, [FrameId::from_u128(1)], &edit).unwrap_err();
        assert!(error.to_string().contains("64 MiB"));
        assert_eq!(project.schema_version, 2);
    }
    project.timeline.frames[0].effects.push(Effect::Darken {
        region: PhysicalRect::new(0, 0, 8, 4).unwrap(),
        amount_percent: 20,
    });
    let edit = ComposedFrameEdit::ImageEffect(ComposedEffectEdit::Replace {
        index: 0,
        effect: ComposedImageEffect::Shadow(shadow()),
    });
    assert!(
        edit_composed_frames(&project, [FrameId::from_u128(1)], &edit)
            .unwrap_err()
            .to_string()
            .contains("legacy prefix")
    );
    assert_eq!(project.schema_version, 2);
}

fn two_shadows(width: u32, height: u32) -> ProjectManifest {
    let mut project = fixture();
    project.canvas.size = PhysicalSize::new(width, height).unwrap();
    for frame in &mut project.timeline.frames {
        frame.transform.output_size = Some(project.canvas.size);
    }
    for _ in 0..2 {
        apply(
            &mut project,
            image_edit(ComposedImageEffect::Shadow(
                gif_from_screen_domain::ImageShadowStyle {
                    blur_radius_hundredths: 1000,
                    depth_hundredths: 0,
                    ..shadow()
                },
            )),
        );
    }
    project
}

fn replace_first_shadow(blur: u16) -> ComposedFrameEdit {
    ComposedFrameEdit::ImageEffect(ComposedEffectEdit::Replace {
        index: 0,
        effect: ComposedImageEffect::Shadow(gif_from_screen_domain::ImageShadowStyle {
            blur_radius_hundredths: blur,
            depth_hundredths: 0,
            ..shadow()
        }),
    })
}

#[test]
fn replacing_an_early_effect_checks_later_memory_even_when_a_final_crop_is_small() {
    let mut project = two_shadows(4000, 4000);
    for cropped in [false, true] {
        if cropped {
            apply(
                &mut project,
                ComposedFrameEdit::Crop(PhysicalRect::new(0, 0, 10, 10).unwrap()),
            );
        }
        let before = project.clone();
        let error = edit_composed_frames(
            &project,
            [FrameId::from_u128(1)],
            &replace_first_shadow(9000),
        )
        .unwrap_err();
        assert!(error.to_string().contains("64 MiB"));
        assert_eq!(project, before);
    }
}

#[test]
fn gif_dimensions_are_checked_at_the_final_output_without_rejecting_valid_intermediates() {
    let mut project = two_shadows(65_500, 1);
    assert!(
        edit_composed_frames(
            &project,
            [FrameId::from_u128(1)],
            &replace_first_shadow(3000)
        )
        .unwrap_err()
        .to_string()
        .contains("GIF")
    );
    apply(
        &mut project,
        ComposedFrameEdit::Crop(PhysicalRect::new(0, 0, 10, 10).unwrap()),
    );
    apply(&mut project, replace_first_shadow(3000));
    assert_eq!(project.canvas.size, PhysicalSize::new(10, 10).unwrap());
    let plan = geometry(
        &project.timeline.frames[0],
        PhysicalSize::new(8, 4).unwrap(),
    )
    .unwrap();
    assert_eq!(plan.step_input_size(3).unwrap().width.get(), 65_540);
}

#[test]
fn effect_validation_uses_its_stage_and_clear_keeps_composite_identities() {
    let mut project = fixture();
    apply(
        &mut project,
        ComposedFrameEdit::Resize(PhysicalSize::new(3, 2).unwrap()),
    );
    let bad = ComposedFrameEdit::Effect(FrameEffectEdit::Add(Effect::Blur {
        region: PhysicalRect::new(4, 0, 2, 2).unwrap(),
        radius: 1,
    }));
    assert!(edit_composed_frames(&project, [FrameId::from_u128(1)], &bad).is_err());
    owned(&mut project, 20, true, 255);
    let effect = Effect::Blur {
        region: PhysicalRect::new(0, 0, 3, 2).unwrap(),
        radius: 1,
    };
    apply(
        &mut project,
        ComposedFrameEdit::Effect(FrameEffectEdit::Add(effect.clone())),
    );
    apply(
        &mut project,
        ComposedFrameEdit::Resize(PhysicalSize::new(1, 1).unwrap()),
    );
    // An earlier effect's region may be larger than the final canvas.
    apply(
        &mut project,
        ComposedFrameEdit::Effect(FrameEffectEdit::Replace {
            index: 0,
            effect: Effect::Blur {
                region: PhysicalRect::new(0, 0, 3, 2).unwrap(),
                radius: 2,
            },
        }),
    );
    let anchor = project.timeline.overlay_tracks[0]
        .frame_cells
        .as_ref()
        .unwrap()[0]
        .stage;
    apply(
        &mut project,
        ComposedFrameEdit::Effect(FrameEffectEdit::Clear),
    );
    assert_eq!(
        project.timeline.overlay_tracks[0]
            .frame_cells
            .as_ref()
            .unwrap()[0]
            .stage,
        anchor
    );
    assert_eq!(frame_effect_count(&project.timeline.frames[0]), 0);
    project.validate().unwrap();
}

#[test]
fn removing_earlier_geometry_rejects_invalid_later_region_without_mutation() {
    let mut project = fixture();
    apply(
        &mut project,
        ComposedFrameEdit::Resize(PhysicalSize::new(16, 8).unwrap()),
    );
    apply(
        &mut project,
        ComposedFrameEdit::Crop(PhysicalRect::new(10, 0, 4, 4).unwrap()),
    );
    let before = project.clone();
    assert!(
        edit_composed_frames(
            &project,
            [FrameId::from_u128(1)],
            &ComposedFrameEdit::ClearResize
        )
        .is_err()
    );
    assert_eq!(project, before);
}

#[test]
fn malformed_legacy_effect_can_be_removed_or_replaced_to_repair_rendering() {
    let mut project = fixture();
    project.timeline.frames[0].transform.crop = Some(PhysicalRect::new(0, 0, 2, 2).unwrap());
    project.timeline.frames[0].effects.push(Effect::Blur {
        region: PhysicalRect::new(6, 0, 2, 2).unwrap(),
        radius: 1,
    });
    // Older domain validation accepted a canvas region outside the cropped frame.
    project.validate().unwrap();
    assert!(geometry(&project.timeline.frames[0], project.canvas.size).is_err());
    let mut repaired = project.clone();
    apply(
        &mut repaired,
        ComposedFrameEdit::Effect(FrameEffectEdit::Clear),
    );
    assert!(geometry(&repaired.timeline.frames[0], project.canvas.size).is_ok());
    apply(
        &mut project,
        ComposedFrameEdit::Effect(FrameEffectEdit::Replace {
            index: 0,
            effect: Effect::Blur {
                region: PhysicalRect::new(0, 0, 2, 2).unwrap(),
                radius: 1,
            },
        }),
    );
    assert!(geometry(&project.timeline.frames[0], project.canvas.size).is_ok());
}

#[test]
fn copied_cells_keep_frame_local_stage_ids_and_repeated_edits_undo_exactly() {
    let mut project = fixture();
    owned(&mut project, 20, true, 255);
    apply(
        &mut project,
        ComposedFrameEdit::Rotate(QuarterTurn::Clockwise90),
    );
    let clipboard = copy_selected_frames(&project, [FrameId::from_u128(1)]).unwrap();
    let mut id = 100_u128;
    let command = paste_frame_clipboard(&project, &clipboard, Some(FrameId::from_u128(2)), || {
        id += 1;
        FrameId::from_u128(id)
    })
    .unwrap();
    project.apply_command(&command).unwrap();
    project.validate().unwrap();
    let original = project.clone();
    let mut session = EditorSession::new(project, 8).unwrap();
    for edit in [
        ComposedFrameEdit::FlipHorizontal,
        ComposedFrameEdit::FlipVertical,
        ComposedFrameEdit::Resize(PhysicalSize::new(2, 6).unwrap()),
    ] {
        let command =
            edit_composed_frames(session.project(), [FrameId::from_u128(1)], &edit).unwrap();
        session.execute(&command).unwrap();
    }
    for _ in 0..3 {
        assert!(session.undo().unwrap());
    }
    assert_eq!(session.project().timeline, original.timeline);
    assert_eq!(session.project().canvas, original.canvas);
    for _ in 0..3 {
        assert!(session.redo().unwrap());
    }
    session.project().validate().unwrap();
}
