//! Version-one physical-pixel object geometry. Legacy `Shape` never enters here.
//!
//! Contours follow the pinned `ScreenToGif` a4d0a67 shape definitions and WPF
//! rectangle/ellipse layout at 96 DPI. The later tiny-skia AA rasterization is
//! an independent renderer, not a claim of byte equality with WPF.

use gif_from_screen_domain::{VectorShape, VectorShapeKind};

use crate::{InkFigure, InkFillRule, InkPath, InkPoint, InkSegment, RenderError};

mod hit;
mod raster;
mod wpf;
mod wpf_layout;
pub(crate) use raster::{MAX_WORK, paint};
pub use wpf::wpf_vector_shape_geometry;

/// Maximum objects accepted by the independent editable-overlay preview.
pub const MAX_VECTOR_PREVIEW_SHAPES: usize = 256;

/// Bounded shared contour for painting guides, picking and marquee intersection.
/// The contour includes its physical translation and clockwise center rotation.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorShapeGeometry {
    outline: InkPath,
    bounds: [InkPoint; 2],
    nonempty_fill: bool,
}

impl VectorShapeGeometry {
    /// Original line/cubic centerline, with an explicitly closed figure.
    pub fn outline(&self) -> &InkPath {
        &self.outline
    }

    /// Conservative contour bounds, excluding stroke expansion.
    pub const fn bounds(&self) -> [InkPoint; 2] {
        self.bounds
    }

    /// Closed-contour interior/boundary picking, independent of paint alpha.
    /// Non-finite positions and zero-area contours are not selectable.
    pub fn hit_test(&self, point: InkPoint) -> bool {
        self.nonempty_fill && hit::contains(&self.outline.figures[0], point)
    }

    /// Actual line/cubic intersection or interior containment, not bounding-box picking.
    /// Invalid/non-finite rectangles return false. Degenerate point/line queries
    /// are allowed; zero-area object contours are not selectable.
    pub fn intersects_rect(&self, minimum: InkPoint, maximum: InkPoint) -> bool {
        self.nonempty_fill && hit::intersects_rect(&self.outline.figures[0], minimum, maximum)
    }
}

/// Builds the same fixed-complexity contour used by the renderer and editor.
/// Radius is clamped independently on each rectangle centerline axis. The
/// upstream block arrow intentionally has no half-stroke origin correction.
///
/// # Errors
/// Rejects invalid versions, parameters and domain coordinate bounds.
pub fn vector_shape_geometry(shape: &VectorShape) -> Result<VectorShapeGeometry, RenderError> {
    shape.validate().map_err(invalid)?;
    let x = signed_pixels(shape.bounds.x_hundredths);
    let y = signed_pixels(shape.bounds.y_hundredths);
    let width = unsigned_pixels(shape.bounds.width_hundredths);
    let height = unsigned_pixels(shape.bounds.height_hundredths);
    let stroke = f64::from(shape.stroke_width_hundredths) / 100.0;
    let half = stroke / 2.0;
    let mut figure = match shape.kind {
        VectorShapeKind::Rectangle | VectorShapeKind::Ellipse => {
            let w = (width - stroke).max(0.0);
            let h = (height - stroke).max(0.0);
            let radius = f64::from(shape.corner_radius_hundredths) / 100.0;
            let (rx, ry) = if shape.kind == VectorShapeKind::Ellipse {
                (w / 2.0, h / 2.0)
            } else {
                (radius.min(w / 2.0), radius.min(h / 2.0))
            };
            rounded_rect(x + half, y + half, w, h, rx, ry)
        }
        VectorShapeKind::Triangle => polygon(&[
            point(x + width / 2.0, y + half),
            point(x + width - half, y + height - half),
            point(x + half, y + height - half),
        ]),
        VectorShapeKind::BlockArrow => {
            // Arrow.cs: the TODO to add StrokeThickness/2 is not implemented
            // upstream. Preserve the nine-point concave outline, including it.
            let w = width - stroke;
            let h = height - stroke;
            polygon(&[
                point(x + w * 0.6898, y + h * 0.4),
                point(x, y + h * 0.4),
                point(x, y + h * 0.65),
                point(x + w * 0.6898, y + h * 0.65),
                point(x + w * 0.3684, y + h),
                point(x + w * 0.6608, y + h),
                point(x + w, y + h * 0.5),
                point(x + w * 0.6608, y),
                point(x + w * 0.3684, y),
            ])
        }
    };
    let nonempty_fill = match shape.kind {
        VectorShapeKind::Rectangle | VectorShapeKind::Ellipse => width > stroke && height > stroke,
        VectorShapeKind::Triangle | VectorShapeKind::BlockArrow => {
            shape.bounds.width_hundredths != u64::from(shape.stroke_width_hundredths)
                && shape.bounds.height_hundredths != u64::from(shape.stroke_width_hundredths)
        }
    };
    rotate(
        &mut figure,
        point(x + width / 2.0, y + height / 2.0),
        shape.rotation_hundredths,
    );
    let bounds = control_bounds(&figure);
    Ok(VectorShapeGeometry {
        outline: InkPath {
            fill_rule: InkFillRule::NonZero,
            figures: vec![figure],
        },
        bounds,
        nonempty_fill,
    })
}

fn invalid(reason: impl Into<String>) -> RenderError {
    RenderError::InvalidVectorShape {
        reason: reason.into(),
    }
}

fn signed_pixels(value: i64) -> f64 {
    f64::from(i32::try_from(value).expect("validated vector coordinate fits i32")) / 100.0
}

fn unsigned_pixels(value: u64) -> f64 {
    f64::from(u32::try_from(value).expect("validated vector extent fits u32")) / 100.0
}

const fn point(x: f64, y: f64) -> InkPoint {
    InkPoint { x, y }
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

fn rounded_rect(x: f64, y: f64, width: f64, height: f64, rx: f64, ry: f64) -> InkFigure {
    // WPF RectangleGeometry's four cubic corner construction.
    const K: f64 = 0.552_284_749_830_793_3;
    let right = x + width;
    let bottom = y + height;
    InkFigure {
        start: point(x + rx, y),
        segments: vec![
            InkSegment::LineTo(point(right - rx, y)),
            InkSegment::CubicTo {
                control1: point(right - rx + K * rx, y),
                control2: point(right, y + ry - K * ry),
                to: point(right, y + ry),
            },
            InkSegment::LineTo(point(right, bottom - ry)),
            InkSegment::CubicTo {
                control1: point(right, bottom - ry + K * ry),
                control2: point(right - rx + K * rx, bottom),
                to: point(right - rx, bottom),
            },
            InkSegment::LineTo(point(x + rx, bottom)),
            InkSegment::CubicTo {
                control1: point(x + rx - K * rx, bottom),
                control2: point(x, bottom - ry + K * ry),
                to: point(x, bottom - ry),
            },
            InkSegment::LineTo(point(x, y + ry)),
            InkSegment::CubicTo {
                control1: point(x, y + ry - K * ry),
                control2: point(x + rx - K * rx, y),
                to: point(x + rx, y),
            },
        ],
        closed: true,
    }
}

fn rotate(figure: &mut InkFigure, center: InkPoint, angle: u16) {
    let (sin, cos) = match angle {
        0 => (0.0, 1.0),
        9_000 => (1.0, 0.0),
        18_000 => (0.0, -1.0),
        27_000 => (-1.0, 0.0),
        _ => (f64::from(angle) / 100.0).to_radians().sin_cos(),
    };
    let transform = |p: &mut InkPoint| {
        let (x, y) = (p.x - center.x, p.y - center.y);
        *p = point(center.x + x * cos - y * sin, center.y + x * sin + y * cos);
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

fn control_bounds(figure: &InkFigure) -> [InkPoint; 2] {
    let (mut minimum, mut maximum) = (figure.start, figure.start);
    let mut extend = |p: InkPoint| {
        minimum.x = minimum.x.min(p.x);
        minimum.y = minimum.y.min(p.y);
        maximum.x = maximum.x.max(p.x);
        maximum.y = maximum.y.max(p.y);
    };
    for segment in &figure.segments {
        match *segment {
            InkSegment::LineTo(to) => extend(to),
            InkSegment::CubicTo {
                control1,
                control2,
                to,
            } => {
                extend(control1);
                extend(control2);
                extend(to);
            }
        }
    }
    [minimum, maximum]
}

#[cfg(test)]
mod tests;
