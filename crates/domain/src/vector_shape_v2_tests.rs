use super::{
    tests::{project, track},
    *,
};
use crate::{
    BlendMode, CompositePrecision, DomainError, EditCommand, FrameId, FrameOverlayCell,
    FrameRenderStep, IndexedFrame, OverlayContent, OverlayTrack, PhysicalRect, ProjectManifest,
    ShapeKind, ValidationIssue,
};

fn boundary(precision: CompositePrecision) -> FrameRenderStep {
    FrameRenderStep::Composite {
        stage_id: 2,
        precision,
    }
}

fn owned_v2() -> OverlayTrack {
    let mut track = track(81, true, VectorShape::wpf_v2());
    track.frame_cells.as_mut().unwrap()[0].stage = Some(2);
    track
}

fn staged_project(precision: CompositePrecision) -> ProjectManifest {
    let mut project = project();
    project.schema_version = 9;
    project.timeline.frames[0].render_steps =
        vec![FrameRenderStep::composite(1), boundary(precision)];
    project
}

fn valid_v2_project() -> ProjectManifest {
    let mut project = staged_project(CompositePrecision::VectorCanvasPbgra8PngV2);
    project.timeline.overlay_tracks.push(owned_v2());
    project.validate().unwrap();
    project
}

fn cell(project: &mut ProjectManifest) -> &mut FrameOverlayCell {
    &mut project.timeline.overlay_tracks[0]
        .frame_cells
        .as_mut()
        .unwrap()[0]
}

fn assert_invalid_vector(project: &ProjectManifest, reason_fragment: &str) {
    let DomainError::InvalidManifest(issues) = project.validate().unwrap_err() else {
        panic!("expected invalid manifest")
    };
    assert!(
        issues.iter().any(|issue| matches!(issue,
        ValidationIssue::InvalidVectorShape { track_id, overlay_id, reason }
        if *track_id == crate::TrackId::from_u128(81)
            && *overlay_id == crate::OverlayId::from_u128(1081)
            && reason.contains(reason_fragment))),
        "{issues:?}"
    );
}

#[test]
fn explicit_v2_preserves_default_and_exact_v1_wire() {
    assert_eq!(VECTOR_SHAPE_VERSION, 1);
    assert_eq!(VECTOR_SHAPE_SCHEMA_VERSION, 8);
    assert_eq!(WPF_VECTOR_SHAPE_VERSION, 2);
    assert_eq!(WPF_VECTOR_SHAPE_SCHEMA_VERSION, 9);
    let original = VectorShape::default();
    let v2 = VectorShape::wpf_v2();
    assert_eq!(original.version, 1);
    assert_eq!(
        v2,
        VectorShape {
            version: 2,
            ..original
        }
    );
    let expected = concat!(
        r#"{"version":1,"kind":"rectangle","bounds":{"x_hundredths":0,"y_hundredths":0,"width_hundredths":10000,"height_hundredths":10000},"#,
        r#""stroke_width_hundredths":400,"stroke":{"red":0,"green":0,"blue":0,"alpha":255},"#,
        r#""fill":{"red":0,"green":0,"blue":0,"alpha":0},"corner_radius_hundredths":0,"rotation_hundredths":0}"#,
    );
    assert_eq!(serde_json::to_string(&original).unwrap(), expected);
    assert_eq!(
        serde_json::from_str::<VectorShape>(expected).unwrap(),
        original
    );
    assert_eq!(
        serde_json::to_string(&v2).unwrap(),
        expected.replacen("\"version\":1", "\"version\":2", 1)
    );
    assert_eq!(original.required_schema_version(), 8);
    assert_eq!(v2.required_schema_version(), 9);
    assert_eq!(
        OverlayContent::VectorShape { shape: v2 }.required_schema_version(),
        9
    );
}

#[test]
fn v2_uses_the_same_bounded_geometry_rules_without_weakening_unknown_version_rejection() {
    for kind in [
        VectorShapeKind::Rectangle,
        VectorShapeKind::Ellipse,
        VectorShapeKind::Triangle,
        VectorShapeKind::BlockArrow,
    ] {
        let valid = VectorShape {
            kind,
            stroke_width_hundredths: 0,
            stroke: Rgba::TRANSPARENT,
            fill: None,
            corner_radius_hundredths: 10_000,
            rotation_hundredths: 35_999,
            ..VectorShape::wpf_v2()
        };
        valid.validate().unwrap();
        for invalid in [
            VectorShape {
                version: 0,
                ..valid
            },
            VectorShape {
                version: 3,
                ..valid
            },
            VectorShape {
                version: u8::MAX,
                ..valid
            },
            VectorShape {
                rotation_hundredths: 36_000,
                ..valid
            },
            VectorShape {
                stroke_width_hundredths: 10_001,
                ..valid
            },
            VectorShape {
                corner_radius_hundredths: 10_001,
                ..valid
            },
            VectorShape {
                bounds: VectorShapeBounds {
                    width_hundredths: 0,
                    ..valid.bounds
                },
                ..valid
            },
            VectorShape {
                bounds: VectorShapeBounds {
                    x_hundredths: i64::MAX,
                    ..valid.bounds
                },
                ..valid
            },
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
        }
    }
}

#[test]
fn v2_stage_wire_schema_and_first_legacy_boundary_are_explicit_even_when_empty() {
    let step = boundary(CompositePrecision::VectorCanvasPbgra8PngV2);
    let wire = r#"{"type":"composite","stage_id":2,"precision":"vector_canvas_pbgra8_png_v2"}"#;
    assert_eq!(serde_json::to_string(&step).unwrap(), wire);
    assert_eq!(serde_json::from_str::<FrameRenderStep>(wire).unwrap(), step);
    assert_eq!(step.required_schema_version(), 9);
    assert!(crate::validate_frame_render_steps(std::slice::from_ref(&step)).is_err());
    let mut project = staged_project(CompositePrecision::VectorCanvasPbgra8PngV2);
    project.validate().unwrap();
    assert_eq!(project.timeline.frames[0].required_schema_version(), 9);
    for schema in [1, 7, 8] {
        project.schema_version = schema;
        assert!(project.validate().is_err());
    }
    project.schema_version = 10;
    assert!(project.validate().is_err());
    let mut project = valid_v2_project();
    cell(&mut project).marks.clear();
    project.validate().unwrap();
}

#[test]
fn hidden_and_zero_opacity_v2_cells_require_their_own_matching_stage() {
    for (visible, opacity) in [(true, 255), (false, 255), (true, 0), (false, 0)] {
        let mut valid = valid_v2_project();
        valid.timeline.overlay_tracks[0].visible = visible;
        valid.timeline.overlay_tracks[0].opacity = opacity;
        valid.validate().unwrap();
        for stage in [None, Some(1), Some(99)] {
            let mut invalid = valid.clone();
            cell(&mut invalid).stage = stage;
            assert_invalid_vector(&invalid, "owner's explicit");
        }
        // The same numeric stage exists on frame 1, but not on this owner.
        let mut invalid = valid;
        cell(&mut invalid).frame_id = FrameId::from_u128(2);
        assert_invalid_vector(&invalid, "owner's explicit");
    }
}

#[test]
fn v2_timed_and_every_old_precision_are_rejected_without_visibility_exemptions() {
    let mut timed = project();
    timed.schema_version = 9;
    timed
        .timeline
        .overlay_tracks
        .push(track(81, false, VectorShape::wpf_v2()));
    assert_invalid_vector(&timed, "frame-owned, not time-anchored");
    for precision in [
        CompositePrecision::LegacyStraightRgba8,
        CompositePrecision::WpfPbgra8PngV1,
        CompositePrecision::VectorCanvasPbgra8PngV1,
    ] {
        let mut invalid = staged_project(precision);
        invalid.timeline.overlay_tracks.push(owned_v2());
        assert_invalid_vector(&invalid, "owner's explicit");
    }
    let mut low_schema = valid_v2_project();
    low_schema.schema_version = 8;
    assert_invalid_vector(&low_schema, "schema 9");
}

#[test]
fn v2_stage_rejects_v1_nonvector_and_enhanced_blend_including_empty_hidden_cells() {
    let valid = valid_v2_project();
    for replacement in [
        OverlayContent::VectorShape {
            shape: VectorShape::default(),
        },
        OverlayContent::Shape {
            kind: ShapeKind::Rectangle,
            bounds: PhysicalRect::new(0, 0, 1, 1).unwrap(),
            stroke_width: 0,
            stroke: Rgba::TRANSPARENT,
            fill: None,
        },
    ] {
        let mut invalid = valid.clone();
        cell(&mut invalid).marks[0].content = replacement;
        let DomainError::InvalidManifest(issues) = invalid.validate().unwrap_err() else {
            panic!("expected invalid manifest")
        };
        assert!(issues.iter().any(|issue| matches!(issue,
            ValidationIssue::InvalidFrameOverlay { reason, .. }
                if reason.contains("only frame-owned version-two"))));
    }
    for blend in [BlendMode::Multiply, BlendMode::Screen] {
        for empty in [false, true] {
            let mut invalid = valid.clone();
            invalid.timeline.overlay_tracks[0].blend_mode = blend;
            if empty {
                cell(&mut invalid).marks.clear();
            }
            assert!(invalid.validate().is_err());
        }
    }
}

#[test]
fn v1_and_v2_coexist_in_distinct_stages_without_reinterpreting_legacy_locations() {
    for old_stage in [None, Some(1), Some(3)] {
        let mut project = valid_v2_project();
        project.timeline.frames[0]
            .render_steps
            .push(FrameRenderStep::Composite {
                stage_id: 3,
                precision: CompositePrecision::VectorCanvasPbgra8PngV1,
            });
        let mut old = track(82, true, VectorShape::default());
        old.frame_cells.as_mut().unwrap()[0].stage = old_stage;
        project.timeline.overlay_tracks.push(old);
        project
            .timeline
            .overlay_tracks
            .push(track(83, false, VectorShape::default()));
        project.validate().unwrap();
        let wire = serde_json::to_vec(&project).unwrap();
        let decoded: ProjectManifest = serde_json::from_slice(&wire).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, project);
    }
}

fn upgrade_command(project: &ProjectManifest) -> EditCommand {
    let mut replacement = project.timeline.frames[0].clone();
    replacement.render_steps = vec![
        FrameRenderStep::composite(1),
        boundary(CompositePrecision::VectorCanvasPbgra8PngV2),
    ];
    EditCommand::Compound {
        commands: vec![
            EditCommand::ReplaceFrame {
                frame_id: replacement.id,
                replacement: Box::new(replacement),
            },
            EditCommand::UpsertOverlayTrack { track: owned_v2() },
        ],
    }
}

#[test]
fn commands_inverses_and_nested_restore_payloads_propagate_schema_nine() {
    let staged = valid_v2_project();
    let frame = staged.timeline.frames[0].clone();
    for command in [
        EditCommand::InsertFrames {
            index: 0,
            frames: vec![frame.clone()],
        },
        EditCommand::RestoreFrames {
            frames: vec![IndexedFrame {
                index: 0,
                frame: frame.clone(),
            }],
        },
        EditCommand::ReplaceFrame {
            frame_id: frame.id,
            replacement: Box::new(frame),
        },
        EditCommand::UpsertOverlayTrack { track: owned_v2() },
        EditCommand::RestoreOverlayTrack {
            index: 0,
            track: owned_v2(),
        },
        EditCommand::RestoreFrameEdit {
            edit: Box::new(EditCommand::SetFrameDurations {
                changes: Vec::new(),
            }),
            overlay_tracks: vec![owned_v2()],
        },
        EditCommand::Compound {
            commands: vec![upgrade_command(&project())],
        },
    ] {
        assert_eq!(command.required_schema_version(), 9);
        let decoded: EditCommand =
            serde_json::from_slice(&serde_json::to_vec(&command).unwrap()).unwrap();
        assert_eq!(decoded, command);
        assert_eq!(decoded.required_schema_version(), 9);
    }
}

#[test]
fn upgrade_and_remove_undo_restore_raw_frames_and_keep_schema_nine() {
    let mut project = project();
    let before = project.clone();
    let applied = project.apply_command(&upgrade_command(&project)).unwrap();
    assert_eq!(project.schema_version, 9);
    let after = project.timeline.clone();
    let redo = project.apply_command(&applied.inverse).unwrap().inverse;
    assert_eq!(redo.required_schema_version(), 9);
    assert_eq!(project.schema_version, 9);
    assert_eq!(project.timeline, before.timeline);
    assert_eq!(project.assets, before.assets);
    project.apply_command(&redo).unwrap();
    assert_eq!(project.timeline, after);
    let removed = project
        .apply_command(&EditCommand::RemoveFrames {
            frame_ids: vec![FrameId::from_u128(1)],
        })
        .unwrap();
    assert_eq!(removed.inverse.required_schema_version(), 9);
    project.apply_command(&removed.inverse).unwrap();
    assert_eq!(project.timeline, after);
    assert_eq!(project.schema_version, 9);
}

#[test]
fn invalid_v2_edit_and_late_compound_failure_leave_all_manifest_bytes_unchanged() {
    let mut project = project();
    let before = serde_json::to_vec(&project).unwrap();
    for command in [
        EditCommand::UpsertOverlayTrack { track: owned_v2() },
        EditCommand::Compound {
            commands: vec![
                upgrade_command(&project),
                EditCommand::RemoveOverlayTrack {
                    track_id: crate::TrackId::from_u128(999),
                },
            ],
        },
    ] {
        assert!(project.apply_command(&command).is_err());
        assert_eq!(serde_json::to_vec(&project).unwrap(), before);
    }
}
