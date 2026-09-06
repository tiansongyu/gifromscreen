use super::*;
use crate::{
    BlendMode, ClipTransform, DomainError, EdgeWidths, EditCommand, FrameId, FrameOverlayCell,
    IndexedFrame, OverlayTrack, PhysicalPx, ProjectManifest, Rgba, TrackId, ValidationIssue,
    model::test_fixtures::{asset, frame, manifest},
};

fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}

fn clip() -> FrameClip {
    frame(1, asset(1).id)
}

fn composite(id: u32) -> FrameRenderStep {
    FrameRenderStep::Composite { stage_id: id }
}

fn staged_project() -> ProjectManifest {
    let mut project = manifest();
    let mut descriptor = asset(1);
    descriptor.kind = crate::AssetKind::Frame {
        size: size(100, 100),
        encoding: crate::RasterEncoding::Rgba8,
    };
    descriptor.byte_len = 40_000;
    project.assets.insert(descriptor.id, descriptor);
    project.canvas.size = size(100, 100);
    let mut first = clip();
    first.render_steps = vec![
        composite(7),
        FrameRenderStep::Resize { size: size(20, 20) },
        composite(9),
    ];
    let mut second = first.clone();
    second.id = FrameId::from_u128(2);
    second.render_steps = vec![composite(11)];
    project.timeline.frames = vec![first, second];
    project
}

fn anchored_track(owner: FrameId, stage: Option<u32>) -> OverlayTrack {
    let mut cell = FrameOverlayCell::whole(owner, 1, Vec::new());
    cell.stage = stage;
    OverlayTrack {
        id: TrackId::from_u128(1),
        frame_cells: Some(vec![cell]),
        annotation: None,
        annotation_scope: None,
        name: "Hidden empty anchored group".to_owned(),
        visible: false,
        opacity: 0,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
    }
}

#[test]
fn legacy_prefix_and_every_step_report_their_actual_input_and_paint_sizes() {
    let mut frame = clip();
    frame.transform = ClipTransform {
        crop: Some(PhysicalRect::new(5, 5, 70, 60).unwrap()),
        output_size: Some(size(30, 50)),
        rotation: QuarterTurn::Clockwise90,
        flip_horizontal: true,
        flip_vertical: true,
    };
    frame.effects = vec![Effect::Blur {
        region: PhysicalRect::new(0, 0, 3, 3).unwrap(),
        radius: 1,
    }];
    frame.render_steps = vec![
        composite(10),
        FrameRenderStep::Crop {
            rect: PhysicalRect::new(2, 3, 20, 10).unwrap(),
        },
        FrameRenderStep::Resize { size: size(30, 40) },
        FrameRenderStep::Rotate {
            rotation: QuarterTurn::Clockwise90,
        },
        FrameRenderStep::FlipHorizontal,
        FrameRenderStep::FlipVertical,
        FrameRenderStep::Effect {
            effect: Effect::Darken {
                region: PhysicalRect::new(0, 0, 40, 30).unwrap(),
                amount_percent: 0,
            },
        },
        composite(20),
        FrameRenderStep::Resize { size: size(80, 60) },
    ];
    let plan = FrameGeometryPlan::new(&frame, size(100, 90)).unwrap();
    assert_eq!(plan.base_size(), size(50, 30));
    assert_eq!(plan.output_size(), size(80, 60));
    assert_eq!(plan.stage_size(Some(10)).unwrap(), size(50, 30));
    assert_eq!(plan.stage_size(Some(20)).unwrap(), size(40, 30));
    assert_eq!(plan.stage_size(None).unwrap(), size(80, 60));
    let expected = [
        size(50, 30),
        size(50, 30),
        size(20, 10),
        size(30, 40),
        size(40, 30),
        size(40, 30),
        size(40, 30),
        size(40, 30),
        size(40, 30),
    ];
    for (index, input) in expected.into_iter().enumerate() {
        assert_eq!(plan.step_input_size(index).unwrap(), input);
    }
    assert!(plan.stage_size(Some(0)).is_err());
    assert!(plan.stage_size(Some(99)).is_err());
    assert!(plan.step_input_size(usize::MAX).is_err());
}

#[test]
fn crop_resize_and_effect_regions_use_current_step_size_and_report_one_based_positions() {
    for bad in [
        FrameRenderStep::Crop {
            rect: PhysicalRect::new(19, 0, 2, 1).unwrap(),
        },
        FrameRenderStep::Resize {
            size: PhysicalSize {
                width: PhysicalPx::ZERO,
                height: PhysicalPx::new(1),
            },
        },
        FrameRenderStep::Effect {
            effect: Effect::Blur {
                region: PhysicalRect::new(80, 80, 10, 10).unwrap(),
                radius: 1,
            },
        },
    ] {
        let mut frame = clip();
        frame.render_steps = vec![
            composite(1),
            FrameRenderStep::Resize { size: size(20, 20) },
            bad,
        ];
        let error = FrameGeometryPlan::new(&frame, size(100, 100)).unwrap_err();
        assert!(error.contains("Render step 3"), "{error}");
    }
    let mut frame = clip();
    frame.transform.crop = Some(PhysicalRect::new(99, 99, 2, 2).unwrap());
    assert!(
        FrameGeometryPlan::new(&frame, size(100, 100))
            .unwrap_err()
            .contains("Legacy crop")
    );
    assert!(
        FrameGeometryPlan::new(
            &clip(),
            PhysicalSize {
                width: PhysicalPx::ZERO,
                height: PhysicalPx::new(1)
            }
        )
        .is_err()
    );
}

#[test]
fn effect_validation_keeps_all_six_renderable_effects_and_legal_noops() {
    let region = PhysicalRect::new(0, 0, 2, 2).unwrap();
    let effects = [
        Effect::Blur {
            region,
            radius: MAX_FRAME_RENDER_EFFECT_RADIUS,
        },
        Effect::Pixelate {
            region,
            block_size: 1,
        },
        Effect::Darken {
            region,
            amount_percent: 0,
        },
        Effect::Lighten {
            region,
            amount_percent: 100,
        },
        Effect::Border {
            widths: EdgeWidths {
                left: u16::MAX,
                right: u16::MAX,
                ..EdgeWidths::default()
            },
            color: Rgba::TRANSPARENT,
        },
        Effect::Shadow {
            offset_x: i32::MIN,
            offset_y: i32::MAX,
            blur_radius: 0,
            color: Rgba::TRANSPARENT,
        },
    ];
    let mut frame = clip();
    frame.effects = effects.to_vec();
    frame.render_steps = vec![composite(1)];
    frame.render_steps.extend(
        effects
            .into_iter()
            .map(|effect| FrameRenderStep::Effect { effect }),
    );
    let plan = FrameGeometryPlan::new(&frame, size(2, 2)).unwrap();
    assert_eq!(plan.output_size(), size(2, 2));
    assert!(
        validate_render_effect(
            &Effect::Border {
                widths: EdgeWidths::default(),
                color: Rgba::TRANSPARENT
            },
            size(2, 2)
        )
        .is_ok()
    );
}

#[test]
fn invalid_effect_parameters_do_not_become_silent_noops() {
    let region = PhysicalRect::new(0, 0, 2, 2).unwrap();
    for effect in [
        Effect::Blur { region, radius: 0 },
        Effect::Blur {
            region,
            radius: MAX_FRAME_RENDER_EFFECT_RADIUS + 1,
        },
        Effect::Pixelate {
            region,
            block_size: 0,
        },
        Effect::Darken {
            region,
            amount_percent: 101,
        },
        Effect::Lighten {
            region,
            amount_percent: 255,
        },
        Effect::Shadow {
            offset_x: 0,
            offset_y: 0,
            blur_radius: MAX_FRAME_RENDER_EFFECT_RADIUS + 1,
            color: Rgba::TRANSPARENT,
        },
        Effect::Cinemagraph {
            mask_asset: asset(2).id,
            invert_mask: false,
        },
    ] {
        let mut frame = clip();
        frame.render_steps = vec![
            composite(1),
            FrameRenderStep::Effect {
                effect: effect.clone(),
            },
        ];
        assert!(
            FrameGeometryPlan::new(&frame, size(2, 2))
                .unwrap_err()
                .contains("Render step 2")
        );
        frame.render_steps.clear();
        frame.effects = vec![effect];
        assert!(
            FrameGeometryPlan::new(&frame, size(2, 2))
                .unwrap_err()
                .contains("Legacy effect 1")
        );
    }
}

#[test]
fn structure_is_bounded_and_composite_ids_are_unique_nonzero_and_frame_local() {
    for invalid in [
        vec![FrameRenderStep::FlipHorizontal],
        vec![composite(0)],
        vec![composite(1), FrameRenderStep::FlipVertical, composite(1)],
        vec![composite(1); MAX_FRAME_RENDER_STEPS + 1],
    ] {
        assert!(validate_frame_render_steps(&invalid).is_err());
    }
    let mut maximum = vec![FrameRenderStep::FlipVertical; MAX_FRAME_RENDER_STEPS];
    maximum[0] = composite(u32::MAX);
    validate_frame_render_steps(&maximum).unwrap();
    validate_frame_render_steps(&[]).unwrap();
    let mut project = staged_project();
    project.timeline.frames[1].render_steps = project.timeline.frames[0].render_steps.clone();
    project.validate().unwrap();
}

#[test]
fn hidden_and_mark_empty_cell_anchors_must_resolve_on_their_actual_owner() {
    for stage in [Some(7), Some(9), None] {
        let mut project = staged_project();
        project
            .timeline
            .overlay_tracks
            .push(anchored_track(FrameId::from_u128(1), stage));
        project.validate().unwrap();
    }
    for (owner, stage) in [(1, 0), (1, 99), (2, 7)] {
        let mut project = staged_project();
        project
            .timeline
            .overlay_tracks
            .push(anchored_track(FrameId::from_u128(owner), Some(stage)));
        let error = project.validate().unwrap_err();
        let DomainError::InvalidManifest(issues) = error else {
            panic!("expected invalid manifest")
        };
        assert!(issues.iter().any(|issue| matches!(issue, ValidationIssue::InvalidFrameOverlay { reason, .. } if reason.contains("does not exist"))));
    }
}

#[test]
fn legacy_manifest_acceptance_is_preserved_but_planning_uses_real_effect_input_size() {
    let mut project = staged_project();
    project.timeline.frames.truncate(1);
    let frame = &mut project.timeline.frames[0];
    frame.render_steps.clear();
    frame.transform.crop = Some(PhysicalRect::new(0, 0, 20, 20).unwrap());
    frame.effects = vec![Effect::Blur {
        region: PhysicalRect::new(80, 80, 10, 10).unwrap(),
        radius: 1,
    }];
    for version in [1, 2, 3] {
        project.schema_version = version;
        project.validate().unwrap();
    }
    assert!(FrameGeometryPlan::new(&project.timeline.frames[0], size(100, 100)).is_err());
    project.timeline.frames[0].render_steps = vec![composite(1)];
    assert!(project.validate().is_err());
    // A later resize/canvas change must not reject a valid legacy effect that
    // was executed on the larger prefix before that resize.
    project.timeline.frames[0].transform.crop = None;
    project.timeline.frames[0]
        .render_steps
        .push(FrameRenderStep::Resize { size: size(20, 20) });
    project.canvas.size = size(20, 20);
    project.validate().unwrap();
}

#[test]
fn empty_new_fields_preserve_legacy_wire_shape_and_stage_payloads_require_v3() {
    let frame = clip();
    let bytes = serde_json::to_string(&frame).unwrap();
    assert!(!bytes.contains("render_steps"));
    let read: FrameClip = serde_json::from_str(&bytes).unwrap();
    assert_eq!(serde_json::to_string(&read).unwrap(), bytes);
    let cell = FrameOverlayCell::whole(frame.id, 1, Vec::new());
    let bytes = serde_json::to_string(&cell).unwrap();
    assert!(!bytes.contains("stage"));
    assert_eq!(
        serde_json::to_string(&serde_json::from_str::<FrameOverlayCell>(&bytes).unwrap()).unwrap(),
        bytes
    );
    let mut project = staged_project();
    for version in [1, 2] {
        project.schema_version = version;
        assert!(project.validate().is_err());
    }
    let staged = project.timeline.frames[0].clone();
    let track = anchored_track(staged.id, Some(7));
    let commands = [
        EditCommand::InsertFrames {
            index: 0,
            frames: vec![staged.clone()],
        },
        EditCommand::RestoreFrames {
            frames: vec![IndexedFrame {
                index: 0,
                frame: staged.clone(),
            }],
        },
        EditCommand::ReplaceFrame {
            frame_id: staged.id,
            replacement: Box::new(staged.clone()),
        },
        EditCommand::UpsertOverlayTrack {
            track: track.clone(),
        },
        EditCommand::RestoreOverlayTrack {
            index: 0,
            track: track.clone(),
        },
        EditCommand::RestoreFrameEdit {
            edit: Box::new(EditCommand::RemoveFrames {
                frame_ids: vec![staged.id],
            }),
            overlay_tracks: vec![track],
        },
    ];
    for command in commands {
        assert_eq!(command.required_schema_version(), 3);
        assert_eq!(
            EditCommand::Compound {
                commands: vec![command]
            }
            .required_schema_version(),
            3
        );
    }
    assert_eq!(
        EditCommand::InsertFrames {
            index: 0,
            frames: vec![frame]
        }
        .required_schema_version(),
        1
    );
    assert_eq!(anchored_track(staged.id, None).required_schema_version(), 2);
}

#[test]
fn failed_stage_or_anchor_commands_roll_back_schema_and_all_other_state() {
    let mut project = staged_project();
    for frame in &mut project.timeline.frames {
        frame.render_steps.clear();
    }
    project.schema_version = 2;
    let before = project.clone();
    let mut replacement = project.timeline.frames[0].clone();
    replacement.render_steps = vec![composite(0)];
    assert!(
        project
            .apply_command(&EditCommand::ReplaceFrame {
                frame_id: replacement.id,
                replacement: Box::new(replacement),
            })
            .is_err()
    );
    assert_eq!(project, before);
    assert!(
        project
            .apply_command(&EditCommand::UpsertOverlayTrack {
                track: anchored_track(FrameId::from_u128(1), Some(7)),
            })
            .is_err()
    );
    assert_eq!(project, before);
}

#[test]
fn stage_effect_asset_references_are_never_lost_even_for_unsupported_persisted_effects() {
    let mut project = staged_project();
    let id = asset(2).id;
    project.timeline.frames[0]
        .render_steps
        .push(FrameRenderStep::Effect {
            effect: Effect::Cinemagraph {
                mask_asset: id,
                invert_mask: true,
            },
        });
    assert_eq!(
        project.timeline.frames[0]
            .referenced_effect_assets()
            .collect::<Vec<_>>(),
        [id]
    );
    assert!(project.references_asset(id));
    let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
        panic!("invalid effect expected")
    };
    assert!(issues.contains(&ValidationIssue::MissingEffectAsset {
        frame_id: FrameId::from_u128(1),
        asset_id: id
    }));
}
