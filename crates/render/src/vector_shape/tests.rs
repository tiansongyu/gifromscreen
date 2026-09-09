use std::sync::atomic::{AtomicUsize, Ordering};

use gif_from_screen_domain::{
    AssetId, BlendMode, CaptureBinding, CaptureMetadata, ClipTransform, CompositePrecision,
    DurationUs, FrameClip, FrameId, FrameOverlayCell, FrameOverlayMark, FrameRenderStep,
    OverlayContent, OverlayId, OverlayTrack, PhysicalSize, Rgba, TimeUs, TrackId, VectorShape,
    VectorShapeBounds, VectorShapeKind,
};

use super::{point, vector_shape_geometry};
use crate::{
    AssetProviderError, CancellationToken, CpuRenderer, NeverCancel, OverlayRenderPlan,
    RenderError, RenderLimits, RgbaSurface, render_vector_shapes_preview,
};

fn shape(kind: VectorShapeKind) -> VectorShape {
    VectorShape {
        kind,
        bounds: VectorShapeBounds {
            x_hundredths: 200,
            y_hundredths: 200,
            width_hundredths: 1_200,
            height_hundredths: 1_200,
        },
        stroke_width_hundredths: 200,
        stroke: color(0, 0, 255, 128),
        fill: Some(color(255, 0, 0, 128)),
        ..VectorShape::default()
    }
}

const fn color(red: u8, green: u8, blue: u8, alpha: u8) -> Rgba {
    Rgba {
        red,
        green,
        blue,
        alpha,
    }
}

fn clip() -> FrameClip {
    FrameClip {
        id: FrameId::from_u128(1),
        asset_id: AssetId::from_digest([1; 32]),
        duration: DurationUs::new(100_000).unwrap(),
        transform: ClipTransform::default(),
        effects: Vec::new(),
        capture_metadata: CaptureMetadata::default(),
        capture_binding: CaptureBinding::NotRecorded,
        capture_clock: None,
        render_steps: vec![
            FrameRenderStep::composite(1),
            FrameRenderStep::Composite {
                stage_id: 2,
                precision: CompositePrecision::WpfPbgra8PngV1,
            },
        ],
    }
}

fn track(shapes: &[VectorShape]) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(1),
        name: "new vector".into(),
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
        annotation: None,
        annotation_scope: None,
        frame_cells: Some(vec![FrameOverlayCell {
            frame_id: clip().id,
            scopes: Vec::new(),
            input_replay: None,
            stage: Some(2),
            marks: shapes
                .iter()
                .copied()
                .enumerate()
                .map(|(index, shape)| FrameOverlayMark {
                    id: OverlayId::from_u128(u128::try_from(index + 1).unwrap()),
                    z_index: 0,
                    content: OverlayContent::VectorShape { shape },
                })
                .collect(),
        }]),
    }
}

fn transparent_provider(_: AssetId) -> Result<RgbaSurface, AssetProviderError> {
    Ok(RgbaSurface::new(
        PhysicalSize::new(16, 16).unwrap(),
        vec![0; 16 * 16 * 4],
    )?)
}

#[test]
fn shared_geometry_keeps_per_axis_radius_and_actual_triangle_and_arrow_vertices() {
    let mut rounded = shape(VectorShapeKind::Rectangle);
    rounded.bounds.width_hundredths = 3_000;
    rounded.bounds.height_hundredths = 1_000;
    rounded.corner_radius_hundredths = 10_000;
    let geometry = vector_shape_geometry(&rounded).unwrap();
    assert_eq!(geometry.outline().figures[0].start, point(17.0, 3.0)); // rx14, ry4
    assert!(geometry.hit_test(point(17.0, 7.0)));
    assert!(!geometry.hit_test(point(3.1, 3.1)));
    let triangle = vector_shape_geometry(&shape(VectorShapeKind::Triangle)).unwrap();
    assert_eq!(triangle.outline().figures[0].start, point(8.0, 3.0));
    assert!(triangle.hit_test(point(8.0, 8.0)));
    assert!(!triangle.hit_test(point(3.1, 3.1)));
    let arrow = vector_shape_geometry(&shape(VectorShapeKind::BlockArrow)).unwrap();
    assert_eq!(arrow.outline().figures[0].segments.len(), 8);
    assert_eq!(arrow.outline().figures[0].start, point(8.898, 6.0));
    assert!(arrow.hit_test(point(3.0, 7.0)));
    assert!(!arrow.hit_test(point(3.0, 3.0)));
    assert!(!arrow.intersects_rect(point(2.1, 2.1), point(3.0, 3.0)));
    assert!(arrow.intersects_rect(point(3.0, 6.5), point(4.0, 7.0)));
    assert!(arrow.intersects_rect(point(1.0, 1.0), point(15.0, 15.0)));
}

#[test]
fn rotated_transparent_shapes_pick_the_real_cubic_or_polygon_and_reject_invalid_queries() {
    for kind in [
        VectorShapeKind::Rectangle,
        VectorShapeKind::Ellipse,
        VectorShapeKind::Triangle,
        VectorShapeKind::BlockArrow,
    ] {
        let mut source = shape(kind);
        source.fill = Some(Rgba::TRANSPARENT);
        source.stroke.alpha = 0;
        source.rotation_hundredths = 9_000;
        let geometry = vector_shape_geometry(&source).unwrap();
        assert!(geometry.hit_test(point(8.0, 8.0)));
        assert!(!geometry.hit_test(point(-100.0, 8.0)));
        assert!(!geometry.hit_test(point(f64::NAN, 8.0)));
        assert!(!geometry.intersects_rect(point(10.0, 10.0), point(0.0, 0.0)));
        assert!(!geometry.intersects_rect(point(f64::NEG_INFINITY, 0.0), point(20.0, 20.0)));
    }
    let ellipse = vector_shape_geometry(&shape(VectorShapeKind::Ellipse)).unwrap();
    assert!(!ellipse.intersects_rect(point(3.0, 3.0), point(3.5, 3.5)));
    assert!(ellipse.intersects_rect(point(7.9, 2.9), point(8.1, 3.1)));
    assert!(ellipse.intersects_rect(point(7.0, 7.0), point(9.0, 9.0)));
}

#[test]
fn direct_detached_and_preview_routes_share_fill_stroke_and_fractional_aa() {
    let mut sources = vec![
        shape(VectorShapeKind::Triangle),
        shape(VectorShapeKind::Rectangle),
    ];
    sources[0].rotation_hundredths = 3_333;
    sources[1].bounds.x_hundredths = 625;
    sources[1].bounds.y_hundredths = -75;
    sources[1].corner_radius_hundredths = 225;
    let tracks = vec![track(&sources)];
    let frame = clip();
    let renderer = CpuRenderer::default();
    let direct = renderer
        .render_clip_with_overlays(
            &frame,
            &tracks,
            TimeUs::ZERO,
            &transparent_provider,
            &NeverCancel,
        )
        .unwrap();
    let plan = OverlayRenderPlan::for_frame(&tracks, frame.id, TimeUs::ZERO, &NeverCancel).unwrap();
    let detached = renderer
        .render_clip_with_overlay_plan(&frame, &plan, &transparent_provider, &NeverCancel)
        .unwrap();
    let preview = render_vector_shapes_preview(
        &sources,
        [16, 16],
        [16, 16],
        RenderLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(direct, detached);
    assert_eq!(direct, preview);
    assert!(
        preview
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] > 0 && pixel[3] < 100)
    );
    assert!(
        preview
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[0] != 0 && pixel[2] != 0)
    );
}

#[test]
fn preview_scales_geometry_without_modifying_persisted_bounds_or_stroke() {
    let source = shape(VectorShapeKind::Ellipse);
    let original = source;
    let enlarged = render_vector_shapes_preview(
        &[source],
        [16, 16],
        [32, 32],
        RenderLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(source, original);
    assert_eq!(enlarged.size(), PhysicalSize::new(32, 32).unwrap());
    assert!(
        enlarged
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] != 0)
    );
    assert_eq!(&enlarged.pixels()[0..4], &[0, 0, 0, 0]);
}

#[test]
fn fill_and_stroke_form_one_mark_before_track_opacity_in_both_precisions() {
    let source = shape(VectorShapeKind::Rectangle);
    let offset = (8 * 16 + 3) * 4;
    for precision in [
        CompositePrecision::LegacyStraightRgba8,
        CompositePrecision::WpfPbgra8PngV1,
    ] {
        let mut frame = clip();
        frame.render_steps[1] = FrameRenderStep::Composite {
            stage_id: 2,
            precision,
        };
        for (opacity, alpha) in [(255, 192), (128, 96)] {
            let mut overlay = track(&[source]);
            overlay.opacity = opacity;
            let rendered = CpuRenderer::default()
                .render_clip_with_overlays(
                    &frame,
                    &[overlay],
                    TimeUs::ZERO,
                    &transparent_provider,
                    &NeverCancel,
                )
                .unwrap();
            assert_eq!(&rendered.pixels()[offset..offset + 4], &[85, 0, 170, alpha]);
            assert_ne!(
                rendered.pixels()[offset + 3],
                112,
                "brushes must not each get track opacity"
            );
        }
    }
}

#[test]
fn legacy_vector_blends_match_its_frozen_mark_and_wpf_rejects_non_normal_blends() {
    let source = shape(VectorShapeKind::Triangle);
    let frozen = render_vector_shapes_preview(
        &[source],
        [16, 16],
        [16, 16],
        RenderLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    let provider = |id: AssetId| -> Result<RgbaSurface, AssetProviderError> {
        if id == AssetId::from_digest([2; 32]) {
            return Ok(frozen.clone());
        }
        Ok(RgbaSurface::new(
            PhysicalSize::new(16, 16).unwrap(),
            [41, 85, 129, 177].repeat(256),
        )?)
    };
    for blend in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
        let mut frame = clip();
        frame.render_steps[1] = FrameRenderStep::composite(2);
        let mut vector = track(&[source]);
        vector.opacity = 137;
        vector.blend_mode = blend;
        let mut raster = vector.clone();
        raster.frame_cells.as_mut().unwrap()[0].marks[0].content = OverlayContent::Raster {
            asset_id: AssetId::from_digest([2; 32]),
            position: gif_from_screen_domain::PhysicalPoint::default(),
            size: PhysicalSize::new(16, 16).unwrap(),
            opacity: 255,
        };
        let renderer = CpuRenderer::default();
        let actual = renderer
            .render_clip_with_overlays(
                &frame,
                std::slice::from_ref(&vector),
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            )
            .unwrap();
        let expected = renderer
            .render_clip_with_overlays(&frame, &[raster], TimeUs::ZERO, &provider, &NeverCancel)
            .unwrap();
        assert_eq!(actual, expected);
        if blend != BlendMode::Normal {
            assert!(
                renderer
                    .render_clip_with_overlays(
                        &clip(),
                        &[vector],
                        TimeUs::ZERO,
                        &provider,
                        &NeverCancel
                    )
                    .is_err()
            );
        }
    }
}

#[test]
fn vector_marks_follow_only_their_own_stage_then_later_geometry() {
    let mut first = shape(VectorShapeKind::Rectangle);
    first.bounds = VectorShapeBounds {
        x_hundredths: 100,
        y_hundredths: 100,
        width_hundredths: 400,
        height_hundredths: 400,
    };
    first.stroke_width_hundredths = 0;
    first.fill = Some(color(255, 0, 0, 255));
    let mut later = shape(VectorShapeKind::Triangle);
    later.stroke_width_hundredths = 0;
    later.bounds = VectorShapeBounds {
        x_hundredths: 0,
        y_hundredths: 0,
        width_hundredths: 800,
        height_hundredths: 800,
    };
    later.fill = Some(color(0, 0, 255, 255));
    let mut frame = clip();
    frame.render_steps.extend([
        FrameRenderStep::Resize {
            size: PhysicalSize::new(32, 32).unwrap(),
        },
        FrameRenderStep::Rotate {
            rotation: gif_from_screen_domain::QuarterTurn::Clockwise90,
        },
        FrameRenderStep::Composite {
            stage_id: 3,
            precision: CompositePrecision::WpfPbgra8PngV1,
        },
    ]);
    let mut second = track(&[later]);
    second.id = TrackId::from_u128(2);
    second.frame_cells.as_mut().unwrap()[0].stage = Some(3);
    second.frame_cells.as_mut().unwrap()[0].marks[0].id = OverlayId::from_u128(2);
    let tracks = [track(&[first]), second];
    let original = frame.clone();
    let renderer = CpuRenderer::default();
    let output = renderer
        .render_clip_with_overlays(
            &frame,
            &tracks,
            TimeUs::ZERO,
            &transparent_provider,
            &NeverCancel,
        )
        .unwrap();
    let at = |x: usize, y: usize| &output.pixels()[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
    assert_eq!(at(24, 4), &[255, 0, 0, 255]);
    assert_eq!(at(4, 4), &[0, 0, 255, 255]);
    assert_eq!(at(12, 12), &[0; 4]);
    let plan = OverlayRenderPlan::for_frame(&tracks, frame.id, TimeUs::ZERO, &NeverCancel).unwrap();
    assert_eq!(
        renderer
            .render_clip_with_overlay_plan(&frame, &plan, &transparent_provider, &NeverCancel)
            .unwrap(),
        output
    );
    assert_eq!(frame, original);
}

struct CancelAfter(AtomicUsize);

#[test]
fn vector_group_rounds_its_isolated_pm_canvas_only_once_over_the_frame() {
    let mut first = shape(VectorShapeKind::Rectangle);
    first.stroke_width_hundredths = 0;
    first.fill = Some(color(10, 99, 200, 83));
    let mut second = first;
    second.fill = Some(color(80, 140, 2, 117));
    let layers = [track(&[first, second])];
    let provider = |_: AssetId| -> Result<RgbaSurface, AssetProviderError> {
        Ok(RgbaSurface::new(
            PhysicalSize::new(16, 16).unwrap(),
            [200, 30, 80, 77].repeat(16 * 16),
        )?)
    };
    let mut frame = clip();
    let direct = CpuRenderer::default()
        .render_clip_with_overlays(&frame, &layers, TimeUs::ZERO, &provider, &NeverCancel)
        .unwrap();
    frame.render_steps[1] = FrameRenderStep::Composite {
        stage_id: 2,
        precision: CompositePrecision::VectorCanvasPbgra8PngV1,
    };
    let isolated = CpuRenderer::default()
        .render_clip_with_overlays(&frame, &layers, TimeUs::ZERO, &provider, &NeverCancel)
        .unwrap();
    let center = (8 * 16 + 8) * 4;
    assert_eq!(&direct.pixels()[center..center + 4], &[80, 114, 60, 190]);
    assert_eq!(&isolated.pixels()[center..center + 4], &[81, 112, 60, 190]);
}

impl CancellationToken for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.0.fetch_sub(1, Ordering::Relaxed) <= 1
    }
}

#[test]
fn invalid_inputs_limits_and_midwork_cancellation_never_return_partial_preview() {
    let valid = shape(VectorShapeKind::Rectangle);
    let mut invalid = valid;
    invalid.bounds.width_hundredths = 0;
    assert!(matches!(
        vector_shape_geometry(&invalid),
        Err(RenderError::InvalidVectorShape { .. })
    ));
    assert!(
        render_vector_shapes_preview(
            &[invalid],
            [16, 16],
            [16, 16],
            RenderLimits::default(),
            &NeverCancel
        )
        .is_err()
    );
    assert!(
        render_vector_shapes_preview(
            &[valid; 257],
            [16, 16],
            [16, 16],
            RenderLimits::default(),
            &NeverCancel
        )
        .is_err()
    );
    assert!(
        render_vector_shapes_preview(
            &[valid],
            [0, 16],
            [16, 16],
            RenderLimits::default(),
            &NeverCancel
        )
        .is_err()
    );
    assert!(matches!(
        render_vector_shapes_preview(
            &[valid],
            [16, 16],
            [16, 16],
            RenderLimits {
                max_surface_bytes: 1_023
            },
            &NeverCancel
        ),
        Err(RenderError::SurfaceLimitExceeded { .. })
    ));
    assert!(matches!(
        render_vector_shapes_preview(
            &[valid],
            [16, 16],
            [16, 16],
            RenderLimits {
                max_surface_bytes: 1_024
            },
            &NeverCancel
        ),
        Err(RenderError::EffectWorkingMemoryLimitExceeded { .. })
    ));
    assert!(matches!(
        render_vector_shapes_preview(
            &[valid],
            [16, 16],
            [16, 16],
            RenderLimits::default(),
            &CancelAfter(AtomicUsize::new(1))
        ),
        Err(RenderError::Cancelled)
    ));
    for checks in [3, 20, 100] {
        let mut large = shape(VectorShapeKind::Ellipse);
        large.bounds.width_hundredths = 100_000;
        large.bounds.height_hundredths = 100_000;
        assert!(matches!(
            render_vector_shapes_preview(
                &[large],
                [1_024, 1_024],
                [1_024, 1_024],
                RenderLimits::default(),
                &CancelAfter(AtomicUsize::new(checks))
            ),
            Err(RenderError::Cancelled)
        ));
    }
}
