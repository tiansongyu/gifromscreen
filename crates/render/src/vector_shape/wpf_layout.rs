//! Independent 96-DPI WPF layout; not part of the persisted V1 renderer.
//!
//! Contract: explicit Width/Height, Canvas.Left/Top, zero Margin, default
//! alignments, UseLayoutRounding=true, no `LayoutTransform`, left-to-right flow,
//! and relative RenderTransformOrigin=(.5,.5). The shape keeps its default
//! ClipToBounds=false (distinct from the parent Canvas clip). Rotation does not
//! participate in layout.
//!
//! Derived from dotnet/wpf v9.0.0: FrameworkElement.MeasureCore/ArrangeCore,
//! UIElement.RoundLayoutValue, Shapes/{Shape,Rectangle,Ellipse}.GetNaturalSize,
//! and WpfGfx/core/geometry/{strokefigure.cpp,utils.cpp}. In particular, natural
//! size is max(bounds.Right/Bottom, 0), not the bounding rectangle's dimensions.
//! The custom `ScreenToGif` a4d0a67 Triangle/Arrow keep their *requested* vertices.
//! Native polygon input and returned bounds cross a float32 boundary; joins
//! themselves use double precision. No tiny-skia bounds or pixel AA are used.
//!
//! Primary sources (fixed tag):
//! - <https://github.com/dotnet/wpf/blob/v9.0.0/src/Microsoft.DotNet.Wpf/src/PresentationFramework/System/Windows/FrameworkElement.cs>
//! - <https://github.com/dotnet/wpf/blob/v9.0.0/src/Microsoft.DotNet.Wpf/src/WpfGfx/core/geometry/strokefigure.cpp>
//! - <https://github.com/dotnet/wpf/blob/v9.0.0/src/Microsoft.DotNet.Wpf/src/WpfGfx/core/geometry/utils.cpp>
//!
//! `FrameworkElement` may additionally clip the arranged ink to the explicit
//! requested dimensions. Its rectangle is defined in local coordinates and
//! follows the same rotation/offset as the shape; returning it applies no mask.
//! In particular, WPF's alpha-bearing software target clips an isolated visual
//! layer, not each primitive's geometry. `drawingcontext.cpp` `PushEffects` /
//! `PushLayer` / `DrawLayer` and `sw/swlib/swsurfrt.cpp` `BeginLayerInternal` establish
//! that separate PM intermediate and geometric-mask pass. This layout API must
//! not be mistaken for a primitive-intersection rasterization contract.

use gif_from_screen_domain::{VectorShape, VectorShapeKind};

use crate::RenderError;

type Point = [f64; 2];

/// The untransformed arranged location and size. The relative rotation center
/// in local coordinates is `render_size / 2`, not the requested size / 2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Layout {
    pub offset: Point,
    pub render_size: Point,
    /// Optional local `[x, y, width, height]` from `GetLayoutClip`, excluding
    /// parent Canvas clipping. `Some` with a zero axis is an empty clip, not None.
    pub clip: Option<[f64; 4]>,
}

/// See the module contract: this does not implement arbitrary WPF layout trees.
pub(super) fn layout(shape: &VectorShape) -> Result<Layout, RenderError> {
    shape.validate().map_err(invalid)?;
    let width = f64::from(u32::try_from(shape.bounds.width_hundredths).map_err(invalid)?);
    let height = f64::from(u32::try_from(shape.bounds.height_hundredths).map_err(invalid)?);
    let requested = [width / 100.0, height / 100.0];
    let explicit_size = requested.map(round);
    let stroke = f64::from(shape.stroke_width_hundredths) / 100.0;
    let natural = match shape.kind {
        VectorShapeKind::Rectangle | VectorShapeKind::Ellipse => [stroke; 2],
        VectorShapeKind::Triangle => polygon_natural(&triangle(requested, stroke), stroke, false),
        VectorShapeKind::BlockArrow => polygon_natural(&arrow(requested, stroke), stroke, true),
    };
    let mut render_size = [0.0; 2];
    for axis in 0..2 {
        // Explicit dimension => min=max=rounded requested dimension. Measure
        // reports that clipped size but saves max(natural,min) as unclipped.
        // Arrange grows to the saved size and rounds before ArrangeOverride.
        render_size[axis] = round(natural[axis].max(explicit_size[axis]));
    }
    let x = f64::from(i32::try_from(shape.bounds.x_hundredths).map_err(invalid)?) / 100.0;
    let y = f64::from(i32::try_from(shape.bounds.y_hundredths).map_err(invalid)?) / 100.0;
    let result = Layout {
        offset: [round(x), round(y)],
        render_size,
        clip: local_clip(explicit_size, render_size),
    };
    if result
        .offset
        .into_iter()
        .chain(render_size)
        .any(|v| !v.is_finite())
    {
        return Err(invalid("WPF vector layout produced a non-finite value"));
    }
    Ok(result)
}

fn local_clip(explicit_size: Point, render_size: Point) -> Option<[f64; 4]> {
    // Fixed contract specialization of FrameworkElement.GetLayoutClip:
    // 1. Finite Width/Height force min=max=rounded explicit dimensions M.
    // 2. Canvas gives the child its clipped DesiredSize M as the layout slot.
    // 3. Arrange marks NeedsClipBounds if unclipped natural size exceeds M.
    // 4. GetLayoutClip sees maxClip=M and ink=RenderSize. Its local-clip test
    //    succeeds exactly when RenderSize exceeds M on either axis.
    // 5. After ink=min(RenderSize,M), ink=M, so slot=M cannot require another
    //    clip or alignment shift. It returns RectangleGeometry(0,0,Mw,Mh).
    // All compared dimensions are bounded integers; a difference is >=1, far
    // above DoubleUtil.LessThan's epsilon. Fractional unclipped size can set
    // NeedsClipBounds but still round to M; both GetLayoutClip tests then fail
    // and the native method returns null. Do not clip based on natural bounds.
    (render_size[0] > explicit_size[0] || render_size[1] > explicit_size[1]).then_some([
        0.0,
        0.0,
        explicit_size[0],
        explicit_size[1],
    ])
}

fn invalid(reason: impl std::fmt::Display) -> RenderError {
    RenderError::InvalidVectorShape {
        reason: reason.to_string(),
    }
}

fn round(value: f64) -> f64 {
    let rounded = value.round_ties_even();
    // WPF signed zero has no layout meaning; expose one canonical zero.
    if rounded == 0.0 { 0.0 } else { rounded }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "WPF polygon input and CBounds::SetRect explicitly convert double to float32"
)]
fn native_float(value: f64) -> f64 {
    f64::from(value as f32)
}

fn triangle([width, height]: Point, stroke: f64) -> [Point; 3] {
    let half = stroke / 2.0;
    [
        [width / 2.0, half],
        [width - half, height - half],
        [half, height - half],
    ]
}

fn arrow([width, height]: Point, stroke: f64) -> [Point; 9] {
    let w = width - stroke;
    let h = height - stroke;
    // Original Arrow.cs intentionally does not clamp negative extents and has
    // no half-stroke translation. These constants are not rounded layout size.
    [
        [w * 0.6898, h * 0.4],
        [0.0, h * 0.4],
        [0.0, h * 0.65],
        [w * 0.6898, h * 0.65],
        [w * 0.3684, h],
        [w * 0.6608, h],
        [w, h * 0.5],
        [w * 0.6608, 0.0],
        [w * 0.3684, 0.0],
    ]
}

const TOLERANCE: f64 = 0.25;
const TURN_FUZZ: f64 = 1.0e-4;
const DIVISION_FUZZ: f64 = 1.0e-6;
const LINE_LENGTH_SQUARED_MIN: f64 = TOLERANCE * TURN_FUZZ;

#[derive(Clone, Copy, Default)]
struct Edge {
    start: Point,
    end: Point,
    direction: Point,
    skipped_before: bool,
}

fn polygon_natural<const N: usize>(requested: &[Point; N], stroke: f64, smooth: bool) -> Point {
    // Fixed arrays: Triangle has 3 edges, Arrow 9. No data-dependent allocation
    // or unbounded subdivision, including degenerate and overscan requests.
    let points = requested.map(|p| p.map(native_float));
    if stroke == 0.0 {
        return points.into_iter().fold([0.0; 2], maximum);
    }
    let (edges, count, closing_skipped) = edges(&points);
    let radius = stroke / 2.0;
    let mut bound = [f64::NEG_INFINITY; 2];
    if count == 0 {
        // CWidener::WidenClosedFigure: an entirely degenerate closed path is a
        // point with round start/end caps, not an empty or miter-extended path.
        bound = add(points[0], [radius; 2]);
    } else {
        let mut pen_direction = normalize(edges[0].direction);
        for i in 0..count {
            let incoming = edges[i];
            let outgoing = edges[(i + 1) % count];
            let normal = scale([pen_direction[1], -pen_direction[0]], radius);
            for center in [incoming.start, incoming.end] {
                bound = maximum(bound, add(center, normal));
                bound = maximum(bound, sub(center, normal));
            }
            let skipped = outgoing.skipped_before || (i + 1 == count && closing_skipped);
            let turned = if smooth {
                // Arrow.cs marks every LineTo(..., true, true) as smooth;
                // CWidener forces Round even when the public Pen is Miter.
                round_join_bound(&mut bound, incoming, outgoing, radius, pen_direction)
            } else {
                join_bound(
                    &mut bound,
                    incoming,
                    outgoing,
                    radius,
                    if skipped { 1.0 } else { 10.0 },
                    pen_direction,
                )
            };
            if turned {
                pen_direction = normalize(outgoing.direction);
            }
            // A native flat-turn early exit does not update the pen radius
            // vector; the following line must retain its previous offset.
        }
    }
    bound.map(|v| native_float(v).max(0.0))
}

fn edges<const N: usize>(points: &[Point; N]) -> ([Edge; N], usize, bool) {
    let mut result = [Edge::default(); N];
    let mut count = 0;
    let mut current = points[0];
    let mut skipped = false;
    for i in 0..N {
        let end = points[(i + 1) % N];
        let direction = sub(end, current);
        // CLineSegment::GetFirstTangent skips the segment without advancing the
        // previous point. The next accepted segment therefore spans that gap.
        if dot(direction, direction) < LINE_LENGTH_SQUARED_MIN {
            skipped = true;
            continue;
        }
        result[count] = Edge {
            start: current,
            end,
            direction,
            skipped_before: skipped,
        };
        count += 1;
        current = end;
        skipped = false;
    }
    (result, count, skipped)
}

fn join_bound(
    bound: &mut Point,
    incoming: Edge,
    outgoing: Edge,
    radius: f64,
    limit: f64,
    unit_in: Point,
) -> bool {
    let det = cross(incoming.direction, outgoing.direction);
    let product = dot(incoming.direction, outgoing.direction);
    let unit_out = normalize(outgoing.direction);
    let center = outgoing.start;
    if det.abs() <= product.abs() * TURN_FUZZ {
        if product <= 0.0 {
            // CSimplePen::Do180DegreesMiter moves both rails along the incoming
            // direction to the nominal limit, then switches rail sides.
            let end = add(incoming.end, scale(unit_in, radius * limit));
            let normal = scale([unit_in[1], -unit_in[0]], radius);
            *bound = maximum(*bound, add(end, normal));
            *bound = maximum(*bound, sub(end, normal));
        }
        return product <= 0.0;
    }
    let side = if det > 0.0 { 1.0 } else { -1.0 };
    let outer = |unit: Point| scale([unit[1], -unit[0]], radius * side);
    let point_in = add(incoming.end, outer(unit_in));
    let point_out = add(center, outer(unit_out));
    *bound = maximum(*bound, point_out);
    let difference = sub(point_out, point_in);
    let numerator_in = cross(difference, outgoing.direction);
    let numerator_out = cross(difference, incoming.direction);
    let intersection_safe = numerator_in * det > 0.0
        && numerator_out * det < 0.0
        && det.abs() > numerator_in.abs() * DIVISION_FUZZ;
    let rad_squared = radius * radius;
    let rad_dot = -rad_squared * dot(unit_in, unit_out);
    let limit_squared = (radius * limit) * (radius * limit);
    if intersection_safe
        && rad_dot * limit_squared <= rad_squared * (limit_squared - 2.0 * rad_squared)
    {
        *bound = maximum(
            *bound,
            add(point_in, scale(incoming.direction, numerator_in / det)),
        );
    } else if !intersection_safe && rad_dot < 0.0 {
        *bound = maximum(*bound, point_out);
    } else {
        // MilLineJoin::Miter clips at the miter limit, unlike MiterClipped's
        // misleadingly named bevel fallback. Same half-angle construction as
        // CSimplePen::DoLimitedMiter, without an unstable bisector division.
        let denominator = rad_squared.midpoint(rad_dot).max(0.0).sqrt();
        let numerator = (radius * limit - ((rad_squared - rad_dot) / 2.0).max(0.0).sqrt()).max(0.0);
        if denominator > numerator * DIVISION_FUZZ {
            let distance = radius * numerator / denominator;
            *bound = maximum(*bound, add(point_in, scale(unit_in, distance)));
            *bound = maximum(*bound, sub(point_out, scale(unit_out, distance)));
        }
    }
    true
}

// The following bounded arc/bounds arithmetic follows dotnet/wpf v9.0.0,
// CSimplePen::RoundCorner/GetBezierDistance and CBounds::GetDerivativeZeros.
// Those sources are Copyright .NET Foundation, licensed MIT. Only these fixed
// polygon-join cases are modeled, not the general WPF pen/curve subsystem.
fn round_join_bound(
    bound: &mut Point,
    incoming: Edge,
    outgoing: Edge,
    radius: f64,
    pen_direction: Point,
) -> bool {
    let determinant = cross(incoming.direction, outgoing.direction);
    let product = dot(incoming.direction, outgoing.direction);
    if determinant.abs() <= product.abs() * TURN_FUZZ && product > 0.0 {
        return false;
    }
    let before = scale(pen_direction, radius);
    let after = scale(normalize(outgoing.direction), radius);
    let side = if determinant > 0.0 { 1.0 } else { -1.0 };
    let offset = |v: Point| scale([v[1], -v[0]], side);
    let start = add(incoming.end, offset(before));
    let end = add(outgoing.start, offset(after));
    let radius_squared = radius * radius;
    let product = dot(before, after);
    let refinement = if radius < TOLERANCE {
        -2.0
    } else {
        let ratio = 1.0 - TOLERANCE / radius;
        2.0 * ratio * ratio - 1.0
    } * radius_squared;
    if product > refinement {
        // Native RoundCorner deliberately substitutes the bevel for a shallow
        // arc at this *bounds* tolerance. Do not use an ideal circle extent.
        *bound = maximum(*bound, end);
    } else if product >= 0.0 {
        let distance = bezier_distance(product, radius);
        cubic_bound(
            bound,
            [
                start,
                add(start, scale(before, distance)),
                sub(end, scale(after, distance)),
                end,
            ],
        );
    } else {
        // WPF uses the complex square root of the two tangent radius vectors,
        // not atan2 or a tessellation. Preserve its double-precision sequence.
        let real_squared = after[0] * before[0] - after[1] * before[1];
        let imaginary_squared = after[0] * before[1] + after[1] * before[0];
        let sign = if imaginary_squared > 0.0 { 1.0 } else { -1.0 };
        let mut middle = [
            radius_squared.midpoint(real_squared).abs().sqrt(),
            sign * (0.5 * (radius_squared - real_squared)).abs().sqrt(),
        ];
        let direction = scale([after[1], -after[0]], side);
        if dot(middle, direction) < 0.0 {
            middle = scale(middle, -1.0);
        }
        let distance = bezier_distance(dot(after, middle).abs(), radius);
        let midpoint = add(outgoing.start, offset(middle));
        let middle_control = scale(middle, distance);
        cubic_bound(
            bound,
            [
                start,
                add(start, scale(before, distance)),
                sub(midpoint, middle_control),
                midpoint,
            ],
        );
        cubic_bound(
            bound,
            [
                midpoint,
                add(midpoint, middle_control),
                sub(end, scale(after, distance)),
                end,
            ],
        );
    }
    true
}

fn bezier_distance(product: f64, radius: f64) -> f64 {
    let squared = radius * radius;
    let average = squared.midpoint(product);
    if average < 0.0 || squared - average <= 0.0 {
        return 0.0;
    }
    let denominator = (squared - average).sqrt();
    let numerator = (4.0 / 3.0) * (radius - average.sqrt());
    if numerator <= denominator * DIVISION_FUZZ {
        0.0
    } else {
        numerator / denominator
    }
}

fn cubic_bound(bound: &mut Point, controls: [Point; 4]) {
    *bound = maximum(maximum(*bound, controls[0]), controls[3]);
    for axis in 0..2 {
        let values = controls.map(|p| p[axis]);
        let (roots, count) = derivative_roots(values);
        for &t in &roots[..count] {
            let s = 1.0 - t;
            let t_squared = t * t;
            let s_squared = s * s;
            let value = values[0] * s * s_squared
                + 3.0 * values[1] * t * s_squared
                + 3.0 * values[2] * t_squared * s
                + values[3] * t * t_squared;
            bound[axis] = bound[axis].max(value);
        }
    }
}

fn derivative_roots([start, control1, control2, end]: [f64; 4]) -> ([f64; 2], usize) {
    if (control1 - start) * (end - control1) >= 0.0 && (control2 - start) * (end - control2) >= 0.0
    {
        return ([0.0; 2], 0);
    }
    let a = control1 - start;
    let b = control2 - control1;
    let c = end - control2;
    if a.abs() < b.abs() * DIVISION_FUZZ && c.abs() < b.abs() * DIVISION_FUZZ {
        return ([0.0; 2], 0);
    }
    let reversed = a.abs() > c.abs();
    let (mut roots, count) = if reversed {
        positive_roots(a, b, c)
    } else {
        positive_roots(c, b, a)
    };
    for value in &mut roots[..count] {
        *value = if reversed {
            1.0 / (1.0 + *value)
        } else {
            *value / (1.0 + *value)
        };
    }
    (roots, count)
}

fn positive_roots(a: f64, b: f64, c: f64) -> ([f64; 2], usize) {
    let mut result = [0.0; 2];
    let mut count = 0;
    let discriminant = b * b - a * c;
    if discriminant > 0.0 {
        let root = discriminant.sqrt();
        for value in [(-b - root) / a, (-b + root) / a] {
            if value > 0.0 {
                result[count] = value;
                count += 1;
            }
        }
    }
    (result, count)
}

fn maximum(a: Point, b: Point) -> Point {
    [a[0].max(b[0]), a[1].max(b[1])]
}
fn add(a: Point, b: Point) -> Point {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1]]
}
fn scale(a: Point, factor: f64) -> Point {
    [a[0] * factor, a[1] * factor]
}
fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn cross(a: Point, b: Point) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn normalize(a: Point) -> Point {
    scale(a, 1.0 / dot(a, a).sqrt())
}

#[cfg(test)]
mod tests {
    use gif_from_screen_domain::{Rgba, VectorShapeBounds};

    use super::*;

    fn assert_point(actual: Point, expected: Point) {
        assert_eq!(
            actual.map(f64::to_bits),
            expected.map(f64::to_bits),
            "actual={actual:?}, expected={expected:?}"
        );
    }

    fn shape(kind: VectorShapeKind, rect: [i32; 4], stroke: u32) -> VectorShape {
        VectorShape {
            kind,
            bounds: VectorShapeBounds {
                x_hundredths: i64::from(rect[0]),
                y_hundredths: i64::from(rect[1]),
                width_hundredths: u64::try_from(rect[2]).unwrap(),
                height_hundredths: u64::try_from(rect[3]).unwrap(),
            },
            stroke_width_hundredths: stroke,
            ..VectorShape::default()
        }
    }

    #[test]
    fn measured_windows_layouts_match_exactly() {
        use VectorShapeKind::{BlockArrow, Ellipse, Rectangle, Triangle};
        // Real WPF output: /tmp/gfs-vector-layout-ca6fb2a/stdout.log. These are
        // layout observations, not synthetic pixel expectations.
        for (kind, rect, stroke, offset, render_size) in [
            (
                Rectangle,
                [100, 100, 1400, 1400],
                0,
                [1.0, 1.0],
                [14.0, 14.0],
            ),
            (
                Rectangle,
                [125, 175, 1250, 1125],
                50,
                [1.0, 2.0],
                [12.0, 11.0],
            ),
            (
                Triangle,
                [200, 100, 1000, 1400],
                125,
                [2.0, 1.0],
                [10.0, 14.0],
            ),
            (Rectangle, [625, 50, 300, 1500], 50, [6.0, 0.0], [3.0, 15.0]),
            (
                Triangle,
                [-25, 200, 1500, 1125],
                225,
                [0.0, 2.0],
                [16.0, 11.0],
            ),
            (
                BlockArrow,
                [25, 25, 1500, 1475],
                125,
                [0.0, 0.0],
                [15.0, 15.0],
            ),
            (
                Ellipse,
                [175, 225, 1125, 1250],
                50,
                [2.0, 2.0],
                [11.0, 12.0],
            ),
        ] {
            let actual = layout(&shape(kind, rect, stroke)).unwrap();
            // Original hosted log did not report GetClip. Preserve its exact
            // observed claims here; source-derived clip tests are separate.
            assert_point(actual.offset, offset);
            assert_point(actual.render_size, render_size);
        }
    }

    #[test]
    fn triangle_miter_natural_size_and_relative_rotation_center() {
        let mut request = shape(VectorShapeKind::Triangle, [-25, 200, 1500, 1125], 225);
        request.rotation_hundredths = 3300;
        let measured = layout(&request).unwrap();
        let center = scale(measured.render_size, 0.5);
        assert_point(center, [8.0, 5.5]);
        let (sin, cos) = 33.0_f64.to_radians().sin_cos();
        let origin = add(
            measured.offset,
            [
                center[0] * (1.0 - cos) + center[1] * sin,
                center[1] * (1.0 - cos) - center[0] * sin,
            ],
        );
        assert!((origin[0] - 4.286_150_149_019_257).abs() < 1.0e-13);
        assert!((origin[1] + 1.469_800_403_820_048_7).abs() < 1.0e-13);
    }

    #[test]
    fn hosted_extreme_layout_probes_confirm_arranged_sizes_and_actual_local_clips() {
        use VectorShapeKind::{BlockArrow, Triangle};
        // Windows producer 34309842487, source 4e28a35, actual
        // VisualTreeHelper.GetClip paths in VECTOR_LAYOUT logs. Expected sizes
        // and clip rectangles are independently observed, not computed here.
        for (kind, width, height, stroke, render_size, clip) in [
            (
                Triangle,
                1500,
                226,
                225,
                [25.0, 2.0],
                Some([0.0, 0.0, 15.0, 2.0]),
            ),
            (
                Triangle,
                100,
                200,
                1000,
                [14.0, 15.0],
                Some([0.0, 0.0, 1.0, 2.0]),
            ),
            (
                BlockArrow,
                100,
                200,
                1000,
                [5.0, 5.0],
                Some([0.0, 0.0, 1.0, 2.0]),
            ),
            (BlockArrow, 300, 1500, 200, [3.0, 15.0], None),
        ] {
            assert_eq!(
                layout(&shape(kind, [200, 200, width, height], stroke)).unwrap(),
                Layout {
                    offset: [2.0, 2.0],
                    render_size,
                    clip
                }
            );
        }
    }

    #[test]
    fn transparent_stroke_still_measures_but_zero_stroke_does_not() {
        let mut request = shape(VectorShapeKind::Triangle, [0, 0, 1500, 1125], 225);
        let opaque = layout(&request).unwrap();
        request.stroke = Rgba::TRANSPARENT;
        request.fill = None;
        assert_eq!(layout(&request).unwrap(), opaque);
        request.stroke_width_hundredths = 0;
        assert_point(layout(&request).unwrap().render_size, [15.0, 11.0]);
    }

    #[test]
    fn dimensions_smaller_than_stroke_preserve_natural_size_and_requested_vertices() {
        for kind in [VectorShapeKind::Rectangle, VectorShapeKind::Ellipse] {
            assert_point(
                layout(&shape(kind, [0, 0, 100, 200], 1000))
                    .unwrap()
                    .render_size,
                [10.0; 2],
            );
        }
        // Both custom shapes invert their raw interior axes instead of
        // clamping them to zero. This is distinct from Rectangle/Ellipse.
        assert_eq!(
            triangle([1.0, 2.0], 10.0),
            [[0.5, 5.0], [-4.0, -3.0], [5.0, -3.0]]
        );
        assert!(arrow([1.0, 2.0], 10.0)[6][0] < 0.0);
        for kind in [VectorShapeKind::Triangle, VectorShapeKind::BlockArrow] {
            assert!(
                layout(&shape(kind, [0, 0, 100, 200], 1000))
                    .unwrap()
                    .render_size
                    .into_iter()
                    .all(f64::is_finite)
            );
        }
    }

    #[test]
    fn ties_even_including_negative_positions_and_overscan() {
        let request = shape(VectorShapeKind::Rectangle, [-150, -250, 1250, 1350], 0);
        assert_eq!(
            layout(&request).unwrap(),
            Layout {
                offset: [-2.0, -2.0],
                render_size: [12.0, 14.0],
                clip: None,
            }
        );
        let request = shape(
            VectorShapeKind::Triangle,
            [-13_107_000, -13_107_000, 13_107_000, 13_107_000],
            10_000,
        );
        assert_point(layout(&request).unwrap().offset, [-131_070.0; 2]);
        assert!(
            layout(&request)
                .unwrap()
                .render_size
                .into_iter()
                .all(f64::is_finite)
        );
    }

    #[test]
    fn miter_limit_clips_instead_of_beveling_and_handles_reversal() {
        let mut bound = [0.0; 2];
        let incoming = Edge {
            start: [-1.0, 0.0],
            end: [0.0, 0.0],
            direction: [1.0, 0.0],
            skipped_before: false,
        };
        let outgoing = Edge {
            direction: [-1.0, 0.01],
            ..Edge::default()
        };
        join_bound(&mut bound, incoming, outgoing, 1.0, 10.0, [1.0, 0.0]);
        assert!(
            bound[0] > 9.9 && bound[0] < 10.1,
            "clipped miter, not bevel: {bound:?}"
        );
        let reverse = Edge {
            direction: [-1.0, 0.0],
            ..Edge::default()
        };
        bound = [0.0; 2];
        join_bound(&mut bound, incoming, reverse, 1.0, 10.0, [1.0, 0.0]);
        assert_point(bound, [10.0, 1.0]);
        bound = [0.0; 2];
        join_bound(&mut bound, incoming, reverse, 1.0, 1.0, [1.0, 0.0]);
        assert_point(bound, [1.0, 1.0]);
    }

    #[test]
    fn arrow_smooth_join_uses_round_bounds_not_triangle_miter() {
        let incoming = Edge {
            start: [-1.0, 0.0],
            end: [0.0, 0.0],
            direction: [1.0, 0.0],
            skipped_before: false,
        };
        let reverse = Edge {
            direction: [-1.0, 0.0],
            ..Edge::default()
        };
        let mut round = [0.0; 2];
        round_join_bound(&mut round, incoming, reverse, 1.0, [1.0, 0.0]);
        assert_point(round, [1.0, 1.0]);
        let mut miter = [0.0; 2];
        join_bound(&mut miter, incoming, reverse, 1.0, 10.0, [1.0, 0.0]);
        assert_point(miter, [10.0, 1.0]);
        let vertices = arrow([3.0, 15.0], 2.0);
        let smooth = polygon_natural(&vertices, 2.0, true);
        let sharp = polygon_natural(&vertices, 2.0, false);
        assert!(
            sharp[0] > smooth[0],
            "Arrow smooth flags must matter: {smooth:?} versus {sharp:?}"
        );
    }

    #[test]
    fn round_bounds_find_cubic_extrema_and_respect_native_shallow_arc_tolerance() {
        // Quarter arc straddles +X; neither endpoint is its X maximum.
        let incoming = Edge {
            direction: [1.0, 1.0],
            ..Edge::default()
        };
        let outgoing = Edge {
            direction: [-1.0, 1.0],
            ..Edge::default()
        };
        let mut bound = [0.0; 2];
        round_join_bound(
            &mut bound,
            incoming,
            outgoing,
            1.0,
            normalize(incoming.direction),
        );
        assert!((bound[0] - 1.0).abs() < 1.0e-14);
        // At radius < .25, WPF bounds widening substitutes a bevel even for
        // this right-angle join. The ideal circular max would incorrectly be .1.
        bound = [0.0; 2];
        round_join_bound(
            &mut bound,
            incoming,
            outgoing,
            0.1,
            normalize(incoming.direction),
        );
        assert!((bound[0] - 0.1 / 2.0_f64.sqrt()).abs() < 1.0e-14);
        // A cubic may exceed its endpoints: use its true extrema, not its
        // control hull (which would overestimate to 1 rather than 3/4).
        bound = [0.0; 2];
        cubic_bound(&mut bound, [[0.0, 0.0], [1.0, 1.0], [1.0, 1.0], [0.0, 0.0]]);
        assert_point(bound, [0.75, 0.75]);
    }

    #[test]
    fn zero_length_closed_polygon_becomes_round_point_not_miter_spike() {
        assert_point(polygon_natural(&[[5.0, 5.0]; 3], 10.0, false), [10.0; 2]);
        assert_point(polygon_natural(&[[0.0, 0.0]; 9], 10.0, true), [5.0; 2]);
        for width in [1, 499, 500, 501, 1000] {
            for height in [1, 499, 500, 501, 1000] {
                for kind in [VectorShapeKind::Triangle, VectorShapeKind::BlockArrow] {
                    let result = layout(&shape(kind, [-50, -50, width, height], 500)).unwrap();
                    assert!(
                        result
                            .render_size
                            .into_iter()
                            .all(|v| v.is_finite() && v >= 0.0)
                    );
                }
            }
        }
    }

    #[test]
    fn flat_turn_keeps_pen_direction_and_skipped_edges_keep_their_gap() {
        let incoming = Edge {
            direction: [1.0, 0.0],
            ..Edge::default()
        };
        let shallow = Edge {
            direction: [1.0, 0.000_01],
            ..Edge::default()
        };
        let mut bound = [0.0; 2];
        assert!(!join_bound(
            &mut bound,
            incoming,
            shallow,
            1.0,
            10.0,
            [1.0, 0.0]
        ));
        assert!(!round_join_bound(
            &mut bound,
            incoming,
            shallow,
            1.0,
            [1.0, 0.0]
        ));
        let (segments, count, _) = edges(&[[0.0, 0.0], [0.001, 0.0], [1.0, 0.0]]);
        assert_eq!(count, 2);
        assert!(segments[0].skipped_before);
        assert_point(segments[0].start, [0.0, 0.0]);
        assert_point(segments[0].end, [1.0, 0.0]);
    }

    #[test]
    fn native_float_boundary_precedes_layout_rounding() {
        // CBounds::SetRect's float32 conversion can move a double just above a
        // half-pixel onto the exact tie. Removing it would arrange to 17.
        assert_eq!(
            round(native_float(16.500_000_1)).to_bits(),
            16.0_f64.to_bits()
        );
        assert_eq!(round(16.500_000_1).to_bits(), 17.0_f64.to_bits());
        assert_eq!(
            native_float(131_069.99).to_bits(),
            131_069.992_187_5_f64.to_bits()
        );
    }

    #[test]
    fn invalid_metadata_is_rejected_before_calculation() {
        let mut request = VectorShape::default();
        request.bounds.x_hundredths = i64::MAX;
        assert!(layout(&request).is_err());
        request = VectorShape::default();
        request.bounds.width_hundredths = u64::MAX;
        assert!(layout(&request).is_err());
        request = VectorShape {
            version: 0,
            ..VectorShape::default()
        };
        assert!(layout(&request).is_err());
    }

    fn assert_clip(actual: Option<[f64; 4]>, expected: Option<[f64; 4]>) {
        assert_eq!(
            actual.map(|r| r.map(f64::to_bits)),
            expected.map(|r| r.map(f64::to_bits))
        );
    }

    #[test]
    fn expanded_natural_size_clips_both_axes_to_explicit_layout_size() {
        // Source-derived FE.GetLayoutClip expectations, not captured Windows
        // GetClip observations (those are being added independently).
        let request = shape(VectorShapeKind::Triangle, [-25, 200, 1500, 1125], 225);
        let result = layout(&request).unwrap();
        assert_point(result.render_size, [16.0, 11.0]);
        assert_clip(result.clip, Some([0.0, 0.0, 15.0, 11.0]));
        for kind in [VectorShapeKind::Rectangle, VectorShapeKind::Ellipse] {
            let result = layout(&shape(kind, [0, 0, 100, 200], 1000)).unwrap();
            assert_point(result.render_size, [10.0; 2]);
            assert_clip(result.clip, Some([0.0, 0.0, 1.0, 2.0]));
        }
    }

    #[test]
    fn rounding_does_not_invent_a_clip_from_fractional_natural_ink() {
        for kind in [VectorShapeKind::Rectangle, VectorShapeKind::Ellipse] {
            // Natural 1.49 exceeds requested rounded 1, but rounded RenderSize
            // is still 1: NeedsClipBounds alone does not yield a visual clip.
            let result = layout(&shape(kind, [0, 0, 100, 100], 149)).unwrap();
            assert_point(result.render_size, [1.0; 2]);
            assert_clip(result.clip, None);
            let result = layout(&shape(kind, [0, 0, 100, 100], 150)).unwrap();
            assert_point(result.render_size, [2.0; 2]);
            assert_clip(result.clip, Some([0.0, 0.0, 1.0, 1.0]));
        }
    }

    #[test]
    fn zero_axis_clip_is_retained_and_is_not_unbounded() {
        let result = layout(&shape(VectorShapeKind::Rectangle, [0, 0, 49, 150], 100)).unwrap();
        assert_point(result.render_size, [1.0, 2.0]);
        assert_clip(result.clip, Some([0.0, 0.0, 0.0, 2.0]));
        let result = layout(&shape(VectorShapeKind::Rectangle, [0, 0, 49, 49], 0)).unwrap();
        assert_point(result.render_size, [0.0; 2]);
        assert_clip(result.clip, None);
    }

    #[test]
    fn local_clip_does_not_absorb_translation_rotation_or_parent_canvas() {
        let mut request = shape(VectorShapeKind::Triangle, [0, 0, 1500, 1125], 225);
        let original = layout(&request).unwrap();
        request.bounds.x_hundredths = -150;
        request.bounds.y_hundredths = 65_535;
        request.rotation_hundredths = 3300;
        request.stroke = Rgba::TRANSPARENT;
        request.fill = None;
        let moved = layout(&request).unwrap();
        assert_point(moved.offset, [-2.0, 655.0]);
        assert_point(moved.render_size, original.render_size);
        assert_clip(moved.clip, original.clip);
        assert_point(scale(moved.render_size, 0.5), [8.0, 5.5]);
    }
}
