//! Cursor patches match embedding before the frame's crop/resize/rotate/flip pipeline.

use super::{check_cancelled, geometry::GeometryOperation};
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

#[cfg(test)]
fn transform_cursor(
    source: &RgbaSurface,
    frame: &FrameClip,
    frame_size: PhysicalSize,
    cancellation: &AtomicBool,
) -> Result<Option<(RgbaSurface, PhysicalPoint)>, String> {
    transform_cursor_at_stage(source, frame, frame_size, None, cancellation)
}

pub(super) fn transform_cursor_at_stage(
    source: &RgbaSurface,
    frame: &FrameClip,
    frame_size: PhysicalSize,
    stage: Option<u32>,
    cancellation: &AtomicBool,
) -> Result<Option<(RgbaSurface, PhysicalPoint)>, String> {
    check_cancelled(cancellation)?;
    let Some(position) = frame.capture_metadata.cursor_position else {
        return Ok(None);
    };
    let hotspot = frame.capture_metadata.cursor_hotspot.unwrap_or_default();
    let initial = (
        i64::from(position.x.get()) - i64::from(hotspot.x.get()),
        i64::from(position.y.get()) - i64::from(hotspot.y.get()),
    );
    let mut previous: Option<(RgbaSurface, PhysicalPoint)> = None;
    for geometry in super::geometry::geometry_to_stage(frame, frame_size, stage)? {
        check_cancelled(cancellation)?;
        let (input_size, transform) = match geometry {
            GeometryOperation::Transform {
                input_size,
                transform,
            } => (input_size, transform),
            GeometryOperation::PlaceCanvas {
                input_size,
                output_size,
                source_origin,
            } => {
                let (patch, position) = previous.take().ok_or(
                    "Canvas placement requires a cursor clipped to its source frame first.",
                )?;
                let position = placed_patch_origin(
                    position,
                    patch.size(),
                    input_size,
                    output_size,
                    source_origin,
                )?;
                previous = Some((patch, position));
                continue;
            }
        };
        let (source, origin) = previous
            .as_ref()
            .map_or((source, initial), |(pixels, position)| {
                (
                    pixels,
                    (i64::from(position.x.get()), i64::from(position.y.get())),
                )
            });
        let Some(patch) = transform_patch(source, origin, input_size, transform, cancellation)?
        else {
            return Ok(None);
        };
        previous = Some(patch);
    }
    Ok(previous)
}

fn placed_patch_origin(
    position: PhysicalPoint,
    patch_size: PhysicalSize,
    input_size: PhysicalSize,
    output_size: PhysicalSize,
    source_origin: PhysicalPoint,
) -> Result<PhysicalPoint, String> {
    if !(PhysicalRect {
        origin: position,
        size: patch_size,
    })
    .fits_within(input_size)
    {
        return Err("Cursor patch is outside the pre-placement canvas.".to_owned());
    }
    let x = position
        .x
        .get()
        .checked_add(source_origin.x.get())
        .ok_or("Cursor canvas placement overflow.")?;
    let y = position
        .y
        .get()
        .checked_add(source_origin.y.get())
        .ok_or("Cursor canvas placement overflow.")?;
    let position = PhysicalPoint {
        x: PhysicalPx::new(x),
        y: PhysicalPx::new(y),
    };
    if !(PhysicalRect {
        origin: position,
        size: patch_size,
    })
    .fits_within(output_size)
    {
        return Err("Cursor patch is outside the expanded canvas.".to_owned());
    }
    Ok(position)
}

fn transform_patch(
    source: &RgbaSurface,
    origin: (i64, i64),
    frame_size: PhysicalSize,
    transform: ClipTransform,
    cancellation: &AtomicBool,
) -> Result<Option<(RgbaSurface, PhysicalPoint)>, String> {
    let crop = transform.crop.unwrap_or(PhysicalRect {
        origin: PhysicalPoint::default(),
        size: frame_size,
    });
    if !crop.fits_within(frame_size) || crop.size.validate().is_err() {
        return Err("Recorded cursor frame crop is invalid.".to_owned());
    }
    let resized = transform.output_size.unwrap_or(crop.size);
    resized.validate().map_err(|error| error.to_string())?;
    if source.pixels().len() > MAX_CURSOR_SURFACE_BYTES {
        return Err("Recorded cursor image exceeds 64 MiB.".to_owned());
    }
    let origin_x = origin.0 - i64::from(crop.origin.x.get());
    let origin_y = origin.1 - i64::from(crop.origin.y.get());
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
        ..transform
    };
    let rendered = if transform == ClipTransform::default() {
        patch
    } else {
        CpuRenderer::with_limits(RenderLimits {
            max_surface_bytes: MAX_CURSOR_SURFACE_BYTES,
        })
        .transform_surface(&patch, transform, &Cancellation(cancellation))
        .map_err(|error| error.to_string())?
    };
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

    fn staged_frame() -> FrameClip {
        use gif_from_screen_domain::{Effect, FrameRenderStep};
        FrameClip {
            id: FrameId::from_u128(7),
            asset_id: AssetId::from_digest([7; 32]),
            duration: DurationUs::new(10).unwrap(),
            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
            capture_clock: None,
            capture_metadata: CaptureMetadata {
                cursor_position: Some(PhysicalPoint {
                    x: PhysicalPx::new(4),
                    y: PhysicalPx::new(3),
                }),
                cursor_hotspot: Some(PhysicalPoint {
                    x: PhysicalPx::new(1),
                    y: PhysicalPx::new(1),
                }),
                ..CaptureMetadata::default()
            },
            transform: ClipTransform {
                crop: Some(PhysicalRect::new(1, 1, 6, 4).unwrap()),
                output_size: Some(PhysicalSize::new(9, 5).unwrap()),
                ..ClipTransform::default()
            },
            effects: Vec::new(),
            render_steps: vec![
                FrameRenderStep::composite(11),
                FrameRenderStep::Crop {
                    rect: PhysicalRect::new(1, 0, 7, 5).unwrap(),
                },
                FrameRenderStep::Resize {
                    size: PhysicalSize::new(5, 7).unwrap(),
                },
                FrameRenderStep::composite(22),
                FrameRenderStep::Rotate {
                    rotation: QuarterTurn::Clockwise90,
                },
                FrameRenderStep::FlipHorizontal,
                FrameRenderStep::Effect {
                    effect: Effect::Blur {
                        region: PhysicalRect::new(0, 0, 7, 5).unwrap(),
                        radius: 1,
                    },
                },
                FrameRenderStep::Resize {
                    size: PhysicalSize::new(11, 8).unwrap(),
                },
                FrameRenderStep::FlipVertical,
                FrameRenderStep::composite(33),
                FrameRenderStep::Crop {
                    rect: PhysicalRect::new(0, 1, 11, 6).unwrap(),
                },
                FrameRenderStep::Resize {
                    size: PhysicalSize::new(4, 9).unwrap(),
                },
            ],
        }
    }

    fn embed(cursor: &RgbaSurface, size: PhysicalSize, origin: (i64, i64)) -> RgbaSurface {
        let mut pixels = vec![0; usize::try_from(size.area().unwrap()).unwrap() * 4];
        for y in 0..cursor.height() {
            for x in 0..cursor.width() {
                let (dx, dy) = (origin.0 + i64::from(x), origin.1 + i64::from(y));
                if dx >= 0
                    && dy >= 0
                    && dx < i64::from(size.width.get())
                    && dy < i64::from(size.height.get())
                {
                    let src = usize::try_from(y * cursor.width() + x).unwrap() * 4;
                    let dst = (usize::try_from(dy).unwrap() * size.width.get() as usize
                        + usize::try_from(dx).unwrap())
                        * 4;
                    pixels[dst..dst + 4].copy_from_slice(&cursor.pixels()[src..src + 4]);
                }
            }
        }
        RgbaSurface::new(size, pixels).unwrap()
    }

    fn geometry_reference(
        frame: &FrameClip,
        source: &RgbaSurface,
        stage: Option<u32>,
    ) -> RgbaSurface {
        use gif_from_screen_domain::FrameRenderStep;
        let renderer = CpuRenderer::default();
        let mut surface = renderer
            .transform_surface(source, frame.transform, &NeverCancel)
            .unwrap();
        for step in &frame.render_steps {
            if matches!(step, FrameRenderStep::Composite { stage_id, .. } if Some(*stage_id) == stage)
            {
                break;
            }
            let placement = match step {
                FrameRenderStep::ImageBorder { style } => {
                    Some(style.placement(surface.size()).unwrap())
                }
                FrameRenderStep::ImageShadow { style } => {
                    Some(style.placement(surface.size()).unwrap())
                }
                _ => None,
            };
            if let Some(placement) = placement {
                // Independent full-canvas baseline: retain the original pixels
                // on transparent expansion, never paint effect/background pixels.
                surface = embed(
                    &surface,
                    placement.output_size,
                    (
                        i64::from(placement.source_origin.x.get()),
                        i64::from(placement.source_origin.y.get()),
                    ),
                );
                continue;
            }
            let mut transform = ClipTransform::default();
            match step {
                FrameRenderStep::FreezeRegion { .. } => {
                    panic!("test input cannot map through frozen pixels")
                }
                FrameRenderStep::Crop { rect } => transform.crop = Some(*rect),
                FrameRenderStep::Resize { size } => transform.output_size = Some(*size),
                FrameRenderStep::Rotate { rotation } => transform.rotation = *rotation,
                FrameRenderStep::FlipHorizontal => transform.flip_horizontal = true,
                FrameRenderStep::FlipVertical => transform.flip_vertical = true,
                FrameRenderStep::Composite { .. }
                | FrameRenderStep::Effect { .. }
                | FrameRenderStep::ImageBorder { .. }
                | FrameRenderStep::ImageShadow { .. } => continue,
            }
            surface = renderer
                .transform_surface(&surface, transform, &NeverCancel)
                .unwrap();
        }
        surface
    }

    fn expanded_frame() -> FrameClip {
        use gif_from_screen_domain::{
            FrameRenderStep, ImageBorderStyle, ImageShadowStyle, Rgba, SignedEdgeWidths,
        };
        let mut frame = staged_frame();
        frame.transform = ClipTransform::default();
        frame.render_steps = vec![
            FrameRenderStep::composite(11),
            FrameRenderStep::ImageBorder {
                style: ImageBorderStyle {
                    widths: SignedEdgeWidths {
                        left_milli: -1500,
                        top_milli: -2500,
                        right_milli: 750,
                        bottom_milli: -500,
                    },
                    color: Rgba {
                        red: 10,
                        green: 240,
                        blue: 90,
                        alpha: 255,
                    },
                    background: Rgba {
                        red: 250,
                        green: 10,
                        blue: 200,
                        alpha: 255,
                    },
                },
            },
            FrameRenderStep::composite(22),
            FrameRenderStep::ImageShadow {
                style: ImageShadowStyle {
                    blur_radius_hundredths: 425,
                    depth_hundredths: 225,
                    direction_hundredths: 18_000,
                    opacity_basis_points: 7500,
                    color: Rgba {
                        red: 200,
                        green: 90,
                        blue: 5,
                        alpha: 255,
                    },
                    background: Rgba {
                        red: 5,
                        green: 90,
                        blue: 210,
                        alpha: 255,
                    },
                },
            },
            FrameRenderStep::Rotate {
                rotation: QuarterTurn::Clockwise90,
            },
            FrameRenderStep::FlipHorizontal,
            FrameRenderStep::composite(33),
            FrameRenderStep::Resize {
                size: PhysicalSize::new(11, 8).unwrap(),
            },
            FrameRenderStep::Crop {
                rect: PhysicalRect::new(1, 1, 9, 6).unwrap(),
            },
            FrameRenderStep::FlipVertical,
        ];
        frame
    }

    #[test]
    fn expanding_effects_translate_only_preclipped_cursor_pixels_before_later_geometry() {
        let cursor = RgbaSurface::new(
            PhysicalSize::new(3, 2).unwrap(),
            vec![
                255, 0, 0, 128, 0, 255, 0, 255, 80, 70, 60, 0, 0, 0, 255, 255, 255, 255, 0, 90, 40,
                30, 20, 0,
            ],
        )
        .unwrap();
        let size = PhysicalSize::new(8, 6).unwrap();
        for (x, y) in [(0, 0), (2, 2), (4, 3), (7, 5), (8, 6), (9, 7)] {
            let mut frame = expanded_frame();
            frame.capture_metadata.cursor_position = Some(PhysicalPoint {
                x: PhysicalPx::new(x),
                y: PhysicalPx::new(y),
            });
            let before = frame.clone();
            let embedded = embed(&cursor, size, (i64::from(x) - 1, i64::from(y) - 1));
            for stage in [Some(11), Some(22), Some(33), None] {
                let expected = geometry_reference(&frame, &embedded, stage);
                let patch = transform_cursor_at_stage(
                    &cursor,
                    &frame,
                    size,
                    stage,
                    &AtomicBool::new(false),
                )
                .unwrap();
                let actual = patch.map_or_else(
                    || RgbaSurface::new(expected.size(), vec![0; expected.pixels().len()]).unwrap(),
                    |(patch, position)| {
                        embed(
                            &patch,
                            expected.size(),
                            (i64::from(position.x.get()), i64::from(position.y.get())),
                        )
                    },
                );
                assert_eq!(actual, expected, "point=({x},{y}), stage={stage:?}");
            }
            assert_eq!(
                frame, before,
                "geometry must not rewrite raw cursor/clock metadata"
            );
        }
    }

    #[test]
    fn staged_nearest_cursor_matches_full_embedding_and_stops_before_its_authoring_stage() {
        let cursor = RgbaSurface::new(
            PhysicalSize::new(3, 2).unwrap(),
            vec![
                255, 0, 0, 128, 0, 255, 0, 255, 80, 70, 60, 0, 0, 0, 255, 255, 255, 255, 0, 90, 40,
                30, 20, 0,
            ],
        )
        .unwrap();
        let size = PhysicalSize::new(8, 6).unwrap();
        for (x, y) in [(0, 0), (2, 2), (4, 3), (7, 5), (8, 6)] {
            let mut frame = staged_frame();
            frame.capture_metadata.cursor_position = Some(PhysicalPoint {
                x: PhysicalPx::new(x),
                y: PhysicalPx::new(y),
            });
            let embedded = embed(&cursor, size, (i64::from(x) - 1, i64::from(y) - 1));
            for stage in [Some(11), Some(22), Some(33), None] {
                let expected = geometry_reference(&frame, &embedded, stage);
                let patch = transform_cursor_at_stage(
                    &cursor,
                    &frame,
                    size,
                    stage,
                    &AtomicBool::new(false),
                )
                .unwrap();
                let actual = patch.map_or_else(
                    || RgbaSurface::new(expected.size(), vec![0; expected.pixels().len()]).unwrap(),
                    |(patch, position)| {
                        embed(
                            &patch,
                            expected.size(),
                            (i64::from(position.x.get()), i64::from(position.y.get())),
                        )
                    },
                );
                assert_eq!(actual, expected, "point=({x},{y}), stage={stage:?}");
            }
        }
    }

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
                        render_steps: Vec::new(),
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
