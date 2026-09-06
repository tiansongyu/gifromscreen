//! Cursor patches match embedding before the frame's crop/resize/rotate/flip pipeline.

use super::check_cancelled;
use gif_from_screen_domain::{
    ClipTransform, FrameClip, PhysicalPoint, PhysicalPx, PhysicalRect, PhysicalSize, QuarterTurn,
};
use gif_from_screen_render::{CancellationToken, CpuRenderer, RenderLimits, RgbaSurface};
use std::sync::atomic::AtomicBool;

const MAX_CURSOR_SURFACE_BYTES: usize = 64 * 1024 * 1024;

pub(super) fn pad_cursor_surface(source: &RgbaSurface) -> Result<RgbaSurface, String> {
    let size = PhysicalSize::new(
        source
            .width()
            .checked_add(1)
            .ok_or_else(|| "Cursor padding width overflow.".to_owned())?,
        source.height(),
    )
    .map_err(|error| error.to_string())?;
    let bytes = size
        .area()
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= MAX_CURSOR_SURFACE_BYTES as u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| "Padded cursor exceeds 64 MiB.".to_owned())?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(bytes)
        .map_err(|_| "Could not allocate cursor padding.".to_owned())?;
    rgba.resize(bytes, 0);
    let row = source.width() as usize * 4;
    let padded = size.width.get() as usize * 4;
    for (source_row, destination_row) in source
        .pixels()
        .chunks_exact(row)
        .zip(rgba.chunks_exact_mut(padded))
    {
        destination_row[..row].copy_from_slice(source_row);
    }
    RgbaSurface::new(size, rgba).map_err(|error| error.to_string())
}
struct Cancellation<'a>(&'a AtomicBool);
impl CancellationToken for Cancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

pub(super) fn transform_cursor(
    source: &RgbaSurface,
    frame: &FrameClip,
    frame_size: PhysicalSize,
    cancellation: &AtomicBool,
) -> Result<Option<(RgbaSurface, PhysicalPoint)>, String> {
    check_cancelled(cancellation)?;
    let Some(position) = frame.capture_metadata.cursor_position else {
        return Ok(None);
    };
    let hotspot = frame.capture_metadata.cursor_hotspot.unwrap_or_default();
    let crop = frame.transform.crop.unwrap_or(PhysicalRect {
        origin: PhysicalPoint::default(),
        size: frame_size,
    });
    if !crop.fits_within(frame_size) || crop.size.validate().is_err() {
        return Err("Recorded cursor frame crop is invalid.".to_owned());
    }
    let resized = frame.transform.output_size.unwrap_or(crop.size);
    resized.validate().map_err(|error| error.to_string())?;
    if source.pixels().len() > MAX_CURSOR_SURFACE_BYTES {
        return Err("Recorded cursor image exceeds 64 MiB.".to_owned());
    }
    let origin_x =
        i64::from(position.x.get()) - i64::from(hotspot.x.get()) - i64::from(crop.origin.x.get());
    let origin_y =
        i64::from(position.y.get()) - i64::from(hotspot.y.get()) - i64::from(crop.origin.y.get());
    let (left, right) = visible_axis(
        origin_x,
        source.width(),
        crop.size.width.get(),
        resized.width.get(),
    );
    let (top, bottom) = visible_axis(
        origin_y,
        source.height(),
        crop.size.height.get(),
        resized.height.get(),
    );
    if left >= right || top >= bottom {
        return Ok(None);
    }
    let size = PhysicalSize::new(right - left, bottom - top).map_err(|error| error.to_string())?;
    let bytes = size
        .area()
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= MAX_CURSOR_SURFACE_BYTES as u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| "The transformed cursor exceeds 64 MiB.".to_owned())?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(bytes)
        .map_err(|_| "Unable to allocate the transformed cursor.".to_owned())?;
    rgba.resize(bytes, 0);
    for y in top..bottom {
        check_cancelled(cancellation)?;
        let sy = i64::try_from(
            u64::from(y) * u64::from(crop.size.height.get()) / u64::from(resized.height.get()),
        )
        .expect("source y bounded")
            - origin_y;
        for x in left..right {
            if (x - left).is_multiple_of(1024) {
                check_cancelled(cancellation)?;
            }
            let sx = i64::try_from(
                u64::from(x) * u64::from(crop.size.width.get()) / u64::from(resized.width.get()),
            )
            .expect("source x bounded")
                - origin_x;
            let src = (usize::try_from(sy).expect("visible y") * source.width() as usize
                + usize::try_from(sx).expect("visible x"))
                * 4;
            let dst = ((y - top) as usize * size.width.get() as usize + (x - left) as usize) * 4;
            rgba[dst..dst + 4].copy_from_slice(&source.pixels()[src..src + 4]);
        }
    }
    let patch = RgbaSurface::new(size, rgba).map_err(|error| error.to_string())?;
    let transform = ClipTransform {
        crop: None,
        output_size: None,
        ..frame.transform
    };
    let clip = FrameClip {
        transform,
        effects: Vec::new(),
        ..frame.clone()
    };
    let provider = |_| Ok(patch.clone());
    let rendered = CpuRenderer::with_limits(RenderLimits {
        max_surface_bytes: MAX_CURSOR_SURFACE_BYTES,
    })
    .render_clip(&clip, &provider, &Cancellation(cancellation))
    .map_err(|error| error.to_string())?;
    let position = transform_patch_origin(left, top, size, resized, transform);
    Ok(Some((rendered, position)))
}

fn visible_axis(origin: i64, extent: u32, crop: u32, resized: u32) -> (u32, u32) {
    let left = u64::try_from(origin.clamp(0, i64::from(crop))).expect("nonnegative clipped start");
    let right = u64::try_from((origin + i64::from(extent)).clamp(0, i64::from(crop)))
        .expect("nonnegative clipped end");
    // floor(dst * crop / resized) belongs to [left,right) exactly on these boundaries.
    (
        u32::try_from((left * u64::from(resized)).div_ceil(u64::from(crop))).expect("bounded left"),
        u32::try_from((right * u64::from(resized)).div_ceil(u64::from(crop)))
            .expect("bounded right"),
    )
}

fn transform_patch_origin(
    x: u32,
    y: u32,
    size: PhysicalSize,
    canvas: PhysicalSize,
    transform: ClipTransform,
) -> PhysicalPoint {
    let (w, h) = (size.width.get(), size.height.get());
    let (cw, ch) = (canvas.width.get(), canvas.height.get());
    let (mut x, mut y, w, h, cw, ch) = match transform.rotation {
        QuarterTurn::Zero => (x, y, w, h, cw, ch),
        QuarterTurn::Clockwise90 => (ch - y - h, x, h, w, ch, cw),
        QuarterTurn::Clockwise180 => (cw - x - w, ch - y - h, w, h, cw, ch),
        QuarterTurn::Clockwise270 => (y, cw - x - w, h, w, ch, cw),
    };
    if transform.flip_horizontal {
        x = cw - x - w;
    }
    if transform.flip_vertical {
        y = ch - y - h;
    }
    PhysicalPoint {
        x: PhysicalPx::new(x),
        y: PhysicalPx::new(y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::{AssetId, CaptureMetadata, DurationUs, FrameId};
    use gif_from_screen_render::NeverCancel;

    #[test]
    fn small_patch_matches_embedding_before_every_frame_transform() {
        let cursor = RgbaSurface::new(
            PhysicalSize::new(3, 3).unwrap(),
            vec![
                255, 0, 0, 128, 0, 255, 0, 255, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 0, 200, 0,
                255, 255, 255, 255, 255, 255, 255, 60, 70, 80, 128, 0, 0, 0, 0,
            ],
        )
        .unwrap();
        let source_size = PhysicalSize::new(8, 5).unwrap();
        for point in [(0, 0), (2, 2), (7, 4), (1, 1)] {
            for rotation in [
                QuarterTurn::Zero,
                QuarterTurn::Clockwise90,
                QuarterTurn::Clockwise180,
                QuarterTurn::Clockwise270,
            ] {
                for flips in [(false, false), (true, false), (false, true), (true, true)] {
                    let metadata = CaptureMetadata {
                        cursor_position: Some(PhysicalPoint {
                            x: PhysicalPx::new(point.0),
                            y: PhysicalPx::new(point.1),
                        }),
                        cursor_hotspot: Some(PhysicalPoint {
                            x: PhysicalPx::new(1),
                            y: PhysicalPx::new(1),
                        }),
                        ..CaptureMetadata::default()
                    };
                    let clip = FrameClip {
                        capture_clock: None,
                        capture_binding: gif_from_screen_domain::CaptureBinding::Original,
                        id: FrameId::from_u128(1),
                        asset_id: AssetId::from_digest([1; 32]),
                        duration: DurationUs::new(1000).unwrap(),
                        capture_metadata: metadata,
                        transform: ClipTransform {
                            crop: Some(PhysicalRect::new(1, 1, 5, 3).unwrap()),
                            output_size: Some(PhysicalSize::new(7, 4).unwrap()),
                            rotation,
                            flip_horizontal: flips.0,
                            flip_vertical: flips.1,
                        },
                        effects: Vec::new(),
                    };
                    let mut embedded = vec![0; 8 * 5 * 4];
                    for cy in 0..3_i64 {
                        for cx in 0..3_i64 {
                            let x = i64::from(point.0) - 1 + cx;
                            let y = i64::from(point.1) - 1 + cy;
                            if (0..8).contains(&x) && (0..5).contains(&y) {
                                let dst = usize::try_from(y * 8 + x).unwrap() * 4;
                                let src = usize::try_from(cy * 3 + cx).unwrap() * 4;
                                embedded[dst..dst + 4]
                                    .copy_from_slice(&cursor.pixels()[src..src + 4]);
                            }
                        }
                    }
                    let source = RgbaSurface::new(source_size, embedded).unwrap();
                    let expected = CpuRenderer::new()
                        .render_clip(&clip, &|_| Ok(source.clone()), &NeverCancel)
                        .unwrap();
                    let patch =
                        transform_cursor(&cursor, &clip, source_size, &AtomicBool::new(false))
                            .unwrap();
                    let mut actual = vec![0; expected.pixels().len()];
                    if let Some((patch, position)) = patch {
                        for y in 0..patch.height() {
                            for x in 0..patch.width() {
                                let src = (y * patch.width() + x) as usize * 4;
                                let dst = ((position.y.get() + y) * expected.width()
                                    + position.x.get()
                                    + x) as usize
                                    * 4;
                                actual[dst..dst + 4].copy_from_slice(&patch.pixels()[src..src + 4]);
                            }
                        }
                    }
                    assert_eq!(
                        actual,
                        expected.pixels(),
                        "point={point:?} rotation={rotation:?} flips={flips:?}"
                    );
                }
            }
        }
    }
}
