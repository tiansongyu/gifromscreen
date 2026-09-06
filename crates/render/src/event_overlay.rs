//! Small, allocation-free event geometry and frozen per-frame progress labels.

use gif_from_screen_domain::ProgressDirection;

use super::{
    CANCELLATION_PIXEL_INTERVAL, CancellationToken, FrameAssetProvider, OverlayContent,
    OverlayLayer, RenderError, RenderLimits, Rgba, RgbaSurface, TimeUs, TimelineSpan, blend_pixel,
    check_cancelled, checked_byte_len, composite_raster_overlay, unsupported_overlay,
};

pub(super) fn legacy_progress_amount(span: TimelineSpan, sample_time: TimeUs) -> u32 {
    let elapsed = sample_time.get().saturating_sub(span.start.get());
    u32::try_from(u128::from(elapsed) * 1_000_000 / u128::from(span.duration.get()))
        .unwrap_or(1_000_000)
        .min(1_000_000)
}

pub(super) fn composite_event<P: FrameAssetProvider + ?Sized, C: CancellationToken + ?Sized>(
    destination: &mut RgbaSurface,
    layer: OverlayLayer<'_>,
    sample_time: TimeUs,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    match layer.content {
        OverlayContent::MouseClick {
            position,
            color,
            radius,
            ..
        } => {
            let radius = i64::from(*radius);
            let cx = i64::from(position.x.get());
            let cy = i64::from(position.y.get());
            scan_signed(
                destination,
                layer,
                (cx - radius, cy - radius, cx + radius + 1, cy + radius + 1),
                cancellation,
                |x, y| {
                    (((x - cx) * (x - cx) + (y - cy) * (y - cy)) <= radius * radius)
                        .then_some(*color)
                },
            )
        }
        OverlayContent::Cursor { .. } => composite_cursor(
            destination,
            layer,
            sample_time,
            provider,
            limits,
            cancellation,
        ),
        OverlayContent::Progress { .. } => composite_progress(
            destination,
            layer,
            sample_time,
            provider,
            limits,
            cancellation,
        ),
        content => Err(unsupported_overlay(layer.id, content)),
    }
}

fn composite_cursor<P: FrameAssetProvider + ?Sized, C: CancellationToken + ?Sized>(
    destination: &mut RgbaSurface,
    layer: OverlayLayer<'_>,
    _sample_time: TimeUs,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    match layer.content {
        OverlayContent::Cursor {
            cursor_asset: Some(asset_id),
            position,
            hotspot,
        } => {
            let source =
                provider
                    .load_rgba8(*asset_id)
                    .map_err(|source| RenderError::OverlayAssetLoad {
                        overlay_id: layer.id,
                        asset_id: *asset_id,
                        source,
                    })?;
            let requested = checked_byte_len(source.size())?;
            if requested > limits.max_surface_bytes {
                return Err(RenderError::SurfaceLimitExceeded {
                    requested,
                    limit: limits.max_surface_bytes,
                });
            }
            let x = i64::from(position.x.get()) - i64::from(hotspot.x.get());
            let y = i64::from(position.y.get()) - i64::from(hotspot.y.get());
            scan_signed(
                destination,
                layer,
                (
                    x,
                    y,
                    x + i64::from(source.width()),
                    y + i64::from(source.height()),
                ),
                cancellation,
                |dx, dy| {
                    let index = source.byte_offset(
                        u32::try_from(dx - x).expect("clipped cursor x"),
                        u32::try_from(dy - y).expect("clipped cursor y"),
                    );
                    let p = &source.pixels()[index..index + 4];
                    Some(Rgba {
                        red: p[0],
                        green: p[1],
                        blue: p[2],
                        alpha: p[3],
                    })
                },
            )
        }
        OverlayContent::Cursor {
            cursor_asset: None,
            position,
            ..
        } => {
            // A deterministic built-in arrow also supplies the manual Wayland fallback.
            let x = i64::from(position.x.get());
            let y = i64::from(position.y.get());
            scan_signed(
                destination,
                layer,
                (x, y, x + 12, y + 18),
                cancellation,
                |dx, dy| {
                    let (dx, dy) = (dx - x, dy - y);
                    if dx * 3 > dy * 2 || (dy > 12 && !(4..=7).contains(&dx)) {
                        return None;
                    }
                    let edge = dx == 0 || dx * 3 + 3 > dy * 2 || dy == 17;
                    let value = if edge { 0 } else { 255 };
                    Some(Rgba {
                        red: value,
                        green: value,
                        blue: value,
                        alpha: 255,
                    })
                },
            )
        }
        content => Err(unsupported_overlay(layer.id, content)),
    }
}

fn composite_progress<P: FrameAssetProvider + ?Sized, C: CancellationToken + ?Sized>(
    destination: &mut RgbaSurface,
    layer: OverlayLayer<'_>,
    sample_time: TimeUs,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    match layer.content {
        OverlayContent::Progress {
            bounds,
            foreground,
            background,
            style,
            ..
        } => {
            let amount = if let Some(style) = style {
                style.amount_millionths
            } else {
                let span = layer.span.ok_or(RenderError::InvalidOverlayGeometry {
                    overlay_id: layer.id,
                    reason: "frame-owned progress requires a frozen style",
                })?;
                legacy_progress_amount(span, sample_time)
            }
            .min(1_000_000);
            let direction = style
                .as_ref()
                .map_or(ProgressDirection::LeftToRight, |style| style.direction);
            let x = i64::from(bounds.origin.x.get());
            let y = i64::from(bounds.origin.y.get());
            let w = i64::from(bounds.size.width.get());
            let h = i64::from(bounds.size.height.get());
            let extent = match direction {
                ProgressDirection::LeftToRight | ProgressDirection::RightToLeft => {
                    bounds.size.width.get()
                }
                ProgressDirection::TopToBottom | ProgressDirection::BottomToTop => {
                    bounds.size.height.get()
                }
            };
            let filled = style.as_ref().and_then(|style| style.fraction).map_or_else(
                || {
                    u32::try_from((u64::from(extent) * u64::from(amount)).div_ceil(1_000_000))
                        .expect("legacy fill is bounded by extent")
                },
                |fraction| fraction.scaled_rounded(extent),
            );
            scan_signed(
                destination,
                layer,
                (x, y, x + w, y + h),
                cancellation,
                |dx, dy| {
                    let offset = match direction {
                        ProgressDirection::LeftToRight => dx - x,
                        ProgressDirection::RightToLeft => x + w - 1 - dx,
                        ProgressDirection::TopToBottom => dy - y,
                        ProgressDirection::BottomToTop => y + h - 1 - dy,
                    };
                    Some(if offset < i64::from(filled) {
                        *foreground
                    } else {
                        *background
                    })
                },
            )?;
            if let Some(style) = style
                && let Some(label) = &style.label
            {
                composite_raster_overlay(
                    destination,
                    layer.id,
                    label.asset_id,
                    style.label_position,
                    label.size,
                    255,
                    layer.track_opacity,
                    layer.blend_mode,
                    provider,
                    limits,
                    cancellation,
                )?;
            }
            Ok(())
        }
        content => Err(unsupported_overlay(layer.id, content)),
    }
}
fn scan_signed<C: CancellationToken + ?Sized>(
    destination: &mut RgbaSurface,
    layer: OverlayLayer<'_>,
    bounds: (i64, i64, i64, i64),
    cancellation: &C,
    pixel: impl Fn(i64, i64) -> Option<Rgba>,
) -> Result<(), RenderError> {
    let (left, top, right, bottom) = bounds;
    let left = left.clamp(0, i64::from(destination.width()));
    let top = top.clamp(0, i64::from(destination.height()));
    let right = right.clamp(left, i64::from(destination.width()));
    let bottom = bottom.clamp(top, i64::from(destination.height()));
    for y in top..bottom {
        check_cancelled(cancellation)?;
        for x in left..right {
            if x % i64::from(CANCELLATION_PIXEL_INTERVAL) == 0 {
                check_cancelled(cancellation)?;
            }
            if let Some(color) = pixel(x, y) {
                let offset = destination.byte_offset(
                    u32::try_from(x).expect("clipped x"),
                    u32::try_from(y).expect("clipped y"),
                );
                blend_pixel(
                    &mut destination.pixels_mut()[offset..offset + 4],
                    &[color.red, color.green, color.blue, color.alpha],
                    255,
                    layer.track_opacity,
                    layer.blend_mode,
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeverCancel;
    use gif_from_screen_domain::{
        AssetId, BlendMode, DurationUs, OverlayId, OverlayItem, PhysicalPoint, PhysicalPx,
        PhysicalRect, PhysicalSize, ProgressStyle, TimelineSpan,
    };

    fn render(content: OverlayContent) -> RgbaSurface {
        render_size(content, PhysicalSize::new(4, 4).unwrap())
    }

    fn render_size(content: OverlayContent, size: PhysicalSize) -> RgbaSurface {
        let item = OverlayItem {
            id: OverlayId::from_u128(2),
            z_index: 0,
            span: TimelineSpan {
                start: TimeUs::ZERO,
                duration: DurationUs::new(10).unwrap(),
            },
            content,
        };
        let mut destination = RgbaSurface::new(
            size,
            vec![0; usize::try_from(size.area().unwrap()).unwrap() * 4],
        )
        .unwrap();
        let provider = |_id: AssetId| {
            RgbaSurface::new(PhysicalSize::new(2, 2).unwrap(), vec![255; 16])
                .map_err(|e| -> crate::AssetProviderError { Box::new(e) })
        };
        composite_event(
            &mut destination,
            OverlayLayer {
                id: item.id,
                content: &item.content,
                span: Some(item.span),
                z_index: item.z_index,
                track_opacity: 255,
                blend_mode: BlendMode::Normal,
                track_index: 0,
                item_index: 0,
            },
            TimeUs::ZERO,
            &provider,
            RenderLimits::default(),
            &NeverCancel,
        )
        .unwrap();
        destination
    }

    #[test]
    fn progress_all_directions_fill_exactly_half() {
        for direction in [
            ProgressDirection::LeftToRight,
            ProgressDirection::RightToLeft,
            ProgressDirection::TopToBottom,
            ProgressDirection::BottomToTop,
        ] {
            let output = render(OverlayContent::Progress {
                bounds: PhysicalRect {
                    origin: PhysicalPoint::default(),
                    size: PhysicalSize::new(4, 4).unwrap(),
                },
                foreground: Rgba {
                    red: 255,
                    green: 0,
                    blue: 0,
                    alpha: 255,
                },
                background: Rgba::TRANSPARENT,
                show_frame_number: false,
                style: Some(ProgressStyle {
                    amount_millionths: 500_000,
                    fraction: None,
                    direction,
                    label: None,
                    label_position: PhysicalPoint::default(),
                    label_text: String::new(),
                }),
            });
            assert_eq!(
                output
                    .pixels()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p[3] != 0)
                    .count(),
                8
            );
            let expected_first = matches!(
                direction,
                ProgressDirection::LeftToRight | ProgressDirection::TopToBottom
            );
            assert_eq!(output.pixels()[3] != 0, expected_first);
        }
    }

    #[test]
    fn exact_progress_rounds_even_while_legacy_progress_keeps_old_pixels() {
        use gif_from_screen_domain::ProgressFraction;
        for (width, amount, fraction, expected) in [
            (10, 333_333, None, 4),
            (10, 0, ProgressFraction::new(1, 3), 3),
            (9, 166_666, ProgressFraction::new(1, 6), 2),
            (3, 166_666, ProgressFraction::new(1, 6), 0),
        ] {
            for direction in [
                ProgressDirection::LeftToRight,
                ProgressDirection::RightToLeft,
                ProgressDirection::TopToBottom,
                ProgressDirection::BottomToTop,
            ] {
                let vertical = matches!(
                    direction,
                    ProgressDirection::TopToBottom | ProgressDirection::BottomToTop
                );
                let size = if vertical {
                    PhysicalSize::new(1, width)
                } else {
                    PhysicalSize::new(width, 1)
                }
                .unwrap();
                let content = OverlayContent::Progress {
                    bounds: PhysicalRect {
                        origin: PhysicalPoint::default(),
                        size,
                    },
                    foreground: Rgba {
                        red: 255,
                        green: 0,
                        blue: 0,
                        alpha: 255,
                    },
                    background: Rgba::TRANSPARENT,
                    show_frame_number: false,
                    style: Some(ProgressStyle {
                        amount_millionths: amount,
                        fraction,
                        direction,
                        label: None,
                        label_position: PhysicalPoint::default(),
                        label_text: String::new(),
                    }),
                };
                let output = render_size(content, size);
                assert_eq!(
                    output
                        .pixels()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .filter(|pixel| pixel[3] != 0)
                        .count(),
                    expected,
                    "width={width}, direction={direction:?}, fraction={fraction:?}"
                );
            }
        }
    }

    #[test]
    fn cursor_hotspot_clips_without_saturating_position() {
        let output = render(OverlayContent::Cursor {
            cursor_asset: Some(AssetId::from_digest([1; 32])),
            position: PhysicalPoint::default(),
            hotspot: PhysicalPoint {
                x: PhysicalPx::new(1),
                y: PhysicalPx::new(1),
            },
        });
        assert_eq!(output.pixels()[3], 255);
        assert_eq!(
            output
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] != 0)
                .count(),
            1
        );
    }

    #[test]
    fn click_at_canvas_corner_remains_round_and_clipped() {
        let output = render(OverlayContent::MouseClick {
            position: PhysicalPoint::default(),
            button: gif_from_screen_domain::MouseButton::Left,
            color: Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 128,
            },
            radius: 2,
        });
        assert_eq!(output.pixels()[3], 128);
        assert_eq!(output.pixels()[(2 * 4 + 2) * 4 + 3], 0);
    }
}
