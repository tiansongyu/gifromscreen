use gif_from_screen_domain::{
    AssetId, BlendMode, FrameClip, OverlayContent, OverlayId, OverlayTrack, PhysicalPoint,
    PhysicalSize, TimeUs,
};

use crate::{
    CancellationToken, CpuRenderer, FrameAssetProvider, RenderError, RenderLimits, RgbaSurface,
    SurfaceError, surface::checked_byte_len,
};

const CANCELLATION_PIXEL_INTERVAL: u32 = 1_024;

/// Stable identity pair for one active raster overlay and its immutable asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterOverlayAsset {
    /// Overlay item referencing the asset.
    pub overlay_id: OverlayId,
    /// Immutable RGBA8 raster asset.
    pub asset_id: AssetId,
}

#[derive(Clone, Copy, Debug)]
struct RasterLayer {
    overlay_id: OverlayId,
    asset_id: AssetId,
    position: PhysicalPoint,
    size: PhysicalSize,
    item_opacity: u8,
    track_opacity: u8,
    blend_mode: BlendMode,
    z_index: i32,
    track_index: usize,
    item_index: usize,
}

impl CpuRenderer {
    /// Renders one clip and composites all raster overlays active at `sample_time`.
    ///
    /// Overlay spans use half-open point sampling: an item is active when
    /// `span.start <= sample_time < span.end()`. Visible non-zero-opacity raster items are ordered
    /// globally by `(z_index, track order, item order)`. Each raster is nearest-neighbor sampled at
    /// its requested size directly into the clip surface, so right/bottom clipping never allocates
    /// a full resized copy. Track and item opacity multiply source alpha. Normal, Multiply, and
    /// Screen use deterministic straight-alpha source-over composition. Non-raster overlay content
    /// is intentionally left unchanged for later renderer milestones.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] for base or overlay asset loading failures, invalid/oversized source
    /// surfaces, overflowing spans or plan sizes, allocation failure, or cancellation. The output
    /// is not observable on failure.
    pub fn render_clip_with_raster_overlays<P, C>(
        &self,
        clip: &FrameClip,
        tracks: &[OverlayTrack],
        sample_time: TimeUs,
        provider: &P,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError>
    where
        P: FrameAssetProvider + ?Sized,
        C: CancellationToken + ?Sized,
    {
        let mut surface = self.render_clip(clip, provider, cancellation)?;
        composite_active_raster_overlays(
            &mut surface,
            tracks,
            sample_time,
            provider,
            self.limits(),
            cancellation,
        )?;
        Ok(surface)
    }
}

/// Returns raster assets from visible, non-zero-opacity overlays active at `sample_time`.
///
/// Results follow the same deterministic z/track/item ordering used by
/// [`CpuRenderer::render_clip_with_raster_overlays`]. Repeated asset identities are retained so
/// callers can preserve overlay context or deduplicate explicitly.
///
/// # Errors
///
/// Returns [`RenderError`] for an overflowing active span, plan size/allocation failure, or
/// cancellation.
pub fn active_raster_overlay_assets<C>(
    tracks: &[OverlayTrack],
    sample_time: TimeUs,
    cancellation: &C,
) -> Result<Vec<RasterOverlayAsset>, RenderError>
where
    C: CancellationToken + ?Sized,
{
    let layers = active_raster_layers(tracks, sample_time, cancellation)?;
    let mut assets = Vec::new();
    assets.try_reserve_exact(layers.len()).map_err(|_| {
        RenderError::OverlayPlanAllocationFailed {
            requested: layers.len(),
        }
    })?;
    assets.extend(layers.into_iter().map(|layer| RasterOverlayAsset {
        overlay_id: layer.overlay_id,
        asset_id: layer.asset_id,
    }));
    Ok(assets)
}

fn composite_active_raster_overlays<P, C>(
    destination: &mut RgbaSurface,
    tracks: &[OverlayTrack],
    sample_time: TimeUs,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError>
where
    P: FrameAssetProvider + ?Sized,
    C: CancellationToken + ?Sized,
{
    for layer in active_raster_layers(tracks, sample_time, cancellation)? {
        check_cancelled(cancellation)?;
        if layer.position.x.get() >= destination.width()
            || layer.position.y.get() >= destination.height()
        {
            continue;
        }
        if layer.size.width.get() == 0 || layer.size.height.get() == 0 {
            return Err(SurfaceError::EmptyDimensions {
                width: layer.size.width.get(),
                height: layer.size.height.get(),
            }
            .into());
        }
        let source = provider.load_rgba8(layer.asset_id).map_err(|source| {
            RenderError::OverlayAssetLoad {
                overlay_id: layer.overlay_id,
                asset_id: layer.asset_id,
                source,
            }
        })?;
        let requested = checked_byte_len(source.size())?;
        if requested > limits.max_surface_bytes {
            return Err(RenderError::SurfaceLimitExceeded {
                requested,
                limit: limits.max_surface_bytes,
            });
        }
        composite_layer(destination, &source, layer, cancellation)?;
    }
    Ok(())
}

fn active_raster_layers<C>(
    tracks: &[OverlayTrack],
    sample_time: TimeUs,
    cancellation: &C,
) -> Result<Vec<RasterLayer>, RenderError>
where
    C: CancellationToken + ?Sized,
{
    let mut layers = Vec::new();
    for (track_index, track) in tracks.iter().enumerate() {
        check_cancelled(cancellation)?;
        if !track.visible || track.opacity == 0 {
            continue;
        }
        for (item_index, item) in track.items.iter().enumerate() {
            check_cancelled(cancellation)?;
            let OverlayContent::Raster {
                asset_id,
                position,
                size,
                opacity,
            } = &item.content
            else {
                continue;
            };
            if *opacity == 0 {
                continue;
            }
            let end = item.span.end().ok_or(RenderError::OverlaySpanOverflow {
                overlay_id: item.id,
            })?;
            if sample_time < item.span.start || sample_time >= end {
                continue;
            }
            let requested = layers
                .len()
                .checked_add(1)
                .ok_or(RenderError::OverlayPlanSizeOverflow)?;
            layers
                .try_reserve(1)
                .map_err(|_| RenderError::OverlayPlanAllocationFailed { requested })?;
            layers.push(RasterLayer {
                overlay_id: item.id,
                asset_id: *asset_id,
                position: *position,
                size: *size,
                item_opacity: *opacity,
                track_opacity: track.opacity,
                blend_mode: track.blend_mode,
                z_index: item.z_index,
                track_index,
                item_index,
            });
        }
    }
    layers.sort_unstable_by_key(|layer| (layer.z_index, layer.track_index, layer.item_index));
    check_cancelled(cancellation)?;
    Ok(layers)
}

fn composite_layer<C>(
    destination: &mut RgbaSurface,
    source: &RgbaSurface,
    layer: RasterLayer,
    cancellation: &C,
) -> Result<(), RenderError>
where
    C: CancellationToken + ?Sized,
{
    let start_x = layer.position.x.get();
    let start_y = layer.position.y.get();
    let visible_width = layer.size.width.get().min(destination.width() - start_x);
    let visible_height = layer.size.height.get().min(destination.height() - start_y);
    let scaled_width = u64::from(layer.size.width.get());
    let scaled_height = u64::from(layer.size.height.get());
    for local_y in 0..visible_height {
        check_cancelled(cancellation)?;
        let source_y =
            u32::try_from(u64::from(local_y) * u64::from(source.height()) / scaled_height)
                .expect("nearest overlay y coordinate is bounded by source height");
        for local_x in 0..visible_width {
            if local_x.is_multiple_of(CANCELLATION_PIXEL_INTERVAL) {
                check_cancelled(cancellation)?;
            }
            let source_x =
                u32::try_from(u64::from(local_x) * u64::from(source.width()) / scaled_width)
                    .expect("nearest overlay x coordinate is bounded by source width");
            let source_offset = source.byte_offset(source_x, source_y);
            let destination_offset = destination.byte_offset(start_x + local_x, start_y + local_y);
            let source_pixel = &source.pixels()[source_offset..source_offset + 4];
            let destination_pixel =
                &mut destination.pixels_mut()[destination_offset..destination_offset + 4];
            blend_pixel(
                destination_pixel,
                source_pixel,
                layer.item_opacity,
                layer.track_opacity,
                layer.blend_mode,
            );
        }
    }
    Ok(())
}

fn blend_pixel(
    destination: &mut [u8],
    source: &[u8],
    item_opacity: u8,
    track_opacity: u8,
    blend_mode: BlendMode,
) {
    let opacity_denominator = u64::from(u8::MAX).pow(2);
    let source_alpha = (u64::from(source[3]) * u64::from(item_opacity) * u64::from(track_opacity)
        + opacity_denominator / 2)
        / opacity_denominator;
    if source_alpha == 0 {
        return;
    }
    let destination_alpha = u64::from(destination[3]);
    let inverse_source_alpha = u64::from(u8::MAX) - source_alpha;
    let output_alpha_numerator =
        source_alpha * u64::from(u8::MAX) + destination_alpha * inverse_source_alpha;
    if output_alpha_numerator == 0 {
        destination.copy_from_slice(&[0; 4]);
        return;
    }
    for channel in 0..3 {
        let source_channel = u64::from(source[channel]);
        let destination_channel = u64::from(destination[channel]);
        let blended = u64::from(blend_channel(
            source[channel],
            destination[channel],
            blend_mode,
        ));
        let premultiplied_numerator =
            inverse_source_alpha * destination_channel * destination_alpha
                + (u64::from(u8::MAX) - destination_alpha) * source_channel * source_alpha
                + source_alpha * destination_alpha * blended;
        destination[channel] = u8::try_from(
            (premultiplied_numerator + output_alpha_numerator / 2) / output_alpha_numerator,
        )
        .expect("bounded blend inputs produce an RGBA8 channel");
    }
    destination[3] =
        u8::try_from((output_alpha_numerator + u64::from(u8::MAX) / 2) / u64::from(u8::MAX))
            .expect("source-over alpha remains within RGBA8");
}

fn blend_channel(source: u8, destination: u8, mode: BlendMode) -> u8 {
    match mode {
        BlendMode::Normal => source,
        BlendMode::Multiply => {
            let product = u16::from(source) * u16::from(destination);
            u8::try_from((product + 127) / 255).expect("multiplication blend remains within u8")
        }
        BlendMode::Screen => {
            let inverse_product = u16::from(255 - source) * u16::from(255 - destination);
            255 - u8::try_from((inverse_product + 127) / 255)
                .expect("screen blend remains within u8")
        }
    }
}

fn check_cancelled<C: CancellationToken + ?Sized>(cancellation: &C) -> Result<(), RenderError> {
    if cancellation.is_cancelled() {
        Err(RenderError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use gif_from_screen_domain::{
        CaptureMetadata, ClipTransform, DurationUs, OverlayItem, PhysicalPx, TimelineSpan, TrackId,
    };

    use super::*;
    use crate::NeverCancel;

    fn asset(number: u8) -> AssetId {
        AssetId::from_digest([number; 32])
    }

    fn point(x: u32, y: u32) -> PhysicalPoint {
        PhysicalPoint {
            x: PhysicalPx::new(x),
            y: PhysicalPx::new(y),
        }
    }

    fn surface(width: u32, height: u32, pixels: &[u8]) -> RgbaSurface {
        RgbaSurface::new(PhysicalSize::new(width, height).unwrap(), pixels.to_vec()).unwrap()
    }

    fn clip(asset_id: AssetId) -> FrameClip {
        FrameClip {
            id: gif_from_screen_domain::FrameId::from_u128(1),
            asset_id,
            duration: DurationUs::new(10).unwrap(),
            transform: ClipTransform::default(),
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn raster_item(
        number: u128,
        asset_id: AssetId,
        z_index: i32,
        start: u64,
        duration: u64,
        position: PhysicalPoint,
        size: PhysicalSize,
        opacity: u8,
    ) -> OverlayItem {
        OverlayItem {
            id: OverlayId::from_u128(number),
            span: TimelineSpan {
                start: TimeUs::new(start),
                duration: DurationUs::new(duration).unwrap(),
            },
            z_index,
            content: OverlayContent::Raster {
                asset_id,
                position,
                size,
                opacity,
            },
        }
    }

    fn track(
        number: u128,
        visible: bool,
        opacity: u8,
        blend_mode: BlendMode,
        items: Vec<OverlayItem>,
    ) -> OverlayTrack {
        OverlayTrack {
            id: TrackId::from_u128(number),
            name: format!("track {number}"),
            visible,
            opacity,
            blend_mode,
            items,
        }
    }

    #[test]
    fn active_assets_use_half_open_time_and_global_stable_layer_order() {
        let size = PhysicalSize::new(1, 1).unwrap();
        let tracks = vec![
            track(
                1,
                true,
                255,
                BlendMode::Normal,
                vec![
                    raster_item(1, asset(1), 5, 10, 10, point(0, 0), size, 255),
                    raster_item(2, asset(2), 0, 10, 10, point(0, 0), size, 255),
                    raster_item(3, asset(3), -5, 10, 10, point(0, 0), size, 0),
                ],
            ),
            track(
                2,
                true,
                255,
                BlendMode::Screen,
                vec![raster_item(4, asset(4), 0, 10, 10, point(0, 0), size, 255)],
            ),
            track(
                3,
                false,
                255,
                BlendMode::Normal,
                vec![raster_item(
                    5,
                    asset(5),
                    -10,
                    10,
                    10,
                    point(0, 0),
                    size,
                    255,
                )],
            ),
        ];

        assert!(
            active_raster_overlay_assets(&tracks, TimeUs::new(9), &NeverCancel)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            active_raster_overlay_assets(&tracks, TimeUs::new(10), &NeverCancel).unwrap(),
            [
                RasterOverlayAsset {
                    overlay_id: OverlayId::from_u128(2),
                    asset_id: asset(2),
                },
                RasterOverlayAsset {
                    overlay_id: OverlayId::from_u128(4),
                    asset_id: asset(4),
                },
                RasterOverlayAsset {
                    overlay_id: OverlayId::from_u128(1),
                    asset_id: asset(1),
                },
            ]
        );
        assert!(
            active_raster_overlay_assets(&tracks, TimeUs::new(20), &NeverCancel)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn render_resizes_nearest_clips_right_edge_and_preserves_straight_alpha() {
        let base_id = asset(1);
        let overlay_id = asset(2);
        let sources = BTreeMap::from([
            (base_id, surface(4, 1, &[0, 0, 255, 255].repeat(4))),
            (overlay_id, surface(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128])),
        ]);
        let provider = move |asset_id| {
            sources.get(&asset_id).cloned().ok_or_else(|| {
                Box::new(io::Error::new(io::ErrorKind::NotFound, "missing"))
                    as crate::AssetProviderError
            })
        };
        let tracks = [track(
            1,
            true,
            255,
            BlendMode::Normal,
            vec![raster_item(
                1,
                overlay_id,
                0,
                0,
                10,
                point(1, 0),
                PhysicalSize::new(4, 1).unwrap(),
                255,
            )],
        )];

        let rendered = CpuRenderer::new()
            .render_clip_with_raster_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            )
            .unwrap();

        assert_eq!(
            rendered.pixels(),
            [
                0, 0, 255, 255, // untouched
                255, 0, 0, 255, // scaled source x=0
                255, 0, 0, 255, // scaled source x=0
                0, 128, 127, 255, // clipped scaled source x=1, alpha-composited
            ]
        );
    }

    #[test]
    fn z_order_and_off_canvas_layers_control_loading_and_pixels() {
        let base_id = asset(1);
        let low_id = asset(2);
        let high_id = asset(3);
        let sources = BTreeMap::from([
            (base_id, surface(1, 1, &[0, 0, 0, 255])),
            (low_id, surface(1, 1, &[255, 0, 0, 255])),
            (high_id, surface(1, 1, &[0, 255, 0, 255])),
        ]);
        let provider = move |asset_id| {
            sources.get(&asset_id).cloned().ok_or_else(|| {
                Box::new(io::Error::new(io::ErrorKind::NotFound, "must not load"))
                    as crate::AssetProviderError
            })
        };
        let size = PhysicalSize::new(1, 1).unwrap();
        let tracks = [
            track(
                1,
                true,
                255,
                BlendMode::Normal,
                vec![
                    raster_item(1, high_id, 10, 0, 10, point(0, 0), size, 255),
                    raster_item(2, asset(99), 20, 0, 10, point(9, 9), size, 255),
                ],
            ),
            track(
                2,
                true,
                255,
                BlendMode::Normal,
                vec![raster_item(3, low_id, -10, 0, 10, point(0, 0), size, 255)],
            ),
        ];
        let rendered = CpuRenderer::new()
            .render_clip_with_raster_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            )
            .unwrap();
        assert_eq!(rendered.pixels(), [0, 255, 0, 255]);
    }

    #[test]
    fn blend_modes_and_combined_opacity_are_integer_deterministic() {
        let source = [200, 100, 50, 255];
        let original = [100, 150, 200, 255];
        for (mode, expected) in [
            (BlendMode::Normal, [200, 100, 50, 255]),
            (BlendMode::Multiply, [78, 59, 39, 255]),
            (BlendMode::Screen, [222, 191, 211, 255]),
        ] {
            let mut destination = original;
            blend_pixel(&mut destination, &source, 255, 255, mode);
            assert_eq!(destination, expected);
        }

        let mut destination = [0, 0, 0, 255];
        blend_pixel(
            &mut destination,
            &[255, 255, 255, 255],
            128,
            128,
            BlendMode::Normal,
        );
        assert_eq!(destination, [64, 64, 64, 255]);

        let mut transparent = [99, 88, 77, 0];
        blend_pixel(
            &mut transparent,
            &[255, 0, 0, 128],
            255,
            255,
            BlendMode::Multiply,
        );
        assert_eq!(transparent, [255, 0, 0, 128]);
    }

    #[test]
    fn span_provider_and_limit_errors_are_typed() {
        let size = PhysicalSize::new(1, 1).unwrap();
        let invalid_span = [track(
            1,
            true,
            255,
            BlendMode::Normal,
            vec![raster_item(
                9,
                asset(2),
                0,
                u64::MAX,
                1,
                point(0, 0),
                size,
                255,
            )],
        )];
        assert!(matches!(
            active_raster_overlay_assets(&invalid_span, TimeUs::new(u64::MAX), &NeverCancel),
            Err(RenderError::OverlaySpanOverflow { overlay_id })
                if overlay_id == OverlayId::from_u128(9)
        ));

        let base_id = asset(1);
        let missing_id = asset(2);
        let provider = move |asset_id| {
            if asset_id == base_id {
                Ok(surface(1, 1, &[0, 0, 0, 255]))
            } else {
                Err(Box::new(io::Error::new(io::ErrorKind::NotFound, "missing"))
                    as crate::AssetProviderError)
            }
        };
        let tracks = [track(
            1,
            true,
            255,
            BlendMode::Normal,
            vec![raster_item(4, missing_id, 0, 0, 1, point(0, 0), size, 255)],
        )];
        assert!(matches!(
            CpuRenderer::new().render_clip_with_raster_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            ),
            Err(RenderError::OverlayAssetLoad {
                overlay_id,
                asset_id,
                ..
            }) if overlay_id == OverlayId::from_u128(4) && asset_id == missing_id
        ));

        let sources = BTreeMap::from([
            (base_id, surface(1, 1, &[0, 0, 0, 255])),
            (missing_id, surface(2, 1, &[0; 8])),
        ]);
        let provider = move |asset_id| {
            sources.get(&asset_id).cloned().ok_or_else(|| {
                Box::new(io::Error::new(io::ErrorKind::NotFound, "missing"))
                    as crate::AssetProviderError
            })
        };
        assert!(matches!(
            CpuRenderer::with_limits(RenderLimits {
                max_surface_bytes: 4,
            })
            .render_clip_with_raster_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            ),
            Err(RenderError::SurfaceLimitExceeded {
                requested: 8,
                limit: 4,
            })
        ));
    }

    #[test]
    fn pixel_loop_observes_cancellation_without_returning_partial_output() {
        struct CancelAfter {
            checks: AtomicUsize,
            cancel_at: usize,
        }
        impl CancellationToken for CancelAfter {
            fn is_cancelled(&self) -> bool {
                self.checks.fetch_add(1, Ordering::Relaxed) >= self.cancel_at
            }
        }

        let size = PhysicalSize::new(2_048, 1).unwrap();
        let mut destination = RgbaSurface::new(size, [0, 0, 0, 255].repeat(2_048)).unwrap();
        let source = RgbaSurface::new(size, [255, 255, 255, 255].repeat(2_048)).unwrap();
        let layer = RasterLayer {
            overlay_id: OverlayId::from_u128(1),
            asset_id: asset(1),
            position: point(0, 0),
            size,
            item_opacity: 255,
            track_opacity: 255,
            blend_mode: BlendMode::Normal,
            z_index: 0,
            track_index: 0,
            item_index: 0,
        };
        let cancellation = CancelAfter {
            checks: AtomicUsize::new(0),
            cancel_at: 2,
        };

        assert!(matches!(
            composite_layer(&mut destination, &source, layer, &cancellation),
            Err(RenderError::Cancelled)
        ));
        assert_eq!(cancellation.checks.load(Ordering::Relaxed), 3);
        assert_eq!(&destination.pixels()[0..4], &[255, 255, 255, 255]);
        assert_eq!(
            &destination.pixels()[1_500 * 4..1_500 * 4 + 4],
            &[0, 0, 0, 255]
        );
    }
}
