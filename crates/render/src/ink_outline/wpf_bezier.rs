//! Port of the default WPF ink fitting algorithm, not a substitute spline.
//! Algorithm source: dotnet/wpf a04736acb8edb533756131d3d5fc55f15cd03d6a,
//! PresentationCore/MS/internal/Ink/{Bezier,CuspData}.cs and Ink/Stroke.cs.
//! Original source copyright .NET Foundation and contributors, MIT licensed.

use super::{Budget, MAX_STYLUS_XY, MIN_STYLUS_XY, V, clone_samples, invalid, limit, math};
use crate::{CancellationToken, InkError, InkSample, InkStroke};

const TO_HIMETRIC: f64 = 2540.0 / 96.0;
const TO_AVALON: f64 = 96.0 / 2540.0;

pub(super) fn fit<C: CancellationToken + ?Sized>(
    stroke: &InkStroke,
    budget: &mut Budget<'_, C>,
) -> Result<Vec<InkSample>, InkError> {
    let mut data = Data::new(stroke, budget)?;
    if data.points.len() < 2 {
        return clone_samples(&stroke.samples, budget);
    }
    let Some(controls) = construct(&mut data, budget)? else {
        return clone_samples(&stroke.samples, budget);
    };
    // GetBezierStylusPoints uses the unpressured tip bounding box in DIP.
    let tolerance = ((2.0 * stroke.attributes.width.min(stroke.attributes.height)).log10()
        * (TO_HIMETRIC / 2.0))
        .max(0.5);
    let mut points = Vec::new();
    budget.push_point(&mut points, controls[0])?;
    for segment in controls.windows(4).step_by(3) {
        flatten(segment, tolerance, &mut points, budget)?;
    }
    for point in &mut points {
        budget.tick(1)?;
        *point = *point * TO_AVALON;
    }
    interpolate(&stroke.samples, &points, budget)
}

fn reserved<T, C: CancellationToken + ?Sized>(
    count: usize,
    budget: &mut Budget<'_, C>,
) -> Result<Vec<T>, InkError> {
    budget.allocate::<T>(count)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| limit("fitting allocation"))?;
    Ok(values)
}

struct Data {
    points: Vec<V>,
    nodes: Vec<f64>,
    previous: Vec<usize>,
    next: Vec<usize>,
    cusps: Vec<usize>,
    distance: f64,
    span: f64,
}
impl Data {
    fn new<C: CancellationToken + ?Sized>(
        stroke: &InkStroke,
        budget: &mut Budget<'_, C>,
    ) -> Result<Self, InkError> {
        let count = stroke.samples.len();
        let mut points = reserved(count, budget)?;
        let mut nodes = reserved(count, budget)?;
        points.push(V::from(stroke.samples[0].position) * TO_HIMETRIC);
        nodes.push(0.0);
        for samples in stroke.samples.windows(2) {
            budget.tick(1)?;
            let previous = V::from(samples[0].position);
            let next = V::from(samples[1].position);
            if !math::close(previous.x, next.x) || !math::close(previous.y, next.y) {
                let next = next * TO_HIMETRIC;
                nodes.push(nodes[nodes.len() - 1] + (next - points[points.len() - 1]).length());
                points.push(next);
            }
        }
        let mut previous = reserved(points.len(), budget)?;
        previous.resize(points.len(), 0);
        let mut next = reserved(points.len(), budget)?;
        next.resize(points.len(), 0);
        let cusps = reserved(points.len(), budget)?;
        let mut data = Self {
            points,
            nodes,
            previous,
            next,
            cusps,
            distance: 0.0,
            span: 3.0,
        };
        if data.points.len() >= 2 {
            let mut low = data.points[0];
            let mut high = low;
            for p in &data.points {
                budget.tick(1)?;
                low.x = low.x.min(p.x);
                low.y = low.y.min(p.y);
                high.x = high.x.max(p.x);
                high.y = high.y.max(p.y);
            }
            data.distance = (high.x - low.x).abs() + (high.y - low.y).abs();
            if data.distance > 0.0 {
                let length = data.nodes[data.nodes.len() - 1];
                let count = f64::from(
                    u32::try_from(data.points.len())
                        .map_err(|_| limit("source count exceeds WPF indices"))?,
                );
                data.span = 0.75_f64 * (length * length) / (count * data.distance);
            }
            data.span = data.span.max(1.0);
            data.find_cusps(budget)?;
        }
        Ok(data)
    }
    fn tan_links<C: CancellationToken + ?Sized>(
        &mut self,
        error: f64,
        budget: &mut Budget<'_, C>,
    ) -> Result<(), InkError> {
        let error = error.max(1.0);
        // CDataPoint is a C# value type initialized with zeros, not -1. Preserve
        // those defaults; the source's negative-link fallback is unreachable.
        // The first matching j is monotone because chord lengths are sorted.
        // Preserve the original write order/results without its O(n²) rescan.
        let mut j = 1;
        for i in 0..self.points.len() {
            budget.tick(1)?;
            j = j.max(i + 1);
            while j < self.points.len() {
                budget.tick(1)?;
                if self.nodes[j] - self.nodes[i] >= error {
                    break;
                }
                j += 1;
            }
            if j < self.points.len() {
                self.next[i] = j;
                self.previous[j] = i;
            }
        }
        Ok(())
    }
    fn adjacent<C: CancellationToken + ?Sized>(
        &self,
        index: usize,
        previous_cusp: usize,
        budget: &mut Budget<'_, C>,
    ) -> Result<(bool, usize, usize), InkError> {
        let mut more = index < self.points.len();
        let index = index.min(self.points.len() - 1);
        let mut next = index + 1;
        while next < self.points.len() {
            budget.tick(1)?;
            if self.nodes[next] - self.nodes[index] >= self.span {
                break;
            }
            next += 1;
        }
        if next >= self.points.len() {
            more = false;
            next = self.points.len() - 1;
        }
        let mut previous = index.saturating_sub(1);
        if index != 0 {
            while previous >= previous_cusp {
                budget.tick(1)?;
                if self.nodes[index] - self.nodes[previous] >= self.span || previous == 0 {
                    break;
                }
                previous -= 1;
            }
        }
        Ok((more, previous, next))
    }
    fn curvature(&self, previous: usize, current: usize, next: usize) -> f64 {
        let v = self.points[current] - self.points[previous];
        let w = self.points[next] - self.points[current];
        let length = v.length() * w.length();
        if math::zero(length) {
            0.0
        } else {
            1.0 - v.dot(w) / length
        }
    }
    fn find_cusps<C: CancellationToken + ?Sized>(
        &mut self,
        budget: &mut Budget<'_, C>,
    ) -> Result<(), InkError> {
        self.cusps.push(0);
        let (more, _, next) = self.adjacent(0, 0, budget)?;
        if !more {
            if self.points.len() > 1 {
                self.cusps.push(next);
            }
            return Ok(());
        }
        let mut index = next;
        let mut previous_cusp = 0;
        loop {
            budget.tick(1)?;
            let (more, previous, next) = self.adjacent(index, previous_cusp, budget)?;
            if !more {
                break;
            }
            let curvature = self.curvature(previous, index, next);
            if curvature > 0.80 {
                let mut maximum = curvature;
                let mut maximum_index = index;
                let (more, _, end) = self.adjacent(next, previous_cusp, budget)?;
                if !more {
                    break;
                }
                let mut candidate = previous + 1;
                while candidate <= end {
                    budget.tick(1)?;
                    let (more, left, right) = self.adjacent(candidate, previous_cusp, budget)?;
                    if !more {
                        break;
                    }
                    let current = self.curvature(left, candidate, right);
                    if current > maximum {
                        maximum = current;
                        maximum_index = candidate;
                    }
                    candidate += 1;
                }
                self.cusps.push(maximum_index);
                index = end + 1;
                previous_cusp = maximum_index;
            } else if curvature < 0.035 {
                index = next;
            } else {
                index += 1;
            }
        }
        self.cusps.push(self.points.len() - 1);
        Ok(())
    }
    fn next_cusp(&self, index: usize) -> usize {
        self.cusps
            .get(self.cusps.partition_point(|cusp| *cusp <= index))
            .copied()
            .unwrap_or(self.points.len() - 1)
    }
    fn tangent(
        &self,
        at: usize,
        previous_cusp: usize,
        next_cusp: usize,
        reverse: bool,
        is_cusp: bool,
    ) -> Option<V> {
        let value = if is_cusp {
            let (first, second) = if reverse {
                let first = self.previous[at];
                if first < previous_cusp {
                    (usize::midpoint(previous_cusp, at), previous_cusp)
                } else {
                    (first, self.previous[first].max(previous_cusp))
                }
            } else {
                let first = self.next[at];
                if first > next_cusp {
                    (usize::midpoint(next_cusp, at), next_cusp)
                } else {
                    (first, self.next[first].min(next_cusp))
                }
            };
            self.points[first] + self.points[second] * 0.5 - self.points[at] * 1.5
        } else {
            let second = self.previous[at];
            let (second, third) = if second < previous_cusp {
                (usize::midpoint(previous_cusp, at), previous_cusp)
            } else {
                (second, self.previous[second].max(previous_cusp))
            };
            let next = self.next[at].min(next_cusp);
            self.points[at] + self.points[second] + self.points[third] * 0.5
                - self.points[next] * 2.5
        };
        if math::zero(value.dot(value)) {
            None
        } else {
            Some(value.normalized())
        }
    }
}

fn construct<C: CancellationToken + ?Sized>(
    data: &mut Data,
    budget: &mut Budget<'_, C>,
) -> Result<Option<Vec<V>>, InkError> {
    let mut controls = Vec::new();
    budget.push_point(&mut controls, data.points[0])?;
    if data.points.len() <= 3 {
        add_segment(
            data,
            0,
            data.points.len() - 1,
            V::ZERO,
            V::ZERO,
            &mut controls,
            budget,
        )?;
        return Ok(Some(controls));
    }
    let error = f64::from(0.03_f32) * (data.distance * TO_AVALON);
    data.tan_links(0.5 * error, budget)?;
    let error = error * error;
    let mut from = 0;
    let mut previous_cusp = 0;
    let mut next_cusp = 0;
    let mut is_cusp = true;
    let mut end_tangent = V::ZERO;
    loop {
        budget.tick(1)?;
        let start_tangent = if is_cusp {
            previous_cusp = next_cusp;
            next_cusp = data.next_cusp(from);
            let Some(value) = data.tangent(from, previous_cusp, next_cusp, false, true) else {
                return Ok(None);
            };
            value
        } else {
            -end_tangent
        };
        let mut to = from + 3;
        let done;
        loop {
            budget.tick(1)?;
            to += 1;
            if to >= data.points.len() - 1 {
                to = data.points.len() - 1;
                is_cusp = true;
                done = true;
                break;
            }
            if to >= next_cusp {
                to = next_cusp;
                is_cusp = true;
                done = false;
                break;
            }
            is_cusp = false;
            let spacing = (to - from) / 4;
            if !co_cubic(
                data,
                [
                    from,
                    from + spacing,
                    usize::midpoint(to, from),
                    to - spacing,
                    to,
                ],
                error,
            ) {
                done = false;
                break;
            }
        }
        let Some(value) = data.tangent(to, previous_cusp, next_cusp, true, is_cusp) else {
            return Ok(None);
        };
        end_tangent = value;
        add_segment(
            data,
            from,
            to,
            start_tangent,
            end_tangent,
            &mut controls,
            budget,
        )?;
        if done {
            break;
        }
        if to <= from {
            return Err(invalid("non-progressing WPF fit"));
        }
        from = to;
    }
    Ok(Some(controls))
}

fn co_cubic(data: &Data, i: [usize; 5], error: f64) -> bool {
    let node = |a: usize, b: usize| data.nodes[i[b]] - data.nodes[i[a]];
    let d04 = node(0, 4);
    let d01 = d04 / node(0, 1);
    let d02 = d04 / node(0, 2);
    let d03 = d04 / node(0, 3);
    let d12 = d04 / node(1, 2);
    let d13 = d04 / node(1, 3);
    let d14 = d04 / node(1, 4);
    let d23 = d04 / node(2, 3);
    let d24 = d04 / node(2, 4);
    let d34 = d04 / node(3, 4);
    let value = data.points[i[0]] * (d01 * d02 * d03) - data.points[i[1]] * (d01 * d12 * d13 * d14)
        + data.points[i[2]] * (d02 * d12 * d23 * d24)
        - data.points[i[3]] * (d03 * d13 * d23 * d34)
        + data.points[i[4]] * (d14 * d24 * d34);
    value.dot(value) < error
}

fn add_segment<C: CancellationToken + ?Sized>(
    data: &Data,
    from: usize,
    to: usize,
    start: V,
    end: V,
    controls: &mut Vec<V>,
    budget: &mut Budget<'_, C>,
) -> Result<(), InkError> {
    let (first, second) = match to - from {
        1 => (
            (data.points[from] * 2.0 + data.points[to]) * (1.0 / 3.0),
            (data.points[from] + data.points[to] * 2.0) * (1.0 / 3.0),
        ),
        2 => {
            let t = (data.nodes[from + 1] - data.nodes[from]) / (data.nodes[to] - data.nodes[from]);
            let s = 1.0 - t;
            if t < 0.001 || s < 0.001 {
                (
                    (data.points[from] * 2.0 + data.points[to]) * (1.0 / 3.0),
                    (data.points[from] + data.points[to] * 2.0) * (1.0 / 3.0),
                )
            } else {
                let tt = 1.0 / t;
                let ss = 1.0 / s;
                let middle = data.points[from + 1] * (tt * ss);
                // Preserve Bezier.cs AddParabola literally: its first control's
                // final term uses the middle point, despite the nearby derivation.
                (
                    (middle + data.points[from] * (1.0 - s * tt)
                        - data.points[from + 1] * (t * ss))
                        * (1.0 / 3.0),
                    (middle - data.points[from] * (s * tt) + data.points[to] * (1.0 - t * ss))
                        * (1.0 / 3.0),
                )
            }
        }
        _ => least_squares(data, from, to, start, end, budget)?,
    };
    budget.push_point(controls, first)?;
    budget.push_point(controls, second)?;
    budget.push_point(controls, data.points[to])
}

fn least_squares<C: CancellationToken + ?Sized>(
    data: &Data,
    from: usize,
    to: usize,
    v: V,
    w: V,
    budget: &mut Budget<'_, C>,
) -> Result<(V, V), InkError> {
    let (mut a11, mut a12, mut a22, mut b1, mut b2) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut b11, mut b12, mut b21, mut b22) = (0.0, 0.0, 0.0, 0.0);
    for index in from + 1..to {
        budget.tick(1)?;
        let t = (data.nodes[index] - data.nodes[from]) / (data.nodes[to] - data.nodes[from]);
        let t2 = t * t;
        let r = 1.0 - t;
        let r2 = r * r;
        let f0 = r2 * r;
        let f1 = 3.0 * r2 * t;
        let f2 = 3.0 * r * t2;
        let f3 = t2 * t;
        a11 += f1 * f1;
        a22 += f2 * f2;
        a12 += f1 * f2;
        b11 -= (f0 + f1) * f1;
        b12 -= (f2 + f3) * f1;
        b1 += f1 * data.points[index].dot(v);
        b21 -= (f0 + f1) * f2;
        b22 -= (f2 + f3) * f2;
        b2 += f2 * data.points[index].dot(w);
    }
    a12 *= v.dot(w);
    b1 += v.dot(data.points[from]) * b11 + v.dot(data.points[to]) * b12;
    b2 += w.dot(data.points[from]) * b21 + w.dot(data.points[to]) * b22;
    let mut s = b1 * a22 - b2 * a12;
    let mut u = b2 * a11 - b1 * a12;
    let determinant = a11 * a22 - a12 * a12;
    let mut accept =
        determinant.abs() > s.abs() * f64::EPSILON && determinant.abs() > u.abs() * f64::EPSILON;
    if accept {
        s /= determinant;
        u /= determinant;
        accept = s > 1e-6 && u > 1e-6;
    }
    if !accept {
        s = (data.nodes[to] - data.nodes[from]) / 3.0;
        u = s;
    }
    Ok((data.points[from] + v * s, data.points[to] + w * u))
}

fn evaluate(points: &[V], t: f64) -> V {
    let s = 1.0 - t;
    let q0 = points[0] * s + points[1] * t;
    let q1 = points[1] * s + points[2] * t;
    let q2 = points[2] * s + points[3] * t;
    let q0 = q0 * s + q1 * t;
    let q1 = q1 * s + q2 * t;
    q0 * s + q1 * t
}

fn flatten<C: CancellationToken + ?Sized>(
    controls: &[V],
    tolerance: f64,
    points: &mut Vec<V>,
    budget: &mut Budget<'_, C>,
) -> Result<(), InkError> {
    budget.tick(1)?;
    let mut curvedness = 0.0_f64;
    for index in 1..=2 {
        curvedness = curvedness
            .max(((controls[index - 1] + controls[index + 1]) * 0.5 - controls[index]).length());
    }
    if curvedness <= 0.5 * tolerance {
        return budget.push_point(points, controls[3]);
    }
    let count = flatten_count(curvedness, tolerance)?;
    let d = 1.0 / f64::from(count);
    let mut q = [V::ZERO; 4];
    q[0] = controls[0];
    for (point, index) in q.iter_mut().skip(1).zip(1_u16..) {
        *point = evaluate(controls, f64::from(index) * d);
        budget.push_point(points, *point)?;
    }
    for i in 1..=3 {
        for k in 0..=3 - i {
            q[k] = q[k + 1] - q[k];
        }
    }
    for _ in 4..=count {
        budget.tick(1)?;
        for k in 1..=3 {
            q[k] += q[k - 1];
        }
        budget.push_point(points, q[3])?;
    }
    Ok(())
}

fn interpolate<C: CancellationToken + ?Sized>(
    source: &[InkSample],
    points: &[V],
    budget: &mut Budget<'_, C>,
) -> Result<Vec<InkSample>, InkError> {
    let mut output = reserved(points.len(), budget)?;
    let sample = |position: V, pressure: f32| -> Result<InkSample, InkError> {
        if !pressure.is_finite() {
            return Err(invalid("non-finite fitted pressure"));
        }
        position.point()?;
        let position = V::new(
            position.x.clamp(MIN_STYLUS_XY, MAX_STYLUS_XY),
            position.y.clamp(MIN_STYLUS_XY, MAX_STYLUS_XY),
        )
        .point()?;
        Ok(InkSample {
            position,
            pressure: pressure.clamp(0.0, 1.0),
        })
    };
    output.push(sample(points[0], source[0].pressure)?);
    if points.len() == 1 {
        return Ok(output);
    }
    let mut bezier_length = 0.0;
    let mut previous_length = 0.0;
    let mut original_length = (V::from(source[1].position) - V::from(source[0].position)).length();
    let mut index = 1;
    for current in 1..points.len() - 1 {
        budget.tick(1)?;
        bezier_length += (points[current] - points[current - 1]).length();
        while index < source.len() {
            budget.tick(1)?;
            if bezier_length >= previous_length && bezier_length < original_length {
                let percent = pressure_fraction(bezier_length, previous_length, original_length);
                let pressure = percent * (source[index].pressure - source[index - 1].pressure)
                    + source[index - 1].pressure;
                output.push(sample(points[current], pressure)?);
                break;
            }
            index += 1;
            if index < source.len() {
                previous_length = original_length;
                original_length += (V::from(source[index].position)
                    - V::from(source[index - 1].position))
                .length();
            }
        }
    }
    output.push(sample(
        points[points.len() - 1],
        source[source.len() - 1].pressure,
    )?);
    budget.check_cancelled()?;
    Ok(output)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "WPF explicitly floors the sample count then caps it at 1000"
)]
fn flatten_count(curvedness: f64, tolerance: f64) -> Result<u16, InkError> {
    if !curvedness.is_finite() || !tolerance.is_finite() || tolerance <= 0.0 {
        return Err(invalid("non-finite Bezier flattening"));
    }
    Ok(((curvedness / tolerance).sqrt().floor() + 3.0).min(1000.0) as u16)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "Stroke.GetInterpolatedStylusPoints explicitly narrows each accumulated length to float32 before subtracting"
)]
fn pressure_fraction(length: f64, previous: f64, next: f64) -> f32 {
    (length as f32 - previous as f32) / (next as f32 - previous as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InkAttributes, InkLimits, InkPoint, NeverCancel};

    fn stroke(points: &[(f64, f64, f32)]) -> InkStroke {
        InkStroke {
            samples: points
                .iter()
                .map(|(x, y, pressure)| InkSample {
                    position: InkPoint { x: *x, y: *y },
                    pressure: *pressure,
                })
                .collect(),
            attributes: InkAttributes {
                fit_to_curve: true,
                width: 4.0,
                height: 2.0,
                ..InkAttributes::default()
            },
        }
    }
    fn near(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-10 * (1.0 + expected.abs()),
            "{actual} != {expected}"
        );
    }

    #[test]
    fn three_point_controls_preserve_the_reference_implementation_not_a_different_parabola() {
        let pen = stroke(&[(0.0, 0.0, 0.0), (1.0, 1.0, 0.5), (2.0, 0.0, 1.0)]);
        let limits = InkLimits::default();
        let mut budget = Budget::new(&limits, &NeverCancel);
        let mut data = Data::new(&pen, &mut budget).unwrap();
        let controls = construct(&mut data, &mut budget).unwrap().unwrap();
        assert_eq!(controls.len(), 4);
        near(controls[1].x, TO_HIMETRIC);
        near(controls[1].y, TO_HIMETRIC);
        near(controls[2].x, 4.0 * TO_HIMETRIC / 3.0);
        near(controls[2].y, 4.0 * TO_HIMETRIC / 3.0);
        near(controls[3].x, 2.0 * TO_HIMETRIC);
        near(controls[3].y, 0.0);
    }

    #[test]
    fn pressure_interpolation_follows_distance_and_not_point_index() {
        let pen = stroke(&[(0.0, 0.0, 0.0), (1.0, 0.0, 1.0), (3.0, 0.0, 0.0)]);
        let points = [
            V::new(0.0, 0.0),
            V::new(0.5, 0.0),
            V::new(1.0, 0.0),
            V::new(2.0, 0.0),
            V::new(3.0, 0.0),
        ];
        let limits = InkLimits::default();
        let mut budget = Budget::new(&limits, &NeverCancel);
        let actual = interpolate(&pen.samples, &points, &mut budget).unwrap();
        assert_eq!(
            actual
                .iter()
                .map(|sample| sample.pressure.to_bits())
                .collect::<Vec<_>>(),
            [0.0_f32, 0.5, 1.0, 0.5, 0.0].map(f32::to_bits)
        );
        let mut budget = Budget::new(&limits, &NeverCancel);
        assert!(interpolate(&pen.samples, &[V::new(f64::INFINITY, 0.0)], &mut budget).is_err());
    }

    #[test]
    fn straight_fitted_curves_can_remove_interior_pressure_peaks_as_wpf_does() {
        let pen = stroke(&[
            (0.0, 0.0, 0.1),
            (1.0, 0.0, 1.0),
            (2.0, 0.0, 0.9),
            (3.0, 0.0, 0.2),
        ]);
        let limits = InkLimits::default();
        let mut budget = Budget::new(&limits, &NeverCancel);
        let fitted = fit(&pen, &mut budget).unwrap();
        assert_eq!(fitted.len(), 2);
        assert_eq!(fitted[0].pressure.to_bits(), 0.1_f32.to_bits());
        assert_eq!(fitted[1].pressure.to_bits(), 0.2_f32.to_bits());
    }

    #[test]
    fn monotone_tangent_search_preserves_every_reference_array_write() {
        let pen = InkStroke {
            samples: (0..200)
                .map(|i| InkSample {
                    position: InkPoint {
                        x: f64::from(i) * 0.125,
                        y: f64::from(i % 7),
                    },
                    pressure: 0.5,
                })
                .collect(),
            attributes: InkAttributes::default(),
        };
        let limits = InkLimits::default();
        let mut budget = Budget::new(&limits, &NeverCancel);
        let mut data = Data::new(&pen, &mut budget).unwrap();
        for error in [0.0_f64, 1.0, 29.0, 300.0, 1e10] {
            let mut previous = vec![0; data.points.len()];
            let mut next = previous.clone();
            for (i, next) in next.iter_mut().enumerate() {
                for (j, previous) in previous.iter_mut().enumerate().skip(i + 1) {
                    if data.nodes[j] - data.nodes[i] >= error.max(1.0) {
                        *next = j;
                        *previous = i;
                        break;
                    }
                }
            }
            data.previous.fill(0);
            data.next.fill(0);
            data.tan_links(error, &mut budget).unwrap();
            assert_eq!(data.previous, previous);
            assert_eq!(data.next, next);
        }
    }
}
