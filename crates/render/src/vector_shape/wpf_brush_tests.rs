use std::sync::atomic::{AtomicUsize, Ordering};

use gif_from_screen_domain::{PhysicalSize, Rgba, VectorShape, VectorShapeBounds, VectorShapeKind};
use sha2::{Digest, Sha256};

use super::prepare_wpf_brush_paths;
use crate::{
    CancellationToken, InkError, InkFillRule, InkLimits, InkSegment, NeverCancel,
    rasterize_ink_paths,
};

fn shape(kind: VectorShapeKind) -> VectorShape {
    VectorShape {
        kind,
        bounds: VectorShapeBounds {
            x_hundredths: 200,
            y_hundredths: 200,
            width_hundredths: 800,
            height_hundredths: 800,
        },
        stroke_width_hundredths: 125,
        stroke: Rgba {
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
        },
        fill: None,
        ..VectorShape::default()
    }
}

#[test]
fn actual_windows_triangle_stroke_matches_all_1024_rgba_bytes() {
    // Windows WPF reference generator d1ba41e, coverage-triangle-stroke,
    // 16x16 transparent input, white 1.25px stroke, no fill. The strict raw
    // reference SHA is recorded here, not regenerated from this implementation.
    let paths = prepare_wpf_brush_paths(
        &shape(VectorShapeKind::Triangle),
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert!(paths.layout_clip.is_none());
    let coverage = rasterize_ink_paths(
        &[paths.stroke.unwrap()],
        PhysicalSize::new(16, 16).unwrap(),
        false,
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    let rgba: Vec<u8> = coverage
        .into_iter()
        .flat_map(|coverage| {
            let channel = u8::try_from((u16::from(coverage) * 4 * 255 + 128) >> 8).unwrap();
            crate::wpf_pixels::unpremultiply([channel; 4])
        })
        .collect();
    assert_eq!(
        format!("{:x}", Sha256::digest(&rgba)),
        "c3ac02f8231d3b84eaebc32479ccefc9635e431507ef2906537d68422d727d91"
    );
}

#[test]
fn compact_rectangle_uses_original_radius_for_separate_alternate_outer_inner_contours() {
    let mut source = shape(VectorShapeKind::Rectangle);
    source.bounds.width_hundredths = 600;
    source.bounds.height_hundredths = 1_000;
    source.stroke_width_hundredths = 200;
    source.corner_radius_hundredths = 10_000;
    let paths = prepare_wpf_brush_paths(&source, &InkLimits::default(), &NeverCancel).unwrap();
    let stroke = paths.stroke.unwrap();
    assert_eq!(stroke.fill_rule, InkFillRule::EvenOdd);
    assert_eq!(stroke.figures.len(), 2);
    let bounds = stroke
        .figures
        .iter()
        .map(super::super::control_bounds)
        .collect::<Vec<_>>();
    assert_eq!(
        bounds[0],
        [
            super::super::point(2.0, 2.0),
            super::super::point(8.0, 12.0)
        ]
    );
    assert_eq!(
        bounds[1],
        [
            super::super::point(4.0, 4.0),
            super::super::point(6.0, 10.0)
        ]
    );
    let size = PhysicalSize::new(16, 16).unwrap();
    let component = |figure: &crate::InkFigure| {
        rasterize_ink_paths(
            &[crate::InkPath {
                fill_rule: InkFillRule::NonZero,
                figures: vec![figure.clone()],
            }],
            size,
            false,
            &InkLimits::default(),
            &NeverCancel,
        )
        .unwrap()
    };
    let outer = component(&stroke.figures[0]);
    let inner = component(&stroke.figures[1]);
    let coverage = rasterize_ink_paths(
        &[stroke],
        PhysicalSize::new(16, 16).unwrap(),
        false,
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(
        coverage,
        outer
            .iter()
            .zip(&inner)
            .map(|(outer, inner)| outer - inner)
            .collect::<Vec<_>>(),
        "alternate compact contours remove exactly the inner covered samples"
    );
    assert!(inner[7 * 16 + 5] > 0);
    assert!(coverage[7 * 16 + 2] > 0);
}

#[test]
fn triangle_miters_are_lines_and_arrow_smooth_joins_retain_native_cubics() {
    for kind in [VectorShapeKind::Triangle, VectorShapeKind::BlockArrow] {
        let mut source = shape(kind);
        source.stroke_width_hundredths = 400;
        source.bounds.width_hundredths = 2_000;
        source.bounds.height_hundredths = 2_000;
        let paths = prepare_wpf_brush_paths(&source, &InkLimits::default(), &NeverCancel).unwrap();
        let stroke = paths.stroke.unwrap();
        let has_curve = stroke
            .figures
            .iter()
            .flat_map(|f| &f.segments)
            .any(|segment| matches!(segment, InkSegment::CubicTo { .. }));
        assert_eq!(has_curve, kind == VectorShapeKind::BlockArrow);
        assert!(stroke.figures.iter().all(|figure| figure.closed));
        assert_eq!(stroke.fill_rule, InkFillRule::NonZero);
    }
}

#[test]
fn ellipse_uses_native_tangent_widening_with_finite_bounded_results_at_gif_extremes() {
    for (width, height, stroke, rotation) in [
        (1_200, 1_700, 225, 1_337),
        (6_553_500, 6_553_500, 10_000, 0),
        (13_107_000, 1, 10_000, 9_000),
        (1, 13_107_000, 1, 0),
    ] {
        let mut source = shape(VectorShapeKind::Ellipse);
        source.bounds.x_hundredths = 0;
        source.bounds.y_hundredths = 0;
        source.bounds.width_hundredths = width;
        source.bounds.height_hundredths = height;
        source.stroke_width_hundredths = stroke;
        source.rotation_hundredths = rotation;
        let original = source;
        let paths = prepare_wpf_brush_paths(&source, &InkLimits::default(), &NeverCancel).unwrap();
        let stroke = paths.stroke.unwrap();
        assert!(!stroke.figures.is_empty());
        for figure in &stroke.figures {
            let bounds = super::super::control_bounds(figure);
            assert!(
                bounds
                    .into_iter()
                    .all(|p| p.x.is_finite() && p.y.is_finite())
            );
        }
        assert!(
            stroke
                .figures
                .iter()
                .map(|f| f.segments.len())
                .sum::<usize>()
                <= InkLimits::default().max_segments
        );
        assert_eq!(source, original);
    }
}

#[test]
fn transparent_stroke_keeps_layout_growth_and_zero_extent_clip_is_not_none() {
    let mut source = shape(VectorShapeKind::Rectangle);
    source.bounds.width_hundredths = 1;
    source.bounds.height_hundredths = 200;
    source.stroke_width_hundredths = 1_000;
    source.stroke.alpha = 0;
    let paths = prepare_wpf_brush_paths(&source, &InkLimits::default(), &NeverCancel).unwrap();
    assert!(paths.stroke.is_none());
    assert!(paths.layout_clip.is_some());
    let clip = paths.layout_clip.unwrap();
    let bounds = super::super::control_bounds(&clip.figures[0]);
    assert_eq!(bounds[0].x.to_bits(), bounds[1].x.to_bits());
    let expected = super::super::wpf_vector_shape_geometry(&source).unwrap();
    assert_eq!(&paths.fill, expected.outline());
}

struct CancelAfter {
    checks: AtomicUsize,
    at: usize,
}

#[test]
fn native_nearly_flat_and_180_degree_corner_state_is_retained() {
    let limits = InkLimits::default();
    let mut budget = super::Budget::new(&limits, &NeverCancel).unwrap();
    let mut pen = super::Pen::new([0.0, 0.0], [1.0, 0.0], 2.0, &mut budget).unwrap();
    pen.line([4.0, 0.0], &mut budget).unwrap();
    let original = (pen.radial, pen.offset, pen.tangent);
    pen.corner(
        [4.0, 0.0],
        [1.0, 0.0],
        [1.0, 1.0e-6],
        false,
        false,
        false,
        &mut budget,
    )
    .unwrap();
    assert_eq!((pen.radial, pen.offset, pen.tangent), original);
    pen.corner(
        [4.0, 0.0],
        [1.0, 0.0],
        [-1.0, 1.0e-8],
        true,
        false,
        false,
        &mut budget,
    )
    .unwrap();
    assert!(
        pen.rails[1]
            .segments
            .iter()
            .any(|segment| matches!(segment, InkSegment::CubicTo { .. })),
        "near-180 turn keeps native default RIGHT outer side"
    );
    assert!(
        pen.rails[0]
            .segments
            .iter()
            .all(|segment| matches!(segment, InkSegment::LineTo(_)))
    );
}
impl CancellationToken for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::Relaxed) >= self.at
    }
}

#[test]
fn malformed_limits_and_mid_curve_cancellation_return_no_partial_geometry() {
    let source = shape(VectorShapeKind::Ellipse);
    for limits in [
        InkLimits {
            max_work: 0,
            ..InkLimits::default()
        },
        InkLimits {
            max_points: 1,
            ..InkLimits::default()
        },
        InkLimits {
            max_segments: 0,
            ..InkLimits::default()
        },
        InkLimits {
            max_bytes: 1,
            ..InkLimits::default()
        },
    ] {
        assert!(matches!(
            prepare_wpf_brush_paths(&source, &limits, &NeverCancel),
            Err(InkError::Limit(_))
        ));
    }
    for at in [0, 8, 30, 100] {
        let cancellation = CancelAfter {
            checks: AtomicUsize::new(0),
            at,
        };
        assert!(matches!(
            prepare_wpf_brush_paths(&source, &InkLimits::default(), &cancellation),
            Err(InkError::Cancelled)
        ));
    }
    let mut invalid = source;
    invalid.bounds.width_hundredths = 0;
    assert!(matches!(
        prepare_wpf_brush_paths(&invalid, &InkLimits::default(), &NeverCancel),
        Err(InkError::Invalid(_))
    ));
}

#[test]
fn measured_brush_is_identical_and_reports_the_exact_work_limit() {
    let limits = InkLimits::default();
    for kind in [
        VectorShapeKind::Rectangle,
        VectorShapeKind::Ellipse,
        VectorShapeKind::Triangle,
        VectorShapeKind::BlockArrow,
    ] {
        let source = shape(kind);
        let expected = prepare_wpf_brush_paths(&source, &limits, &NeverCancel).unwrap();
        let (actual, work) =
            super::prepare_wpf_brush_paths_measured(&source, &limits, &NeverCancel).unwrap();
        assert_eq!(actual, expected);
        assert!(work > 0 && work < limits.max_work);
        let exact = InkLimits {
            max_work: work,
            ..limits
        };
        let (again, charged) =
            super::prepare_wpf_brush_paths_measured(&source, &exact, &NeverCancel).unwrap();
        assert_eq!(again, actual);
        assert_eq!(charged, work);
        let short = InkLimits {
            max_work: work - 1,
            ..limits
        };
        assert!(matches!(
            super::prepare_wpf_brush_paths_measured(&source, &short, &NeverCancel),
            Err(InkError::Limit(_))
        ));
    }
}

#[test]
fn brush_meter_includes_stroke_work_without_recharging_unused_quota() {
    let limits = InkLimits::default();
    let mut source = shape(VectorShapeKind::Ellipse);
    source.stroke.alpha = 0;
    let (fill_only, minimal) =
        super::prepare_wpf_brush_paths_measured(&source, &limits, &NeverCancel).unwrap();
    source.stroke.alpha = 255;
    let (painted, full) =
        super::prepare_wpf_brush_paths_measured(&source, &limits, &NeverCancel).unwrap();
    assert_eq!(fill_only.fill, painted.fill);
    assert_eq!(fill_only.layout_clip, painted.layout_clip);
    assert!(fill_only.stroke.is_none() && painted.stroke.is_some());
    assert!(full > minimal);
    let enlarged = InkLimits {
        max_work: limits.max_work * 2,
        ..limits
    };
    assert_eq!(
        super::prepare_wpf_brush_paths_measured(&source, &enlarged, &NeverCancel)
            .unwrap()
            .1,
        full
    );
}

#[test]
fn measured_brush_retains_zero_budget_and_identical_cancellation_check_sequence() {
    let source = shape(VectorShapeKind::Ellipse);
    let limits = InkLimits::default();
    let zero = InkLimits {
        max_work: 0,
        ..limits
    };
    assert!(matches!(
        super::prepare_wpf_brush_paths_measured(&source, &zero, &NeverCancel),
        Err(InkError::Limit(_))
    ));
    for at in [0, 8, 30, 100] {
        let old = CancelAfter {
            checks: AtomicUsize::new(0),
            at,
        };
        let measured = CancelAfter {
            checks: AtomicUsize::new(0),
            at,
        };
        let expected = prepare_wpf_brush_paths(&source, &limits, &old);
        let actual = super::prepare_wpf_brush_paths_measured(&source, &limits, &measured)
            .map(|(paths, _)| paths);
        assert!(matches!(actual, Err(InkError::Cancelled)));
        assert_eq!(actual, expected);
        assert_eq!(
            old.checks.load(Ordering::Relaxed),
            measured.checks.load(Ordering::Relaxed)
        );
    }
}
