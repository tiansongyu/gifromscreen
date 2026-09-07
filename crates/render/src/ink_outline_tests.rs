use super::*;
use crate::{InkPoint, NeverCancel};
use std::sync::atomic::{AtomicUsize, Ordering};

fn sample(x: f64, y: f64, pressure: f32) -> InkSample {
    InkSample {
        position: InkPoint { x, y },
        pressure,
    }
}
fn stroke(tip: InkTip, samples: Vec<InkSample>) -> InkStroke {
    InkStroke {
        samples,
        attributes: InkAttributes {
            width: 4.0,
            height: 2.0,
            tip,
            ..InkAttributes::default()
        },
    }
}
fn bounds(path: &InkPath) -> [f64; 4] {
    let mut result = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut include = |point: InkPoint| {
        result[0] = result[0].min(point.x);
        result[1] = result[1].min(point.y);
        result[2] = result[2].max(point.x);
        result[3] = result[3].max(point.y);
    };
    for figure in &path.figures {
        include(figure.start);
        for segment in &figure.segments {
            match segment {
                InkSegment::LineTo(point) => include(*point),
                InkSegment::CubicTo {
                    control1,
                    control2,
                    to,
                } => {
                    include(*control1);
                    include(*control2);
                    include(*to);
                }
            }
        }
    }
    result
}
fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-10 * (1.0 + expected.abs()),
        "{actual} != {expected}"
    );
}
fn outline(stroke: &InkStroke) -> InkPath {
    outline_ink_stroke(stroke, &InkLimits::default(), &NeverCancel).unwrap()
}

#[test]
fn pressure_is_the_wpf_float32_scale_and_ignore_pressure_uses_unscaled_tip() {
    for tip in [InkTip::Ellipse, InkTip::Rectangle] {
        for (pressure, scale) in [(0.0, 0.25), (0.5, 1.0), (1.0, 1.75)] {
            let mut pen = stroke(tip, vec![sample(-1.25, 2.5, pressure)]);
            let output = outline(&pen);
            let actual = bounds(&output);
            for (a, b) in actual.into_iter().zip([
                -1.25 - 2.0 * scale,
                2.5 - scale,
                -1.25 + 2.0 * scale,
                2.5 + scale,
            ]) {
                near(a, b);
            }
            assert_eq!(output.fill_rule, InkFillRule::NonZero);
            assert!(output.figures[0].closed);
            pen.attributes.ignore_pressure = true;
            for (actual, expected) in bounds(&outline(&pen))
                .into_iter()
                .zip([-3.25, 1.5, 0.75, 3.5])
            {
                near(actual, expected);
            }
        }
    }
    let pressure = 0.6_f32;
    let node = Node::new(sample(0.0, 0.0, pressure), InkAttributes::default());
    assert_eq!(
        node.pressure.to_bits(),
        f64::from(1.5_f32 * pressure + 0.25_f32).to_bits()
    );
}

#[test]
fn ellipse_uses_four_reference_cubics_not_a_polygon() {
    let pen = stroke(InkTip::Ellipse, vec![sample(0.0, 0.0, 0.5)]);
    let path = outline(&pen);
    assert_eq!(path.figures.len(), 1);
    assert_eq!(path.figures[0].segments.len(), 4);
    assert_eq!(path.figures[0].start, InkPoint { x: -2.0, y: 0.0 });
    let InkSegment::CubicTo {
        control1,
        control2,
        to,
    } = path.figures[0].segments[0]
    else {
        panic!("ellipse must use cubics")
    };
    near(control1.x, -2.0);
    near(control1.y, -ELLIPSE_K);
    near(control2.x, -2.0 * ELLIPSE_K);
    near(control2.y, -1.0);
    assert_eq!(to, InkPoint { x: 0.0, y: -1.0 });
}

#[test]
fn variable_pressure_rect_and_ellipse_sweeps_do_not_use_maximum_width_everywhere() {
    for tip in [InkTip::Rectangle, InkTip::Ellipse] {
        let pen = stroke(tip, vec![sample(0.0, 0.0, 0.0), sample(10.0, 0.0, 1.0)]);
        let path = outline(&pen);
        let actual = bounds(&path);
        near(actual[0], -0.5);
        near(actual[2], 13.5);
        near(actual[1], -1.75);
        near(actual[3], 1.75);
        if tip == InkTip::Ellipse {
            assert_eq!(path.figures.len(), 3);
            assert!(
                path.figures[2]
                    .segments
                    .iter()
                    .all(|segment| matches!(segment, InkSegment::LineTo(_)))
            );
        } else {
            assert_eq!(path.figures.len(), 2);
            assert!(path.figures[1].segments.len() >= 4);
        }
    }
}

#[test]
fn circular_connecting_contour_matches_common_tangents_and_containment_is_safe() {
    let attributes = InkAttributes {
        width: 2.0,
        height: 2.0,
        ..InkAttributes::default()
    };
    let first = Node::new(sample(0.0, 0.0, 0.0), attributes);
    let last = Node::new(sample(10.0, 0.0, 1.0), attributes);
    let quad = ellipse_quad(first, last, attributes).unwrap().unwrap();
    near(quad[0].x, -0.0375);
    near(quad[0].y, -0.25 * (1.0_f64 - 0.15 * 0.15).sqrt());
    near(quad[1].x, 9.7375);
    near(quad[1].y, -1.75 * (1.0_f64 - 0.15 * 0.15).sqrt());
    assert!(
        ellipse_quad(
            first,
            Node::new(sample(0.5, 0.0, 1.0), attributes),
            attributes
        )
        .unwrap()
        .is_none()
    );
    assert!(
        ellipse_quad(
            first,
            Node::new(sample(0.0, 0.0, 1.0), attributes),
            attributes
        )
        .unwrap()
        .is_none()
    );
    for tip in [InkTip::Rectangle, InkTip::Ellipse] {
        let pen = stroke(tip, vec![sample(2.0, 3.0, 0.0), sample(2.0, 3.0, 1.0)]);
        for (actual, expected) in bounds(&outline(&pen))
            .into_iter()
            .zip([-1.5, 1.25, 5.5, 4.75])
        {
            near(actual, expected);
        }
    }
}

#[test]
fn rectangle_hulls_keep_consistent_nonzero_winding_and_true_endpoints() {
    for (x, y) in [
        (10.0, 4.0),
        (-10.0, 4.0),
        (-10.0, -4.0),
        (10.0, -4.0),
        (0.0, 0.0),
    ] {
        let pen = stroke(
            InkTip::Rectangle,
            vec![sample(0.0, 0.0, 0.5), sample(x, y, 0.5)],
        );
        let path = outline(&pen);
        for figure in path.figures {
            let mut last = figure.start;
            let mut twice_area = 0.0;
            for segment in figure.segments {
                let InkSegment::LineTo(point) = segment else {
                    panic!("rectangle curve")
                };
                twice_area += last.x * point.y - last.y * point.x;
                last = point;
            }
            assert!(twice_area > 0.0);
        }
    }
}

#[test]
fn raw_single_and_duplicate_fitted_samples_preserve_pressure() {
    let mut pen = stroke(InkTip::Ellipse, vec![sample(0.25, -2.0, 0.1)]);
    pen.attributes.fit_to_curve = true;
    assert_eq!(
        fitted_ink_samples(&pen, &InkLimits::default(), &NeverCancel).unwrap(),
        pen.samples
    );
    pen.samples.push(sample(0.25, -2.0, 0.9));
    assert_eq!(
        fitted_ink_samples(&pen, &InkLimits::default(), &NeverCancel).unwrap(),
        pen.samples
    );
    pen.attributes.fit_to_curve = false;
    pen.samples.push(sample(8.0, 9.0, 0.5));
    assert_eq!(
        fitted_ink_samples(&pen, &InkLimits::default(), &NeverCancel).unwrap(),
        pen.samples
    );
}

#[test]
fn actual_bezier_fitting_handles_lines_parabolas_curves_and_cusps() {
    for points in [
        vec![(0.0, 0.0), (10.0, 0.0)],
        vec![(0.0, 0.0), (3.0, 4.0), (9.0, 0.0)],
        vec![(0.0, 0.0), (2.0, 3.0), (4.0, 4.0), (6.0, 3.0), (8.0, 0.0)],
        vec![
            (0.0, 0.0),
            (2.0, 0.0),
            (4.0, 0.0),
            (2.0, 0.0),
            (0.0, 0.0),
            (-2.0, 1.0),
        ],
    ] {
        let count = points.len();
        let samples = points
            .into_iter()
            .enumerate()
            .map(|(i, (x, y))| {
                sample(
                    x,
                    y,
                    f32::from(u16::try_from(i).unwrap())
                        / f32::from(u16::try_from(count - 1).unwrap()),
                )
            })
            .collect();
        let mut pen = stroke(InkTip::Ellipse, samples);
        pen.attributes.fit_to_curve = true;
        let fitted = fitted_ink_samples(&pen, &InkLimits::default(), &NeverCancel).unwrap();
        assert!(fitted.len() >= 2);
        near(fitted[0].position.x, pen.samples[0].position.x);
        near(fitted[0].position.y, pen.samples[0].position.y);
        let last = *fitted.last().unwrap();
        let original = *pen.samples.last().unwrap();
        near(last.position.x, original.position.x);
        near(last.position.y, original.position.y);
        assert_eq!(fitted[0].pressure.to_bits(), 0.0_f32.to_bits());
        assert_eq!(last.pressure.to_bits(), 1.0_f32.to_bits());
        assert!(fitted.iter().all(|sample| sample.position.x.is_finite()
            && sample.position.y.is_finite()
            && sample.pressure.is_finite()
            && (0.0..=1.0).contains(&sample.pressure)));
        assert!(!outline(&pen).figures.is_empty());
    }
}

#[test]
fn invalid_numeric_input_is_rejected_without_clamping_infinity() {
    let good = stroke(InkTip::Ellipse, vec![sample(0.0, 0.0, 0.5)]);
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        let mut pen = good.clone();
        pen.samples[0].position.x = value;
        assert!(matches!(
            outline_ink_stroke(&pen, &InkLimits::default(), &NeverCancel),
            Err(InkError::Invalid(_))
        ));
    }
    for value in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        let mut pen = good.clone();
        pen.samples[0].pressure = value;
        assert!(fitted_ink_samples(&pen, &InkLimits::default(), &NeverCancel).is_err());
    }
    for width in [f64::NAN, f64::INFINITY, 0.0, 100.1] {
        let mut pen = good.clone();
        pen.attributes.width = width;
        assert!(outline_ink_stroke(&pen, &InkLimits::default(), &NeverCancel).is_err());
    }
    let mut empty = good;
    empty.samples.clear();
    assert!(outline_ink_stroke(&empty, &InkLimits::default(), &NeverCancel).is_err());
}

struct CancelAfter(AtomicUsize, usize);
impl CancellationToken for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.0.fetch_add(1, Ordering::Relaxed) >= self.1
    }
}

#[test]
fn points_segments_bytes_work_and_mid_fit_cancellation_are_all_bounded() {
    let pen = stroke(
        InkTip::Ellipse,
        vec![sample(0.0, 0.0, 0.5), sample(4.0, 1.0, 0.7)],
    );
    for limits in [
        InkLimits {
            max_points: 1,
            ..InkLimits::default()
        },
        InkLimits {
            max_segments: 3,
            ..InkLimits::default()
        },
        InkLimits {
            max_bytes: 1,
            ..InkLimits::default()
        },
        InkLimits {
            max_work: 1,
            ..InkLimits::default()
        },
    ] {
        assert!(matches!(
            outline_ink_stroke(&pen, &limits, &NeverCancel),
            Err(InkError::Limit(_))
        ));
    }
    let mut fitted = pen;
    fitted.attributes.fit_to_curve = true;
    fitted.samples = (0..500)
        .map(|index| sample(f64::from(index), f64::from(index % 13), 0.5))
        .collect();
    for after in [0, 500, 1500] {
        let cancel = CancelAfter(AtomicUsize::new(0), after);
        assert!(matches!(
            fitted_ink_samples(&fitted, &InkLimits::default(), &cancel),
            Err(InkError::Cancelled)
        ));
    }
    assert!(matches!(
        fitted_ink_samples(
            &fitted,
            &InkLimits {
                max_work: 1000,
                ..InkLimits::default()
            },
            &NeverCancel
        ),
        Err(InkError::Limit(_))
    ));
}

#[test]
fn batch_budgets_are_shared_and_empty_members_are_not_discarded() {
    let first = stroke(InkTip::Ellipse, vec![sample(0.0, 0.0, 0.5)]);
    let second = stroke(InkTip::Rectangle, vec![sample(10.0, 0.0, 0.5)]);
    let output = outline_ink_strokes(
        &[first.clone(), second.clone()],
        &InkLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(output, [outline(&first), outline(&second)]);
    for limits in [
        InkLimits {
            max_segments: 4,
            ..InkLimits::default()
        },
        InkLimits {
            max_points: 1,
            ..InkLimits::default()
        },
        InkLimits {
            max_work: 10,
            ..InkLimits::default()
        },
    ] {
        assert!(outline_ink_stroke(&first, &limits, &NeverCancel).is_ok());
        assert!(matches!(
            outline_ink_strokes(&[first.clone(), second.clone()], &limits, &NeverCancel),
            Err(InkError::Limit(_))
        ));
    }
    let mut empty = second;
    empty.samples.clear();
    assert!(matches!(
        outline_ink_strokes(&[first.clone(), empty], &InkLimits::default(), &NeverCancel),
        Err(InkError::Invalid(_))
    ));
    assert!(matches!(
        outline_ink_strokes(&vec![first; 257], &InkLimits::default(), &NeverCancel),
        Err(InkError::Limit(_))
    ));
    assert!(
        outline_ink_strokes(&[], &InkLimits::default(), &NeverCancel)
            .unwrap()
            .is_empty()
    );
}
