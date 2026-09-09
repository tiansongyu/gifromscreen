use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::{
    InkFillRule, InkPoint, InkSegment, rasterize_ink_paths, rasterize_ink_paths_measured,
};
use super::*;
use crate::{InkFigure, NeverCancel};

fn point(x: f64, y: f64) -> InkPoint {
    InkPoint { x, y }
}
fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}
fn rect(x: u32, y: u32, width: u32, height: u32) -> PhysicalRect {
    PhysicalRect::new(x, y, width, height).unwrap()
}
fn polygon(points: &[InkPoint]) -> InkFigure {
    InkFigure {
        start: points[0],
        segments: points[1..]
            .iter()
            .copied()
            .map(InkSegment::LineTo)
            .collect(),
        closed: true,
    }
}
fn rectangle(x: f64, y: f64, width: f64, height: f64) -> InkPath {
    InkPath {
        fill_rule: InkFillRule::NonZero,
        figures: vec![polygon(&[
            point(x, y),
            point(x + width, y),
            point(x + width, y + height),
            point(x, y + height),
        ])],
    }
}
fn fixtures() -> Vec<InkPath> {
    let mut hole = rectangle(-100.0, -100.0, 300.0, 300.0);
    hole.fill_rule = InkFillRule::EvenOdd;
    hole.figures
        .extend(rectangle(3.25, 4.5, 22.0, 17.125).figures);
    vec![
        hole,
        InkPath {
            fill_rule: InkFillRule::NonZero,
            figures: vec![InkFigure {
                start: point(10.0, -3.0),
                closed: true,
                segments: vec![
                    InkSegment::CubicTo {
                        control1: point(-8.0, 5.0),
                        control2: point(28.0, 20.0),
                        to: point(10.0, 24.0),
                    },
                    InkSegment::LineTo(point(33.0625, 11.03125)),
                ],
            }],
        },
        rectangle(0.031_249_9, -0.09375, 4.75, 13.5),
    ]
}

fn crop(full: &[u8], canvas: PhysicalSize, area: PhysicalRect) -> Vec<u8> {
    let stride = usize::try_from(canvas.width.get()).unwrap();
    let mut result = Vec::new();
    for y in area.origin.y.get()..area.end_y().unwrap() {
        let begin =
            usize::try_from(y).unwrap() * stride + usize::try_from(area.origin.x.get()).unwrap();
        result.extend_from_slice(
            &full[begin..begin + usize::try_from(area.size.width.get()).unwrap()],
        );
    }
    result
}

#[test]
fn tiled_regions_reconstruct_full_global_curve_union_holes_and_outside() {
    let canvas = size(40, 30);
    let paths = fixtures();
    let original = paths.clone();
    for outside in [false, true] {
        let full =
            rasterize_ink_paths(&paths, canvas, outside, &InkLimits::default(), &NeverCancel)
                .unwrap();
        let mut assembled = vec![0; full.len()];
        for y in (0..30).step_by(9) {
            for x in (0..40).step_by(7) {
                let area = rect(x, y, (40 - x).min(7), (30 - y).min(9));
                let mask = rasterize_ink_paths_region_measured(
                    &paths,
                    canvas,
                    area,
                    outside,
                    &InkLimits::default(),
                    &NeverCancel,
                )
                .unwrap();
                assert_eq!(mask.coverage, crop(&full, canvas, area));
                assert_eq!(mask.origin, area.origin);
                assert_eq!(mask.size, area.size);
                for row in 0..area.size.height.get() {
                    let start = usize::try_from((y + row) * 40 + x).unwrap();
                    let local = usize::try_from(row * area.size.width.get()).unwrap();
                    let width = usize::try_from(area.size.width.get()).unwrap();
                    assembled[start..start + width]
                        .copy_from_slice(&mask.coverage[local..local + width]);
                }
            }
        }
        assert_eq!(assembled, full);
    }
    assert_eq!(paths, original);
}

#[test]
fn full_region_preserves_old_work_bytes_and_cancellation_checkpoints() {
    let paths = fixtures();
    let canvas = size(40, 30);
    let full_counter = Counter::new(usize::MAX);
    let (full, work) =
        rasterize_ink_paths_measured(&paths, canvas, false, &InkLimits::default(), &full_counter)
            .unwrap();
    let region_counter = Counter::new(usize::MAX);
    let region = rasterize_ink_paths_region_measured(
        &paths,
        canvas,
        rect(0, 0, 40, 30),
        false,
        &InkLimits::default(),
        &region_counter,
    )
    .unwrap();
    assert_eq!(region.coverage, full);
    assert_eq!(region.work, work);
    assert_eq!(
        region_counter.calls.load(Ordering::Relaxed),
        full_counter.calls.load(Ordering::Relaxed)
    );
}

#[test]
fn global_float_quantization_is_not_replaced_by_local_requantization() {
    // At this large origin f32 rounds the edge onto the 1/32-pixel tie. Moving
    // the double point near zero before quantization instead rounds below it.
    let paths = [rectangle(1_024.031_249_9, 0.0, 1.0, 1.0)];
    let canvas = size(2048, 2);
    let area = rect(1024, 0, 2, 2);
    let full =
        rasterize_ink_paths(&paths, canvas, false, &InkLimits::default(), &NeverCancel).unwrap();
    let region = rasterize_ink_paths_region_measured(
        &paths,
        canvas,
        area,
        false,
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(region.coverage, crop(&full, canvas, area));
    let incorrect = rasterize_ink_paths(
        &[rectangle(0.031_249_9, 0.0, 1.0, 1.0)],
        area.size,
        false,
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_ne!(region.coverage, incorrect);
}

#[test]
fn large_global_coordinates_and_negative_points_match_full_slice() {
    let canvas = size(131_072, 3);
    let paths = [
        rectangle(131_066.031_249_9, -0.03125, 4.5, 2.25),
        rectangle(-20.0, -20.0, 30.0, 30.0),
    ];
    let area = rect(131_064, 0, 8, 3);
    let full =
        rasterize_ink_paths(&paths, canvas, false, &InkLimits::default(), &NeverCancel).unwrap();
    let region = rasterize_ink_paths_region_measured(
        &paths,
        canvas,
        area,
        false,
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(region.coverage, crop(&full, canvas, area));
}

#[test]
fn rotated_real_vector_contours_match_global_full_raster() {
    use gif_from_screen_domain::{VectorShape, VectorShapeBounds, VectorShapeKind};
    let canvas = size(40, 30);
    for kind in [
        VectorShapeKind::Rectangle,
        VectorShapeKind::Ellipse,
        VectorShapeKind::Triangle,
        VectorShapeKind::BlockArrow,
    ] {
        let shape = VectorShape {
            kind,
            bounds: VectorShapeBounds {
                x_hundredths: -125,
                y_hundredths: 350,
                width_hundredths: 1800,
                height_hundredths: 1300,
            },
            rotation_hundredths: 3300,
            stroke_width_hundredths: 125,
            corner_radius_hundredths: 25,
            ..VectorShape::default()
        };
        let paths = [crate::wpf_vector_shape_geometry(&shape)
            .unwrap()
            .outline()
            .clone()];
        let full = rasterize_ink_paths(&paths, canvas, false, &InkLimits::default(), &NeverCancel)
            .unwrap();
        for area in [rect(0, 0, 8, 9), rect(7, 8, 14, 10), rect(20, 15, 20, 15)] {
            let region = rasterize_ink_paths_region_measured(
                &paths,
                canvas,
                area,
                false,
                &InkLimits::default(),
                &NeverCancel,
            )
            .unwrap();
            assert_eq!(region.coverage, crop(&full, canvas, area), "{kind:?}");
        }
    }
}

#[test]
fn off_region_crossings_are_kept_for_containment_and_winding() {
    let canvas = size(64, 64);
    let area = rect(20, 20, 5, 5);
    let mask = rasterize_ink_paths_region_measured(
        &[rectangle(-100.0, -100.0, 300.0, 300.0)],
        canvas,
        area,
        false,
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(mask.coverage, vec![64; 25]);
    for outside in [false, true] {
        let mask = rasterize_ink_paths_region_measured(
            &[],
            canvas,
            area,
            outside,
            &InkLimits::default(),
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(mask.coverage, vec![if outside { 64 } else { 0 }; 25]);
    }
}

#[test]
fn invalid_regions_points_and_mask_only_memory_are_rejected() {
    let canvas = size(8, 8);
    assert!(
        rasterize_ink_paths_region_measured(
            &[],
            canvas,
            rect(7, 7, 2, 2),
            false,
            &InkLimits::default(),
            &NeverCancel
        )
        .is_err()
    );
    let mut bad = rectangle(1.0, 1.0, 2.0, 2.0);
    bad.figures[0].start.x = f64::NAN;
    assert!(
        rasterize_ink_paths_region_measured(
            &[bad],
            canvas,
            rect(0, 0, 2, 2),
            false,
            &InkLimits::default(),
            &NeverCancel
        )
        .is_err()
    );
    let limits = InkLimits {
        max_bytes: 9,
        ..InkLimits::default()
    };
    assert!(matches!(
        rasterize_ink_paths_region_measured(
            &[],
            canvas,
            rect(0, 0, 3, 3),
            false,
            &limits,
            &NeverCancel
        ),
        Err(InkError::Limit(_))
    ));
}

struct Counter {
    calls: AtomicUsize,
    stop: usize,
}
impl Counter {
    fn new(stop: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            stop,
        }
    }
}
impl CancellationToken for Counter {
    fn is_cancelled(&self) -> bool {
        self.calls.fetch_add(1, Ordering::Relaxed) >= self.stop
    }
}

#[test]
fn regional_actual_work_limit_and_each_cancellation_boundary_are_enforced() {
    let canvas = size(20, 20);
    let area = rect(8, 9, 5, 4);
    let paths = fixtures();
    let counter = Counter::new(usize::MAX);
    let expected = rasterize_ink_paths_region_measured(
        &paths,
        canvas,
        area,
        false,
        &InkLimits::default(),
        &counter,
    )
    .unwrap();
    let exact = InkLimits {
        max_work: expected.work,
        ..InkLimits::default()
    };
    assert_eq!(
        rasterize_ink_paths_region_measured(&paths, canvas, area, false, &exact, &NeverCancel)
            .unwrap()
            .coverage,
        expected.coverage
    );
    let short = InkLimits {
        max_work: expected.work - 1,
        ..exact
    };
    assert!(matches!(
        rasterize_ink_paths_region_measured(&paths, canvas, area, false, &short, &NeverCancel),
        Err(InkError::Limit(_))
    ));
    let count = counter.calls.load(Ordering::Relaxed);
    assert!(count > 10 && count < 10_000);
    for stop in 0..count {
        assert!(
            matches!(
                rasterize_ink_paths_region_measured(
                    &paths,
                    canvas,
                    area,
                    false,
                    &InkLimits::default(),
                    &Counter::new(stop)
                ),
                Err(InkError::Cancelled)
            ),
            "checkpoint {stop}"
        );
    }
}
