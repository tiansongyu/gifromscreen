//! Conservative global geometry bounds and integer-only regional destinations.

use gif_from_screen_domain::{PhysicalPoint, PhysicalRect};

use super::{
    Budget, CancellationToken, InkError, InkPath, InkRegionMask, InkSegment, PhysicalSize, Result,
    Rgba, WorkKind, checked_byte_len, clip_mask, composite_visual, memory_limit, paint_mask,
};

pub(super) fn whole(size: PhysicalSize) -> PhysicalRect {
    PhysicalRect {
        origin: PhysicalPoint::default(),
        size,
    }
}

pub(super) fn bounds<C: CancellationToken + ?Sized>(
    path: &InkPath,
    canvas: PhysicalSize,
    budget: &mut Budget<'_, C>,
) -> Result<Option<PhysicalRect>> {
    let mut low = [f64::INFINITY; 2];
    let mut high = [f64::NEG_INFINITY; 2];
    let mut visit = |point: crate::InkPoint| -> Result<()> {
        budget.charge(WorkKind::Brush, 1)?;
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(InkError::Invalid(
                "WPF region bounds require finite control points".into(),
            ));
        }
        for (axis, value) in [point.x, point.y].into_iter().enumerate() {
            low[axis] = low[axis].min(value);
            high[axis] = high[axis].max(value);
        }
        Ok(())
    };
    for figure in &path.figures {
        visit(figure.start)?;
        for segment in &figure.segments {
            match *segment {
                InkSegment::LineTo(to) => visit(to)?,
                InkSegment::CubicTo {
                    control1,
                    control2,
                    to,
                } => {
                    visit(control1)?;
                    visit(control2)?;
                    visit(to)?;
                }
            }
        }
    }
    if low[0] >= high[0] || low[1] >= high[1] {
        return Ok(None);
    }
    // The entire cubic control hull, not an ideal ellipse/curve box. Two whole
    // pixels conservatively cover native f32/28.4 quantization and HFD error.
    // These bounds select storage only; no point is translated or quantized.
    let left = coordinate(low[0].floor() - 2.0, canvas.width.get());
    let top = coordinate(low[1].floor() - 2.0, canvas.height.get());
    let right = coordinate(high[0].ceil() + 2.0, canvas.width.get());
    let bottom = coordinate(high[1].ceil() + 2.0, canvas.height.get());
    if left >= right || top >= bottom {
        return Ok(None);
    }
    PhysicalRect::new(left, top, right - left, bottom - top)
        .map(Some)
        .map_err(|e| InkError::Invalid(e.to_string()))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite control bounds are floor/ceil integers clamped to the u32 canvas axis"
)]
fn coordinate(value: f64, maximum: u32) -> u32 {
    value.max(0.0).min(f64::from(maximum)) as u32
}

pub(super) fn intersect(first: PhysicalRect, second: PhysicalRect) -> Option<PhysicalRect> {
    let left = first.origin.x.get().max(second.origin.x.get());
    let top = first.origin.y.get().max(second.origin.y.get());
    let right = first.end_x()?.min(second.end_x()?);
    let bottom = first.end_y()?.min(second.end_y()?);
    if left >= right || top >= bottom {
        return None;
    }
    PhysicalRect::new(left, top, right - left, bottom - top).ok()
}

pub(super) fn union(
    first: Option<PhysicalRect>,
    second: Option<PhysicalRect>,
) -> Option<PhysicalRect> {
    match (first, second) {
        (None, other) | (other, None) => other,
        (Some(a), Some(b)) => {
            let x = a.origin.x.get().min(b.origin.x.get());
            let y = a.origin.y.get().min(b.origin.y.get());
            PhysicalRect::new(
                x,
                y,
                a.end_x()?.max(b.end_x()?) - x,
                a.end_y()?.max(b.end_y()?) - y,
            )
            .ok()
        }
    }
}

pub(super) struct Target<'a> {
    pub area: PhysicalRect,
    pub pixels: &'a mut [u8],
}

impl Target<'_> {
    pub fn apply<C: CancellationToken + ?Sized>(
        &mut self,
        mask: &InkRegionMask,
        color: Option<Rgba>,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        let area = PhysicalRect {
            origin: mask.origin,
            size: mask.size,
        };
        self.validate(area)?;
        if mask.coverage.len() != checked_byte_len(area.size)? / 4 {
            return Err(InkError::Invalid(
                "WPF regional mask length mismatch".into(),
            ));
        }
        budget.charge_pixels(mask.coverage.len())?;
        let width = usize::try_from(area.size.width.get()).map_err(|_| memory_limit())?;
        for (row, coverage) in mask.coverage.chunks_exact(width).enumerate() {
            let pixels = self.row(area, row)?;
            if let Some(color) = color {
                paint_mask(pixels, coverage, color, budget.cancel)?;
            } else {
                clip_mask(pixels, coverage, budget.cancel)?;
            }
        }
        Ok(())
    }

    pub fn composite<C: CancellationToken + ?Sized>(
        &mut self,
        area: PhysicalRect,
        visual: &[u8],
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        self.validate(area)?;
        if visual.len() != checked_byte_len(area.size)? {
            return Err(InkError::Invalid(
                "WPF regional visual length mismatch".into(),
            ));
        }
        budget.charge_pixels(visual.len() / 4)?;
        let stride = usize::try_from(area.size.width.get()).map_err(|_| memory_limit())? * 4;
        for (row, source) in visual.chunks_exact(stride).enumerate() {
            composite_visual(self.row(area, row)?, source, budget.cancel)?;
        }
        Ok(())
    }

    fn validate(&self, area: PhysicalRect) -> Result<()> {
        if self.pixels.len() != checked_byte_len(self.area.size)?
            || intersect(self.area, area) != Some(area)
        {
            return Err(InkError::Invalid(
                "WPF regional write must fit its target".into(),
            ));
        }
        Ok(())
    }

    fn row(&mut self, area: PhysicalRect, row: usize) -> Result<&mut [u8]> {
        let x = usize::try_from(area.origin.x.get() - self.area.origin.x.get())
            .map_err(|_| memory_limit())?;
        let y = usize::try_from(area.origin.y.get() - self.area.origin.y.get())
            .map_err(|_| memory_limit())?
            + row;
        let stride = usize::try_from(self.area.size.width.get()).map_err(|_| memory_limit())?;
        let start = (y * stride + x) * 4;
        let length = usize::try_from(area.size.width.get()).map_err(|_| memory_limit())? * 4;
        self.pixels
            .get_mut(start..start + length)
            .ok_or_else(|| InkError::Invalid("WPF regional row exceeds target".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InkFigure, InkFillRule, InkPoint, NeverCancel, RenderLimits};

    #[test]
    fn bound_uses_full_control_hull_with_padding_and_rejects_nonfinite() {
        let point = |x, y| InkPoint { x, y };
        let mut path = InkPath {
            fill_rule: InkFillRule::NonZero,
            figures: vec![InkFigure {
                start: point(10.0, 10.0),
                closed: true,
                segments: vec![InkSegment::CubicTo {
                    control1: point(0.0, 60.0),
                    control2: point(80.0, -20.0),
                    to: point(20.0, 20.0),
                }],
            }],
        };
        let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        let canvas = PhysicalSize::new(100, 100).unwrap();
        assert_eq!(
            bounds(&path, canvas, &mut budget).unwrap(),
            Some(PhysicalRect::new(0, 0, 82, 62).unwrap())
        );
        assert_eq!(budget.usage.brush, 4);
        path.figures[0].start.x = f64::NAN;
        assert!(bounds(&path, canvas, &mut budget).is_err());
        assert_eq!(
            bounds(&InkPath::default(), canvas, &mut budget).unwrap(),
            None
        );
    }

    #[test]
    fn regional_target_uses_global_integer_offsets_without_changing_other_pixels() {
        let area = PhysicalRect::new(10, 20, 4, 3).unwrap();
        let mask_area = PhysicalRect::new(11, 21, 2, 1).unwrap();
        let mut pixels = vec![0; 48];
        let mut target = Target {
            area,
            pixels: &mut pixels,
        };
        let mask = InkRegionMask {
            origin: mask_area.origin,
            size: mask_area.size,
            coverage: vec![64, 32],
            work: 0,
        };
        let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        target
            .apply(
                &mask,
                Some(Rgba {
                    red: 255,
                    green: 0,
                    blue: 0,
                    alpha: 255,
                }),
                &mut budget,
            )
            .unwrap();
        assert_eq!(&target.pixels[20..28], &[255, 0, 0, 255, 128, 0, 0, 128]);
        assert!(
            target.pixels[..20]
                .iter()
                .chain(&target.pixels[28..])
                .all(|v| *v == 0)
        );
        assert_eq!(budget.usage.pixels, 2);
        let before = target.pixels.to_vec();
        let outside = InkRegionMask {
            origin: PhysicalPoint::default(),
            ..mask
        };
        assert!(target.apply(&outside, None, &mut budget).is_err());
        assert_eq!(target.pixels, before);
    }
}
