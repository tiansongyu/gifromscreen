//! Fixed-complexity line/cubic queries: no per-pixel mask and no UI approximation.

use crate::{InkFigure, InkPoint, InkSegment};

const EPSILON: f64 = 1.0e-8;

fn finite(point: InkPoint) -> bool {
    point.x.is_finite() && point.y.is_finite()
}

pub(super) fn contains(figure: &InkFigure, point: InkPoint) -> bool {
    if !finite(point) {
        return false;
    }
    let mut winding = 0_i32;
    let mut boundary = false;
    segments(figure, |start, segment| match segment {
        InkSegment::LineTo(end) => {
            if on_line(point, start, end) {
                boundary = true;
            }
            if (start.y <= point.y && point.y < end.y) || (end.y <= point.y && point.y < start.y) {
                let t = (point.y - start.y) / (end.y - start.y);
                let x = start.x + (end.x - start.x) * t;
                if x > point.x {
                    winding += if end.y > start.y { 1 } else { -1 };
                }
            }
        }
        InkSegment::CubicTo {
            control1,
            control2,
            to,
        } => {
            let curve = [start, control1, control2, to];
            let values = curve.map(|p| p.y);
            for (lo, hi) in intervals(values) {
                let (a, b) = (at(values, lo), at(values, hi));
                if point.y >= a.min(b) - EPSILON && point.y <= a.max(b) + EPSILON {
                    if (a - b).abs() <= EPSILON {
                        if (point.y - a).abs() <= EPSILON {
                            let xs = curve.map(|p| p.x);
                            let (minimum, maximum) = extent(xs, lo, hi);
                            boundary |=
                                point.x >= minimum - EPSILON && point.x <= maximum + EPSILON;
                        }
                    } else {
                        let t = root(values, point.y, lo, hi);
                        let x = at(curve.map(|p| p.x), t);
                        boundary |= (x - point.x).abs() <= EPSILON;
                        if x > point.x
                            && ((a <= point.y && point.y < b) || (b <= point.y && point.y < a))
                        {
                            winding += if b > a { 1 } else { -1 };
                        }
                    }
                }
            }
        }
    });
    boundary || winding != 0
}

pub(super) fn intersects_rect(figure: &InkFigure, minimum: InkPoint, maximum: InkPoint) -> bool {
    if !finite(minimum) || !finite(maximum) || minimum.x > maximum.x || minimum.y > maximum.y {
        return false;
    }
    let inside =
        |p: InkPoint| p.x >= minimum.x && p.x <= maximum.x && p.y >= minimum.y && p.y <= maximum.y;
    let corners = [
        minimum,
        InkPoint {
            x: maximum.x,
            y: minimum.y,
        },
        maximum,
        InkPoint {
            x: minimum.x,
            y: maximum.y,
        },
    ];
    let mut intersected = false;
    segments(figure, |start, segment| {
        if intersected {
            return;
        }
        match segment {
            InkSegment::LineTo(end) => {
                intersected = inside(start)
                    || inside(end)
                    || (0..4)
                        .any(|i| lines_intersect(start, end, corners[i], corners[(i + 1) % 4]));
            }
            InkSegment::CubicTo {
                control1,
                control2,
                to,
            } => {
                let curve = [start, control1, control2, to];
                intersected = inside(start)
                    || inside(to)
                    || curve_edge(
                        curve.map(|p| p.x),
                        curve.map(|p| p.y),
                        minimum.x,
                        minimum.y,
                        maximum.y,
                    )
                    || curve_edge(
                        curve.map(|p| p.x),
                        curve.map(|p| p.y),
                        maximum.x,
                        minimum.y,
                        maximum.y,
                    )
                    || curve_edge(
                        curve.map(|p| p.y),
                        curve.map(|p| p.x),
                        minimum.y,
                        minimum.x,
                        maximum.x,
                    )
                    || curve_edge(
                        curve.map(|p| p.y),
                        curve.map(|p| p.x),
                        maximum.y,
                        minimum.x,
                        maximum.x,
                    );
            }
        }
    });
    intersected || corners.into_iter().any(|p| contains(figure, p))
}

fn curve_edge(values: [f64; 4], other: [f64; 4], edge: f64, minimum: f64, maximum: f64) -> bool {
    intervals(values).any(|(lo, hi)| {
        let (a, b) = (at(values, lo), at(values, hi));
        if edge < a.min(b) - EPSILON || edge > a.max(b) + EPSILON {
            return false;
        }
        if (a - b).abs() <= EPSILON {
            let (low, high) = extent(other, lo, hi);
            high >= minimum - EPSILON && low <= maximum + EPSILON
        } else {
            let value = at(other, root(values, edge, lo, hi));
            value >= minimum - EPSILON && value <= maximum + EPSILON
        }
    })
}

fn segments(figure: &InkFigure, mut visit: impl FnMut(InkPoint, InkSegment)) {
    let mut start = figure.start;
    for segment in &figure.segments {
        visit(start, *segment);
        start = match *segment {
            InkSegment::LineTo(to) | InkSegment::CubicTo { to, .. } => to,
        };
    }
    visit(start, InkSegment::LineTo(figure.start));
}

fn on_line(p: InkPoint, a: InkPoint, b: InkPoint) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length = dx.hypot(dy);
    if length == 0.0 {
        return (p.x - a.x).hypot(p.y - a.y) <= EPSILON;
    }
    let cross = (p.x - a.x) * dy - (p.y - a.y) * dx;
    cross.abs() <= EPSILON * length
        && p.x >= a.x.min(b.x) - EPSILON
        && p.x <= a.x.max(b.x) + EPSILON
        && p.y >= a.y.min(b.y) - EPSILON
        && p.y <= a.y.max(b.y) + EPSILON
}

fn lines_intersect(a: InkPoint, b: InkPoint, c: InkPoint, d: InkPoint) -> bool {
    if on_line(a, c, d) || on_line(b, c, d) || on_line(c, a, b) || on_line(d, a, b) {
        return true;
    }
    let cross = |p: InkPoint, q: InkPoint, r: InkPoint| {
        (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)
    };
    cross(a, b, c).is_sign_positive() != cross(a, b, d).is_sign_positive()
        && cross(c, d, a).is_sign_positive() != cross(c, d, b).is_sign_positive()
}

fn at([a, b, c, d]: [f64; 4], parameter: f64) -> f64 {
    let ab = a + (b - a) * parameter;
    let bc = b + (c - b) * parameter;
    let cd = c + (d - c) * parameter;
    let abc = ab + (bc - ab) * parameter;
    let bcd = bc + (cd - bc) * parameter;
    abc + (bcd - abc) * parameter
}

fn cuts([a, b, c, d]: [f64; 4]) -> ([f64; 4], usize) {
    let (aa, bb, cc) = (-a + 3.0 * b - 3.0 * c + d, 2.0 * (a - 2.0 * b + c), b - a);
    let mut roots = [0.0, 1.0, 0.0, 0.0];
    let mut count = 2;
    let mut add = |root: f64| {
        if root > 0.0 && root < 1.0 {
            roots[count] = root;
            count += 1;
        }
    };
    if aa == 0.0 {
        if bb != 0.0 {
            add(-cc / bb);
        }
    } else {
        let discriminant = bb * bb - 4.0 * aa * cc;
        if discriminant >= 0.0 {
            let factor = -0.5 * (bb + discriminant.sqrt().copysign(bb));
            if factor == 0.0 {
                add(-bb / (2.0 * aa));
            } else {
                add(factor / aa);
                add(cc / factor);
            }
        }
    }
    roots[..count].sort_by(f64::total_cmp);
    (roots, count)
}

fn intervals(values: [f64; 4]) -> impl Iterator<Item = (f64, f64)> {
    let (values, length) = cuts(values);
    (1..length).map(move |index| (values[index - 1], values[index]))
}

fn root(values: [f64; 4], target: f64, mut lo: f64, mut hi: f64) -> f64 {
    let ascending = at(values, hi) >= at(values, lo);
    for _ in 0..56 {
        let mid = lo.midpoint(hi);
        if (at(values, mid) < target) == ascending {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo.midpoint(hi)
}

fn extent(values: [f64; 4], lo: f64, hi: f64) -> (f64, f64) {
    let (a, b) = (at(values, lo), at(values, hi));
    let (mut minimum, mut maximum) = (a.min(b), a.max(b));
    let (cuts, count) = cuts(values);
    for t in &cuts[..count] {
        if *t > lo && *t < hi {
            let value = at(values, *t);
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
    }
    (minimum, maximum)
}
