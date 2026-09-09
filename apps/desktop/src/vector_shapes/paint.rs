use eframe::egui;
use gif_from_screen_render::{InkPoint, InkSegment};

use super::{
    draft::{Draft, GestureKind, Handle},
    geometry::{GeometryCache, handles, ink},
    input::{HANDLE_RADIUS, Mapping},
};

const COLOR: egui::Color32 = egui::Color32::from_rgb(242, 153, 74);

/// Selection guides only. Fill and authored strokes come from `RasterPreview`.
pub(super) fn guides(
    painter: &egui::Painter,
    mapping: Mapping,
    draft: &Draft,
    geometry: &GeometryCache,
) {
    for object in draft
        .objects
        .iter()
        .filter(|o| draft.selected.contains(&o.id))
    {
        if let Some(geometry) = geometry.get(object.id) {
            for figure in &geometry.outline().figures {
                let mut last = figure.start;
                let mut points = vec![mapping.screen(last)];
                for segment in &figure.segments {
                    match *segment {
                        InkSegment::LineTo(to) => {
                            points.push(mapping.screen(to));
                            last = to;
                        }
                        InkSegment::CubicTo {
                            control1,
                            control2,
                            to,
                        } => {
                            // A bounded guide tessellation of the shared cubic,
                            // never used for hit testing or exported geometry.
                            for step in 1..=16 {
                                let t = f64::from(step) / 16.0;
                                let u = 1.0 - t;
                                points.push(mapping.screen(InkPoint {
                                    x: u * u * u * last.x
                                        + 3.0 * u * u * t * control1.x
                                        + 3.0 * u * t * t * control2.x
                                        + t * t * t * to.x,
                                    y: u * u * u * last.y
                                        + 3.0 * u * u * t * control1.y
                                        + 3.0 * u * t * t * control2.y
                                        + t * t * t * to.y,
                                }));
                            }
                            last = to;
                        }
                    }
                }
                if figure.closed {
                    points.push(mapping.screen(figure.start));
                }
                painter.add(egui::Shape::line(points, egui::Stroke::new(1.0_f32, COLOR)));
            }
        }
    }
    if let Some(primary) = draft.primary() {
        let positions = handles(primary);
        let rotation = primary.shape.rotation_hundredths;
        let top = mapping.handle_position(Handle::Top, positions[1].1, rotation);
        let rotate = mapping.handle_position(Handle::Rotate, positions[8].1, rotation);
        painter.line_segment([top, rotate], egui::Stroke::new(1.0_f32, COLOR));
        for (handle, point) in positions {
            let position = mapping.handle_position(handle, point, rotation);
            if handle == Handle::Rotate {
                painter.circle(
                    position,
                    HANDLE_RADIUS,
                    egui::Color32::from_gray(25),
                    egui::Stroke::new(1.5_f32, COLOR),
                );
            } else {
                painter.rect(
                    egui::Rect::from_center_size(position, egui::Vec2::splat(HANDLE_RADIUS * 2.0)),
                    1.0,
                    egui::Color32::from_gray(25),
                    egui::Stroke::new(1.0_f32, COLOR),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }
    if let Some(GestureKind::Marquee { start, end, .. }) = draft.gesture.as_ref().map(|g| g.kind) {
        painter.rect_stroke(
            egui::Rect::from_two_pos(mapping.screen(ink(start)), mapping.screen(ink(end))),
            0.0,
            egui::Stroke::new(1.0_f32, COLOR),
            egui::StrokeKind::Inside,
        );
    }
}
