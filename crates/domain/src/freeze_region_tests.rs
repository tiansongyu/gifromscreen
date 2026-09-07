use super::*;
use crate::{
    AssetDescriptor, AssetKind, CaptureClockContext, CaptureClockId, DomainError, EditCommand,
    FrameId, PhysicalPx, ProjectManifest, RasterEncoding, TimeUs, ValidationIssue,
    model::test_fixtures::{asset, frame, manifest},
};

fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}

fn baseline(number: u8) -> AssetDescriptor {
    AssetDescriptor {
        id: asset(number).id,
        byte_len: 32,
        kind: AssetKind::Frame {
            size: size(4, 2),
            encoding: RasterEncoding::Rgba8,
        },
    }
}

fn freeze(id: AssetId, view: PhysicalSize) -> FrameRenderStep {
    FrameRenderStep::FreezeRegion {
        baseline_asset: id,
        baseline_size: view,
        region: PhysicalRect::new(0, 0, 1, 1).unwrap(),
        invert: false,
    }
}

fn project() -> ProjectManifest {
    let mut project = manifest();
    project.canvas.size = size(2, 4);
    for number in [1, 2] {
        let descriptor = baseline(number);
        project.assets.insert(descriptor.id, descriptor);
    }
    let mut clip = frame(1, asset(1).id);
    clip.capture_metadata.captured_at = Some(TimeUs::new(777));
    clip.capture_clock = Some(CaptureClockContext {
        id: Some(CaptureClockId::from_u128(77)),
        sampled_at: TimeUs::new(777),
    });
    clip.render_steps = vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::Resize { size: size(2, 4) },
        freeze(asset(2).id, size(2, 4)),
        FrameRenderStep::composite(2),
    ];
    project.timeline.frames.push(clip);
    project
}

#[test]
fn freeze_uses_current_input_dimensions_and_later_geometry_keeps_its_usual_order() {
    let mut project = project();
    project.validate().unwrap();
    let clip = &mut project.timeline.frames[0];
    let plan = FrameGeometryPlan::new(clip, size(4, 2)).unwrap();
    assert_eq!(plan.base_size(), size(4, 2));
    assert_eq!(plan.step_input_size(2).unwrap(), size(2, 4));
    assert_eq!(plan.stage_size(Some(2)).unwrap(), size(2, 4));
    clip.render_steps.push(FrameRenderStep::Rotate {
        rotation: QuarterTurn::Clockwise90,
    });
    assert_eq!(
        FrameGeometryPlan::new(clip, size(4, 2))
            .unwrap()
            .output_size(),
        size(4, 2)
    );
    clip.render_steps[1] = FrameRenderStep::Resize { size: size(4, 2) };
    let error = FrameGeometryPlan::new(clip, size(4, 2)).unwrap_err();
    assert!(error.contains("step 3") && error.contains("without resizing"));
}

#[test]
fn freeze_region_requires_nonempty_fitting_motion_rectangle_for_both_invert_modes() {
    let mut project = project();
    for invert in [false, true] {
        for region in [
            PhysicalRect::new(2, 0, 1, 1).unwrap(),
            PhysicalRect {
                origin: Default::default(),
                size: PhysicalSize {
                    width: PhysicalPx::new(0),
                    height: PhysicalPx::new(1),
                },
            },
            PhysicalRect {
                origin: crate::PhysicalPoint {
                    x: PhysicalPx::new(u32::MAX),
                    y: PhysicalPx::new(0),
                },
                size: size(1, 1),
            },
        ] {
            project.timeline.frames[0].render_steps[2] = FrameRenderStep::FreezeRegion {
                baseline_asset: asset(2).id,
                baseline_size: size(2, 4),
                region,
                invert,
            };
            assert!(project.validate().is_err());
        }
        project.timeline.frames[0].render_steps[2] = FrameRenderStep::FreezeRegion {
            baseline_asset: asset(2).id,
            baseline_size: size(2, 4),
            region: PhysicalRect::new(0, 0, 2, 4).unwrap(),
            invert,
        };
        project.validate().unwrap();
    }
}

#[test]
fn raw_view_shape_aliases_preserve_storage_roles_but_reject_wrong_formats_and_lengths() {
    for kind in [
        AssetKind::Frame {
            size: size(4, 2),
            encoding: RasterEncoding::Rgba8,
        },
        AssetKind::OverlayImage {
            size: size(4, 2),
            encoding: RasterEncoding::Rgba8,
        },
        AssetKind::Mask {
            size: size(4, 2),
            encoding: RasterEncoding::Rgba8,
        },
    ] {
        let descriptor = AssetDescriptor {
            kind,
            ..baseline(2)
        };
        let before = descriptor.clone();
        validate_raw_rgba_view(&descriptor, size(2, 4)).unwrap();
        assert_eq!(descriptor, before);
        assert!(validate_raw_rgba_view(&descriptor, size(3, 3)).is_err());
    }
    for kind in [
        AssetKind::Frame {
            size: size(4, 2),
            encoding: RasterEncoding::Png,
        },
        AssetKind::Frame {
            size: size(4, 2),
            encoding: RasterEncoding::Qoi,
        },
        AssetKind::ImportedSource {
            media_type: "application/octet-stream".into(),
        },
        AssetKind::Frame {
            size: size(1, 1),
            encoding: RasterEncoding::Rgba8,
        },
    ] {
        assert!(
            validate_raw_rgba_view(
                &AssetDescriptor {
                    kind,
                    ..baseline(2)
                },
                size(2, 4)
            )
            .is_err()
        );
    }
    for length in [0, 31, 33, u64::MAX] {
        assert!(
            validate_raw_rgba_view(
                &AssetDescriptor {
                    byte_len: length,
                    ..baseline(2)
                },
                size(2, 4)
            )
            .is_err()
        );
    }
    assert!(validate_raw_rgba_view(&baseline(2), size(u32::MAX, u32::MAX)).is_err());
}

#[test]
fn missing_or_compressed_baselines_fail_manifest_validation_without_reinterpreting_pixels() {
    let mut project = project();
    project.assets.remove(&asset(2).id);
    let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
        panic!("expected invalid manifest")
    };
    assert!(issues.contains(&ValidationIssue::MissingEffectAsset {
        frame_id: FrameId::from_u128(1),
        asset_id: asset(2).id
    }));
    project.assets.insert(
        asset(2).id,
        AssetDescriptor {
            kind: AssetKind::Mask {
                size: size(4, 2),
                encoding: RasterEncoding::Png,
            },
            ..baseline(2)
        },
    );
    let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
        panic!("expected invalid manifest")
    };
    assert!(issues.iter().any(|issue| matches!(issue, ValidationIssue::InvalidFrameRenderSteps { reason, .. } if reason.contains("raw RGBA8"))));
}

#[test]
fn freeze_refs_protect_assets_and_schema_six_is_sticky_through_exact_visual_undo() {
    let mut project = project();
    for version in 1..=5 {
        project.schema_version = version;
        assert!(project.validate().is_err());
    }
    project.schema_version = 6;
    project.validate().unwrap();
    assert_eq!(
        project.timeline.frames[0]
            .referenced_effect_assets()
            .collect::<Vec<_>>(),
        [asset(2).id]
    );
    assert!(project.references_asset(asset(2).id));
    let before = project.clone();
    assert!(matches!(
        project.apply_command(&EditCommand::UnregisterAsset {
            asset_id: asset(2).id
        }),
        Err(DomainError::AssetStillReferenced(_))
    ));
    assert_eq!(project, before);
    let mut replacement = project.timeline.frames[0].clone();
    replacement.render_steps.remove(2);
    let inverse = project
        .apply_command(&EditCommand::ReplaceFrame {
            frame_id: replacement.id,
            replacement: Box::new(replacement),
        })
        .unwrap()
        .inverse;
    assert_eq!(inverse.required_schema_version(), 6);
    assert!(!project.references_asset(asset(2).id));
    project.apply_command(&inverse).unwrap();
    assert_eq!(project.schema_version, 6);
    assert_eq!(project.timeline, before.timeline);
    assert_eq!(project.assets, before.assets);
    let json = serde_json::to_vec(&project).unwrap();
    assert_eq!(
        serde_json::from_slice::<ProjectManifest>(&json).unwrap(),
        project
    );
}

#[test]
fn all_step_refs_are_enumerated_without_losing_legacy_unsupported_effect_assets() {
    let mut clip = project().timeline.frames.remove(0);
    clip.effects.push(Effect::Cinemagraph {
        mask_asset: asset(3).id,
        invert_mask: false,
    });
    clip.render_steps.push(FrameRenderStep::Effect {
        effect: Effect::Cinemagraph {
            mask_asset: asset(4).id,
            invert_mask: true,
        },
    });
    assert_eq!(
        clip.referenced_effect_assets().collect::<Vec<_>>(),
        [asset(3).id, asset(2).id, asset(4).id]
    );
    assert_eq!(clip.all_effects().count(), 2);
    assert_eq!(clip.required_schema_version(), 6);
}
