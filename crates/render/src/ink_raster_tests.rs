use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::{NeverCancel, ink::InkFigure};

fn point(x: f64, y: f64) -> InkPoint {
    InkPoint { x, y }
}
fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
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

fn ellipse(x: f64, y: f64, width: f64, height: f64) -> InkPath {
    let k = 0.552_284_749_830_793_6;
    let (rx, ry) = (width / 2.0, height / 2.0);
    let (cx, cy, right, bottom) = (x + rx, y + ry, x + width, y + height);
    let cubic = |control1, control2, to| InkSegment::CubicTo {
        control1,
        control2,
        to,
    };
    InkPath {
        fill_rule: InkFillRule::NonZero,
        figures: vec![InkFigure {
            start: point(cx, y),
            closed: true,
            segments: vec![
                cubic(point(cx - k * rx, y), point(x, cy - k * ry), point(x, cy)),
                cubic(
                    point(x, cy + k * ry),
                    point(cx - k * rx, bottom),
                    point(cx, bottom),
                ),
                cubic(
                    point(cx + k * rx, bottom),
                    point(right, cy + k * ry),
                    point(right, cy),
                ),
                cubic(
                    point(right, cy - k * ry),
                    point(cx + k * rx, y),
                    point(cx, y),
                ),
            ],
        }],
    }
}

fn coverage(paths: &[InkPath], width: u32, height: u32) -> Vec<u8> {
    rasterize_ink_paths(
        paths,
        size(width, height),
        false,
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap()
}

#[test]
fn integer_and_fractional_rectangles_use_half_open_subpixel_samples() {
    assert_eq!(
        coverage(&[rectangle(1.0, 1.0, 2.0, 2.0)], 4, 4),
        [0, 0, 0, 0, 0, 64, 64, 0, 0, 64, 64, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        coverage(&[rectangle(0.25, 0.5, 1.5, 1.25)], 3, 3),
        [24, 24, 0, 36, 36, 0, 0, 0, 0]
    );
    assert_eq!(
        coverage(&[rectangle(0.03125, 0.0, 1.0, 1.0)], 2, 1),
        [56, 8]
    );
}

#[test]
fn native_half_up_quantization_is_not_bankers_or_away_from_zero() {
    assert_eq!(
        quantize(point(7.78125, -0.03125)).unwrap(),
        Point { x: 125, y: 0 }
    );
    assert_eq!(
        quantize(point(0.03125, -0.09375)).unwrap(),
        Point { x: 1, y: -1 }
    );
    assert_eq!(ceil_div(-5, 2), -2);
    assert_eq!(ceil_div(5, 2), 3);
}

#[test]
fn empty_paths_and_negative_clipping_are_explicit() {
    assert_eq!(coverage(&[], 2, 2), [0; 4]);
    assert_eq!(
        rasterize_ink_paths(&[], size(2, 2), true, &InkLimits::default(), &NeverCancel).unwrap(),
        [64; 4]
    );
    assert_eq!(
        coverage(&[rectangle(-1.0, -1.0, 2.0, 2.0)], 2, 2),
        [64, 0, 0, 0]
    );
    let mut open = rectangle(0.0, 0.0, 1.0, 1.0);
    open.figures[0].closed = false;
    assert_eq!(coverage(&[open], 2, 1), [64, 0]);
}

#[test]
fn independent_paths_union_before_coverage_not_by_alpha_or_winding_sum() {
    let a = rectangle(0.0, 0.0, 0.75, 1.0);
    let mut b = rectangle(0.25, 0.0, 0.75, 1.0);
    let reversed = [
        point(0.25, 1.0),
        point(1.0, 1.0),
        point(1.0, 0.0),
        point(0.25, 0.0),
    ];
    b.figures[0] = polygon(&reversed);
    assert_eq!(coverage(&[a.clone(), a.clone()], 1, 1), [48]);
    assert_eq!(coverage(&[a, b], 1, 1), [64]);
}

#[test]
fn holes_keep_each_paths_fill_rule_and_another_path_can_fill_them() {
    let mut ring = rectangle(0.0, 0.0, 3.0, 3.0);
    ring.figures
        .push(rectangle(1.0, 1.0, 1.0, 1.0).figures.remove(0));
    assert_eq!(coverage(&[ring.clone()], 3, 3), [64; 9]);
    ring.fill_rule = InkFillRule::EvenOdd;
    assert_eq!(
        coverage(&[ring.clone()], 3, 3),
        [64, 64, 64, 64, 0, 64, 64, 64, 64]
    );
    assert_eq!(
        coverage(&[ring.clone(), rectangle(1.0, 1.0, 1.0, 1.0)], 3, 3),
        [64; 9]
    );
    ring.fill_rule = InkFillRule::NonZero;
    ring.figures[1] = polygon(&[
        point(1.0, 2.0),
        point(2.0, 2.0),
        point(2.0, 1.0),
        point(1.0, 1.0),
    ]);
    assert_eq!(coverage(&[ring], 3, 3), [64, 64, 64, 64, 0, 64, 64, 64, 64]);
}

#[test]
fn reference_clipping_uses_coverage64_directly_in_typed_pm_space() {
    let original = RgbaSurface::new(size(2, 1), vec![3, 7, 255, 255, 200, 90, 45, 128]).unwrap();
    let snapshot = clip_ink_reference(
        &original,
        &[rectangle(0.0, 0.0, 0.5, 1.0)],
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(snapshot.pixels(), [2, 4, 128, 128, 100, 45, 23, 128]);
    assert_eq!(original.pixels(), [3, 7, 255, 255, 200, 90, 45, 128]);
    let invisible = RgbaSurface::new(size(1, 1), vec![255, 31, 127, 0]).unwrap();
    assert_eq!(
        clip_ink_reference(&invisible, &[], &InkLimits::default(), &NeverCancel)
            .unwrap()
            .pixels(),
        [0; 4]
    );
}

#[test]
fn ordinary_large_ellipse_uses_64_bit_fallback_instead_of_rejecting_or_fixed_subdivision() {
    let path = ellipse(0.0, 0.0, 256.0, 256.0);
    let result = coverage(&[path], 256, 256);
    assert_eq!(result[0], 0);
    assert_eq!(result[128 * 256 + 128], 64);
    assert!(result.iter().any(|value| (1..64).contains(value)));
    let curve = hfd::Cubic::new([
        Point { x: 0, y: 0 },
        Point { x: 16_000, y: 0 },
        Point { x: 32_000, y: 0 },
        Point { x: 48_000, y: 0 },
    ]);
    assert!(matches!(curve, hfd::Cubic::Large(_)));
    let mut curve = curve;
    let limits = InkLimits::default();
    let mut budget = Budget::new(&limits, &NeverCancel, 0).unwrap();
    assert_eq!(
        curve.next(&mut budget).unwrap(),
        Some(Point { x: 48_000, y: 0 })
    );
    assert_eq!(curve.next(&mut budget).unwrap(), None);
}

#[test]
fn point_segment_work_and_combined_memory_limits_fail_explicitly() {
    let path = rectangle(0.0, 0.0, 2.0, 2.0);
    for limits in [
        InkLimits {
            max_points: 3,
            ..InkLimits::default()
        },
        InkLimits {
            max_segments: 3,
            ..InkLimits::default()
        },
        InkLimits {
            max_work: 3,
            ..InkLimits::default()
        },
        InkLimits {
            max_bytes: 3,
            ..InkLimits::default()
        },
    ] {
        assert!(matches!(
            rasterize_ink_paths(
                std::slice::from_ref(&path),
                size(2, 2),
                false,
                &limits,
                &NeverCancel
            ),
            Err(InkError::Limit(_))
        ));
    }
    let source = RgbaSurface::new(size(1, 1), vec![0; 4]).unwrap();
    assert!(matches!(
        clip_ink_reference(
            &source,
            &[],
            &InkLimits {
                max_bytes: 8,
                ..InkLimits::default()
            },
            &NeverCancel
        ),
        Err(InkError::Limit(_))
    ));
    assert!(matches!(
        rasterize_ink_paths(
            &[],
            size(1, u32::MAX),
            false,
            &InkLimits::default(),
            &NeverCancel
        ),
        Err(InkError::Limit(_))
    ));
}

#[test]
fn nonfinite_and_out_of_range_coordinates_cannot_overflow_or_be_silently_skipped() {
    for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            rasterize_ink_paths(
                &[rectangle(x, 0.0, 1.0, 1.0)],
                size(1, 1),
                false,
                &InkLimits::default(),
                &NeverCancel
            ),
            Err(InkError::Invalid(_))
        ));
    }
    for x in [f64::MAX, -f64::MAX, 600_000.0, -600_000.0] {
        assert!(matches!(
            rasterize_ink_paths(
                &[rectangle(x, 0.0, 1.0, 1.0)],
                size(1, 1),
                false,
                &InkLimits::default(),
                &NeverCancel
            ),
            Err(InkError::Limit(_))
        ));
    }
}

struct CancelAfter {
    calls: AtomicUsize,
    after: usize,
}
impl CancellationToken for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.calls.fetch_add(1, Ordering::Relaxed) >= self.after
    }
}

#[test]
fn cancellation_interrupts_preparation_sort_scans_and_shader_without_mutation() {
    let source = RgbaSurface::new(size(64, 64), [51, 127, 211, 128].repeat(64 * 64)).unwrap();
    let paths = [
        ellipse(-10.0, -10.0, 80.0, 80.0),
        rectangle(30.0, 20.0, 15.0, 15.0),
    ];
    let before = source.clone();
    for after in [0, 20, 200, 2_000] {
        let token = CancelAfter {
            calls: AtomicUsize::new(0),
            after,
        };
        assert!(matches!(
            clip_ink_reference(&source, &paths, &InkLimits::default(), &token),
            Err(InkError::Cancelled)
        ));
        assert_eq!(source, before);
    }
}

#[test]
fn native_small_ellipse_coverage_matches_recorded_windows_case() {
    // Live-region counts are 64 minus Windows run 34078589472's white clip
    // coverage.c64, ellipse-opaque-opaque. Input is independently built here.
    let rows = [
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 1, 28, 51, 56, 52, 28, 1, 0, 0, 0],
        [0, 0, 38, 64, 64, 64, 64, 64, 41, 0, 0, 0],
        [0, 0, 57, 64, 64, 64, 64, 64, 63, 0, 0, 0],
        [0, 0, 43, 64, 64, 64, 64, 64, 46, 0, 0, 0],
        [0, 0, 3, 36, 59, 64, 60, 36, 3, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ];
    assert_eq!(
        coverage(&[ellipse(2.0, 2.0, 7.0, 5.0)], 12, 10),
        rows.concat()
    );
}

#[test]
fn native_fractional_ellipse_preserves_quantization_and_adaptive_curve_shape() {
    // Same Windows run, ellipse_fractional-opaque-opaque. These are coverage
    // counts, not final pixels copied into the producer.
    let rows = [
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 20, 31, 28, 11, 0, 0, 0, 0],
        [0, 0, 1, 48, 64, 64, 64, 63, 26, 0, 0, 0],
        [0, 0, 31, 64, 64, 64, 64, 64, 63, 7, 0, 0],
        [0, 0, 48, 64, 64, 64, 64, 64, 64, 22, 0, 0],
        [0, 0, 40, 64, 64, 64, 64, 64, 64, 14, 0, 0],
        [0, 0, 9, 61, 64, 64, 64, 64, 46, 0, 0, 0],
        [0, 0, 0, 11, 44, 55, 52, 34, 3, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ];
    assert_eq!(
        coverage(&[ellipse(2.125, 1.375, 7.25, 6.5)], 12, 10),
        rows.concat()
    );
}

#[test]
fn both_hfd_branches_match_the_original_native_integer_flattener() {
    use sha2::{Digest, Sha256};
    // Mechanical algorithm evidence, not a new Windows rendering claim: exact
    // upstream MIT bezier.h/cpp at a04736ac, compiled with a tiny portable
    // integer-type/SAL harness. Hash packed little-endian i32 x/y outputs.
    let cases = [
        (
            [[88, 32], [57, 32], [32, 50], [32, 72]],
            4,
            "de718968369ed3f71e6d23486812c450ca3727f8a1ee7ab0e088b0ab2f52fbf7",
        ),
        (
            [[0, 0], [0, 4096], [4096, 4096], [4096, 0]],
            32,
            "1e08fbcf9380d656e57a9b8161720c4afd7aeacef67d787d610a9ba13f13b5ce",
        ),
        (
            [
                [1000, -3000],
                [-40000, 16000],
                [30000, -20000],
                [12000, 36000],
            ],
            182,
            "5faab0073127224b0d463ac5e72f97c14ccee41dbb6fcd6866ce1826008e11e5",
        ),
        (
            [
                [0, 0],
                [0, 1_200_000],
                [1_200_000, 1_200_000],
                [1_200_000, 0],
            ],
            1024,
            "39c911fcde9e294290f34d0552bd8956626df6f75f025a38f9b728219147c859",
        ),
        (
            [[0, 0], [16000, 0], [32000, 0], [48000, 0]],
            1,
            "1d59dd4b6268e8cd3bb60a98fa99c6761147312b10f2ddff786e96139bcf0796",
        ),
    ];
    let limits = InkLimits::default();
    for (points, count, expected) in cases {
        let mut curve = hfd::Cubic::new(points.map(|[x, y]| Point { x, y }));
        let mut budget = Budget::new(&limits, &NeverCancel, 0).unwrap();
        let mut digest = Sha256::new();
        let mut emitted = 0;
        while let Some(point) = curve.next(&mut budget).unwrap() {
            digest.update(point.x.to_le_bytes());
            digest.update(point.y.to_le_bytes());
            emitted += 1;
        }
        assert_eq!(emitted, count);
        assert_eq!(format!("{:x}", digest.finalize()), expected);
    }
}

#[test]
fn deterministic_fractional_rectangle_unions_match_independent_sample_membership() {
    let mut state = 0x1926_2026_u32;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };
    for _ in 0..128 {
        let mut rectangles = Vec::new();
        for _ in 0..4 {
            let x = (f64::from(next() % 320) - 64.0) / 32.0;
            let y = (f64::from(next() % 320) - 64.0) / 32.0;
            let width = f64::from(next() % 192 + 1) / 32.0;
            let height = f64::from(next() % 192 + 1) / 32.0;
            rectangles.push((x, y, width, height));
        }
        let paths = rectangles
            .iter()
            .map(|&(x, y, w, h)| rectangle(x, y, w, h))
            .collect::<Vec<_>>();
        let expected = (0..64)
            .map(|pixel| {
                let mut covered = 0;
                for sy in 0..8 {
                    for sx in 0..8 {
                        let x16 = f64::from((pixel % 8) * 16 + sx * 2);
                        let y16 = f64::from((pixel / 8) * 16 + sy * 2);
                        covered += u8::from(rectangles.iter().any(|&(x, y, w, h)| {
                            x16 >= (16.0 * x + 0.5).floor()
                                && x16 < (16.0 * (x + w) + 0.5).floor()
                                && y16 >= (16.0 * y + 0.5).floor()
                                && y16 < (16.0 * (y + h) + 0.5).floor()
                        }));
                    }
                }
                covered
            })
            .collect::<Vec<_>>();
        assert_eq!(coverage(&paths, 8, 8), expected);
    }
}

#[test]
fn flattened_curve_segments_are_budgeted_even_when_the_viewport_is_tiny() {
    let limits = InkLimits {
        max_segments: 4,
        ..InkLimits::default()
    };
    assert!(matches!(
        rasterize_ink_paths(
            &[ellipse(-128.0, -128.0, 256.0, 256.0)],
            size(1, 1),
            false,
            &limits,
            &NeverCancel
        ),
        Err(InkError::Limit(_))
    ));
}

#[test]
fn measured_raster_preserves_pixels_and_reports_the_exact_exhaustion_boundary() {
    let limits = InkLimits::default();
    for paths in [
        Vec::new(),
        vec![rectangle(0.25, 1.75, 8.5, 11.0)],
        vec![
            ellipse(-2.0, -1.0, 18.0, 15.0),
            rectangle(3.0, 4.0, 4.0, 3.0),
        ],
    ] {
        for outside in [false, true] {
            let original =
                rasterize_ink_paths(&paths, size(16, 16), outside, &limits, &NeverCancel).unwrap();
            let (actual, work) =
                rasterize_ink_paths_measured(&paths, size(16, 16), outside, &limits, &NeverCancel)
                    .unwrap();
            assert_eq!(actual, original);
            assert!(work > 0 && work < limits.max_work);
            let exact = InkLimits {
                max_work: work,
                ..limits
            };
            let (again, charged) =
                rasterize_ink_paths_measured(&paths, size(16, 16), outside, &exact, &NeverCancel)
                    .unwrap();
            assert_eq!(again, actual);
            assert_eq!(charged, work);
            let short = InkLimits {
                max_work: work - 1,
                ..limits
            };
            assert!(matches!(
                rasterize_ink_paths_measured(&paths, size(16, 16), outside, &short, &NeverCancel),
                Err(InkError::Limit(_))
            ));
        }
    }
}

#[test]
fn raster_meter_grows_with_performed_work_and_keeps_zero_limits_and_cancellation() {
    let limits = InkLimits::default();
    let (_, small) =
        rasterize_ink_paths_measured(&[], size(4, 4), false, &limits, &NeverCancel).unwrap();
    let (_, large) =
        rasterize_ink_paths_measured(&[], size(8, 8), false, &limits, &NeverCancel).unwrap();
    assert!(large > small);
    let paths = [rectangle(1.0, 1.0, 3.0, 3.0)];
    let (_, populated) =
        rasterize_ink_paths_measured(&paths, size(8, 8), false, &limits, &NeverCancel).unwrap();
    assert!(populated > large);
    let zero = InkLimits {
        max_work: 0,
        ..limits
    };
    assert!(matches!(
        rasterize_ink_paths_measured(&paths, size(8, 8), false, &zero, &NeverCancel),
        Err(InkError::Limit(_))
    ));
    for after in [0, 5, 30] {
        let old = CancelAfter {
            calls: AtomicUsize::new(0),
            after,
        };
        let measured = CancelAfter {
            calls: AtomicUsize::new(0),
            after,
        };
        let expected = rasterize_ink_paths(&paths, size(16, 16), false, &limits, &old);
        let result = rasterize_ink_paths_measured(&paths, size(16, 16), false, &limits, &measured)
            .map(|(pixels, _)| pixels);
        assert!(matches!(result, Err(InkError::Cancelled)));
        assert_eq!(result, expected);
        assert_eq!(
            old.calls.load(Ordering::Relaxed),
            measured.calls.load(Ordering::Relaxed)
        );
    }
}
