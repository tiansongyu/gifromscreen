//! Bounded 96-DPI closed-shape brush paths, separate from persisted V1 pixels.
//!
//! Adapted from dotnet/wpf a04736acb8edb533756131d3d5fc55f15cd03d6a:
//! core/geometry/{compactshapes.cpp,strokefigure.cpp,bezierflattener.cpp,utils.cpp}.
//! Copyright .NET Foundation, MIT; see
//! packaging/licenses/upstream/dotnet-wpf-MIT.txt. No tiny-skia widening is used.

use std::mem::size_of;

use gif_from_screen_domain::{VectorShape, VectorShapeKind};

use super::{point, rounded_rect, wpf_layout, wpf_vector_shape_geometry};
use crate::{
    CancellationToken, InkError, InkFigure, InkFillRule, InkLimits, InkPath, InkPoint, InkSegment,
};

/// Separate brush primitives; coverage is applied independently in fill/stroke order.
#[derive(Clone, Debug, PartialEq)]
pub struct WpfBrushPaths {
    /// Arranged and transformed geometry for the fill brush.
    pub fill: InkPath,
    /// Widened geometry for a nontransparent, nonzero-width stroke brush.
    pub stroke: Option<InkPath>,
    /// The local `FrameworkElement` layout clip transformed with the object.
    /// Its coverage applies to the completed offscreen shape layer; it is not
    /// intersected with each brush before antialiasing.
    pub layout_clip: Option<InkPath>,
}

type Result<T> = std::result::Result<T, InkError>;
type Point = [f64; 2];
const TOLERANCE: f64 = 0.25;
const TURN_FUZZ: f64 = 1.0e-4;
const DIVISION_FUZZ: f64 = 1.0e-6;

/// Prepares native-style paths without changing the VectorShape/V1 contract.
/// Transparent stroke still affects layout, but needs no painted stroke path.
///
/// # Errors
/// Rejects malformed geometry, exhausted point/segment/work/memory budgets,
/// allocation failures and cancellation.
pub fn prepare_wpf_brush_paths<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    limits: &InkLimits,
    cancellation: &C,
) -> Result<WpfBrushPaths> {
    prepare_wpf_brush_paths_measured(shape, limits, cancellation).map(|(paths, _)| paths)
}

/// Performs the unchanged preparation and reports the work units actually
/// charged by its budget, including its final cancellation/work check.
pub(crate) fn prepare_wpf_brush_paths_measured<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    limits: &InkLimits,
    cancellation: &C,
) -> Result<(WpfBrushPaths, u64)> {
    let mut budget = Budget::new(limits, cancellation)?;
    shape.validate().map_err(InkError::Invalid)?;
    // The existing shared fill builder has fixed complexity (at most eight
    // segments); include its transient original+result allocations before use.
    budget.memory(
        2 * (size_of::<InkPath>() + size_of::<InkFigure>() + 8 * size_of::<InkSegment>()),
    )?;
    budget.work(64)?;
    let fill = wpf_vector_shape_geometry(shape)
        .map_err(|error| InkError::Invalid(error.to_string()))?
        .outline;
    budget.count_path(&fill)?;
    let layout = wpf_layout::layout(shape).map_err(|error| InkError::Invalid(error.to_string()))?;
    let transform = Transform::new(shape.rotation_hundredths, layout.render_size, layout.offset);
    let layout_clip = layout
        .clip
        .map(|[x, y, width, height]| -> Result<InkPath> {
            let mut path = polygon_path(
                &[
                    [x, y],
                    [x + width, y],
                    [x + width, y + height],
                    [x, y + height],
                ],
                &mut budget,
            )?;
            transform_path(&mut path, transform);
            Ok(path)
        })
        .transpose()?;
    let stroke = if shape.stroke_width_hundredths == 0 || shape.stroke.alpha == 0 {
        None
    } else {
        Some(match shape.kind {
            VectorShapeKind::Rectangle => {
                rectangle_stroke(shape, layout.render_size, transform, &mut budget)?
            }
            _ => native_stroke(shape, layout.render_size, transform, &mut budget)?,
        })
    };
    budget.work(1)?;
    Ok((
        WpfBrushPaths {
            fill,
            stroke,
            layout_clip,
        },
        budget.work,
    ))
}

fn rectangle_stroke<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    size: Point,
    transform: Transform,
    budget: &mut Budget<'_, C>,
) -> Result<InkPath> {
    // DrawRoundedRectangle uses CRectangle for equal input radii. Its compact
    // widener constructs outer/inner rectangles BEFORE applying WorldToDevice;
    // it does not widen the already axis-clamped centerline cubic.
    let width = native(f64::from(shape.stroke_width_hundredths) / 100.0);
    let half = native(width * 0.5);
    let left = native(f64::from(shape.stroke_width_hundredths) / 200.0);
    let top = left;
    let right = native(
        left + native((size[0] - f64::from(shape.stroke_width_hundredths) / 100.0).max(0.0)),
    );
    let bottom =
        native(top + native((size[1] - f64::from(shape.stroke_width_hundredths) / 100.0).max(0.0)));
    let radius = native(f64::from(shape.corner_radius_hundredths) / 100.0);
    let mut result = InkPath {
        fill_rule: InkFillRule::EvenOdd,
        figures: Vec::new(),
    };
    let outer = [
        native(left - half),
        native(top - half),
        native(right + half),
        native(bottom + half),
    ];
    add_rectangle(
        &mut result,
        outer,
        native(radius + half),
        radius != 0.0,
        budget,
    )?;
    if width < native(right - left) && width < native(bottom - top) {
        let inner = [
            native(left + half),
            native(top + half),
            native(right - half),
            native(bottom - half),
        ];
        add_rectangle(
            &mut result,
            inner,
            native(radius - half).max(0.0),
            true,
            budget,
        )?;
    }
    transform_path(&mut result, transform);
    Ok(result)
}

fn add_rectangle<C: CancellationToken + ?Sized>(
    path: &mut InkPath,
    [left, top, right, bottom]: [f64; 4],
    radius: f64,
    rounded: bool,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    let width = native(right - left);
    let height = native(bottom - top);
    let mut figure = if rounded && radius != 0.0 {
        budget.memory(size_of::<InkFigure>() + 8 * size_of::<InkSegment>())?;
        rounded_rect(
            left,
            top,
            width,
            height,
            radius.min(width / 2.0),
            radius.min(height / 2.0),
        )
    } else {
        let mut figure = InkFigure {
            start: point(left, top),
            segments: Vec::new(),
            closed: true,
        };
        for to in [point(right, top), point(right, bottom), point(left, bottom)] {
            budget.push(&mut figure.segments, InkSegment::LineTo(to))?;
        }
        figure
    };
    map_figure(&mut figure, |point| [native(point[0]), native(point[1])]);
    budget.count_figure(&figure)?;
    budget.push(&mut path.figures, figure)
}

fn polygon_path<C: CancellationToken + ?Sized>(
    points: &[Point],
    budget: &mut Budget<'_, C>,
) -> Result<InkPath> {
    let mut figure = InkFigure {
        start: point(points[0][0], points[0][1]),
        segments: Vec::new(),
        closed: true,
    };
    for p in &points[1..] {
        budget.push(&mut figure.segments, InkSegment::LineTo(point(p[0], p[1])))?;
    }
    budget.count_figure(&figure)?;
    let mut figures = Vec::new();
    budget.push(&mut figures, figure)?;
    Ok(InkPath {
        fill_rule: InkFillRule::NonZero,
        figures,
    })
}

fn native_stroke<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    size: Point,
    transform: Transform,
    budget: &mut Budget<'_, C>,
) -> Result<InkPath> {
    let width = native(f64::from(shape.stroke_width_hundredths) / 100.0);
    let radius = transform.radius(width / 2.0);
    if radius < TOLERANCE * 0.004 {
        return Ok(InkPath::default());
    }
    let mut figure = stroke_spine(shape, size, width, budget)?;
    map_figure(&mut figure, |p| transform.point(p));
    let smooth = shape.kind != VectorShapeKind::Triangle;
    let mut pen = None;
    let mut current = from_ink(figure.start);
    let original = current;
    let mut first = current;
    let mut first_tangent = [0.0; 2];
    let mut incoming = [0.0; 2];
    let mut skipped = false;
    let mut first_skipped = false;
    for segment in figure
        .segments
        .iter()
        .copied()
        .chain(std::iter::once(InkSegment::LineTo(figure.start)))
    {
        budget.work(1)?;
        let tangent = match segment {
            InkSegment::LineTo(to) => {
                let vector = sub(from_ink(to), current);
                (dot(vector, vector) >= TOLERANCE * TURN_FUZZ).then_some(vector)
            }
            InkSegment::CubicTo {
                control1,
                control2,
                to,
            } => first_curve_tangent([
                current,
                from_ink(control1),
                from_ink(control2),
                from_ink(to),
            ]),
        };
        let Some(tangent) = tangent else {
            skipped = true;
            first_skipped |= pen.is_none();
            continue;
        };
        if let Some(pen) = &mut pen {
            Pen::corner(
                pen, current, incoming, tangent, smooth, skipped, false, budget,
            )?;
        } else {
            first = current;
            first_tangent = tangent;
            pen = Some(Pen::new(current, tangent, radius, budget)?);
        }
        let pen = pen.as_mut().expect("initialized pen");
        match segment {
            InkSegment::LineTo(to) => {
                current = from_ink(to);
                pen.line(current, budget)?;
                incoming = tangent;
            }
            InkSegment::CubicTo {
                control1,
                control2,
                to,
            } => {
                let curve = [
                    current,
                    from_ink(control1),
                    from_ink(control2),
                    from_ink(to),
                ];
                incoming = flatten_with_tangents(curve, pen, budget)?;
                current = from_ink(to);
            }
        }
        skipped = false;
    }
    if let Some(mut pen) = pen {
        pen.corner(
            first,
            incoming,
            first_tangent,
            smooth,
            skipped || first_skipped,
            true,
            budget,
        )?;
        pen.finish(budget)
    } else {
        point_stroke(original, radius, budget)
    }
}

fn stroke_spine<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    size: Point,
    width: f64,
    budget: &mut Budget<'_, C>,
) -> Result<InkFigure> {
    let mut local = *shape;
    local.bounds.x_hundredths = 0;
    local.bounds.y_hundredths = 0;
    local.rotation_hundredths = 0;
    budget.memory(size_of::<InkFigure>() + 8 * size_of::<InkSegment>())?;
    if shape.kind == VectorShapeKind::Ellipse {
        Ok(ellipse(size, width))
    } else {
        Ok(wpf_vector_shape_geometry(&local)
            .map_err(|e| InkError::Invalid(e.to_string()))?
            .outline
            .figures
            .remove(0))
    }
}

fn point_stroke<C: CancellationToken + ?Sized>(
    center: Point,
    radius: f64,
    budget: &mut Budget<'_, C>,
) -> Result<InkPath> {
    // An entirely degenerate closed stroke uses round caps in CWidener.
    budget.memory(size_of::<InkFigure>() + 8 * size_of::<InkSegment>())?;
    let mut figure = rounded_rect(
        center[0] - radius,
        center[1] - radius,
        radius * 2.0,
        radius * 2.0,
        radius,
        radius,
    );
    map_figure(&mut figure, |p| p.map(native));
    budget.count_figure(&figure)?;
    let mut figures = Vec::new();
    budget.push(&mut figures, figure)?;
    Ok(InkPath {
        fill_rule: InkFillRule::NonZero,
        figures,
    })
}

fn ellipse(size: Point, stroke: f64) -> InkFigure {
    // CFigureData::InitAsEllipse starts at the rightmost point, clockwise.
    const ARC: f64 = 0.552_284_749_830_793_4;
    let radii = [
        native((size[0] - stroke).max(0.0) / 2.0),
        native((size[1] - stroke).max(0.0) / 2.0),
    ];
    let center = [
        native(stroke / 2.0 + radii[0]),
        native(stroke / 2.0 + radii[1]),
    ];
    let mid = radii.map(|r| native(r * ARC));
    let coord = |x, y| point(native(center[0] + x), native(center[1] + y));
    InkFigure {
        start: coord(radii[0], 0.0),
        closed: true,
        segments: vec![
            InkSegment::CubicTo {
                control1: coord(radii[0], mid[1]),
                control2: coord(mid[0], radii[1]),
                to: coord(0.0, radii[1]),
            },
            InkSegment::CubicTo {
                control1: coord(-mid[0], radii[1]),
                control2: coord(-radii[0], mid[1]),
                to: coord(-radii[0], 0.0),
            },
            InkSegment::CubicTo {
                control1: coord(-radii[0], -mid[1]),
                control2: coord(-mid[0], -radii[1]),
                to: coord(0.0, -radii[1]),
            },
            InkSegment::CubicTo {
                control1: coord(mid[0], -radii[1]),
                control2: coord(radii[0], -mid[1]),
                to: coord(radii[0], 0.0),
            },
        ],
    }
}

struct Rail {
    start: Point,
    segments: Vec<InkSegment>,
}
impl Rail {
    fn end(&self) -> Point {
        self.segments
            .last()
            .map_or(self.start, |segment| end(*segment))
    }
    fn line<C: CancellationToken + ?Sized>(
        &mut self,
        to: Point,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        let segment = InkSegment::LineTo(to_ink(to.map(native)));
        budget.count_segment(segment)?;
        budget.push(&mut self.segments, segment)
    }
    fn curve<C: CancellationToken + ?Sized>(
        &mut self,
        vertices: [Point; 3],
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        let [control1, control2, to] = vertices.map(|p| to_ink(p.map(native)));
        let segment = InkSegment::CubicTo {
            control1,
            control2,
            to,
        };
        budget.count_segment(segment)?;
        budget.push(&mut self.segments, segment)
    }
    fn set_end(&mut self, to: Point) {
        let to = to_ink(to.map(native));
        match self.segments.last_mut() {
            Some(InkSegment::LineTo(p) | InkSegment::CubicTo { to: p, .. }) => *p = to,
            None => self.start = from_ink(to),
        }
    }
}

struct Pen {
    radius: f64,
    refinement: f64,
    radial: Point,
    offset: Point,
    previous: Point,
    tangent: Point,
    current: [Point; 2],
    rails: [Rail; 2],
    extras: Vec<InkFigure>,
}

impl Pen {
    fn new<C: CancellationToken + ?Sized>(
        center: Point,
        direction: Point,
        radius: f64,
        budget: &mut Budget<'_, C>,
    ) -> Result<Self> {
        budget.work(1)?;
        budget.points = budget
            .points
            .checked_add(2)
            .filter(|n| *n <= budget.limits.max_points)
            .ok_or_else(|| InkError::Limit("WPF brush point limit exceeded".into()))?;
        let radial = normalize(direction, radius)?;
        let offset = right(radial);
        let current = [sub(center, offset), add(center, offset)];
        let start = current[1].map(native);
        let mut rails = [
            Rail {
                start,
                segments: Vec::new(),
            },
            Rail {
                start,
                segments: Vec::new(),
            },
        ];
        rails[0].line(current[0], budget)?;
        let refinement = if radius < TOLERANCE {
            -2.0
        } else {
            let ratio = 1.0 - TOLERANCE / radius;
            2.0 * ratio * ratio - 1.0
        } * radius
            * radius;
        Ok(Self {
            radius,
            refinement,
            radial,
            offset,
            previous: center,
            tangent: direction,
            current,
            rails,
            extras: Vec::new(),
        })
    }

    fn line<C: CancellationToken + ?Sized>(
        &mut self,
        point: Point,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        self.current = [sub(point, self.offset), add(point, self.offset)];
        self.previous = point;
        for side in 0..2 {
            self.rails[side].line(self.current[side], budget)?;
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "literal native corner contract preserves tangents, smooth/skipped/closing flags independently"
    )]
    fn corner<C: CancellationToken + ?Sized>(
        &mut self,
        center: Point,
        incoming: Point,
        outgoing: Point,
        smooth: bool,
        skipped: bool,
        closing: bool,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        budget.work(1)?;
        let radial = normalize(outgoing, self.radius)?;
        let offset = right(radial);
        let next = [sub(center, offset), add(center, offset)];
        let determinant = cross(incoming, outgoing);
        let product = dot(incoming, outgoing);
        let flat = determinant.abs() <= product.abs() * TURN_FUZZ;
        if flat && product > 0.0 {
            return Ok(());
        } // Crucially retains old pen offset.
        // GetTurningInfo retains its default RIGHT side for near-180 turns,
        // even if the tiny determinant is positive.
        let side = if flat {
            1
        } else {
            usize::from(determinant <= 0.0)
        };
        let limit = if skipped { 1.0 } else { 10.0 };
        if flat && !smooth {
            let extension = scale(self.radial, limit);
            for side in 0..2 {
                self.current[side] = add(self.current[side], extension);
                self.rails[side].set_end(self.current[side]);
            }
            let ends = [self.rails[0].end(), self.rails[1].end()];
            self.rails[0].line(ends[1], budget)?;
            self.rails[1].line(ends[0], budget)?;
        } else {
            self.rails[1 - side].line(center, budget)?;
            self.rails[1 - side].line(next[1 - side], budget)?;
            if smooth {
                self.round_corner(center, next[side], radial, side, budget)?;
            } else {
                self.miter(
                    incoming,
                    outgoing,
                    radial,
                    next[side],
                    determinant,
                    side,
                    limit,
                    closing,
                    budget,
                )?;
            }
        }
        self.radial = radial;
        self.offset = offset;
        self.previous = center;
        self.tangent = outgoing;
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "native miter's guarded intersection and closing-extension inputs remain explicit"
    )]
    fn miter<C: CancellationToken + ?Sized>(
        &mut self,
        incoming: Point,
        outgoing: Point,
        radial: Point,
        next: Point,
        determinant: f64,
        side: usize,
        limit: f64,
        closing: bool,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        let difference = sub(next, self.current[side]);
        let numerator_in = cross(difference, outgoing);
        let numerator_out = cross(difference, incoming);
        let safe = numerator_in * determinant > 0.0
            && numerator_out * determinant < 0.0
            && determinant.abs() > numerator_in.abs() * DIVISION_FUZZ;
        let product = -dot(radial, self.radial);
        let squared = self.radius * self.radius;
        let limit_squared = (limit * self.radius) * (limit * self.radius);
        let miter = if !safe {
            (product < 0.0).then_some(next)
        } else if product * limit_squared <= squared * (limit_squared - 2.0 * squared) {
            Some(add(
                self.current[side],
                scale(incoming, numerator_in / determinant),
            ))
        } else {
            None
        };
        if let Some(miter) = miter {
            self.rails[side].line(miter, budget)?;
            self.current[side] = miter;
            if closing {
                self.rails[side].line(next, budget)?;
                self.current[side] = next;
            }
        } else {
            let denominator = squared.midpoint(product);
            if denominator > 0.0 {
                let denominator = denominator.sqrt();
                let numerator =
                    (self.radius * limit - ((squared - product) / 2.0).max(0.0).sqrt()).max(0.0);
                if denominator > numerator * DIVISION_FUZZ {
                    let ratio = numerator / denominator;
                    for p in [
                        add(self.current[side], scale(self.radial, ratio)),
                        sub(next, scale(radial, ratio)),
                        next,
                    ] {
                        self.rails[side].line(p, budget)?;
                    }
                    self.current[side] = next;
                }
            }
        }
        Ok(())
    }

    fn round_corner<C: CancellationToken + ?Sized>(
        &mut self,
        center: Point,
        next: Point,
        radial: Point,
        side: usize,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        budget.work(1)?;
        let product = dot(radial, self.radial);
        let start = self.current[side];
        if product > self.refinement {
            self.rails[side].line(next, budget)?;
        } else if product >= 0.0 {
            let distance = bezier_distance(product, self.radius);
            self.rails[side].curve(
                [
                    add(start, scale(self.radial, distance)),
                    sub(next, scale(radial, distance)),
                    next,
                ],
                budget,
            )?;
        } else {
            let real_squared = radial[0] * self.radial[0] - radial[1] * self.radial[1];
            let imaginary_squared = radial[0] * self.radial[1] + radial[1] * self.radial[0];
            let squared = self.radius * self.radius;
            let mut middle = [
                squared.midpoint(real_squared).abs().sqrt(),
                if imaginary_squared > 0.0 { 1.0 } else { -1.0 }
                    * (0.5 * (squared - real_squared)).abs().sqrt(),
            ];
            let direction = if side == 0 {
                scale(right(radial), -1.0)
            } else {
                right(radial)
            };
            if dot(middle, direction) < 0.0 {
                middle = scale(middle, -1.0);
            }
            let distance = bezier_distance(dot(radial, middle).abs(), self.radius);
            let midpoint = add(
                center,
                scale(right(middle), if side == 0 { -1.0 } else { 1.0 }),
            );
            let control = scale(middle, distance);
            self.rails[side].curve(
                [
                    add(start, scale(self.radial, distance)),
                    sub(midpoint, control),
                    midpoint,
                ],
                budget,
            )?;
            self.rails[side].curve(
                [
                    add(midpoint, control),
                    sub(next, scale(radial, distance)),
                    next,
                ],
                budget,
            )?;
        }
        self.current[side] = next;
        Ok(())
    }

    fn round_to<C: CancellationToken + ?Sized>(
        &mut self,
        radial: Point,
        center: Point,
        incoming: Point,
        outgoing: Point,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        let offset = right(radial);
        let next = [sub(center, offset), add(center, offset)];
        let side = usize::from(cross(incoming, outgoing) <= 0.0);
        self.round_corner(center, next[side], radial, side, budget)?;
        self.radial = radial;
        self.offset = offset;
        self.previous = center;
        Ok(())
    }

    fn curve_point<C: CancellationToken + ?Sized>(
        &mut self,
        point: Point,
        tangent: Point,
        last: bool,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        budget.work(1)?;
        let direction = sub(point, self.previous);
        let radial = if dot(tangent, tangent) >= DIVISION_FUZZ * DIVISION_FUZZ {
            normalize(tangent, self.radius)?
        } else {
            self.radial
        };
        if dot(self.radial, radial) < self.refinement {
            if dot(direction, direction) >= DIVISION_FUZZ * DIVISION_FUZZ {
                let along = normalize(direction, self.radius)?;
                self.round_to(along, self.previous, self.tangent, direction, budget)?;
            }
            self.curve_quad(point, direction, budget)?;
            self.round_to(radial, point, direction, tangent, budget)?;
            if last {
                self.curve_quad(point, tangent, budget)?;
            }
        } else {
            self.radial = radial;
            self.offset = right(radial);
            self.curve_quad(point, direction, budget)?;
        }
        self.tangent = tangent;
        self.previous = point;
        Ok(())
    }

    fn curve_quad<C: CancellationToken + ?Sized>(
        &mut self,
        point: Point,
        direction: Point,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        self.current = [sub(point, self.offset), add(point, self.offset)];
        for side in 0..2 {
            let current = self.rails[side].end();
            if dot(sub(self.current[side], current), direction) < 0.0 {
                let triangle = if side == 0 {
                    [current, self.previous, self.current[side]]
                } else {
                    [current, self.current[side], self.previous]
                };
                let mut path = polygon_path(&triangle, budget)?;
                map_figure(&mut path.figures[0], |p| p.map(native));
                budget.push(&mut self.extras, path.figures.remove(0))?;
                self.rails[side].line(point, budget)?;
            }
            self.rails[side].line(self.current[side], budget)?;
        }
        Ok(())
    }

    fn finish<C: CancellationToken + ?Sized>(
        mut self,
        budget: &mut Budget<'_, C>,
    ) -> Result<InkPath> {
        // End flat cap modifies the left endpoint before connecting to right.
        self.rails[0].set_end(self.current[0]);
        self.rails[0].line(self.current[1], budget)?;
        let [mut left, right] = self.rails;
        for index in (0..right.segments.len()).rev() {
            let to = if index == 0 {
                right.start
            } else {
                end(right.segments[index - 1])
            };
            match right.segments[index] {
                InkSegment::LineTo(_) => left.line(to, budget)?,
                InkSegment::CubicTo {
                    control1, control2, ..
                } => left.curve([from_ink(control2), from_ink(control1), to], budget)?,
            }
        }
        let figure = InkFigure {
            start: to_ink(left.start),
            segments: left.segments,
            closed: true,
        };
        budget.push(&mut self.extras, figure)?;
        Ok(InkPath {
            fill_rule: InkFillRule::NonZero,
            figures: self.extras,
        })
    }
}

fn first_curve_tangent(curve: [Point; 4]) -> Option<Point> {
    curve[1..]
        .iter()
        .map(|p| sub(*p, curve[0]))
        .find(|p| dot(*p, *p) > TOLERANCE * TOLERANCE * TURN_FUZZ)
}

fn flatten_with_tangents<C: CancellationToken + ?Sized>(
    curve: [Point; 4],
    pen: &mut Pen,
    budget: &mut Budget<'_, C>,
) -> Result<Point> {
    // Native double-HFD widener. This is distinct from integer HFD used when
    // the completed brush outline later reaches the 28.4 filling rasterizer.
    let mut basis = [
        curve[0],
        sub(curve[3], curve[0]),
        scale(add(sub(curve[1], scale(curve[2], 2.0)), curve[3]), 6.0),
        scale(add(sub(curve[0], scale(curve[1], 2.0)), curve[2]), 6.0),
    ];
    let mut steps = 1_u32;
    let mut step_size = 1.0;
    while norm_max(basis[2]).max(norm_max(basis[3])) > 6.0 * TOLERANCE && step_size > 0.001 {
        budget.work(1)?;
        half(&mut basis, &mut steps, &mut step_size)?;
    }
    while steps > 1 {
        budget.work(1)?;
        basis[0] = add(basis[0], basis[1]);
        let previous = basis[2];
        basis[1] = add(basis[1], previous);
        basis[2] = sub(add(basis[2], previous), basis[3]);
        basis[3] = previous;
        let tangent = sub(sub(scale(basis[1], 6.0), basis[2]), scale(basis[3], 2.0));
        pen.curve_point(basis[0], tangent, false, budget)?;
        steps -= 1;
        if norm_max(basis[2]) > 6.0 * TOLERANCE && step_size > 0.001 {
            half(&mut basis, &mut steps, &mut step_size)?;
        } else {
            while steps.is_multiple_of(2)
                && norm_max(basis[3]) <= 1.5 * TOLERANCE
                && norm_max(sub(scale(basis[2], 2.0), basis[3])) <= 1.5 * TOLERANCE
            {
                budget.work(1)?;
                basis[1] = add(scale(basis[1], 2.0), basis[2]);
                let temporary = sub(scale(basis[2], 2.0), basis[3]);
                basis[3] = scale(basis[3], 4.0);
                basis[2] = scale(temporary, 4.0);
                steps /= 2;
                step_size *= 2.0;
            }
        }
    }
    let fuzz = TOLERANCE * TOLERANCE * TURN_FUZZ / 8.0;
    let tangent = [curve[2], curve[1], curve[0]]
        .into_iter()
        .map(|p| sub(curve[3], p))
        .find(|p| dot(*p, *p) > fuzz)
        .ok_or_else(|| InkError::Invalid("WPF cubic has no final tangent".into()))?;
    pen.curve_point(curve[3], tangent, true, budget)?;
    Ok(tangent)
}

fn half(basis: &mut [Point; 4], steps: &mut u32, step_size: &mut f64) -> Result<()> {
    basis[2] = scale(add(basis[2], basis[3]), 0.125);
    basis[1] = scale(sub(basis[1], basis[2]), 0.5);
    basis[3] = scale(basis[3], 0.25);
    *steps = steps
        .checked_mul(2)
        .filter(|v| *v <= 2_048)
        .ok_or_else(|| InkError::Limit("WPF cubic step budget exceeded".into()))?;
    *step_size *= 0.5;
    Ok(())
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

fn normalize(point: Point, radius: f64) -> Result<Point> {
    let length = (point[0] * point[0] + point[1] * point[1]).sqrt();
    if length <= 0.0 || !length.is_finite() {
        return Err(InkError::Invalid(
            "WPF pen tangent must be finite and nonzero".into(),
        ));
    }
    Ok(scale(point, radius / length))
}
fn from_ink(point: InkPoint) -> Point {
    [point.x, point.y]
}
fn to_ink(p: Point) -> InkPoint {
    point(p[0], p[1])
}
fn end(segment: InkSegment) -> Point {
    match segment {
        InkSegment::LineTo(p) | InkSegment::CubicTo { to: p, .. } => from_ink(p),
    }
}
fn add(a: Point, b: Point) -> Point {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1]]
}
fn scale(a: Point, value: f64) -> Point {
    [a[0] * value, a[1] * value]
}
fn right(a: Point) -> Point {
    [-a[1], a[0]]
}
fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn cross(a: Point, b: Point) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn norm_max(a: Point) -> f64 {
    a[0].abs().max(a[1].abs())
}

#[derive(Clone, Copy)]
struct Transform {
    cosine: f64,
    sine: f64,
    x: f64,
    y: f64,
}

impl Transform {
    fn new(angle: u16, render_size: Point, offset: Point) -> Self {
        let (sine, cosine) = match angle {
            0 => (0.0, 1.0),
            9_000 => (1.0, 0.0),
            18_000 => (0.0, -1.0),
            27_000 => (-1.0, 0.0),
            _ => (f64::from(angle) / 100.0).to_radians().sin_cos(),
        };
        let center = [render_size[0] / 2.0, render_size[1] / 2.0];
        Self {
            cosine: native(cosine),
            sine: native(sine),
            x: native(center[0] - center[0] * cosine + center[1] * sine + offset[0]),
            y: native(center[1] - center[0] * sine - center[1] * cosine + offset[1]),
        }
    }

    fn point(self, point: Point) -> Point {
        let p = point.map(native);
        [
            native(native(native(self.cosine * p[0]) - native(self.sine * p[1])) + self.x),
            native(native(native(self.sine * p[0]) + native(self.cosine * p[1])) + self.y),
        ]
    }

    fn radius(self, radius: f64) -> f64 {
        ((radius * self.cosine) * (radius * self.cosine)
            + (radius * self.sine) * (radius * self.sine))
            .sqrt()
    }
}

fn transform_path(path: &mut InkPath, transform: Transform) {
    for figure in &mut path.figures {
        map_figure(figure, |p| transform.point(p));
    }
}

fn map_figure(figure: &mut InkFigure, map: impl Fn(Point) -> Point) {
    let transform = |p: &mut InkPoint| {
        let result = map([p.x, p.y]);
        *p = point(result[0], result[1]);
    };
    transform(&mut figure.start);
    for segment in &mut figure.segments {
        match segment {
            InkSegment::LineTo(to) => transform(to),
            InkSegment::CubicTo {
                control1,
                control2,
                to,
            } => {
                transform(control1);
                transform(control2);
                transform(to);
            }
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "WPF native geometry, matrix and sink boundaries explicitly store float32"
)]
fn native(value: f64) -> f64 {
    f64::from(value as f32)
}

struct Budget<'a, C: CancellationToken + ?Sized> {
    limits: &'a InkLimits,
    cancel: &'a C,
    work: u64,
    memory: usize,
    points: usize,
    segments: usize,
}

impl<'a, C: CancellationToken + ?Sized> Budget<'a, C> {
    fn new(limits: &'a InkLimits, cancel: &'a C) -> Result<Self> {
        let mut value = Self {
            limits,
            cancel,
            work: 0,
            memory: 0,
            points: 0,
            segments: 0,
        };
        value.work(0)?;
        Ok(value)
    }

    fn work(&mut self, count: u64) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(InkError::Cancelled);
        }
        self.work = self
            .work
            .checked_add(count)
            .filter(|n| *n <= self.limits.max_work)
            .ok_or_else(|| InkError::Limit("WPF brush work limit exceeded".into()))?;
        Ok(())
    }

    fn memory(&mut self, count: usize) -> Result<()> {
        self.work(1)?;
        self.memory = self
            .memory
            .checked_add(count)
            .filter(|n| *n <= self.limits.max_bytes)
            .ok_or_else(|| InkError::Limit("WPF brush memory limit exceeded".into()))?;
        Ok(())
    }

    fn push<T>(&mut self, values: &mut Vec<T>, value: T) -> Result<()> {
        self.work(1)?;
        if values.len() == values.capacity() {
            let additional = values.capacity().max(4);
            let old = values.capacity() * size_of::<T>();
            self.memory(
                additional
                    .checked_mul(size_of::<T>())
                    .and_then(|v| v.checked_add(old))
                    .ok_or_else(|| InkError::Limit("WPF brush allocation size overflow".into()))?,
            )?;
            let previous = values.capacity();
            values
                .try_reserve_exact(additional)
                .map_err(|_| InkError::Limit("Could not allocate WPF brush geometry".into()))?;
            self.memory -= old;
            if values.capacity() > previous + additional {
                self.memory((values.capacity() - previous - additional) * size_of::<T>())?;
            }
        }
        values.push(value);
        Ok(())
    }

    fn count_figure(&mut self, figure: &InkFigure) -> Result<()> {
        self.work(1)?;
        self.points = self
            .points
            .checked_add(1)
            .ok_or_else(|| InkError::Limit("WPF point count overflow".into()))?;
        for segment in &figure.segments {
            self.count_segment(*segment)?;
        }
        if self.points > self.limits.max_points {
            return Err(InkError::Limit("WPF brush point limit exceeded".into()));
        }
        Ok(())
    }

    fn count_segment(&mut self, segment: InkSegment) -> Result<()> {
        self.work(1)?;
        self.points = self
            .points
            .checked_add(if matches!(segment, InkSegment::LineTo(_)) {
                1
            } else {
                3
            })
            .filter(|n| *n <= self.limits.max_points)
            .ok_or_else(|| InkError::Limit("WPF brush point limit exceeded".into()))?;
        self.segments = self
            .segments
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_segments)
            .ok_or_else(|| InkError::Limit("WPF brush segment limit exceeded".into()))?;
        Ok(())
    }

    fn count_path(&mut self, path: &InkPath) -> Result<()> {
        for figure in &path.figures {
            self.count_figure(figure)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "wpf_brush_tests.rs"]
mod tests;
