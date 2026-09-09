use super::*;
use crate::{
    BlendMode, DomainError, DurationUs, EditCommand, FrameDurationChange, FrameId,
    FrameOverlayCell, FrameOverlayMark, FrameRenderStep, OverlayContent, OverlayId, OverlayItem,
    OverlayTrack, PhysicalRect, ProjectManifest, ShapeKind, TimeUs, TimelineSpan, TrackId,
    ValidationIssue,
    model::test_fixtures::{asset, frame, manifest},
};

pub(super) fn project() -> ProjectManifest {
    let mut project = manifest();
    // These fixtures exercise the original vector schema, not the newest writer.
    project.schema_version = 8;
    let asset = asset(1);
    project.timeline.frames = vec![frame(1, asset.id), frame(2, asset.id)];
    project.assets.insert(asset.id, asset);
    project
}

pub(super) fn track(number: u128, owned: bool, shape: VectorShape) -> OverlayTrack {
    let content = OverlayContent::VectorShape { shape };
    let mark = OverlayId::from_u128(number + 1_000);
    OverlayTrack {
        id: TrackId::from_u128(number),
        name: "Vector 用户 {name}".into(),
        visible: false,
        opacity: 0,
        blend_mode: BlendMode::Normal,
        annotation: None,
        annotation_scope: None,
        frame_cells: owned.then(|| {
            vec![FrameOverlayCell::whole(
                FrameId::from_u128(1),
                1,
                vec![FrameOverlayMark {
                    id: mark,
                    z_index: 3,
                    content: content.clone(),
                }],
            )]
        }),
        items: if owned {
            Vec::new()
        } else {
            vec![OverlayItem {
                id: mark,
                span: TimelineSpan {
                    start: TimeUs::ZERO,
                    duration: DurationUs::new(100_000).unwrap(),
                },
                z_index: 3,
                content,
            }]
        },
    }
}

#[test]
fn invisible_styles_and_shared_radius_are_valid_for_all_four_new_shapes() {
    let default = VectorShape::default();
    assert_eq!(default.version, 1);
    assert_eq!(default.stroke_width_hundredths, 400);
    assert_eq!(default.fill, Some(Rgba::TRANSPARENT));
    for kind in [
        VectorShapeKind::Rectangle,
        VectorShapeKind::Ellipse,
        VectorShapeKind::Triangle,
        VectorShapeKind::BlockArrow,
    ] {
        for fill in [None, Some(Rgba::TRANSPARENT)] {
            let shape = VectorShape {
                kind,
                stroke_width_hundredths: 0,
                stroke: Rgba::TRANSPARENT,
                fill,
                corner_radius_hundredths: MAX_VECTOR_SHAPE_STYLE_HUNDREDTHS,
                rotation_hundredths: VECTOR_SHAPE_TURN_HUNDREDTHS - 1,
                ..default
            };
            shape.validate().unwrap();
            let mut project = project();
            project.timeline.overlay_tracks.push(track(1, true, shape));
            project.validate().unwrap();
        }
    }
}

#[test]
fn bounds_cover_maximum_gif_axes_and_signed_clipped_overscan() {
    let max_gif = 65_535 * 100;
    for bounds in [
        VectorShapeBounds {
            x_hundredths: 0,
            y_hundredths: 0,
            width_hundredths: max_gif,
            height_hundredths: max_gif,
        },
        VectorShapeBounds {
            x_hundredths: -6_553_500,
            y_hundredths: -6_553_500,
            width_hundredths: max_gif * 2,
            height_hundredths: max_gif * 2,
        },
        VectorShapeBounds {
            x_hundredths: -MAX_VECTOR_SHAPE_COORDINATE_HUNDREDTHS,
            y_hundredths: -1,
            width_hundredths: 1,
            height_hundredths: 1,
        },
        VectorShapeBounds {
            x_hundredths: MAX_VECTOR_SHAPE_COORDINATE_HUNDREDTHS - 1,
            y_hundredths: 0,
            width_hundredths: 1,
            height_hundredths: 1,
        },
    ] {
        bounds.validate().unwrap();
        assert!(bounds.end_x_hundredths().is_some());
        assert!(bounds.end_y_hundredths().is_some());
    }
    assert_eq!(MAX_VECTOR_SHAPE_EXTENT_HUNDREDTHS, max_gif * 2);
}

#[test]
fn all_extreme_integer_bounds_fail_without_overflow_or_wrapping() {
    let normal = VectorShapeBounds::default();
    for bounds in [
        VectorShapeBounds {
            width_hundredths: 0,
            ..normal
        },
        VectorShapeBounds {
            height_hundredths: 0,
            ..normal
        },
        VectorShapeBounds {
            x_hundredths: i64::MIN,
            ..normal
        },
        VectorShapeBounds {
            y_hundredths: i64::MAX,
            ..normal
        },
        VectorShapeBounds {
            width_hundredths: u64::MAX,
            ..normal
        },
        VectorShapeBounds {
            height_hundredths: u64::MAX,
            ..normal
        },
        VectorShapeBounds {
            width_hundredths: MAX_VECTOR_SHAPE_EXTENT_HUNDREDTHS + 1,
            ..normal
        },
        VectorShapeBounds {
            x_hundredths: MAX_VECTOR_SHAPE_COORDINATE_HUNDREDTHS,
            width_hundredths: 1,
            ..normal
        },
        VectorShapeBounds {
            y_hundredths: -MAX_VECTOR_SHAPE_COORDINATE_HUNDREDTHS - 1,
            ..normal
        },
    ] {
        assert!(bounds.validate().is_err(), "{bounds:?}");
    }
    assert_eq!(
        VectorShapeBounds {
            x_hundredths: i64::MAX,
            width_hundredths: 1,
            ..normal
        }
        .end_x_hundredths(),
        None
    );
    assert_eq!(
        VectorShapeBounds {
            height_hundredths: u64::MAX,
            ..normal
        }
        .end_y_hundredths(),
        None
    );
}

#[test]
fn version_style_and_canonical_rotation_are_explicitly_bounded() {
    let original = VectorShape::default();
    for invalid in [
        VectorShape {
            version: 0,
            ..original
        },
        VectorShape {
            version: 3,
            ..original
        },
        VectorShape {
            version: u8::MAX,
            ..original
        },
        VectorShape {
            stroke_width_hundredths: 10_001,
            ..original
        },
        VectorShape {
            stroke_width_hundredths: u32::MAX,
            ..original
        },
        VectorShape {
            corner_radius_hundredths: 10_001,
            ..original
        },
        VectorShape {
            corner_radius_hundredths: u32::MAX,
            ..original
        },
        VectorShape {
            rotation_hundredths: 36_000,
            ..original
        },
        VectorShape {
            rotation_hundredths: u16::MAX,
            ..original
        },
    ] {
        assert!(invalid.validate().is_err());
    }
    VectorShape {
        stroke_width_hundredths: 10_000,
        corner_radius_hundredths: 10_000,
        rotation_hundredths: 35_999,
        ..original
    }
    .validate()
    .unwrap();
}

#[test]
fn new_payload_rejects_unknown_fields_and_non_integer_geometry() {
    let original = serde_json::to_value(VectorShape::default()).unwrap();
    for path in [vec![], vec!["bounds"], vec!["stroke"], vec!["fill"]] {
        let mut invalid = original.clone();
        let mut target = &mut invalid;
        for key in path {
            target = target.get_mut(key).unwrap();
        }
        target
            .as_object_mut()
            .unwrap()
            .insert("unknown_geometry".into(), serde_json::json!(1));
        assert!(serde_json::from_value::<VectorShape>(invalid).is_err());
    }
    for bad in [
        serde_json::json!(1.5),
        serde_json::json!("1"),
        serde_json::Value::Null,
    ] {
        let mut invalid = original.clone();
        invalid["bounds"]["x_hundredths"] = bad;
        assert!(serde_json::from_value::<VectorShape>(invalid).is_err());
    }
    let mut invalid = original.clone();
    invalid.as_object_mut().unwrap().remove("version");
    assert!(serde_json::from_value::<VectorShape>(invalid).is_err());
    let mut invalid = original;
    invalid["kind"] = serde_json::json!("arrow");
    assert!(serde_json::from_value::<VectorShape>(invalid).is_err());
    let mut unsupported = serde_json::to_value(VectorShape::default()).unwrap();
    unsupported["version"] = serde_json::json!(3);
    let unsupported: VectorShape = serde_json::from_value(unsupported).unwrap();
    assert!(unsupported.validate().is_err());
}

#[test]
fn old_shape_wire_and_legacy_envelope_lenience_are_unchanged() {
    for (kind, name) in [
        (ShapeKind::Line, "line"),
        (ShapeKind::Arrow, "arrow"),
        (ShapeKind::Rectangle, "rectangle"),
        (ShapeKind::Ellipse, "ellipse"),
    ] {
        let content = OverlayContent::Shape {
            kind,
            bounds: PhysicalRect::new(2, 3, 40, 20).unwrap(),
            stroke_width: 2,
            stroke: Rgba {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 4,
            },
            fill: None,
        };
        let expected = format!(
            "{{\"type\":\"shape\",\"kind\":\"{name}\",\"bounds\":{{\"origin\":{{\"x\":2,\"y\":3}},\"size\":{{\"width\":40,\"height\":20}}}},\"stroke_width\":2,\"stroke\":{{\"red\":1,\"green\":2,\"blue\":3,\"alpha\":4}},\"fill\":null}}"
        );
        assert_eq!(serde_json::to_string(&content).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<OverlayContent>(&expected).unwrap(),
            content
        );
        assert_eq!(content.required_schema_version(), 1);
        let mut filled = content.clone();
        let OverlayContent::Shape { fill, .. } = &mut filled else {
            unreachable!()
        };
        *fill = Some(Rgba::TRANSPARENT);
        let filled_wire = expected.replace(
            "\"fill\":null",
            "\"fill\":{\"red\":0,\"green\":0,\"blue\":0,\"alpha\":0}",
        );
        assert_eq!(serde_json::to_string(&filled).unwrap(), filled_wire);
        assert_eq!(
            serde_json::from_str::<OverlayContent>(&filled_wire).unwrap(),
            filled
        );
        let mut legacy_extra = serde_json::to_value(&content).unwrap();
        legacy_extra["old_envelope_extra"] = serde_json::json!(true);
        legacy_extra["stroke"]["old_color_extra"] = serde_json::json!(true);
        assert_eq!(
            serde_json::from_value::<OverlayContent>(legacy_extra).unwrap(),
            content
        );
    }
    let content = OverlayContent::VectorShape {
        shape: VectorShape::default(),
    };
    let mut envelope = serde_json::to_value(&content).unwrap();
    assert_eq!(envelope["type"], "vector_shape");
    envelope["unknown_legacy_envelope_key"] = serde_json::json!(true);
    assert_eq!(
        serde_json::from_value::<OverlayContent>(envelope).unwrap(),
        content
    );
    assert_eq!(content.referenced_asset(), None);
}

#[test]
fn timed_owned_and_hidden_vector_marks_require_schema_eight() {
    for owned in [false, true] {
        let track = track(10, owned, VectorShape::default());
        assert_eq!(track.required_schema_version(), 8);
        assert!(!track.visible && track.opacity == 0);
        let mut project = project();
        project.timeline.overlay_tracks.push(track);
        for schema in 1..8 {
            project.schema_version = schema;
            let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
                panic!("manifest error expected")
            };
            assert!(issues.iter().any(|issue| matches!(issue, ValidationIssue::InvalidVectorShape { track_id, overlay_id, reason }
                if *track_id == TrackId::from_u128(10) && *overlay_id == OverlayId::from_u128(1010) && reason.contains("schema 8"))));
        }
        project.schema_version = 8;
        project.validate().unwrap();
        assert_eq!(
            serde_json::from_slice::<ProjectManifest>(&serde_json::to_vec(&project).unwrap())
                .unwrap(),
            project
        );
    }
}

#[test]
fn anchored_and_mixed_track_scans_include_every_new_content() {
    let mut project = project();
    project.timeline.frames[0]
        .render_steps
        .push(FrameRenderStep::composite(1));
    let mut owned = track(11, true, VectorShape::default());
    owned.frame_cells.as_mut().unwrap()[0].stage = Some(1);
    assert_eq!(owned.required_schema_version(), 8);
    project.timeline.overlay_tracks.push(owned.clone());
    project
        .timeline
        .overlay_tracks
        .push(track(12, false, VectorShape::default()));
    project.validate().unwrap();
    let mut invalid_mix = owned;
    invalid_mix.items = track(13, false, VectorShape::default()).items;
    assert_eq!(invalid_mix.required_schema_version(), 8);
    project.timeline.overlay_tracks.push(invalid_mix);
    assert!(
        project.validate().is_err(),
        "mixed representations remain invalid"
    );
}

#[test]
fn validation_identifies_the_exact_track_and_mark_even_when_invisible() {
    for owned in [false, true] {
        let mut project = project();
        project.timeline.overlay_tracks.push(track(
            44,
            owned,
            VectorShape {
                version: 3,
                ..Default::default()
            },
        ));
        let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
            panic!("manifest error expected")
        };
        assert!(issues.iter().any(|issue| matches!(issue, ValidationIssue::InvalidVectorShape { track_id, overlay_id, reason }
            if *track_id == TrackId::from_u128(44) && *overlay_id == OverlayId::from_u128(1044) && reason.contains("version 3"))));
    }
}

#[test]
fn nested_commands_and_restore_payloads_scan_vector_content() {
    let owned = track(1, true, VectorShape::default());
    for command in [
        EditCommand::UpsertOverlayTrack {
            track: owned.clone(),
        },
        EditCommand::RestoreOverlayTrack {
            index: 0,
            track: owned.clone(),
        },
        EditCommand::RestoreFrameEdit {
            edit: Box::new(EditCommand::SetFrameDurations {
                changes: vec![FrameDurationChange {
                    frame_id: FrameId::from_u128(1),
                    duration: DurationUs::new(200_000).unwrap(),
                }],
            }),
            overlay_tracks: vec![owned.clone()],
        },
        EditCommand::Compound {
            commands: vec![EditCommand::Compound {
                commands: vec![EditCommand::UpsertOverlayTrack { track: owned }],
            }],
        },
    ] {
        assert_eq!(command.required_schema_version(), 8);
        let reopened: EditCommand =
            serde_json::from_slice(&serde_json::to_vec(&command).unwrap()).unwrap();
        assert_eq!(reopened, command);
        assert_eq!(reopened.required_schema_version(), 8);
    }
}

#[test]
fn schema_upgrade_is_sticky_through_inverse_and_invalid_compound_is_atomic() {
    for owned in [false, true] {
        let mut project = project();
        project.schema_version = 7;
        let before = project.clone();
        let track = track(5, owned, VectorShape::default());
        let applied = project
            .apply_command(&EditCommand::UpsertOverlayTrack {
                track: track.clone(),
            })
            .unwrap();
        assert_eq!(project.schema_version, 8);
        let undone = project.apply_command(&applied.inverse).unwrap();
        assert_eq!(project.schema_version, 8);
        assert_eq!(project.timeline, before.timeline);
        assert_eq!(project.assets, before.assets);
        assert_eq!(undone.inverse.required_schema_version(), 8);
        project.apply_command(&undone.inverse).unwrap();
        assert_eq!(project.timeline.overlay_tracks, vec![track]);

        let mut project = before;
        let bytes = serde_json::to_vec(&project).unwrap();
        let mut canvas = project.canvas.clone();
        canvas.size = crate::PhysicalSize::new(16, 16).unwrap();
        let bad = self::track(
            6,
            owned,
            VectorShape {
                stroke_width_hundredths: u32::MAX,
                ..Default::default()
            },
        );
        assert!(
            project
                .apply_command(&EditCommand::Compound {
                    commands: vec![
                        EditCommand::SetCanvas { canvas },
                        EditCommand::UpsertOverlayTrack { track: bad },
                    ]
                })
                .is_err()
        );
        assert_eq!(serde_json::to_vec(&project).unwrap(), bytes);
    }
}

#[test]
fn removing_a_frame_records_a_schema_eight_inverse_and_restores_vector_payload() {
    let mut project = project();
    project
        .timeline
        .overlay_tracks
        .push(track(7, true, VectorShape::default()));
    let before = project.timeline.clone();
    let removed = project
        .apply_command(&EditCommand::RemoveFrames {
            frame_ids: vec![FrameId::from_u128(1)],
        })
        .unwrap();
    assert_eq!(removed.inverse.required_schema_version(), 8);
    project.apply_command(&removed.inverse).unwrap();
    assert_eq!(project.schema_version, 8);
    assert_eq!(project.timeline, before);
}

fn vector_boundary() -> FrameRenderStep {
    FrameRenderStep::Composite {
        stage_id: 2,
        precision: crate::CompositePrecision::VectorCanvasPbgra8PngV1,
    }
}

#[test]
fn vector_canvas_precision_is_explicit_schema_eight_and_preserves_old_wire() {
    assert_eq!(vector_boundary().required_schema_version(), 8);
    assert_eq!(
        serde_json::to_string(&vector_boundary()).unwrap(),
        r#"{"type":"composite","stage_id":2,"precision":"vector_canvas_pbgra8_png_v1"}"#
    );
    assert_eq!(
        serde_json::to_string(&FrameRenderStep::composite(1)).unwrap(),
        r#"{"type":"composite","stage_id":1}"#
    );
    let mut project = project();
    project.timeline.frames[0].render_steps =
        vec![FrameRenderStep::composite(1), vector_boundary()];
    project.validate().unwrap(); // An unreferenced/cleared paint stage is valid.
    project.schema_version = 7;
    assert!(project.validate().is_err());
    assert!(crate::validate_frame_render_steps(&[vector_boundary()]).is_err());
}

#[test]
fn vector_canvas_rejects_non_normal_and_mixed_marks_even_when_hidden() {
    let mut project = project();
    project.timeline.frames[0].render_steps =
        vec![FrameRenderStep::composite(1), vector_boundary()];
    let mut vectors = track(71, true, VectorShape::default());
    vectors.frame_cells.as_mut().unwrap()[0].stage = Some(2);
    project.timeline.overlay_tracks.push(vectors);
    project.validate().unwrap();
    for blend in [BlendMode::Multiply, BlendMode::Screen] {
        let mut invalid = project.clone();
        invalid.timeline.overlay_tracks[0].blend_mode = blend;
        assert!(invalid.validate().is_err());
    }
    let old_content = OverlayContent::Shape {
        kind: ShapeKind::Rectangle,
        bounds: PhysicalRect::new(0, 0, 1, 1).unwrap(),
        stroke_width: 0,
        stroke: Rgba::TRANSPARENT,
        fill: None,
    };
    let mut invalid = project.clone();
    invalid.timeline.overlay_tracks[0]
        .frame_cells
        .as_mut()
        .unwrap()[0]
        .marks
        .push(FrameOverlayMark {
            id: OverlayId::from_u128(987),
            z_index: 0,
            content: old_content.clone(),
        });
    let DomainError::InvalidManifest(issues) = invalid.validate().unwrap_err() else {
        unreachable!()
    };
    assert!(issues.iter().any(|issue| matches!(issue, ValidationIssue::InvalidFrameOverlay { reason, .. } if reason.contains("only frame-owned VectorShape"))));
    // Old time-anchored shapes still belong to the unchanged first Legacy stage.
    let mut timed = track(72, false, VectorShape::default());
    timed.items[0].content = old_content;
    project.timeline.overlay_tracks.push(timed);
    project.validate().unwrap();
    project.timeline.overlay_tracks[0]
        .frame_cells
        .as_mut()
        .unwrap()[0]
        .marks
        .clear();
    project.validate().unwrap();
}

#[test]
fn vector_canvas_boundary_only_edit_upgrades_and_undo_never_downgrades() {
    let mut project = project();
    project.schema_version = 7;
    let original = project.timeline.frames[0].clone();
    let mut replacement = original.clone();
    replacement.render_steps = vec![FrameRenderStep::composite(1), vector_boundary()];
    let command = EditCommand::Compound {
        commands: vec![EditCommand::ReplaceFrame {
            frame_id: original.id,
            replacement: Box::new(replacement),
        }],
    };
    assert_eq!(command.required_schema_version(), 8);
    let edit = project.apply_command(&command).unwrap();
    assert_eq!(project.schema_version, 8);
    project.apply_command(&edit.inverse).unwrap();
    assert_eq!(project.timeline.frames[0], original);
    assert_eq!(project.schema_version, 8);
}
