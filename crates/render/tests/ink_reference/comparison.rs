use super::{
    Result,
    corpus::{Corpus, Sample},
};
use gif_from_screen_render::{
    InkAttributes, InkError, InkLimits, InkPoint, InkSample, InkStroke, InkTip, NeverCancel,
    fitted_ink_samples,
};
use serde::Serialize;

const ROUTES: [&str; 3] = ["raw_clone", "forced_bezier", "effective"];

#[derive(Serialize)]
pub(super) struct Witness {
    index: usize,
    expected: Sample,
    actual: Sample,
    absolute_error: [f64; 3],
    ulp_distance: [u64; 3],
}

#[derive(Serialize)]
pub(super) struct Comparison {
    pub id: String,
    pub route: &'static str,
    pub expected_count: usize,
    pub actual_count: usize,
    pub bitwise_equal: bool,
    pub numerically_equal: bool,
    pub mismatching_samples: usize,
    pub max_absolute_error: [f64; 3],
    pub max_ulp_distance: [u64; 3],
    witnesses: Vec<Witness>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub(super) struct RouteSummary {
    route: &'static str,
    cases: usize,
    expected_samples: usize,
    actual_samples: usize,
    bitwise_equal_cases: usize,
    count_mismatches: usize,
    errors: usize,
    max_absolute_error: [f64; 3],
    max_ulp_distance: [u64; 3],
}

pub(super) fn compare_corpus(corpus: &Corpus) -> Result<Vec<Comparison>> {
    if corpus.outline_cases.len() != 38 {
        return Err("Expected all 38 Ink cases before comparison.".into());
    }
    let mut reports = Vec::with_capacity(38 * 3);
    // At most 114 calls: <=57M fitting work units, and at most 8MiB transient
    // geometry per call. No output/input image allocation occurs in this test.
    let limits = InkLimits {
        max_points: 4096,
        max_segments: 32768,
        max_work: 500_000,
        max_bytes: 8 * 1024 * 1024,
    };
    for case in &corpus.outline_cases {
        let attributes = &case.stroke.attributes;
        let tip = match attributes.tip.as_str() {
            "Ellipse" => InkTip::Ellipse,
            "Rectangle" => InkTip::Rectangle,
            _ => return Err("Unsupported tip in comparison.".into()),
        };
        let mut stroke = InkStroke {
            samples: case
                .stroke
                .raw_samples
                .iter()
                .map(|sample| InkSample {
                    position: InkPoint {
                        x: sample.x,
                        y: sample.y,
                    },
                    pressure: sample.pressure,
                })
                .collect(),
            attributes: InkAttributes {
                width: attributes.width,
                height: attributes.height,
                tip,
                fit_to_curve: false,
                ignore_pressure: attributes.ignore_pressure,
            },
        };
        for (route, fit, expected) in [
            (ROUTES[0], false, &case.stroke.raw_samples),
            (ROUTES[1], true, &case.stroke.get_bezier_stylus_points),
            (
                ROUTES[2],
                attributes.fit_to_curve,
                &case.stroke.effective_samples,
            ),
        ] {
            stroke.attributes.fit_to_curve = fit;
            reports.push(difference(
                &case.id,
                route,
                expected,
                fitted_ink_samples(&stroke, &limits, &NeverCancel),
            ));
        }
    }
    Ok(reports)
}

fn ordered64(value: f64) -> u64 {
    let bits = value.to_bits();
    if bits >> 63 != 0 {
        !bits
    } else {
        bits | (1 << 63)
    }
}
fn ordered32(value: f32) -> u32 {
    let bits = value.to_bits();
    if bits >> 31 != 0 {
        !bits
    } else {
        bits | (1 << 31)
    }
}

pub(super) fn difference(
    id: &str,
    route: &'static str,
    expected: &[Sample],
    actual: std::result::Result<Vec<InkSample>, InkError>,
) -> Comparison {
    let mut report = Comparison {
        id: id.into(),
        route,
        expected_count: expected.len(),
        actual_count: 0,
        bitwise_equal: false,
        numerically_equal: false,
        mismatching_samples: 0,
        max_absolute_error: [0.0; 3],
        max_ulp_distance: [0; 3],
        witnesses: Vec::new(),
        error: None,
    };
    let actual = match actual {
        Ok(samples) => samples,
        Err(error) => {
            report.error = Some(error.to_string());
            return report;
        }
    };
    report.actual_count = actual.len();
    for (index, (expected, actual)) in expected.iter().zip(&actual).enumerate() {
        let actual = Sample {
            x: actual.position.x,
            y: actual.position.y,
            pressure: actual.pressure,
        };
        if !actual.x.is_finite() || !actual.y.is_finite() || !actual.pressure.is_finite() {
            report.error = Some(format!("Rust returned non-finite sample {index}."));
            return report;
        }
        let absolute = [
            (expected.x - actual.x).abs(),
            (expected.y - actual.y).abs(),
            f64::from((expected.pressure - actual.pressure).abs()),
        ];
        let ulp = [
            ordered64(expected.x).abs_diff(ordered64(actual.x)),
            ordered64(expected.y).abs_diff(ordered64(actual.y)),
            u64::from(ordered32(expected.pressure).abs_diff(ordered32(actual.pressure))),
        ];
        for channel in 0..3 {
            report.max_absolute_error[channel] =
                report.max_absolute_error[channel].max(absolute[channel]);
            report.max_ulp_distance[channel] = report.max_ulp_distance[channel].max(ulp[channel]);
        }
        if ulp != [0; 3] {
            report.mismatching_samples += 1;
            if report.witnesses.len() < 12 {
                report.witnesses.push(Witness {
                    index,
                    expected: *expected,
                    actual,
                    absolute_error: absolute,
                    ulp_distance: ulp,
                });
            }
        }
    }
    report.bitwise_equal = expected.len() == actual.len() && report.max_ulp_distance == [0; 3];
    report.numerically_equal = expected.len() == actual.len()
        && report
            .max_absolute_error
            .iter()
            .all(|error| error.abs() <= 0.0);
    report
}

pub(super) fn summarize(comparisons: &[Comparison]) -> Vec<RouteSummary> {
    ROUTES
        .iter()
        .map(|route| {
            let mut summary = RouteSummary {
                route,
                cases: 0,
                expected_samples: 0,
                actual_samples: 0,
                bitwise_equal_cases: 0,
                count_mismatches: 0,
                errors: 0,
                max_absolute_error: [0.0; 3],
                max_ulp_distance: [0; 3],
            };
            for row in comparisons.iter().filter(|row| row.route == *route) {
                summary.cases += 1;
                summary.expected_samples += row.expected_count;
                summary.actual_samples += row.actual_count;
                summary.bitwise_equal_cases += usize::from(row.bitwise_equal);
                summary.count_mismatches += usize::from(row.expected_count != row.actual_count);
                summary.errors += usize::from(row.error.is_some());
                for channel in 0..3 {
                    summary.max_absolute_error[channel] =
                        summary.max_absolute_error[channel].max(row.max_absolute_error[channel]);
                    summary.max_ulp_distance[channel] =
                        summary.max_ulp_distance[channel].max(row.max_ulp_distance[channel]);
                }
            }
            summary
        })
        .collect()
}
