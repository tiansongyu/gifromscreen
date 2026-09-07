//! Pressure-aware ink contours. WPF-compatible fitting is kept separate from
//! path union/rasterization so that no pixel quantization enters this stage.

use crate::{
    CancellationToken, InkAttributes, InkError, InkFigure, InkFillRule, InkLimits, InkPath,
    InkSample, InkSegment, InkStroke, InkTip,
};

#[path = "ink_outline/math.rs"]
mod math;
#[path = "ink_outline/wpf_bezier.rs"]
mod wpf_bezier;
use math::V;

const ELLIPSE_K: f64 = 0.552_284_749_830_793_4;
const MIN_STYLUS_XY: f64 = -81_164_736.321_259_6;
const MAX_STYLUS_XY: f64 = 81_164_736.283_464_3;

/// Returns original or WPF Bezier-fitted center samples, with pressure
/// interpolated by accumulated chord length rather than sample index.
///
/// # Errors
/// Rejects non-finite/out-of-range input, exhausted work/storage bounds and
/// cancellation. A failed geometric fit uses the original points as WPF does.
pub fn fitted_ink_samples<C: CancellationToken + ?Sized>(
    stroke: &InkStroke,
    limits: &InkLimits,
    cancellation: &C,
) -> Result<Vec<InkSample>, InkError> {
    let mut budget = Budget::new(limits, cancellation);
    prepare_samples(stroke, &mut budget)
}

/// Expands pressure-dependent rectangular/elliptical tips and their connecting
/// sweeps into a nonzero-winding filled path. It performs no Boolean operations,
/// pixel rasterization or asset I/O.
///
/// # Errors
/// Returns invalid-input, bounded-work/storage and cancellation errors without
/// publishing a partial path.
pub fn outline_ink_stroke<C: CancellationToken + ?Sized>(
    stroke: &InkStroke,
    limits: &InkLimits,
    cancellation: &C,
) -> Result<InkPath, InkError> {
    let mut budget = Budget::new(limits, cancellation);
    outline_with_budget(stroke, &mut budget)
}

/// Expands up to 256 strokes in order using one shared work, storage and segment
/// budget. A malformed or empty member fails the entire batch, without dropping it.
///
/// # Errors
/// Returns the same failures as [`outline_ink_stroke`], or rejects excessive
/// combined source samples/stroke counts before preparing any outlines.
pub fn outline_ink_strokes<C: CancellationToken + ?Sized>(
    strokes: &[InkStroke],
    limits: &InkLimits,
    cancellation: &C,
) -> Result<Vec<InkPath>, InkError> {
    let mut budget = Budget::new(limits, cancellation);
    budget.check_cancelled()?;
    if strokes.len() > 256 {
        return Err(limit("stroke count"));
    }
    let mut samples = 0_usize;
    for stroke in strokes {
        budget.tick(1)?;
        samples = samples
            .checked_add(stroke.samples.len())
            .filter(|count| *count <= limits.max_points)
            .ok_or_else(|| limit("combined source sample count"))?;
    }
    budget.allocate::<InkPath>(strokes.len())?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(strokes.len())
        .map_err(|_| limit("batch path allocation"))?;
    for stroke in strokes {
        output.push(outline_with_budget(stroke, &mut budget)?);
    }
    budget.check_cancelled()?;
    Ok(output)
}

fn outline_with_budget<C: CancellationToken + ?Sized>(
    stroke: &InkStroke,
    budget: &mut Budget<'_, C>,
) -> Result<InkPath, InkError> {
    let samples = prepare_samples(stroke, budget)?;
    let mut path = InkPath {
        fill_rule: InkFillRule::NonZero,
        figures: Vec::new(),
    };
    let figures = if stroke.attributes.tip == InkTip::Ellipse {
        samples
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_sub(1))
            .ok_or_else(|| limit("figure count"))?
    } else {
        samples.len()
    };
    budget.allocate::<InkFigure>(figures)?;
    path.figures
        .try_reserve_exact(figures)
        .map_err(|_| limit("figure allocation"))?;
    for (index, sample) in samples.iter().enumerate() {
        budget.tick(1)?;
        let node = Node::new(*sample, stroke.attributes);
        match stroke.attributes.tip {
            InkTip::Rectangle => {
                if index == 0 {
                    append_polygon(&mut path, &node.rectangle(), budget)?;
                } else {
                    let previous = Node::new(samples[index - 1], stroke.attributes);
                    let mut points = [V::ZERO; 8];
                    points[..4].copy_from_slice(&previous.rectangle());
                    points[4..].copy_from_slice(&node.rectangle());
                    budget.tick(64)?;
                    let (hull, count) = hull(&mut points);
                    append_polygon(&mut path, &hull[..count], budget)?;
                }
            }
            InkTip::Ellipse => {
                append_ellipse(&mut path, node, budget)?;
                if index > 0 {
                    let previous = Node::new(samples[index - 1], stroke.attributes);
                    budget.tick(32)?;
                    if let Some(quad) = ellipse_quad(previous, node, stroke.attributes)? {
                        append_polygon(&mut path, &quad, budget)?;
                    }
                }
            }
        }
    }
    budget.check_cancelled()?;
    Ok(path)
}

fn prepare_samples<C: CancellationToken + ?Sized>(
    stroke: &InkStroke,
    budget: &mut Budget<'_, C>,
) -> Result<Vec<InkSample>, InkError> {
    budget.check_cancelled()?;
    if stroke.samples.is_empty() {
        return Err(invalid("a stroke needs at least one sample"));
    }
    if stroke.samples.len() > budget.limits.max_points {
        return Err(limit("source sample count"));
    }
    let attributes = stroke.attributes;
    if !attributes.width.is_finite()
        || !attributes.height.is_finite()
        || !(1.0..=100.0).contains(&attributes.width)
        || !(1.0..=100.0).contains(&attributes.height)
    {
        return Err(invalid(
            "tip dimensions must be finite and between 1 and 100",
        ));
    }
    for sample in &stroke.samples {
        budget.tick(1)?;
        if !sample.position.x.is_finite()
            || !sample.position.y.is_finite()
            || !(MIN_STYLUS_XY..=MAX_STYLUS_XY).contains(&sample.position.x)
            || !(MIN_STYLUS_XY..=MAX_STYLUS_XY).contains(&sample.position.y)
            || !sample.pressure.is_finite()
            || !(0.0..=1.0).contains(&sample.pressure)
        {
            return Err(invalid(
                "samples need finite WPF-range coordinates and pressure in 0..=1",
            ));
        }
    }
    if attributes.fit_to_curve && stroke.samples.len() >= 2 {
        return wpf_bezier::fit(stroke, budget);
    }
    clone_samples(&stroke.samples, budget)
}

fn clone_samples<C: CancellationToken + ?Sized>(
    samples: &[InkSample],
    budget: &mut Budget<'_, C>,
) -> Result<Vec<InkSample>, InkError> {
    budget.allocate::<InkSample>(samples.len())?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(samples.len())
        .map_err(|_| limit("sample allocation"))?;
    for sample in samples {
        budget.tick(1)?;
        output.push(*sample);
    }
    Ok(output)
}

#[derive(Clone, Copy)]
struct Node {
    center: V,
    rx: f64,
    ry: f64,
    base_rx: f64,
    base_ry: f64,
    pressure: f64,
}

impl Node {
    fn new(sample: InkSample, attributes: InkAttributes) -> Self {
        // Keep both arithmetic operations at float32 precision, as in WPF's
        // StrokeNodeIterator.GetNormalizedPressureFactor.
        let pressure = if attributes.ignore_pressure {
            1.0
        } else {
            f64::from(1.5_f32 * sample.pressure + 0.25_f32)
        };
        Self {
            center: sample.position.into(),
            rx: attributes.width * 0.5 * pressure,
            ry: attributes.height * 0.5 * pressure,
            base_rx: attributes.width * 0.5,
            base_ry: attributes.height * 0.5,
            pressure,
        }
    }
    fn rectangle(self) -> [V; 4] {
        [
            self.center + V::new(-self.rx, -self.ry),
            self.center + V::new(self.rx, -self.ry),
            self.center + V::new(self.rx, self.ry),
            self.center + V::new(-self.rx, self.ry),
        ]
    }
}

fn ellipse_quad(
    first: Node,
    last: Node,
    attributes: InkAttributes,
) -> Result<Option<[V; 4]>, InkError> {
    let rx = attributes.width * 0.5;
    let ry = attributes.height * 0.5;
    let radius = rx.max(ry);
    let offset = last.center - first.center;
    let sx = radius / rx;
    let sy = radius / ry;
    let spine = V::new(offset.x * sx, offset.y * sy);
    let squared = spine.dot(spine);
    let begin_radius = radius * first.pressure;
    let end_radius = radius * last.pressure;
    let delta = end_radius - begin_radius;
    if !squared.is_finite() {
        return Err(invalid("ellipse sweep overflow"));
    }
    if math::close(squared, 0.0) || squared < delta * delta || math::close(squared, delta * delta) {
        return Ok(None);
    }
    let distance = squared.sqrt();
    let unit = spine / distance;
    let normal = V::new(unit.y, -unit.x);
    let ratio_squared = delta * delta / squared;
    let (left, right) = if math::zero(ratio_squared) {
        (normal, -normal)
    } else {
        let side = normal * (1.0 - ratio_squared).sqrt();
        let mut along = unit * ratio_squared.sqrt();
        if first.pressure < last.pressure {
            along = -along;
        }
        (along + side, along - side)
    };
    let from_circle = |vector: V| V::new(vector.x * (1.0 / sx), vector.y * (1.0 / sy));
    let left = from_circle(left);
    let right = from_circle(right);
    Ok(Some([
        first.center + left * begin_radius,
        last.center + left * end_radius,
        last.center + right * end_radius,
        first.center + right * begin_radius,
    ]))
}

fn append_polygon<C: CancellationToken + ?Sized>(
    path: &mut InkPath,
    points: &[V],
    budget: &mut Budget<'_, C>,
) -> Result<(), InkError> {
    if points.len() < 3 {
        return Ok(());
    }
    let mut segments = Vec::new();
    budget.segments(points.len())?;
    segments
        .try_reserve_exact(points.len())
        .map_err(|_| limit("polygon allocation"))?;
    for point in points.iter().skip(1).chain(points.first()) {
        budget.tick(1)?;
        segments.push(InkSegment::LineTo(point.point()?));
    }
    path.figures.push(InkFigure {
        start: points[0].point()?,
        segments,
        closed: true,
    });
    Ok(())
}

fn append_ellipse<C: CancellationToken + ?Sized>(
    path: &mut InkPath,
    node: Node,
    budget: &mut Budget<'_, C>,
) -> Result<(), InkError> {
    budget.segments(4)?;
    let point = |x, y| {
        (node.center
            + V::new(
                node.base_rx * x * node.pressure,
                node.base_ry * y * node.pressure,
            ))
        .point()
    };
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(4)
        .map_err(|_| limit("ellipse allocation"))?;
    for [(x1, y1), (x2, y2), (x, y)] in [
        [(-1.0, -ELLIPSE_K), (-ELLIPSE_K, -1.0), (0.0, -1.0)],
        [(ELLIPSE_K, -1.0), (1.0, -ELLIPSE_K), (1.0, 0.0)],
        [(1.0, ELLIPSE_K), (ELLIPSE_K, 1.0), (0.0, 1.0)],
        [(-ELLIPSE_K, 1.0), (-1.0, ELLIPSE_K), (-1.0, 0.0)],
    ] {
        budget.tick(1)?;
        segments.push(InkSegment::CubicTo {
            control1: point(x1, y1)?,
            control2: point(x2, y2)?,
            to: point(x, y)?,
        });
    }
    path.figures.push(InkFigure {
        start: point(-1.0, 0.0)?,
        segments,
        closed: true,
    });
    Ok(())
}

fn hull(points: &mut [V; 8]) -> ([V; 16], usize) {
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    let mut hull = [V::ZERO; 16];
    let mut len = 0;
    for point in points.iter() {
        while len >= 2 && V::cross(hull[len - 1] - hull[len - 2], *point - hull[len - 1]) <= 0.0 {
            len -= 1;
        }
        hull[len] = *point;
        len += 1;
    }
    let lower = len;
    for point in points.iter().rev().skip(1) {
        while len > lower && V::cross(hull[len - 1] - hull[len - 2], *point - hull[len - 1]) <= 0.0
        {
            len -= 1;
        }
        hull[len] = *point;
        len += 1;
    }
    (hull, len - 1)
}

fn invalid(message: &str) -> InkError {
    InkError::Invalid(message.to_owned())
}
fn limit(message: &str) -> InkError {
    InkError::Limit(message.to_owned())
}

struct Budget<'a, C: CancellationToken + ?Sized> {
    limits: &'a InkLimits,
    cancellation: &'a C,
    work: u64,
    bytes: usize,
    segments: usize,
}
impl<'a, C: CancellationToken + ?Sized> Budget<'a, C> {
    fn new(limits: &'a InkLimits, cancellation: &'a C) -> Self {
        Self {
            limits,
            cancellation,
            work: 0,
            bytes: 0,
            segments: 0,
        }
    }
    fn check_cancelled(&self) -> Result<(), InkError> {
        if self.cancellation.is_cancelled() {
            Err(InkError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn tick(&mut self, count: u64) -> Result<(), InkError> {
        self.check_cancelled()?;
        self.work = self
            .work
            .checked_add(count)
            .filter(|value| *value <= self.limits.max_work)
            .ok_or_else(|| limit("work budget"))?;
        Ok(())
    }
    fn allocate<T>(&mut self, count: usize) -> Result<(), InkError> {
        self.check_cancelled()?;
        self.bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .and_then(|value| self.bytes.checked_add(value))
            .filter(|value| *value <= self.limits.max_bytes)
            .ok_or_else(|| limit("geometry bytes"))?;
        Ok(())
    }
    fn segments(&mut self, count: usize) -> Result<(), InkError> {
        self.segments = self
            .segments
            .checked_add(count)
            .filter(|value| *value <= self.limits.max_segments)
            .ok_or_else(|| limit("generated segment count"))?;
        self.allocate::<InkSegment>(count)
    }
    fn push_point(&mut self, points: &mut Vec<V>, point: V) -> Result<(), InkError> {
        self.tick(1)?;
        if points.len() >= self.limits.max_points {
            return Err(limit("fitted point count"));
        }
        point.point()?;
        if points.len() == points.capacity() {
            let cap = points
                .capacity()
                .max(2)
                .saturating_mul(2)
                .min(self.limits.max_points);
            self.allocate::<V>(cap - points.capacity())?;
            points
                .try_reserve_exact(cap - points.len())
                .map_err(|_| limit("point allocation"))?;
        }
        points.push(point);
        Ok(())
    }
}

#[cfg(test)]
#[path = "ink_outline_tests.rs"]
mod tests;
