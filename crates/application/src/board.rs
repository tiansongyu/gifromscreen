//! Bounded raster drawing for the recorder, including non-accumulating highlighter strokes.

use gif_from_screen_domain::{PhysicalSize, Rgba};

const MAX_EDGE: u32 = 2048;
const MAX_POINTS: usize = 4096;

/// Integer source-canvas coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoardPoint {
    /// Horizontal pixel coordinate.
    pub x: u32,
    /// Vertical pixel coordinate.
    pub y: u32,
}

/// Painting operation fixed for the duration of one stroke.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoardBrush {
    /// Opaque or alpha-bearing source-over paint.
    Pen(Rgba),
    /// Paint at at most 25% opacity, once per stroke regardless of overlap.
    Highlighter(Rgba),
    /// Restore the configured solid or transparent canvas background.
    Eraser,
}

struct Stroke {
    original: Vec<u8>,
    painted: Vec<bool>,
    brush: BoardBrush,
    width: u16,
    previous: BoardPoint,
    points: usize,
}

/// One RGBA canvas plus bounded scratch buffers for the current stroke.
pub struct BoardCanvas {
    size: PhysicalSize,
    background: Rgba,
    pixels: Vec<u8>,
    stroke: Option<Stroke>,
    revision: u64,
}

impl BoardCanvas {
    /// Allocates a canvas at most 2048 × 2048 pixels (16 MiB).
    ///
    /// # Errors
    /// Rejects empty/oversized dimensions or allocation failure.
    pub fn new(size: PhysicalSize, background: Rgba) -> Result<Self, String> {
        let width = size.width.get();
        let height = size.height.get();
        if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
            return Err("Board dimensions must be 1..2048 pixels per edge".to_owned());
        }
        let count = usize::try_from(u64::from(width) * u64::from(height))
            .map_err(|error| error.to_string())?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(count * 4)
            .map_err(|error| error.to_string())?;
        for _ in 0..count {
            pixels.extend_from_slice(&[
                background.red,
                background.green,
                background.blue,
                background.alpha,
            ]);
        }
        Ok(Self {
            size,
            background,
            pixels,
            stroke: None,
            revision: 0,
        })
    }

    /// Fixed canvas dimensions.
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }
    /// Tightly packed, straight-alpha RGBA8 pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    /// Changes after each painted segment, for preview texture invalidation.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    /// Whether a pointer stroke is open.
    pub fn is_drawing(&self) -> bool {
        self.stroke.is_some()
    }

    /// Starts a bounded stroke and paints its initial point.
    ///
    /// # Errors
    /// Rejects invalid coordinates, width, or a second simultaneous stroke.
    pub fn begin(
        &mut self,
        point: BoardPoint,
        brush: BoardBrush,
        width: u16,
    ) -> Result<(), String> {
        self.validate_point(point)?;
        if self.stroke.is_some() || width == 0 || width > 256 {
            return Err("Finish the current stroke; brush width must be 1..256 pixels".to_owned());
        }
        self.stroke = Some(Stroke {
            original: self.pixels.clone(),
            painted: vec![false; self.pixels.len() / 4],
            brush,
            width,
            previous: point,
            points: 0,
        });
        self.extend(point)
    }

    /// Adds one line segment, rejecting more than 4096 samples per stroke.
    ///
    /// # Errors
    /// Rejects out-of-canvas samples, missing strokes or the point limit.
    pub fn extend(&mut self, point: BoardPoint) -> Result<(), String> {
        self.validate_point(point)?;
        let stroke = self
            .stroke
            .as_mut()
            .ok_or_else(|| "No board stroke is active".to_owned())?;
        if stroke.points >= MAX_POINTS {
            return Err("Board stroke reached 4096 points; release to finish it".to_owned());
        }
        paint_segment(self.size, self.background, &mut self.pixels, stroke, point);
        stroke.previous = point;
        stroke.points += 1;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Ends a stroke; returns whether one was in progress.
    pub fn end(&mut self) -> bool {
        self.stroke.take().is_some()
    }

    fn validate_point(&self, point: BoardPoint) -> Result<(), String> {
        if point.x >= self.size.width.get() || point.y >= self.size.height.get() {
            Err("Board point lies outside the canvas".to_owned())
        } else {
            Ok(())
        }
    }
}

fn paint_segment(
    size: PhysicalSize,
    background: Rgba,
    pixels: &mut [u8],
    stroke: &mut Stroke,
    point: BoardPoint,
) {
    let radius = f64::from(stroke.width) / 2.0;
    let from = stroke.previous;
    let left = from.x.min(point.x).saturating_sub(u32::from(stroke.width));
    let top = from.y.min(point.y).saturating_sub(u32::from(stroke.width));
    let right = from
        .x
        .max(point.x)
        .saturating_add(u32::from(stroke.width))
        .min(size.width.get() - 1);
    let bottom = from
        .y
        .max(point.y)
        .saturating_add(u32::from(stroke.width))
        .min(size.height.get() - 1);
    let dx = f64::from(point.x) - f64::from(from.x);
    let dy = f64::from(point.y) - f64::from(from.y);
    let length_squared = dx * dx + dy * dy;
    for y in top..=bottom {
        for x in left..=right {
            let px = f64::from(x) - f64::from(from.x);
            let py = f64::from(y) - f64::from(from.y);
            let t = if length_squared == 0.0 {
                0.0
            } else {
                ((px * dx + py * dy) / length_squared).clamp(0.0, 1.0)
            };
            if (px - dx * t).powi(2) + (py - dy * t).powi(2) > radius * radius {
                continue;
            }
            let index = y as usize * size.width.get() as usize + x as usize;
            if stroke.painted[index] {
                continue;
            }
            stroke.painted[index] = true;
            let target = &mut pixels[index * 4..index * 4 + 4];
            match stroke.brush {
                BoardBrush::Eraser => target.copy_from_slice(&[
                    background.red,
                    background.green,
                    background.blue,
                    background.alpha,
                ]),
                BoardBrush::Pen(color) => {
                    blend(target, &stroke.original[index * 4..index * 4 + 4], color);
                }
                BoardBrush::Highlighter(mut color) => {
                    color.alpha = color.alpha.min(64);
                    blend(target, &stroke.original[index * 4..index * 4 + 4], color);
                }
            }
        }
    }
}

fn blend(target: &mut [u8], original: &[u8], color: Rgba) {
    let alpha = u32::from(color.alpha);
    let old_alpha = u32::from(original[3]);
    let output_alpha = alpha * 255 + old_alpha * (255 - alpha);
    if output_alpha == 0 {
        target.fill(0);
        return;
    }
    for (index, channel) in [color.red, color.green, color.blue].into_iter().enumerate() {
        target[index] = u8::try_from(
            (u32::from(channel) * alpha * 255
                + u32::from(original[index]) * old_alpha * (255 - alpha)
                + output_alpha / 2)
                / output_alpha,
        )
        .unwrap_or(255);
    }
    target[3] = u8::try_from((output_alpha + 127) / 255).unwrap_or(255);
}

#[cfg(test)]
mod tests {
    use super::*;
    const WHITE: Rgba = Rgba {
        red: 255,
        green: 255,
        blue: 255,
        alpha: 255,
    };
    const BLACK: Rgba = Rgba {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 255,
    };

    #[test]
    fn pen_and_eraser_restore_solid_and_transparent_backgrounds() {
        for background in [WHITE, Rgba::TRANSPARENT] {
            let mut canvas =
                BoardCanvas::new(PhysicalSize::new(10, 10).unwrap(), background).unwrap();
            let original = canvas.pixels().to_vec();
            canvas
                .begin(BoardPoint { x: 2, y: 2 }, BoardBrush::Pen(BLACK), 3)
                .unwrap();
            canvas.extend(BoardPoint { x: 7, y: 7 }).unwrap();
            canvas.end();
            assert_ne!(canvas.pixels(), original);
            canvas
                .begin(BoardPoint { x: 2, y: 2 }, BoardBrush::Eraser, 3)
                .unwrap();
            canvas.extend(BoardPoint { x: 7, y: 7 }).unwrap();
            canvas.end();
            assert_eq!(canvas.pixels(), original);
        }
    }

    #[test]
    fn highlighter_does_not_accumulate_at_intersections_within_one_stroke() {
        let mut canvas = BoardCanvas::new(PhysicalSize::new(10, 1).unwrap(), WHITE).unwrap();
        canvas
            .begin(BoardPoint { x: 0, y: 0 }, BoardBrush::Highlighter(BLACK), 1)
            .unwrap();
        canvas.extend(BoardPoint { x: 9, y: 0 }).unwrap();
        let first = canvas.pixels().to_vec();
        canvas.extend(BoardPoint { x: 0, y: 0 }).unwrap();
        assert_eq!(canvas.pixels(), first);
        assert_eq!(&first[..4], &[191, 191, 191, 255]);
        canvas.end();
    }

    #[test]
    fn invalid_dimensions_coordinates_and_point_overflow_are_bounded() {
        assert!(BoardCanvas::new(PhysicalSize::new(2049, 1).unwrap(), WHITE).is_err());
        let mut canvas = BoardCanvas::new(PhysicalSize::new(1, 1).unwrap(), WHITE).unwrap();
        assert!(
            canvas
                .begin(BoardPoint { x: 1, y: 0 }, BoardBrush::Eraser, 1)
                .is_err()
        );
        canvas
            .begin(BoardPoint { x: 0, y: 0 }, BoardBrush::Eraser, 1)
            .unwrap();
        for _ in 1..4096 {
            canvas.extend(BoardPoint { x: 0, y: 0 }).unwrap();
        }
        assert!(canvas.extend(BoardPoint { x: 0, y: 0 }).is_err());
        assert!(canvas.end());
    }
}
