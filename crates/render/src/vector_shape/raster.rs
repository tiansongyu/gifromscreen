//! Bounded, cancellable linearization before tiny-skia's line-only stroker.
//! Each path is rasterized once in canvas coordinates. Independent small-mask
//! clipping changes scanline quantization in tiny-skia, so only composition is
//! tiled. The checked A8 working set is explicit, not hidden behind a tile claim.

use gif_from_screen_domain::{PhysicalSize, Rgba, VectorShape};

use super::{VectorShapeGeometry, invalid, vector_shape_geometry};
use crate::{CancellationToken, InkPoint, InkSegment, RenderError, RenderLimits};

const TILE: u32 = 128;
const MAX_SEGMENTS: usize = 32_768;
const FLATNESS: f64 = 1.0 / 256.0;
// Per input line: owned points, builder/inner/outer/copy capacities and scan
// edges. Only straight segments reach the dependency stroker, with Miter joins.
// No dependency curve recursion, round joins, dash expansion or arbitrary paths.
const WORK_BYTES_PER_LINE: usize = 2_048;
pub(crate) const MAX_WORK: u64 = 100_000_000;

pub(crate) fn paint<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    canvas: PhysicalSize,
    scale: [f64; 2],
    limits: RenderLimits,
    cancel: &C,
    remaining: &mut u64,
    mut pixel: impl FnMut(u32, u32, [u8; 4]),
) -> Result<(), RenderError> {
    cancelled(cancel)?;
    let geometry = vector_shape_geometry(shape)?;
    let fill = shape.fill.filter(|color| color.alpha != 0);
    let stroke =
        (shape.stroke_width_hundredths != 0 && shape.stroke.alpha != 0).then_some(shape.stroke);
    if fill.is_none() && stroke.is_none() {
        return Ok(());
    }
    if scale
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err(invalid("invalid vector output scale"));
    }
    let [minimum, maximum] = geometry.bounds();
    let padding = f64::from(shape.stroke_width_hundredths) / 20.0;
    if (maximum.x + padding) * scale[0] < -1.0
        || (maximum.y + padding) * scale[1] < -1.0
        || (minimum.x - padding) * scale[0] > f64::from(canvas.width.get()) + 1.0
        || (minimum.y - padding) * scale[1] > f64::from(canvas.height.get()) + 1.0
    {
        return Ok(());
    }
    let tolerance = FLATNESS / scale[0].max(scale[1]);
    let count = flattened(&geometry, tolerance, cancel, |_| {})?;
    let area = usize::try_from(u64::from(canvas.width.get()) * u64::from(canvas.height.get()))
        .map_err(|_| RenderError::EffectWorkingMemorySizeOverflow {
            effect: "vector shape",
        })?;
    let masks = usize::from(fill.is_some()) + usize::from(stroke.is_some());
    let working = count
        .checked_add(2)
        .and_then(|n| n.checked_mul(WORK_BYTES_PER_LINE))
        .and_then(|n| area.checked_mul(masks).and_then(|bytes| n.checked_add(bytes)))
        // AlphaRuns owns width+1 entries of u16 run and u8 coverage; reserve
        // extra capacity/alignment as well as the path/edge estimate above.
        .and_then(|n| (canvas.width.get() as usize).checked_add(1)
            .and_then(|width| width.checked_mul(8)).and_then(|bytes| n.checked_add(bytes)))
        .ok_or(RenderError::EffectWorkingMemorySizeOverflow {
            effect: "vector shape",
        })?;
    if working > limits.max_surface_bytes {
        return Err(RenderError::EffectWorkingMemoryLimitExceeded {
            effect: "vector shape",
            requested: working,
            limit: limits.max_surface_bytes,
        });
    }
    let path = linear_path(&geometry, count, tolerance, cancel)?;
    let stroked = if stroke.is_some() {
        cancelled(cancel)?;
        let result = path.stroke(
            &tiny_skia::Stroke {
                width: narrow(f64::from(shape.stroke_width_hundredths) / 100.0),
                miter_limit: 10.0,
                line_cap: tiny_skia::LineCap::Butt,
                line_join: tiny_skia::LineJoin::Miter,
                ..tiny_skia::Stroke::default()
            },
            1.0,
        );
        cancelled(cancel)?;
        if result.is_none() && path.points().iter().any(|point| *point != path.points()[0]) {
            return Err(invalid("could not stroke the bounded vector contour"));
        }
        result
    } else {
        None
    };
    paint_paths(
        [
            fill.map(|color| (&path, color)),
            stroke
                .zip(stroked.as_ref())
                .map(|(color, path)| (path, color)),
        ],
        canvas,
        scale,
        cancel,
        remaining,
        &mut pixel,
    )
}

fn linear_path<C: CancellationToken + ?Sized>(
    geometry: &VectorShapeGeometry,
    count: usize,
    tolerance: f64,
    cancel: &C,
) -> Result<tiny_skia::Path, RenderError> {
    let mut builder = tiny_skia::PathBuilder::with_capacity(count + 2, count + 1);
    let first = geometry.outline().figures[0].start;
    builder.move_to(narrow(first.x), narrow(first.y));
    flattened(geometry, tolerance, cancel, |p| {
        builder.line_to(narrow(p.x), narrow(p.y));
    })?;
    builder.close();
    builder
        .finish()
        .ok_or_else(|| invalid("could not represent vector contour"))
}

fn flattened<C: CancellationToken + ?Sized>(
    geometry: &VectorShapeGeometry,
    tolerance: f64,
    cancel: &C,
    mut emit: impl FnMut(InkPoint),
) -> Result<usize, RenderError> {
    let figure = &geometry.outline().figures[0];
    let mut previous = figure.start;
    let mut count = 0;
    for segment in &figure.segments {
        match *segment {
            InkSegment::LineTo(to) => {
                line(to, &mut count, &mut emit)?;
                previous = to;
            }
            InkSegment::CubicTo {
                control1,
                control2,
                to,
            } => {
                let mut stack = [([InkPoint::default(); 4], 0_u8); 33];
                stack[0] = ([previous, control1, control2, to], 0);
                let mut length = 1;
                while length != 0 {
                    cancelled(cancel)?;
                    length -= 1;
                    let (curve, depth) = stack[length];
                    if flat(curve, tolerance) {
                        line(curve[3], &mut count, &mut emit)?;
                    } else {
                        if depth == 32 {
                            return Err(invalid(
                                "vector curve exceeds the bounded subdivision depth",
                            ));
                        }
                        let (left, right) = split(curve);
                        stack[length] = (right, depth + 1);
                        stack[length + 1] = (left, depth + 1);
                        length += 2;
                    }
                }
                previous = to;
            }
        }
    }
    cancelled(cancel)?;
    Ok(count)
}

fn line(
    point: InkPoint,
    count: &mut usize,
    emit: &mut impl FnMut(InkPoint),
) -> Result<(), RenderError> {
    if *count >= MAX_SEGMENTS {
        return Err(invalid("vector curve exceeds the 32768-segment bound"));
    }
    *count += 1;
    emit(point);
    Ok(())
}

fn flat([a, b, c, d]: [InkPoint; 4], tolerance: f64) -> bool {
    let (dx, dy) = (d.x - a.x, d.y - a.y);
    let length = dx.hypot(dy);
    if length == 0.0 {
        return (b.x - a.x)
            .hypot(b.y - a.y)
            .max((c.x - a.x).hypot(c.y - a.y))
            <= tolerance;
    }
    [b, c]
        .into_iter()
        .all(|p| ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() <= tolerance * length)
}

fn split([a, b, c, d]: [InkPoint; 4]) -> ([InkPoint; 4], [InkPoint; 4]) {
    let midpoint = |p: InkPoint, q: InkPoint| InkPoint {
        x: p.x.midpoint(q.x),
        y: p.y.midpoint(q.y),
    };
    let (ab, bc, cd) = (midpoint(a, b), midpoint(b, c), midpoint(c, d));
    let (abc, bcd) = (midpoint(ab, bc), midpoint(bc, cd));
    let center = midpoint(abc, bcd);
    ([a, ab, abc, center], [center, bcd, cd, d])
}

fn paint_paths<C: CancellationToken + ?Sized>(
    paths: [Option<(&tiny_skia::Path, Rgba)>; 2],
    canvas: PhysicalSize,
    scale: [f64; 2],
    cancel: &C,
    remaining: &mut u64,
    pixel: &mut impl FnMut(u32, u32, [u8; 4]),
) -> Result<(), RenderError> {
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut segments = 0_u64;
    for (path, _) in paths.into_iter().flatten() {
        let rect = path.bounds();
        bounds[0] = bounds[0].min(f64::from(rect.left()) * scale[0]);
        bounds[1] = bounds[1].min(f64::from(rect.top()) * scale[1]);
        bounds[2] = bounds[2].max(f64::from(rect.right()) * scale[0]);
        bounds[3] = bounds[3].max(f64::from(rect.bottom()) * scale[1]);
        segments += u64::try_from(path.len()).expect("bounded path length fits u64");
    }
    if segments == 0 {
        return Ok(());
    }
    let x0 = clipped(bounds[0] - 1.0, canvas.width.get(), false) / TILE * TILE;
    let y0 = clipped(bounds[1] - 1.0, canvas.height.get(), false) / TILE * TILE;
    let x1 = clipped(bounds[2] + 1.0, canvas.width.get(), true);
    let y1 = clipped(bounds[3] + 1.0, canvas.height.get(), true);
    if x0 >= x1 || y0 >= y1 {
        return Ok(());
    }
    let work = u64::from(canvas.width.get())
        .checked_mul(u64::from(canvas.height.get()))
        .and_then(|n| n.checked_mul(paths.iter().flatten().count() as u64))
        .and_then(|n| n.checked_add(segments))
        .and_then(|n| n.checked_add(u64::from(x1 - x0) * u64::from(y1 - y0) * 3))
        .ok_or_else(|| invalid("vector raster work overflows"))?;
    *remaining = remaining
        .checked_sub(work)
        .ok_or_else(|| invalid("vector raster exceeds the 100000000-unit work bound"))?;
    let masks = [
        paths[0]
            .map(|(path, color)| raster_mask(path, color, canvas, scale, cancel))
            .transpose()?,
        paths[1]
            .map(|(path, color)| raster_mask(path, color, canvas, scale, cancel))
            .transpose()?,
    ];
    for y in (y0..y1).step_by(usize::try_from(TILE).expect("tile fits usize")) {
        for x in (x0..x1).step_by(usize::try_from(TILE).expect("tile fits usize")) {
            let (width, height) = ((x1 - x).min(TILE), (y1 - y).min(TILE));
            cancelled(cancel)?;
            for row in y..y + height {
                for column in x..x + width {
                    let index = usize::try_from(
                        u64::from(row) * u64::from(canvas.width.get()) + u64::from(column),
                    )
                    .expect("validated canvas mask area");
                    if column.wrapping_sub(x).is_multiple_of(1_024) {
                        cancelled(cancel)?;
                    }
                    let mut source = [0; 4];
                    for (mask, color) in masks.iter().flatten() {
                        let coverage = mask.data()[index];
                        let covered =
                            color.map(|channel| crate::wpf_pixels::mul_byte(channel, coverage));
                        source = crate::wpf_pixels::over(covered, source);
                    }
                    if source[3] != 0 {
                        pixel(column, row, source);
                    }
                }
            }
        }
    }
    Ok(())
}

fn raster_mask<C: CancellationToken + ?Sized>(
    path: &tiny_skia::Path,
    color: Rgba,
    canvas: PhysicalSize,
    scale: [f64; 2],
    cancel: &C,
) -> Result<(tiny_skia::Mask, [u8; 4]), RenderError> {
    cancelled(cancel)?;
    let length = usize::try_from(u64::from(canvas.width.get()) * u64::from(canvas.height.get()))
        .map_err(|_| invalid("vector mask area overflows"))?;
    let size = tiny_skia::IntSize::from_wh(canvas.width.get(), canvas.height.get())
        .ok_or_else(|| invalid("invalid vector mask dimensions"))?;
    let mut mask = tiny_skia::Mask::from_vec(zeroed(length)?, size)
        .ok_or_else(|| invalid("invalid vector mask length"))?;
    mask.fill_path(
        path,
        tiny_skia::FillRule::Winding,
        true,
        tiny_skia::Transform::from_scale(narrow(scale[0]), narrow(scale[1])),
    );
    cancelled(cancel)?;
    Ok((
        mask,
        crate::wpf_pixels::premultiply([color.red, color.green, color.blue, color.alpha]),
    ))
}

#[cfg(test)]
fn raster_tile<C: CancellationToken + ?Sized>(
    paths: [Option<(&tiny_skia::Path, Rgba)>; 2],
    origin: [u32; 2],
    size: [u32; 2],
    scale: [f64; 2],
    cancel: &C,
) -> Result<Vec<u8>, RenderError> {
    cancelled(cancel)?;
    let length = usize::try_from(size[0] * size[1]).expect("bounded tile area");
    let mut tile = zeroed(length * 4)?;
    let mut mask = tiny_skia::Mask::from_vec(
        zeroed(length)?,
        tiny_skia::IntSize::from_wh(size[0], size[1]).expect("nonempty tile"),
    )
    .expect("validated mask length");
    for (path, color) in paths.into_iter().flatten() {
        cancelled(cancel)?;
        mask.clear();
        mask.fill_path(
            path,
            tiny_skia::FillRule::Winding,
            true,
            tiny_skia::Transform::from_row(
                narrow(scale[0]),
                0.0,
                0.0,
                narrow(scale[1]),
                -narrow(f64::from(origin[0])),
                -narrow(f64::from(origin[1])),
            ),
        );
        cancelled(cancel)?;
        let source =
            crate::wpf_pixels::premultiply([color.red, color.green, color.blue, color.alpha]);
        for (index, (destination, coverage)) in tile
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(mask.data())
            .enumerate()
        {
            if index.is_multiple_of(1_024) {
                cancelled(cancel)?;
            }
            if *coverage != 0 {
                let source = source.map(|channel| crate::wpf_pixels::mul_byte(channel, *coverage));
                *destination = crate::wpf_pixels::over(source, *destination);
            }
        }
    }
    Ok(tile)
}

fn zeroed(bytes: usize) -> Result<Vec<u8>, RenderError> {
    let mut data = Vec::new();
    data.try_reserve_exact(bytes).map_err(|_| {
        RenderError::EffectWorkingMemoryAllocationFailed {
            effect: "vector shape tile",
            requested: bytes,
        }
    })?;
    data.resize(bytes, 0);
    Ok(data)
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "domain geometry and checked output ratios are intentionally converted to the pinned f32 rasterizer"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite floored/ceiled bounds are explicitly clipped to the u32 canvas range"
)]
fn clipped(value: f64, limit: u32, ceil: bool) -> u32 {
    let value = if ceil { value.ceil() } else { value.floor() };
    value.clamp(0.0, f64::from(limit)) as u32
}

fn cancelled<C: CancellationToken + ?Sized>(cancel: &C) -> Result<(), RenderError> {
    if cancel.is_cancelled() {
        Err(RenderError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{FLATNESS, MAX_SEGMENTS, flattened, vector_shape_geometry};
    use crate::NeverCancel;
    use gif_from_screen_domain::{VectorShape, VectorShapeBounds, VectorShapeKind};

    #[test]
    fn full_gif_and_twice_gif_ellipses_including_extremely_flat_axes_fit_the_segment_budget() {
        for (width, height) in [
            (6_553_500, 6_553_500),
            (13_107_000, 13_107_000),
            (13_107_000, 1),
            (1, 13_107_000),
        ] {
            for rotation in [0, 9_000, 12_345, 35_999] {
                let shape = VectorShape {
                    kind: VectorShapeKind::Ellipse,
                    bounds: VectorShapeBounds {
                        x_hundredths: 0,
                        y_hundredths: 0,
                        width_hundredths: width,
                        height_hundredths: height,
                    },
                    rotation_hundredths: rotation,
                    stroke_width_hundredths: 0,
                    ..VectorShape::default()
                };
                let geometry = vector_shape_geometry(&shape).unwrap();
                let count = flattened(&geometry, FLATNESS, &NeverCancel, |_| {}).unwrap();
                assert!(count <= MAX_SEGMENTS);
            }
        }
    }

    #[test]
    fn small_tiles_are_byte_identical_to_a_single_mask_without_edge_seams() {
        use crate::RenderLimits;
        use gif_from_screen_domain::{PhysicalSize, Rgba};
        for kind in [
            VectorShapeKind::Rectangle,
            VectorShapeKind::Ellipse,
            VectorShapeKind::Triangle,
            VectorShapeKind::BlockArrow,
        ] {
            let source = VectorShape {
                kind,
                bounds: VectorShapeBounds {
                    x_hundredths: 3_725,
                    y_hundredths: 2_850,
                    width_hundredths: 26_075,
                    height_hundredths: 19_125,
                },
                stroke_width_hundredths: 225,
                corner_radius_hundredths: 5_375,
                rotation_hundredths: 1_337,
                stroke: Rgba {
                    red: 200,
                    green: 35,
                    blue: 1,
                    alpha: 117,
                },
                fill: Some(Rgba {
                    red: 60,
                    green: 120,
                    blue: 200,
                    alpha: 173,
                }),
                ..VectorShape::default()
            };
            let geometry = vector_shape_geometry(&source).unwrap();
            let count = flattened(&geometry, FLATNESS, &NeverCancel, |_| {}).unwrap();
            let path = super::linear_path(&geometry, count, FLATNESS, &NeverCancel).unwrap();
            let stroke = path
                .stroke(
                    &tiny_skia::Stroke {
                        width: 2.25,
                        miter_limit: 10.0,
                        ..tiny_skia::Stroke::default()
                    },
                    1.0,
                )
                .unwrap();
            let expected = super::raster_tile(
                [
                    Some((&path, source.fill.unwrap())),
                    Some((&stroke, source.stroke)),
                ],
                [0, 0],
                [320, 260],
                [1.0, 1.0],
                &NeverCancel,
            )
            .unwrap();
            let mut actual = vec![0; 320 * 260 * 4];
            let mut budget = super::MAX_WORK;
            super::paint(
                &source,
                PhysicalSize::new(320, 260).unwrap(),
                [1.0, 1.0],
                RenderLimits::default(),
                &NeverCancel,
                &mut budget,
                |x, y, pixel| {
                    let offset = usize::try_from((y * 320 + x) * 4).unwrap();
                    actual[offset..offset + 4].copy_from_slice(&pixel);
                },
            )
            .unwrap();
            let mismatches: Vec<_> = actual
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.as_chunks::<4>().0)
                .enumerate()
                .filter(|(_, (actual, expected))| actual != expected)
                .map(|(index, (actual, expected))| (index % 320, index / 320, *actual, *expected))
                .collect();
            assert!(
                mismatches.is_empty(),
                "tile differences for {kind:?}: {} pixels, first {:?}",
                mismatches.len(),
                &mismatches[..mismatches.len().min(12)]
            );
        }
    }
}
