//! Explicit WPF layout geometry for independent conformance and the next renderer
//! contract. This does not reinterpret persisted version-one vector pixels.

use gif_from_screen_domain::{VectorShape, VectorShapeKind};

use super::{
    VectorShapeGeometry, control_bounds, point, rotate, rounded_rect, vector_shape_geometry,
    wpf_layout,
};
use crate::{InkFigure, InkPath, InkPoint, InkSegment, RenderError};

/// Derives the fixed reference's arranged shape contour in 96-DPI physical space.
/// Rectangle/ellipse use their arranged size; the two custom shapes continue to
/// use requested Width/Height. All rotate around the actual arranged center.
///
/// This is an explicit new geometry API, not the version-one compositor route.
/// Pixel coverage and stroke widening require their own conformance evidence.
///
/// # Errors
/// Rejects invalid/beyond-bound metadata or invalid layout calculations.
pub fn wpf_vector_shape_geometry(shape: &VectorShape) -> Result<VectorShapeGeometry, RenderError> {
    shape.validate().map_err(super::invalid)?;
    let layout = wpf_layout::layout(shape)?;
    let (mut figure, nonempty_fill) = match shape.kind {
        VectorShapeKind::Rectangle | VectorShapeKind::Ellipse => {
            let stroke = f64::from(shape.stroke_width_hundredths) / 100.0;
            let width = (layout.render_size[0] - stroke).max(0.0);
            let height = (layout.render_size[1] - stroke).max(0.0);
            let radius = f64::from(shape.corner_radius_hundredths) / 100.0;
            let (rx, ry) = if shape.kind == VectorShapeKind::Ellipse {
                (width / 2.0, height / 2.0)
            } else {
                (radius.min(width / 2.0), radius.min(height / 2.0))
            };
            (
                rounded_rect(stroke / 2.0, stroke / 2.0, width, height, rx, ry),
                width > 0.0 && height > 0.0,
            )
        }
        VectorShapeKind::Triangle | VectorShapeKind::BlockArrow => {
            let mut local = *shape;
            local.bounds.x_hundredths = 0;
            local.bounds.y_hundredths = 0;
            local.rotation_hundredths = 0;
            let geometry = vector_shape_geometry(&local)?;
            (geometry.outline.figures[0].clone(), geometry.nonempty_fill)
        }
    };
    rotate(
        &mut figure,
        point(layout.render_size[0] / 2.0, layout.render_size[1] / 2.0),
        shape.rotation_hundredths,
    );
    translate(&mut figure, layout.offset);
    let bounds = control_bounds(&figure);
    Ok(VectorShapeGeometry {
        outline: InkPath {
            figures: vec![figure],
            ..InkPath::default()
        },
        bounds,
        nonempty_fill,
    })
}

fn translate(figure: &mut InkFigure, offset: [f64; 2]) {
    let move_point = |point: &mut InkPoint| {
        point.x += offset[0];
        point.y += offset[1];
    };
    move_point(&mut figure.start);
    for segment in &mut figure.segments {
        match segment {
            InkSegment::LineTo(to) => move_point(to),
            InkSegment::CubicTo {
                control1,
                control2,
                to,
            } => {
                move_point(control1);
                move_point(control2);
                move_point(to);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gif_from_screen_domain::VectorShapeBounds;

    use super::*;

    #[test]
    fn arranged_geometry_does_not_reinterpret_or_mutate_vector_one_requests() {
        let shape = VectorShape {
            bounds: VectorShapeBounds {
                x_hundredths: 125,
                y_hundredths: 175,
                width_hundredths: 1250,
                height_hundredths: 1125,
            },
            stroke_width_hundredths: 50,
            corner_radius_hundredths: 25,
            ..VectorShape::default()
        };
        let before = shape;
        let legacy = vector_shape_geometry(&shape).unwrap();
        let arranged = wpf_vector_shape_geometry(&shape).unwrap();
        assert_eq!(shape, before);
        assert_eq!(vector_shape_geometry(&shape).unwrap(), legacy);
        assert_ne!(arranged, legacy);
        assert_eq!(arranged.bounds(), [point(1.25, 2.25), point(12.75, 12.75)]);
        assert_eq!(legacy.bounds(), [point(1.5, 2.0), point(13.5, 12.75)]);
    }

    #[test]
    fn custom_triangle_keeps_requested_vertices_but_uses_arranged_rotation_center() {
        let shape = VectorShape {
            kind: VectorShapeKind::Triangle,
            bounds: VectorShapeBounds {
                x_hundredths: -25,
                y_hundredths: 200,
                width_hundredths: 1500,
                height_hundredths: 1125,
            },
            stroke_width_hundredths: 225,
            rotation_hundredths: 3300,
            ..VectorShape::default()
        };
        let geometry = wpf_vector_shape_geometry(&shape).unwrap();
        // Original local first vertex=(7.5,1.125), center=(8,5.5), offset=(0,2).
        // The raw width is NOT replaced with the arranged width of 16.
        let (sin, cos) = 33.0_f64.to_radians().sin_cos();
        let first = geometry.outline().figures[0].start;
        let expected = point(8.0 - 0.5 * cos + 4.375 * sin, 7.5 - 0.5 * sin - 4.375 * cos);
        assert!((first.x - expected.x).abs() < 1e-12);
        assert!((first.y - expected.y).abs() < 1e-12);
    }
}
