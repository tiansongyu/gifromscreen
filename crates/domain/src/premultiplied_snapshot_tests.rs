use super::*;
use crate::{
    AnnotationMode, AssetId, BlendMode, CaptureBinding, CaptureReplayBlock, DomainError,
    EditCommand, FrameGeometryPlan, FrameId, FrameOverlayCell, FrameOverlayMark, FrameRenderStep,
    HorizontalAlignment, KeyStroke, OverlayContent, OverlayId, OverlayItem, OverlayTrack,
    PhysicalPoint, PhysicalPx, ProjectManifest, RasterEncoding, Rgba, TextRaster, TimeUs,
    TimelineSpan, TrackId, ValidationIssue,
    model::test_fixtures::{asset, frame, manifest},
    recorded_annotation_barrier_at_stage, recorded_annotation_block_at_stage,
    validate_raw_rgba_view,
};

fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}

fn snapshot() -> AssetDescriptor {
    AssetDescriptor {
        id: AssetId::from_digest([9; 32]),
        byte_len: 41,
        kind: AssetKind::PremultipliedSnapshot {
            size: size(2, 3),
            format_version: 1,
        },
    }
}

fn project() -> ProjectManifest {
    let mut project = manifest();
    project.canvas.size = size(2, 3);
    let source = AssetDescriptor {
        id: asset(1).id,
        byte_len: 24,
        kind: AssetKind::Frame {
            size: size(2, 3),
            encoding: RasterEncoding::Rgba8,
        },
    };
    project.timeline.frames.push(frame(1, source.id));
    project.assets.insert(source.id, source);
    let snapshot = snapshot();
    project.assets.insert(snapshot.id, snapshot);
    project
}

fn step() -> FrameRenderStep {
    FrameRenderStep::CinemagraphOverlay {
        snapshot_asset: snapshot().id,
        snapshot_size: size(2, 3),
    }
}

#[test]
fn descriptor_is_typed_versioned_and_bounded_by_exact_container_length() {
    let descriptor = snapshot();
    assert_eq!(PREMULTIPLIED_SNAPSHOT_HEADER_LEN, 7 + 2 + 4 + 4);
    assert_eq!(PREMULTIPLIED_SNAPSHOT_HEADER_LEN % 4, 1);
    assert_eq!(PREMULTIPLIED_SNAPSHOT_FORMAT_VERSION, 1);
    assert_eq!(descriptor.kind.raster_descriptor(), None);
    assert_eq!(descriptor.required_schema_version(), 7);
    validate_premultiplied_snapshot_descriptor(&descriptor).unwrap();
    validate_premultiplied_snapshot_view(&descriptor, size(2, 3)).unwrap();
    assert!(validate_premultiplied_snapshot_view(&descriptor, size(3, 2)).is_err());
    assert!(validate_raw_rgba_view(&descriptor, size(2, 3)).is_err());
    for version in [0, 2, u16::MAX] {
        assert!(
            validate_premultiplied_snapshot_descriptor(&AssetDescriptor {
                kind: AssetKind::PremultipliedSnapshot {
                    size: size(2, 3),
                    format_version: version
                },
                ..descriptor.clone()
            })
            .is_err()
        );
    }
    for length in [0, 24, 40, 42, u64::MAX] {
        assert!(
            validate_premultiplied_snapshot_descriptor(&AssetDescriptor {
                byte_len: length,
                ..descriptor.clone()
            })
            .is_err()
        );
    }
    for dimensions in [
        PhysicalSize {
            width: PhysicalPx::new(0),
            height: PhysicalPx::new(3),
        },
        size(u32::MAX, u32::MAX),
    ] {
        assert!(
            validate_premultiplied_snapshot_descriptor(&AssetDescriptor {
                kind: AssetKind::PremultipliedSnapshot {
                    size: dimensions,
                    format_version: 1
                },
                ..descriptor.clone()
            })
            .is_err()
        );
    }
    let raw = AssetDescriptor {
        byte_len: 24,
        kind: AssetKind::Frame {
            size: size(2, 3),
            encoding: RasterEncoding::Rgba8,
        },
        ..descriptor
    };
    assert!(validate_premultiplied_snapshot_descriptor(&raw).is_err());
    assert_eq!(raw.required_schema_version(), 1);
}

#[test]
fn unreferenced_snapshots_require_schema_seven_and_legacy_asset_wire_stays_unchanged() {
    let mut project = project();
    for version in 1..=6 {
        project.schema_version = version;
        assert!(project.validate().is_err());
    }
    project.schema_version = 7;
    project.validate().unwrap();
    let bytes = serde_json::to_vec(&project).unwrap();
    assert_eq!(
        serde_json::from_slice::<ProjectManifest>(&bytes).unwrap(),
        project
    );
    let source = project.assets.get(&asset(1).id).unwrap();
    let wire = serde_json::to_string(source).unwrap();
    assert!(!wire.contains("format_version") && !wire.contains("snapshot"));
    assert_eq!(
        serde_json::to_string(&serde_json::from_str::<AssetDescriptor>(&wire).unwrap()).unwrap(),
        wire
    );
    project.assets.remove(&snapshot().id);
    project.schema_version = 1;
    project.validate().unwrap();
}

#[test]
fn source_frames_and_recorded_cursor_assets_reject_premultiplied_snapshots() {
    let mut project = project();
    project.timeline.frames[0].asset_id = snapshot().id;
    let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
        panic!("invalid source expected")
    };
    assert!(issues.contains(&ValidationIssue::IncompatibleFrameAsset {
        frame_id: FrameId::from_u128(1),
        asset_id: snapshot().id
    }));
    project.timeline.frames[0].asset_id = asset(1).id;
    project.timeline.frames[0].capture_metadata.cursor_asset = Some(snapshot().id);
    let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
        panic!("invalid cursor expected")
    };
    assert!(issues.contains(&ValidationIssue::IncompatibleCursorAsset {
        frame_id: FrameId::from_u128(1),
        asset_id: snapshot().id
    }));
}

fn ordinary_contents() -> Vec<OverlayContent> {
    let raster = TextRaster {
        asset_id: snapshot().id,
        size: size(2, 3),
    };
    vec![
        OverlayContent::Raster {
            asset_id: snapshot().id,
            position: PhysicalPoint::default(),
            size: size(2, 3),
            opacity: 0,
        },
        OverlayContent::Text {
            text: "PM is not text".into(),
            position: PhysicalPoint::default(),
            max_width: None,
            font_family: "sans".into(),
            font_size_px: 12,
            foreground: Rgba::TRANSPARENT,
            background: None,
            alignment: HorizontalAlignment::Start,
            raster: Some(raster.clone()),
        },
        OverlayContent::KeyStroke {
            text: "PM is not a key raster".into(),
            position: PhysicalPoint::default(),
            raster: Some(raster),
        },
        OverlayContent::Cursor {
            cursor_asset: Some(snapshot().id),
            position: PhysicalPoint::default(),
            hotspot: PhysicalPoint::default(),
        },
    ]
}

#[test]
fn all_hidden_ordinary_overlay_references_reject_pm_without_tightening_old_formats() {
    let mut project = project();
    for content in ordinary_contents() {
        for owned in [false, true] {
            let mut track = OverlayTrack {
                id: TrackId::from_u128(1),
                name: "Hidden ordinary raster".into(),
                visible: false,
                opacity: 0,
                blend_mode: BlendMode::Normal,
                annotation: None,
                annotation_scope: None,
                items: Vec::new(),
                frame_cells: None,
            };
            if owned {
                track.frame_cells = Some(vec![FrameOverlayCell::whole(
                    FrameId::from_u128(1),
                    1,
                    vec![FrameOverlayMark {
                        id: OverlayId::from_u128(1),
                        z_index: 0,
                        content: content.clone(),
                    }],
                )]);
            } else {
                track.items.push(OverlayItem {
                    id: OverlayId::from_u128(1),
                    z_index: 0,
                    span: TimelineSpan {
                        start: TimeUs::ZERO,
                        duration: project.timeline.frames[0].duration,
                    },
                    content: content.clone(),
                });
            }
            project.timeline.overlay_tracks = vec![track];
            let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
                panic!("invalid overlay expected")
            };
            assert!(issues.iter().any(|issue| matches!(issue, ValidationIssue::InvalidFrameOverlay { reason, .. } if reason.contains("cannot consume premultiplied"))));
        }
    }
    project.assets.get_mut(&snapshot().id).unwrap().kind = AssetKind::OverlayImage {
        size: size(2, 3),
        encoding: RasterEncoding::Png,
    };
    // This old metadata-only domain acceptance was never a promise that the
    // current raw-RGBA renderer can decode PNG. The new guard does not alter it.
    project.validate().unwrap();
}

#[test]
fn cinematograph_step_checks_exact_input_and_typed_snapshot_shape_without_scaling() {
    let mut project = project();
    project.timeline.frames[0].render_steps = vec![
        FrameRenderStep::composite(1),
        step(),
        FrameRenderStep::Resize { size: size(3, 2) },
    ];
    project.validate().unwrap();
    let plan = FrameGeometryPlan::new(&project.timeline.frames[0], size(2, 3)).unwrap();
    assert_eq!(plan.step_input_size(1).unwrap(), size(2, 3));
    assert_eq!(plan.output_size(), size(3, 2));
    assert_eq!(step().required_schema_version(), 7);
    assert_eq!(step().referenced_asset(), Some(snapshot().id));
    project.timeline.frames[0].render_steps[1] = FrameRenderStep::CinemagraphOverlay {
        snapshot_asset: snapshot().id,
        snapshot_size: size(3, 2),
    };
    assert!(
        FrameGeometryPlan::new(&project.timeline.frames[0], size(2, 3))
            .unwrap_err()
            .contains("step 2")
    );
    project.timeline.frames[0]
        .render_steps
        .insert(1, FrameRenderStep::Resize { size: size(3, 2) });
    assert!(
        project.validate().is_err(),
        "equal area is not a typed PM shape alias"
    );
    project.timeline.frames[0].render_steps = vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::CinemagraphOverlay {
            snapshot_asset: asset(1).id,
            snapshot_size: size(2, 3),
        },
    ];
    assert!(
        project.validate().is_err(),
        "ordinary RGBA is not a PM snapshot"
    );
}

#[test]
fn registration_and_undo_are_versioned_and_invalid_registration_is_atomic() {
    let mut project = project();
    project.assets.remove(&snapshot().id);
    project.schema_version = 6;
    let before = project.clone();
    let command = EditCommand::Compound {
        commands: vec![EditCommand::RegisterAsset { asset: snapshot() }],
    };
    assert_eq!(command.required_schema_version(), 7);
    let inverse = project.apply_command(&command).unwrap().inverse;
    assert_eq!(project.schema_version, 7);
    let redo = project.apply_command(&inverse).unwrap().inverse;
    assert_eq!(redo.required_schema_version(), 7);
    assert_eq!(project.assets, before.assets);
    assert_eq!(project.timeline, before.timeline);
    let mut invalid = snapshot();
    invalid.byte_len -= 1;
    let before = project.clone();
    assert!(
        project
            .apply_command(&EditCommand::RegisterAsset { asset: invalid })
            .is_err()
    );
    assert_eq!(project, before);
}

#[test]
fn live_snapshot_references_block_unregister_and_create_only_stage_local_input_barriers() {
    let mut project = project();
    let clip = &mut project.timeline.frames[0];
    clip.capture_metadata.key_strokes.push(KeyStroke {
        physical_key: "C".into(),
        display_text: Some("C".into()),
        pressed: true,
        at: TimeUs::ZERO,
        repeat: false,
        modifiers: 0,
    });
    clip.render_steps = vec![
        FrameRenderStep::composite(1),
        step(),
        FrameRenderStep::composite(2),
    ];
    let raw = clip.capture_metadata.clone();
    assert!(!recorded_annotation_barrier_at_stage(
        clip,
        &AnnotationMode::RecordedKeys,
        Some(1)
    ));
    assert_eq!(
        recorded_annotation_block_at_stage(clip, &AnnotationMode::RecordedKeys, None),
        Some(CaptureReplayBlock::MixedImageStage)
    );
    clip.capture_binding = CaptureBinding::ArchivedAfterComposite;
    assert_eq!(
        recorded_annotation_block_at_stage(clip, &AnnotationMode::RecordedKeys, Some(1)),
        Some(CaptureReplayBlock::ArchivedAfterComposite)
    );
    assert_eq!(clip.capture_metadata, raw);
    assert_eq!(
        clip.referenced_effect_assets().collect::<Vec<_>>(),
        [snapshot().id]
    );
    project.validate().unwrap();
    let before = project.clone();
    assert!(matches!(
        project.apply_command(&EditCommand::UnregisterAsset {
            asset_id: snapshot().id
        }),
        Err(DomainError::AssetStillReferenced(_))
    ));
    assert_eq!(project, before);
}
