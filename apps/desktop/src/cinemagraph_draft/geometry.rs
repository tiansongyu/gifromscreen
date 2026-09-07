//! Continuous swept-tip hit testing and stroke splitting, not sample-point deletion.
//!
//! Rectangle tips use exact convex clipping. Ellipse hit testing uses an inscribed
//! 64-sided polygon; this is an explicit editing approximation, not a claim of WPF
//! eraser equivalence. Final painted outlines use the renderer's cubic geometry.

use std::collections::BTreeSet;

use gif_from_screen_render::{InkError, InkLimits, NeverCancel, fitted_ink_samples};

use super::{
    DraftStroke, InkAttributes, InkPoint, InkSample, InkStroke, InkTip, MAX_CINEMAGRAPH_SAMPLES,
    MAX_CINEMAGRAPH_STROKES,
};

const ELLIPSE_SIDES: u32 = 64;
const MAX_WORK: usize = 4_000_000;
const EPSILON: f64 = 1.0e-10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InkBounds {
    pub(crate) min: InkPoint,
    pub(crate) max: InkPoint,
}

impl InkBounds {
    fn include(&mut self, other: Self) {
        self.min.x = self.min.x.min(other.min.x);
        self.min.y = self.min.y.min(other.min.y);
        self.max.x = self.max.x.max(other.max.x);
        self.max.y = self.max.y.max(other.max.y);
    }
}

pub(super) fn finite(point: InkPoint) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

fn pressure(sample: InkSample, attributes: InkAttributes) -> f64 {
    if attributes.ignore_pressure {
        1.0
    } else {
        f64::from(1.5_f32 * sample.pressure + 0.25_f32)
    }
}

fn fitted(stroke: &InkStroke, work: &mut usize) -> Result<Vec<InkSample>, String> {
    // Reserve a per-stroke allowance from the SAME gesture-update
    // budget. Otherwise 256 individually bounded fits could multiply the work
    // limit while the UI waits for one pointer event.
    let allowance = stroke
        .samples
        .len()
        .checked_mul(if stroke.attributes.fit_to_curve {
            128
        } else {
            2
        })
        .and_then(|count| count.checked_add(1_024))
        .ok_or("Ink fitting work accounting overflowed.")?;
    spend(work, allowance)?;
    fitted_ink_samples(
        stroke,
        &InkLimits {
            max_points: MAX_CINEMAGRAPH_SAMPLES,
            max_segments: MAX_CINEMAGRAPH_SAMPLES,
            max_work: allowance as u64,
            max_bytes: 1024 * 1024,
        },
        &NeverCancel,
    )
    .map_err(|error| match error {
        InkError::Limit(reason) => format!(
            "This stroke is too complex for bounded interactive editing ({reason}). Simplify the draft or use Select all / Delete to remove it."
        ),
        other => other.to_string(),
    })
}

pub(super) fn bounds<'a>(
    strokes: impl Iterator<Item = &'a InkStroke>,
) -> Result<Option<InkBounds>, String> {
    let mut bounds: Option<InkBounds> = None;
    let mut count = 0usize;
    let mut work = 0usize;
    for stroke in strokes {
        let samples = fitted(stroke, &mut work)?;
        count += samples.len();
        if count > MAX_CINEMAGRAPH_SAMPLES {
            return Err("Fitted ink exceeds the draft sample limit.".to_owned());
        }
        for sample in samples {
            let half_width = stroke.attributes.width * pressure(sample, stroke.attributes) / 2.0;
            let half_height = stroke.attributes.height * pressure(sample, stroke.attributes) / 2.0;
            let next = InkBounds {
                min: InkPoint {
                    x: sample.position.x - half_width,
                    y: sample.position.y - half_height,
                },
                max: InkPoint {
                    x: sample.position.x + half_width,
                    y: sample.position.y + half_height,
                },
            };
            if let Some(bounds) = &mut bounds {
                bounds.include(next);
            } else {
                bounds = Some(next);
            }
        }
    }
    Ok(bounds)
}

/// Each retained original id maps to zero or more pieces. Only the caller allocates new ids.
pub(super) fn erase(
    strokes: &[DraftStroke],
    previous: InkPoint,
    current: InkPoint,
    eraser: InkAttributes,
    whole: bool,
) -> Result<Vec<(u64, Vec<InkStroke>)>, String> {
    let shape = swept_tip(previous, current, eraser);
    let mut work = 0usize;
    let mut result = Vec::with_capacity(strokes.len());
    let mut sample_count = 0usize;
    let mut stroke_count = 0usize;
    for stroke in strokes {
        let samples = fitted(&stroke.stroke, &mut work)?;
        spend(&mut work, samples.len())?;
        let clip = Clip::new(&shape, stroke.stroke.attributes, &mut work)?;
        let (fragments, touched) = if whole {
            (
                Vec::new(),
                intersects(&samples, stroke.stroke.attributes, &clip, &mut work)?,
            )
        } else {
            split(&samples, stroke.stroke.attributes, &clip, &mut work)?
        };
        let pieces = if !touched {
            vec![stroke.stroke.clone()]
        } else if whole {
            Vec::new()
        } else {
            fragments
                .into_iter()
                .map(|samples| InkStroke {
                    samples,
                    attributes: InkAttributes {
                        fit_to_curve: false,
                        ..stroke.stroke.attributes
                    },
                })
                .collect::<Vec<_>>()
        };
        sample_count += pieces
            .iter()
            .map(|piece| piece.samples.len())
            .sum::<usize>();
        stroke_count += pieces.len();
        if sample_count > MAX_CINEMAGRAPH_SAMPLES || stroke_count > MAX_CINEMAGRAPH_STROKES {
            return Err("Erasing would exceed the bounded stroke or sample limit.".to_owned());
        }
        result.push((stroke.id, pieces));
    }
    Ok(result)
}

pub(super) fn select(
    strokes: &[DraftStroke],
    start: InkPoint,
    end: InkPoint,
) -> Result<BTreeSet<u64>, String> {
    let click = (start.x - end.x).abs() < EPSILON && (start.y - end.y).abs() < EPSILON;
    let shape = rectangle(
        start.x.min(end.x) - if click { 0.5 } else { 0.0 },
        start.y.min(end.y) - if click { 0.5 } else { 0.0 },
        start.x.max(end.x) + if click { 0.5 } else { EPSILON },
        start.y.max(end.y) + if click { 0.5 } else { EPSILON },
    );
    let mut selected = BTreeSet::new();
    let mut work = 0usize;
    for stroke in strokes {
        let samples = fitted(&stroke.stroke, &mut work)?;
        spend(&mut work, samples.len())?;
        let clip = Clip::new(&shape, stroke.stroke.attributes, &mut work)?;
        let hit = intersects(&samples, stroke.stroke.attributes, &clip, &mut work)?;
        if hit {
            if click {
                selected.clear();
            }
            selected.insert(stroke.id);
        }
    }
    Ok(selected)
}

fn tip(attributes: InkAttributes) -> Vec<InkPoint> {
    let x = attributes.width / 2.0;
    let y = attributes.height / 2.0;
    if attributes.tip == InkTip::Rectangle {
        return rectangle(-x, -y, x, y);
    }
    (0..ELLIPSE_SIDES)
        .map(|index| {
            let angle = f64::from(index) * std::f64::consts::TAU / f64::from(ELLIPSE_SIDES);
            InkPoint {
                x: x * angle.cos(),
                y: y * angle.sin(),
            }
        })
        .collect()
}

fn intersects(
    samples: &[InkSample],
    attributes: InkAttributes,
    clip: &Clip,
    work: &mut usize,
) -> Result<bool, String> {
    if samples.len() == 1 {
        return Ok(clip
            .interval(samples[0], samples[0], attributes, work)?
            .is_some());
    }
    for pair in samples.windows(2) {
        if clip.interval(pair[0], pair[1], attributes, work)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn rectangle(left: f64, top: f64, right: f64, bottom: f64) -> Vec<InkPoint> {
    vec![
        InkPoint { x: left, y: top },
        InkPoint { x: right, y: top },
        InkPoint {
            x: right,
            y: bottom,
        },
        InkPoint { x: left, y: bottom },
    ]
}

fn swept_tip(start: InkPoint, end: InkPoint, attributes: InkAttributes) -> Vec<InkPoint> {
    let mut points = Vec::with_capacity(2 * ELLIPSE_SIDES as usize);
    for point in tip(attributes) {
        points.push(InkPoint {
            x: start.x + point.x,
            y: start.y + point.y,
        });
        points.push(InkPoint {
            x: end.x + point.x,
            y: end.y + point.y,
        });
    }
    convex_hull(points)
}

fn cross(a: InkPoint, b: InkPoint, c: InkPoint) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn convex_hull(mut points: Vec<InkPoint>) -> Vec<InkPoint> {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    points.dedup();
    let mut hull = Vec::with_capacity(points.len() * 2);
    for point in &points {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], *point) <= 0.0 {
            hull.pop();
        }
        hull.push(*point);
    }
    let lower = hull.len();
    for point in points.iter().rev().skip(1) {
        while hull.len() > lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], *point) <= 0.0
        {
            hull.pop();
        }
        hull.push(*point);
    }
    hull.pop();
    hull
}

struct Axis {
    direction: InkPoint,
    minimum: f64,
    maximum: f64,
    support: f64,
}

struct Clip {
    axes: Vec<Axis>,
}

impl Clip {
    fn new(
        shape: &[InkPoint],
        attributes: InkAttributes,
        work: &mut usize,
    ) -> Result<Self, String> {
        let tip = tip(attributes);
        let mut axes = Vec::with_capacity(shape.len() + tip.len());
        for polygon in [shape, tip.as_slice()] {
            for (index, point) in polygon.iter().enumerate() {
                let next = polygon[(index + 1) % polygon.len()];
                let direction = InkPoint {
                    x: point.y - next.y,
                    y: next.x - point.x,
                };
                if direction.x.abs() + direction.y.abs() < EPSILON {
                    continue;
                }
                spend(work, shape.len() + tip.len())?;
                let (minimum, maximum) = projection(shape, direction);
                let (_, support) = projection(&tip, direction);
                axes.push(Axis {
                    direction,
                    minimum,
                    maximum,
                    support,
                });
            }
        }
        Ok(Self { axes })
    }

    fn interval(
        &self,
        a: InkSample,
        b: InkSample,
        attributes: InkAttributes,
        work: &mut usize,
    ) -> Result<Option<(f64, f64)>, String> {
        spend(work, self.axes.len())?;
        let mut interval = (0.0, 1.0);
        let pa = pressure(a, attributes);
        let pb = pressure(b, attributes);
        for axis in &self.axes {
            let ca = dot(a.position, axis.direction);
            let cb = dot(b.position, axis.direction);
            if !at_least_zero(
                ca + pa * axis.support - axis.minimum,
                cb + pb * axis.support - axis.minimum,
                &mut interval,
            ) || !at_least_zero(
                axis.maximum - ca + pa * axis.support,
                axis.maximum - cb + pb * axis.support,
                &mut interval,
            ) {
                return Ok(None);
            }
        }
        Ok(Some(interval))
    }
}

fn projection(points: &[InkPoint], direction: InkPoint) -> (f64, f64) {
    points.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(minimum, maximum), point| {
            let value = dot(*point, direction);
            (minimum.min(value), maximum.max(value))
        },
    )
}

fn dot(a: InkPoint, b: InkPoint) -> f64 {
    a.x * b.x + a.y * b.y
}

fn at_least_zero(a: f64, b: f64, interval: &mut (f64, f64)) -> bool {
    if a >= 0.0 && b >= 0.0 {
        return true;
    }
    if a < 0.0 && b < 0.0 {
        return false;
    }
    let crossing = a / (a - b);
    if a < 0.0 {
        interval.0 = interval.0.max(crossing);
    } else {
        interval.1 = interval.1.min(crossing);
    }
    interval.0 <= interval.1
}

fn split(
    samples: &[InkSample],
    attributes: InkAttributes,
    clip: &Clip,
    work: &mut usize,
) -> Result<(Vec<Vec<InkSample>>, bool), String> {
    if samples.len() == 1 {
        let touched = clip
            .interval(samples[0], samples[0], attributes, work)?
            .is_some();
        return Ok((
            if touched {
                Vec::new()
            } else {
                vec![samples.to_vec()]
            },
            touched,
        ));
    }
    let mut fragments = Vec::new();
    let mut current = Vec::new();
    let mut touched = false;
    let mut count = 0;
    for pair in samples.windows(2) {
        if let Some((start, end)) = clip.interval(pair[0], pair[1], attributes, work)?
            && end - start > EPSILON
        {
            touched = true;
            if start > EPSILON {
                append(&mut current, pair[0], &mut count)?;
                append(
                    &mut current,
                    interpolate(pair[0], pair[1], start),
                    &mut count,
                )?;
            }
            if !current.is_empty() {
                fragments.push(std::mem::take(&mut current));
            }
            if end < 1.0 - EPSILON {
                append(&mut current, interpolate(pair[0], pair[1], end), &mut count)?;
                append(&mut current, pair[1], &mut count)?;
            }
        } else {
            append(&mut current, pair[0], &mut count)?;
            append(&mut current, pair[1], &mut count)?;
        }
        if fragments.len() > MAX_CINEMAGRAPH_STROKES {
            return Err("An erased stroke would create too many fragments.".to_owned());
        }
    }
    if !current.is_empty() {
        fragments.push(current);
    }
    Ok((fragments, touched))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "convex interpolation of validated f32 pressure remains in 0..=1"
)]
fn interpolate(a: InkSample, b: InkSample, t: f64) -> InkSample {
    InkSample {
        position: InkPoint {
            x: a.position.x + (b.position.x - a.position.x) * t,
            y: a.position.y + (b.position.y - a.position.y) * t,
        },
        pressure: (f64::from(a.pressure) + (f64::from(b.pressure) - f64::from(a.pressure)) * t)
            as f32,
    }
}

fn append(
    samples: &mut Vec<InkSample>,
    sample: InkSample,
    count: &mut usize,
) -> Result<(), String> {
    if samples.last() == Some(&sample) {
        return Ok(());
    }
    if *count >= MAX_CINEMAGRAPH_SAMPLES {
        return Err("Erasing would exceed the bounded sample limit.".to_owned());
    }
    samples.push(sample);
    *count += 1;
    Ok(())
}

fn spend(work: &mut usize, amount: usize) -> Result<(), String> {
    *work = work
        .checked_add(amount)
        .filter(|total| *total <= MAX_WORK)
        .ok_or("This ink edit exceeds its bounded geometry-work limit.")?;
    Ok(())
}
