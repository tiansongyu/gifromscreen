use std::error::Error;

use gif_from_screen_domain::{
    AssetId, ClipTransform, EdgeWidths, Effect, FrameClip, FrameGeometryPlan, FrameRenderStep,
    PhysicalRect, PhysicalSize, QuarterTurn, Rgba, validate_frame_render_steps,
};

use crate::{
    CancellationToken, RenderError, RgbaSurface, UnsupportedEffect, surface::checked_byte_len,
};

/// Largest supported radius for deterministic blur and shadow effects.
///
/// The cap bounds parameter-driven work at region edges and keeps horizontal
/// channel sums representable as `u32` in the region-blur working buffer.
pub const MAX_BLUR_RADIUS: u16 = 256;

const CANCELLATION_PIXEL_INTERVAL: u32 = 1_024;

/// Boxed provider failure retained as the source of [`RenderError::AssetLoad`].
pub type AssetProviderError = Box<dyn Error + Send + Sync + 'static>;

/// Supplies immutable source frames as normalized RGBA8 surfaces.
///
/// Implementations may decode or read lazily, but must return straight-alpha
/// sRGB pixels. The renderer intentionally knows nothing about filesystem or
/// project-store details.
pub trait FrameAssetProvider: Send + Sync {
    /// Loads one immutable frame asset.
    ///
    /// # Errors
    ///
    /// Returns a boxed provider-specific error when the asset is absent,
    /// unreadable, or cannot be decoded to RGBA8.
    fn load_rgba8(&self, asset_id: AssetId) -> Result<RgbaSurface, AssetProviderError>;
}

impl<F> FrameAssetProvider for F
where
    F: Fn(AssetId) -> Result<RgbaSurface, AssetProviderError> + Send + Sync,
{
    fn load_rgba8(&self, asset_id: AssetId) -> Result<RgbaSurface, AssetProviderError> {
        self(asset_id)
    }
}

/// Resource limits applied to each render surface or effect working buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderLimits {
    /// Maximum byte length for a source, intermediate surface, or individual
    /// effect working buffer.
    pub max_surface_bytes: usize,
}

impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            max_surface_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Deterministic CPU reference renderer.
#[derive(Clone, Debug, Default)]
pub struct CpuRenderer {
    limits: RenderLimits,
}

impl CpuRenderer {
    /// Creates a renderer with a 512 MiB per-buffer limit.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a renderer with caller-supplied resource limits.
    pub const fn with_limits(limits: RenderLimits) -> Self {
        Self { limits }
    }

    /// Returns the active resource limits.
    pub const fn limits(&self) -> RenderLimits {
        self.limits
    }

    /// Loads and renders one frame clip.
    ///
    /// Applies the unchanged canonical transform/effect prefix, then ordered
    /// render steps. Composite steps are no-ops here: this entry point never
    /// draws overlays. Empty steps preserve the legacy pixel pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error when loading fails, geometry is invalid, a surface
    /// exceeds the configured limit, cancellation is requested, or the clip
    /// contains an effect not implemented by this milestone.
    pub fn render_clip<P, C>(
        &self,
        clip: &FrameClip,
        provider: &P,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError>
    where
        P: FrameAssetProvider + ?Sized,
        C: CancellationToken + ?Sized,
    {
        check_cancelled(cancellation)?;
        validate_frame_render_steps(&clip.render_steps).map_err(|reason| {
            RenderError::InvalidRenderSteps {
                frame_id: clip.id,
                reason,
            }
        })?;
        let mut surface = self.render_clip_prefix(clip, provider, cancellation)?;
        for step in &clip.render_steps {
            surface = self.apply_render_step(surface, step, cancellation)?;
        }
        Ok(surface)
    }

    pub(crate) fn render_clip_prefix<
        P: FrameAssetProvider + ?Sized,
        C: CancellationToken + ?Sized,
    >(
        &self,
        clip: &FrameClip,
        provider: &P,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError> {
        check_cancelled(cancellation)?;
        let mut surface =
            provider
                .load_rgba8(clip.asset_id)
                .map_err(|source| RenderError::AssetLoad {
                    asset_id: clip.asset_id,
                    source,
                })?;
        self.ensure_within_limit(surface.size())?;
        check_cancelled(cancellation)?;
        if !clip.render_steps.is_empty() {
            FrameGeometryPlan::new(clip, surface.size()).map_err(|reason| {
                RenderError::InvalidRenderSteps {
                    frame_id: clip.id,
                    reason,
                }
            })?;
        }
        surface = self.apply_transform(surface, clip.transform, cancellation)?;
        for effect in &clip.effects {
            check_cancelled(cancellation)?;
            apply_effect(&mut surface, effect, self.limits, cancellation)?;
        }
        Ok(surface)
    }

    /// Applies canonical geometry to an owned copy of a surface, without asset
    /// loading, effects, ordered stages, overlays or captured input metadata.
    ///
    /// # Errors
    /// Rejects invalid crop geometry, source/intermediate surface limits,
    /// allocation failures and cancellation.
    pub fn transform_surface<C: CancellationToken + ?Sized>(
        &self,
        source: &RgbaSurface,
        transform: ClipTransform,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError> {
        check_cancelled(cancellation)?;
        self.ensure_within_limit(source.size())?;
        let mut copy = RgbaSurface::try_zeroed(source.size())?;
        for y in 0..source.height() {
            check_cancelled(cancellation)?;
            let start = source.byte_offset(0, y);
            let end = if y + 1 < source.height() {
                source.byte_offset(0, y + 1)
            } else {
                source.pixels().len()
            };
            copy.pixels_mut()[start..end].copy_from_slice(&source.pixels()[start..end]);
        }
        self.apply_transform(copy, transform, cancellation)
    }

    fn apply_transform<C: CancellationToken + ?Sized>(
        &self,
        mut surface: RgbaSurface,
        transform: ClipTransform,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError> {
        if let Some(crop) = transform.crop {
            surface = self.crop(&surface, crop, cancellation)?;
        }
        if let Some(output_size) = transform.output_size
            && output_size != surface.size()
        {
            surface = self.resize_nearest(&surface, output_size, cancellation)?;
        }
        if transform.rotation != QuarterTurn::Zero {
            surface = self.rotate(&surface, transform.rotation, cancellation)?;
        }
        if transform.flip_horizontal {
            flip_horizontal(&mut surface, cancellation)?;
        }
        if transform.flip_vertical {
            flip_vertical(&mut surface, cancellation)?;
        }

        Ok(surface)
    }

    pub(crate) fn apply_render_step<C: CancellationToken + ?Sized>(
        &self,
        mut surface: RgbaSurface,
        step: &FrameRenderStep,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError> {
        check_cancelled(cancellation)?;
        match step {
            FrameRenderStep::Crop { rect } => surface = self.crop(&surface, *rect, cancellation)?,
            FrameRenderStep::Resize { size } if *size != surface.size() => {
                surface = self.resize_nearest(&surface, *size, cancellation)?;
            }
            FrameRenderStep::Rotate { rotation } if *rotation != QuarterTurn::Zero => {
                surface = self.rotate(&surface, *rotation, cancellation)?;
            }
            FrameRenderStep::FlipHorizontal => flip_horizontal(&mut surface, cancellation)?,
            FrameRenderStep::FlipVertical => flip_vertical(&mut surface, cancellation)?,
            FrameRenderStep::Effect { effect } => {
                apply_effect(&mut surface, effect, self.limits, cancellation)?;
            }
            FrameRenderStep::Composite { .. }
            | FrameRenderStep::Resize { .. }
            | FrameRenderStep::Rotate { .. } => {}
        }
        Ok(surface)
    }

    fn crop<C: CancellationToken + ?Sized>(
        &self,
        source: &RgbaSurface,
        crop: PhysicalRect,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError> {
        if !crop.fits_within(source.size()) {
            return Err(RenderError::InvalidCrop {
                crop,
                source_width: source.width(),
                source_height: source.height(),
            });
        }
        self.ensure_within_limit(crop.size)?;
        let mut destination = RgbaSurface::try_zeroed(crop.size)?;
        let row_bytes = usize::try_from(u64::from(crop.size.width.get()) * 4)
            .expect("validated surface length guarantees a representable row length");

        for destination_y in 0..crop.size.height.get() {
            check_cancelled(cancellation)?;
            let source_y = crop.origin.y.get() + destination_y;
            let source_start = source.byte_offset(crop.origin.x.get(), source_y);
            let destination_start = destination.byte_offset(0, destination_y);
            destination.pixels_mut()[destination_start..destination_start + row_bytes]
                .copy_from_slice(&source.pixels()[source_start..source_start + row_bytes]);
        }
        Ok(destination)
    }

    fn resize_nearest<C: CancellationToken + ?Sized>(
        &self,
        source: &RgbaSurface,
        output_size: PhysicalSize,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError> {
        self.ensure_within_limit(output_size)?;
        let mut destination = RgbaSurface::try_zeroed(output_size)?;
        let source_width = u64::from(source.width());
        let source_height = u64::from(source.height());
        let destination_width = u64::from(destination.width());
        let destination_height = u64::from(destination.height());

        for destination_y in 0..destination.height() {
            check_cancelled(cancellation)?;
            let source_y =
                u32::try_from(u64::from(destination_y) * source_height / destination_height)
                    .expect("nearest-neighbor y coordinate is bounded by source height");
            for destination_x in 0..destination.width() {
                let source_x =
                    u32::try_from(u64::from(destination_x) * source_width / destination_width)
                        .expect("nearest-neighbor x coordinate is bounded by source width");
                copy_pixel(
                    source,
                    source_x,
                    source_y,
                    &mut destination,
                    destination_x,
                    destination_y,
                );
            }
        }
        Ok(destination)
    }

    fn rotate<C: CancellationToken + ?Sized>(
        &self,
        source: &RgbaSurface,
        rotation: QuarterTurn,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError> {
        let output_size = match rotation {
            QuarterTurn::Clockwise90 | QuarterTurn::Clockwise270 => {
                PhysicalSize::new(source.height(), source.width())
                    .expect("swapping valid dimensions remains valid")
            }
            QuarterTurn::Clockwise180 => source.size(),
            QuarterTurn::Zero => unreachable!("zero rotation is skipped by the caller"),
        };
        self.ensure_within_limit(output_size)?;
        let mut destination = RgbaSurface::try_zeroed(output_size)?;

        for source_y in 0..source.height() {
            check_cancelled(cancellation)?;
            for source_x in 0..source.width() {
                let (destination_x, destination_y) = match rotation {
                    QuarterTurn::Clockwise90 => (source.height() - 1 - source_y, source_x),
                    QuarterTurn::Clockwise180 => (
                        source.width() - 1 - source_x,
                        source.height() - 1 - source_y,
                    ),
                    QuarterTurn::Clockwise270 => (source_y, source.width() - 1 - source_x),
                    QuarterTurn::Zero => unreachable!("zero rotation is skipped by the caller"),
                };
                copy_pixel(
                    source,
                    source_x,
                    source_y,
                    &mut destination,
                    destination_x,
                    destination_y,
                );
            }
        }
        Ok(destination)
    }

    fn ensure_within_limit(&self, size: PhysicalSize) -> Result<(), RenderError> {
        let requested = checked_byte_len(size)?;
        if requested > self.limits.max_surface_bytes {
            return Err(RenderError::SurfaceLimitExceeded {
                requested,
                limit: self.limits.max_surface_bytes,
            });
        }
        Ok(())
    }
}

fn copy_pixel(
    source: &RgbaSurface,
    source_x: u32,
    source_y: u32,
    destination: &mut RgbaSurface,
    destination_x: u32,
    destination_y: u32,
) {
    let source_start = source.byte_offset(source_x, source_y);
    let destination_start = destination.byte_offset(destination_x, destination_y);
    destination.pixels_mut()[destination_start..destination_start + 4]
        .copy_from_slice(&source.pixels()[source_start..source_start + 4]);
}

fn flip_horizontal<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    cancellation: &C,
) -> Result<(), RenderError> {
    for y in 0..surface.height() {
        check_cancelled(cancellation)?;
        for left_x in 0..surface.width() / 2 {
            let right_x = surface.width() - 1 - left_x;
            swap_pixels(surface, left_x, y, right_x, y);
        }
    }
    Ok(())
}

fn flip_vertical<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    cancellation: &C,
) -> Result<(), RenderError> {
    for top_y in 0..surface.height() / 2 {
        check_cancelled(cancellation)?;
        let bottom_y = surface.height() - 1 - top_y;
        for x in 0..surface.width() {
            swap_pixels(surface, x, top_y, x, bottom_y);
        }
    }
    Ok(())
}

fn swap_pixels(
    surface: &mut RgbaSurface,
    first_x: u32,
    first_y: u32,
    second_x: u32,
    second_y: u32,
) {
    let first = surface.byte_offset(first_x, first_y);
    let second = surface.byte_offset(second_x, second_y);
    for channel in 0..4 {
        surface.pixels_mut().swap(first + channel, second + channel);
    }
}

fn apply_effect<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    effect: &Effect,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    match effect {
        Effect::Border { widths, color } => apply_border(surface, *widths, *color, cancellation),
        Effect::Pixelate { region, block_size } => {
            validate_region(surface, "pixelate", *region)?;
            if *block_size == 0 {
                return Err(RenderError::InvalidEffectParameter {
                    effect: "pixelate",
                    parameter: "block_size",
                    value: 0,
                });
            }
            apply_pixelate(surface, *region, *block_size, cancellation)
        }
        Effect::Darken {
            region,
            amount_percent,
        } => {
            validate_region(surface, "darken", *region)?;
            validate_percent("darken", *amount_percent)?;
            apply_tone(surface, *region, *amount_percent, false, cancellation)
        }
        Effect::Lighten {
            region,
            amount_percent,
        } => {
            validate_region(surface, "lighten", *region)?;
            validate_percent("lighten", *amount_percent)?;
            apply_tone(surface, *region, *amount_percent, true, cancellation)
        }
        Effect::Blur { region, radius } => {
            validate_region(surface, "blur", *region)?;
            if *radius == 0 || *radius > MAX_BLUR_RADIUS {
                return Err(RenderError::InvalidEffectParameter {
                    effect: "blur",
                    parameter: "radius",
                    value: u64::from(*radius),
                });
            }
            apply_blur(surface, *region, *radius, limits, cancellation)
        }
        Effect::Shadow {
            offset_x,
            offset_y,
            blur_radius,
            color,
        } => {
            if *blur_radius > MAX_BLUR_RADIUS {
                return Err(RenderError::InvalidEffectParameter {
                    effect: "shadow",
                    parameter: "blur_radius",
                    value: u64::from(*blur_radius),
                });
            }
            apply_shadow(
                surface,
                *offset_x,
                *offset_y,
                *blur_radius,
                *color,
                limits,
                cancellation,
            )
        }
        Effect::Cinemagraph { .. } => Err(RenderError::UnsupportedEffect(
            UnsupportedEffect::Cinemagraph,
        )),
    }
}

fn validate_region(
    surface: &RgbaSurface,
    effect: &'static str,
    region: PhysicalRect,
) -> Result<(), RenderError> {
    if region.size.validate().is_err() || !region.fits_within(surface.size()) {
        return Err(RenderError::InvalidEffectRegion {
            effect,
            region,
            surface_width: surface.width(),
            surface_height: surface.height(),
        });
    }
    Ok(())
}

fn apply_blur<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    region: PhysicalRect,
    radius: u16,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    check_cancelled(cancellation)?;
    let mut horizontal_sums = allocate_blur_working_buffer(region, limits)?;
    check_cancelled(cancellation)?;

    let radius = u32::from(radius);
    calculate_horizontal_blur_sums(surface, region, radius, &mut horizontal_sums, cancellation)?;
    write_vertical_blur(surface, region, radius, &horizontal_sums, cancellation)
}

fn allocate_blur_working_buffer(
    region: PhysicalRect,
    limits: RenderLimits,
) -> Result<Vec<[u32; 4]>, RenderError> {
    let region_rgba_bytes = checked_byte_len(region.size)?;
    let pixel_count = region_rgba_bytes / 4;
    let working_bytes = pixel_count
        .checked_mul(std::mem::size_of::<[u32; 4]>())
        .ok_or(RenderError::EffectWorkingMemorySizeOverflow { effect: "blur" })?;
    if working_bytes > limits.max_surface_bytes {
        return Err(RenderError::EffectWorkingMemoryLimitExceeded {
            effect: "blur",
            requested: working_bytes,
            limit: limits.max_surface_bytes,
        });
    }

    let mut horizontal_sums = Vec::<[u32; 4]>::new();
    horizontal_sums
        .try_reserve_exact(pixel_count)
        .map_err(|_| RenderError::EffectWorkingMemoryAllocationFailed {
            effect: "blur",
            requested: working_bytes,
        })?;
    horizontal_sums.resize(pixel_count, [0; 4]);
    Ok(horizontal_sums)
}

fn calculate_horizontal_blur_sums<C: CancellationToken + ?Sized>(
    surface: &RgbaSurface,
    region: PhysicalRect,
    radius: u32,
    horizontal_sums: &mut [[u32; 4]],
    cancellation: &C,
) -> Result<(), RenderError> {
    let width = region.size.width.get();
    let height = region.size.height.get();

    // Store exact horizontal sums rather than rounded horizontal averages. The
    // vertical pass therefore performs only one rounding step for the complete
    // two-dimensional box kernel.
    for local_y in 0..height {
        check_cancelled(cancellation)?;
        let source_y = region.origin.y.get() + local_y;
        let mut sums = [0_u64; 4];
        for offset in -(i64::from(radius))..=i64::from(radius) {
            let local_x = clamp_region_coordinate(offset, width);
            add_channels(
                &mut sums,
                alpha_weighted_pixel(surface, region.origin.x.get() + local_x, source_y),
            );
        }

        for local_x in 0..width {
            if local_x != 0 && local_x % CANCELLATION_PIXEL_INTERVAL == 0 {
                check_cancelled(cancellation)?;
            }
            let index = region_pixel_index(local_x, local_y, width);
            horizontal_sums[index] = sums.map(|sum| {
                u32::try_from(sum).expect("the blur radius cap keeps horizontal sums representable")
            });

            if local_x + 1 < width {
                let leaving_x =
                    clamp_region_coordinate(i64::from(local_x) - i64::from(radius), width);
                let entering_x =
                    clamp_region_coordinate(i64::from(local_x) + i64::from(radius) + 1, width);
                subtract_channels(
                    &mut sums,
                    alpha_weighted_pixel(surface, region.origin.x.get() + leaving_x, source_y),
                );
                add_channels(
                    &mut sums,
                    alpha_weighted_pixel(surface, region.origin.x.get() + entering_x, source_y),
                );
            }
        }
    }
    Ok(())
}

fn write_vertical_blur<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    region: PhysicalRect,
    radius: u32,
    horizontal_sums: &[[u32; 4]],
    cancellation: &C,
) -> Result<(), RenderError> {
    let width = region.size.width.get();
    let height = region.size.height.get();
    let kernel_width = u64::from(radius) * 2 + 1;
    let sample_count = kernel_width * kernel_width;
    for local_x in 0..width {
        check_cancelled(cancellation)?;
        let mut sums = [0_u64; 4];
        for offset in -(i64::from(radius))..=i64::from(radius) {
            let local_y = clamp_region_coordinate(offset, height);
            add_channels(
                &mut sums,
                horizontal_sums[region_pixel_index(local_x, local_y, width)].map(u64::from),
            );
        }

        for local_y in 0..height {
            if local_y != 0 && local_y % CANCELLATION_PIXEL_INTERVAL == 0 {
                check_cancelled(cancellation)?;
            }
            write_alpha_weighted_average(
                surface,
                region.origin.x.get() + local_x,
                region.origin.y.get() + local_y,
                sums,
                sample_count,
            );

            if local_y + 1 < height {
                let leaving_y =
                    clamp_region_coordinate(i64::from(local_y) - i64::from(radius), height);
                let entering_y =
                    clamp_region_coordinate(i64::from(local_y) + i64::from(radius) + 1, height);
                subtract_channels(
                    &mut sums,
                    horizontal_sums[region_pixel_index(local_x, leaving_y, width)].map(u64::from),
                );
                add_channels(
                    &mut sums,
                    horizontal_sums[region_pixel_index(local_x, entering_y, width)].map(u64::from),
                );
            }
        }
    }
    Ok(())
}

fn clamp_region_coordinate(position: i64, length: u32) -> u32 {
    debug_assert!(length > 0);
    u32::try_from(position.clamp(0, i64::from(length) - 1))
        .expect("a coordinate clamped to a u32 region remains representable")
}

fn region_pixel_index(x: u32, y: u32, width: u32) -> usize {
    usize::try_from(u64::from(y) * u64::from(width) + u64::from(x))
        .expect("the validated blur working-buffer length makes its index representable")
}

fn alpha_weighted_pixel(surface: &RgbaSurface, x: u32, y: u32) -> [u64; 4] {
    let offset = surface.byte_offset(x, y);
    let pixel = &surface.pixels()[offset..offset + 4];
    let alpha = u64::from(pixel[3]);
    [
        u64::from(pixel[0]) * alpha,
        u64::from(pixel[1]) * alpha,
        u64::from(pixel[2]) * alpha,
        alpha,
    ]
}

fn add_channels(sums: &mut [u64; 4], channels: [u64; 4]) {
    for (sum, channel) in sums.iter_mut().zip(channels) {
        *sum += channel;
    }
}

fn subtract_channels(sums: &mut [u64; 4], channels: [u64; 4]) {
    for (sum, channel) in sums.iter_mut().zip(channels) {
        *sum -= channel;
    }
}

fn write_alpha_weighted_average(
    surface: &mut RgbaSurface,
    x: u32,
    y: u32,
    sums: [u64; 4],
    sample_count: u64,
) {
    let alpha_sum = sums[3];
    let alpha = (alpha_sum + sample_count / 2) / sample_count;
    let offset = surface.byte_offset(x, y);
    let destination = &mut surface.pixels_mut()[offset..offset + 4];

    if alpha_sum == 0 {
        // Straight-alpha pixels with zero alpha have no visible color. Clearing
        // hidden RGB makes future effects deterministic and prevents color bleed.
        destination.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }

    for channel in 0..3 {
        destination[channel] = u8::try_from((sums[channel] + alpha_sum / 2) / alpha_sum)
            .expect("an alpha-weighted average of u8 colors remains an u8");
    }
    destination[3] = u8::try_from(alpha).expect("an average of u8 alpha remains an u8");
}

/// Applies a drop shadow without expanding the surface.
///
/// Conceptually the current alpha mask is translated on an infinite transparent
/// plane, blurred with a `(2 * radius + 1)` square box kernel, and then clipped
/// back to the existing canvas. The shadow color's alpha scales the blurred
/// mask. Finally, each original straight-alpha pixel is composited source-over
/// the shadow, so the shadow can only show through behind transparent content.
fn apply_shadow<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    offset_x: i32,
    offset_y: i32,
    radius: u16,
    color: Rgba,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    check_cancelled(cancellation)?;
    let (mut alpha_integral, stride) = allocate_shadow_integral(surface.size(), limits)?;
    check_cancelled(cancellation)?;
    build_alpha_integral(surface, &mut alpha_integral, stride, cancellation)?;
    check_cancelled(cancellation)?;

    let radius = i64::from(radius);
    let kernel_width = u64::try_from(radius * 2 + 1).expect("a u16 radius has a positive kernel");
    let sample_count = kernel_width * kernel_width;
    let width = i64::from(surface.width());
    let height = i64::from(surface.height());
    let offset_x = i64::from(offset_x);
    let offset_y = i64::from(offset_y);

    for destination_y in 0..surface.height() {
        check_cancelled(cancellation)?;
        for destination_x in 0..surface.width() {
            if destination_x != 0 && destination_x % CANCELLATION_PIXEL_INTERVAL == 0 {
                check_cancelled(cancellation)?;
            }

            // Sampling the translated mask at the destination is equivalent to
            // sampling the original mask at destination - offset. i64 covers
            // every u32 canvas coordinate, i32 offset, and supported radius.
            let center_x = i64::from(destination_x) - offset_x;
            let center_y = i64::from(destination_y) - offset_y;
            let left = (center_x - radius).clamp(0, width);
            let right = (center_x + radius + 1).clamp(0, width);
            let top = (center_y - radius).clamp(0, height);
            let bottom = (center_y + radius + 1).clamp(0, height);
            let alpha_sum =
                integral_rectangle_sum(&alpha_integral, stride, left, top, right, bottom);
            let blurred_alpha = (alpha_sum + sample_count / 2) / sample_count;
            let shadow_alpha = (blurred_alpha * u64::from(color.alpha) + 127) / u64::from(u8::MAX);
            let shadow_alpha =
                u8::try_from(shadow_alpha).expect("scaling an alpha mask remains in u8 range");
            composite_original_over_shadow(
                surface,
                destination_x,
                destination_y,
                color,
                shadow_alpha,
            );
        }
    }
    Ok(())
}

fn allocate_shadow_integral(
    size: PhysicalSize,
    limits: RenderLimits,
) -> Result<(Vec<u64>, usize), RenderError> {
    let source_pixels = size
        .area()
        .expect("a validated surface has a representable pixel count");
    if source_pixels.checked_mul(u64::from(u8::MAX)).is_none() {
        return Err(RenderError::EffectWorkingMemorySizeOverflow { effect: "shadow" });
    }

    let stride_u64 = u64::from(size.width.get()) + 1;
    let rows_u64 = u64::from(size.height.get()) + 1;
    let entry_count_u64 = stride_u64
        .checked_mul(rows_u64)
        .ok_or(RenderError::EffectWorkingMemorySizeOverflow { effect: "shadow" })?;
    let entry_count = usize::try_from(entry_count_u64)
        .map_err(|_| RenderError::EffectWorkingMemorySizeOverflow { effect: "shadow" })?;
    let working_bytes = entry_count
        .checked_mul(std::mem::size_of::<u64>())
        .ok_or(RenderError::EffectWorkingMemorySizeOverflow { effect: "shadow" })?;
    if working_bytes > limits.max_surface_bytes {
        return Err(RenderError::EffectWorkingMemoryLimitExceeded {
            effect: "shadow",
            requested: working_bytes,
            limit: limits.max_surface_bytes,
        });
    }

    let mut alpha_integral = Vec::new();
    alpha_integral.try_reserve_exact(entry_count).map_err(|_| {
        RenderError::EffectWorkingMemoryAllocationFailed {
            effect: "shadow",
            requested: working_bytes,
        }
    })?;
    alpha_integral.resize(entry_count, 0_u64);
    let stride = usize::try_from(stride_u64)
        .expect("a representable integral-buffer length has a representable stride");
    Ok((alpha_integral, stride))
}

fn build_alpha_integral<C: CancellationToken + ?Sized>(
    surface: &RgbaSurface,
    alpha_integral: &mut [u64],
    stride: usize,
    cancellation: &C,
) -> Result<(), RenderError> {
    for y in 0..surface.height() {
        check_cancelled(cancellation)?;
        let integral_row = usize::try_from(u64::from(y + 1))
            .expect("a surface row remains representable")
            * stride;
        let previous_row =
            usize::try_from(u64::from(y)).expect("a surface row remains representable") * stride;
        let mut row_sum = 0_u64;
        for x in 0..surface.width() {
            if x != 0 && x % CANCELLATION_PIXEL_INTERVAL == 0 {
                check_cancelled(cancellation)?;
            }
            row_sum += u64::from(surface.pixels()[surface.byte_offset(x, y) + 3]);
            let column =
                usize::try_from(u64::from(x + 1)).expect("a surface column remains representable");
            alpha_integral[integral_row + column] = alpha_integral[previous_row + column] + row_sum;
        }
    }
    Ok(())
}

fn integral_rectangle_sum(
    integral: &[u64],
    stride: usize,
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
) -> u64 {
    if left >= right || top >= bottom {
        return 0;
    }

    let left = usize::try_from(left).expect("clamped coordinates are non-negative");
    let top = usize::try_from(top).expect("clamped coordinates are non-negative");
    let right = usize::try_from(right).expect("clamped coordinates are non-negative");
    let bottom = usize::try_from(bottom).expect("clamped coordinates are non-negative");
    let right_band = integral[bottom * stride + right] - integral[top * stride + right];
    let left_band = integral[bottom * stride + left] - integral[top * stride + left];
    right_band - left_band
}

fn composite_original_over_shadow(
    surface: &mut RgbaSurface,
    x: u32,
    y: u32,
    shadow_color: Rgba,
    shadow_alpha: u8,
) {
    let offset = surface.byte_offset(x, y);
    let destination = &mut surface.pixels_mut()[offset..offset + 4];
    let source = [
        destination[0],
        destination[1],
        destination[2],
        destination[3],
    ];
    let source_alpha = u32::from(source[3]);
    let destination_alpha = u32::from(shadow_alpha);
    let inverse_source_alpha = 255 - source_alpha;
    let output_alpha_numerator = source_alpha * 255 + destination_alpha * inverse_source_alpha;
    if output_alpha_numerator == 0 {
        destination.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }

    let shadow_channels = [shadow_color.red, shadow_color.green, shadow_color.blue];
    for channel in 0..3 {
        let premultiplied_numerator = u32::from(source[channel]) * source_alpha * 255
            + u32::from(shadow_channels[channel]) * destination_alpha * inverse_source_alpha;
        destination[channel] = u8::try_from(
            (premultiplied_numerator + output_alpha_numerator / 2) / output_alpha_numerator,
        )
        .expect("source-over color remains in u8 range");
    }
    destination[3] = u8::try_from((output_alpha_numerator + 127) / 255)
        .expect("source-over alpha remains in u8 range");
}

fn validate_percent(effect: &'static str, amount: u8) -> Result<(), RenderError> {
    if amount > 100 {
        return Err(RenderError::InvalidEffectParameter {
            effect,
            parameter: "amount_percent",
            value: u64::from(amount),
        });
    }
    Ok(())
}

fn apply_border<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    widths: EdgeWidths,
    color: Rgba,
    cancellation: &C,
) -> Result<(), RenderError> {
    let top = u32::from(widths.top).min(surface.height());
    let right = u32::from(widths.right).min(surface.width());
    let bottom = u32::from(widths.bottom).min(surface.height());
    let left = u32::from(widths.left).min(surface.width());
    let right_start = surface.width() - right;
    let bottom_start = surface.height() - bottom;

    for y in 0..surface.height() {
        check_cancelled(cancellation)?;
        for x in 0..surface.width() {
            if y < top || y >= bottom_start || x < left || x >= right_start {
                blend_pixel(surface, x, y, color);
            }
        }
    }
    Ok(())
}

fn apply_pixelate<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    region: PhysicalRect,
    block_size: u16,
    cancellation: &C,
) -> Result<(), RenderError> {
    let end_x = region
        .end_x()
        .expect("a validated effect region has a finite x boundary");
    let end_y = region
        .end_y()
        .expect("a validated effect region has a finite y boundary");
    let block_size = u32::from(block_size);
    let mut block_y = region.origin.y.get();

    while block_y < end_y {
        check_cancelled(cancellation)?;
        let next_y = block_y.saturating_add(block_size).min(end_y);
        let mut block_x = region.origin.x.get();
        while block_x < end_x {
            let next_x = block_x.saturating_add(block_size).min(end_x);
            let mut sums = [0_u64; 4];
            let count = u64::from(next_x - block_x) * u64::from(next_y - block_y);

            for y in block_y..next_y {
                for x in block_x..next_x {
                    let offset = surface.byte_offset(x, y);
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum += u64::from(surface.pixels()[offset + channel]);
                    }
                }
            }
            let average = sums.map(|sum| {
                u8::try_from((sum + count / 2) / count)
                    .expect("the average of u8 channels remains a u8")
            });
            for y in block_y..next_y {
                for x in block_x..next_x {
                    let offset = surface.byte_offset(x, y);
                    surface.pixels_mut()[offset..offset + 4].copy_from_slice(&average);
                }
            }
            block_x = next_x;
        }
        block_y = next_y;
    }
    Ok(())
}

fn apply_tone<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    region: PhysicalRect,
    amount_percent: u8,
    lighten: bool,
    cancellation: &C,
) -> Result<(), RenderError> {
    let end_x = region
        .end_x()
        .expect("a validated effect region has a finite x boundary");
    let end_y = region
        .end_y()
        .expect("a validated effect region has a finite y boundary");
    let amount = u16::from(amount_percent);

    for y in region.origin.y.get()..end_y {
        check_cancelled(cancellation)?;
        for x in region.origin.x.get()..end_x {
            let offset = surface.byte_offset(x, y);
            for channel in &mut surface.pixels_mut()[offset..offset + 3] {
                let value = u16::from(*channel);
                let adjusted = if lighten {
                    value + ((255 - value) * amount + 50) / 100
                } else {
                    (value * (100 - amount) + 50) / 100
                };
                *channel = u8::try_from(adjusted).expect("tone adjustment remains in u8 range");
            }
        }
    }
    Ok(())
}

fn blend_pixel(surface: &mut RgbaSurface, x: u32, y: u32, source: Rgba) {
    let offset = surface.byte_offset(x, y);
    let destination = &mut surface.pixels_mut()[offset..offset + 4];
    let source_alpha = u32::from(source.alpha);
    if source_alpha == 0 {
        return;
    }
    if source_alpha == 255 {
        destination.copy_from_slice(&[source.red, source.green, source.blue, source.alpha]);
        return;
    }

    let destination_alpha = u32::from(destination[3]);
    let inverse_source_alpha = 255 - source_alpha;
    let output_alpha_numerator = source_alpha * 255 + destination_alpha * inverse_source_alpha;
    let output_alpha = (output_alpha_numerator + 127) / 255;
    for channel in 0..3 {
        let source_channel = u32::from([source.red, source.green, source.blue][channel]);
        let destination_channel = u32::from(destination[channel]);
        let premultiplied_numerator = source_channel * source_alpha * 255
            + destination_channel * destination_alpha * inverse_source_alpha;
        destination[channel] = u8::try_from(
            (premultiplied_numerator + output_alpha_numerator / 2) / output_alpha_numerator,
        )
        .expect("source-over color remains in u8 range");
    }
    destination[3] = u8::try_from(output_alpha).expect("source-over alpha remains in u8 range");
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gif_from_screen_domain::{
        CaptureMetadata, ClipTransform, DurationUs, FrameId, PhysicalPoint, PhysicalPx,
    };

    use super::*;

    const ASSET_ID: AssetId = AssetId::from_digest([7; 32]);

    fn labelled_surface(width: u32, height: u32) -> RgbaSurface {
        let size = PhysicalSize::new(width, height).unwrap();
        let pixels = (1..=width * height)
            .flat_map(|label| [u8::try_from(label).unwrap(), 0, 0, 255])
            .collect();
        RgbaSurface::new(size, pixels).unwrap()
    }

    fn opaque_red_surface(width: u32, height: u32, red: &[u8]) -> RgbaSurface {
        let pixels = red
            .iter()
            .flat_map(|channel| [*channel, 0, 0, 255])
            .collect();
        RgbaSurface::new(PhysicalSize::new(width, height).unwrap(), pixels).unwrap()
    }

    fn clip(transform: ClipTransform, effects: Vec<Effect>) -> FrameClip {
        FrameClip {
            render_steps: Vec::new(),
            capture_clock: None,
            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
            id: FrameId::from_u128(1),
            asset_id: ASSET_ID,
            duration: DurationUs::new(100_000).unwrap(),
            transform,
            capture_metadata: CaptureMetadata::default(),
            effects,
        }
    }

    fn provider(surface: RgbaSurface) -> impl FrameAssetProvider {
        move |asset_id| {
            assert_eq!(asset_id, ASSET_ID);
            Ok(surface.clone())
        }
    }

    fn red_matrix(surface: &RgbaSurface) -> Vec<Vec<u8>> {
        (0..surface.height())
            .map(|y| {
                (0..surface.width())
                    .map(|x| surface.pixels()[surface.byte_offset(x, y)])
                    .collect()
            })
            .collect()
    }

    #[test]
    fn golden_transform_order_is_crop_resize_rotate_then_flips() {
        let transform = ClipTransform {
            crop: Some(PhysicalRect::new(1, 0, 2, 3).unwrap()),
            output_size: Some(PhysicalSize::new(4, 2).unwrap()),
            rotation: QuarterTurn::Clockwise90,
            flip_horizontal: true,
            flip_vertical: true,
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(transform, Vec::new()),
                &provider(labelled_surface(4, 3)),
                &crate::NeverCancel,
            )
            .unwrap();

        assert_eq!(rendered.size(), PhysicalSize::new(2, 4).unwrap());
        assert_eq!(
            red_matrix(&rendered),
            vec![vec![3, 7], vec![3, 7], vec![2, 6], vec![2, 6]]
        );
    }

    #[test]
    fn golden_effect_stack_applies_border_pixelate_and_tones_in_order() {
        let size = PhysicalSize::new(4, 3).unwrap();
        let source = RgbaSurface::new(
            size,
            vec![
                10, 20, 30, 255, 20, 40, 60, 255, 30, 60, 90, 255, 40, 80, 120, 255, 50, 100, 150,
                255, 60, 120, 180, 255, 70, 140, 210, 255, 80, 160, 240, 255, 90, 180, 15, 255,
                100, 200, 45, 255, 110, 220, 75, 255, 120, 240, 105, 255,
            ],
        )
        .unwrap();
        let middle = PhysicalRect::new(1, 1, 2, 2).unwrap();
        let effects = vec![
            Effect::Pixelate {
                region: middle,
                block_size: 2,
            },
            Effect::Darken {
                region: PhysicalRect::new(1, 1, 1, 2).unwrap(),
                amount_percent: 50,
            },
            Effect::Lighten {
                region: PhysicalRect::new(2, 1, 1, 2).unwrap(),
                amount_percent: 50,
            },
            Effect::Border {
                widths: EdgeWidths {
                    top: 1,
                    right: 1,
                    bottom: 0,
                    left: 0,
                },
                color: Rgba {
                    red: 1,
                    green: 2,
                    blue: 3,
                    alpha: 255,
                },
            },
        ];
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), effects),
                &provider(source),
                &crate::NeverCancel,
            )
            .unwrap();

        assert_eq!(
            rendered.pixels(),
            &[
                1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255, 1, 2, 3, 255, 50, 100, 150, 255, 43, 85,
                64, 255, 170, 213, 192, 255, 1, 2, 3, 255, 90, 180, 15, 255, 43, 85, 64, 255, 170,
                213, 192, 255, 1, 2, 3, 255,
            ]
        );
    }

    #[test]
    fn golden_blur_is_edge_clamped_to_its_region() {
        let source = opaque_red_surface(
            5,
            3,
            &[200, 0, 0, 0, 201, 202, 0, 90, 0, 203, 204, 0, 0, 0, 205],
        );
        let effect = Effect::Blur {
            region: PhysicalRect::new(1, 0, 3, 3).unwrap(),
            radius: 1,
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(source),
                &crate::NeverCancel,
            )
            .unwrap();

        assert_eq!(
            red_matrix(&rendered),
            vec![
                vec![200, 10, 10, 10, 201],
                vec![202, 10, 10, 10, 203],
                vec![204, 10, 10, 10, 205],
            ]
        );
    }

    #[test]
    fn blur_uses_alpha_weighted_colors_and_canonicalizes_transparency() {
        let source = RgbaSurface::new(
            PhysicalSize::new(3, 1).unwrap(),
            vec![250, 1, 2, 0, 0, 0, 255, 255, 3, 250, 4, 0],
        )
        .unwrap();
        let effect = Effect::Blur {
            region: PhysicalRect::new(0, 0, 3, 1).unwrap(),
            radius: 1,
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(source),
                &crate::NeverCancel,
            )
            .unwrap();
        assert_eq!(
            rendered.pixels(),
            &[0, 0, 255, 85, 0, 0, 255, 85, 0, 0, 255, 85]
        );

        let transparent =
            RgbaSurface::new(PhysicalSize::new(1, 1).unwrap(), vec![255, 100, 50, 0]).unwrap();
        let effect = Effect::Blur {
            region: PhysicalRect::new(0, 0, 1, 1).unwrap(),
            radius: 1,
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(transparent),
                &crate::NeverCancel,
            )
            .unwrap();
        assert_eq!(rendered.pixels(), &[0, 0, 0, 0]);
    }

    #[test]
    fn blur_accepts_the_documented_maximum_radius() {
        let effect = Effect::Blur {
            region: PhysicalRect::new(0, 0, 1, 1).unwrap(),
            radius: MAX_BLUR_RADIUS,
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(opaque_red_surface(1, 1, &[73])),
                &crate::NeverCancel,
            )
            .unwrap();
        assert_eq!(rendered.pixels(), &[73, 0, 0, 255]);
    }

    #[test]
    fn golden_shadow_translates_blurs_and_stays_on_the_existing_canvas() {
        let source = RgbaSurface::new(
            PhysicalSize::new(4, 3).unwrap(),
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 200, 100, 50, 255, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        )
        .unwrap();
        let effect = Effect::Shadow {
            offset_x: 1,
            offset_y: 0,
            blur_radius: 1,
            color: Rgba {
                red: 10,
                green: 20,
                blue: 30,
                alpha: 180,
            },
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(source),
                &crate::NeverCancel,
            )
            .unwrap();

        assert_eq!(rendered.size(), PhysicalSize::new(4, 3).unwrap());
        assert_eq!(
            rendered.pixels(),
            &[
                0, 0, 0, 0, 10, 20, 30, 20, 10, 20, 30, 20, 10, 20, 30, 20, 0, 0, 0, 0, 200, 100,
                50, 255, 10, 20, 30, 20, 10, 20, 30, 20, 0, 0, 0, 0, 10, 20, 30, 20, 10, 20, 30,
                20, 10, 20, 30, 20,
            ]
        );
    }

    #[test]
    fn shadow_blurs_on_an_infinite_transparent_plane_before_edge_clipping() {
        let source = RgbaSurface::new(
            PhysicalSize::new(3, 1).unwrap(),
            vec![100, 110, 120, 255, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        let effect = Effect::Shadow {
            offset_x: -1,
            offset_y: 0,
            blur_radius: 2,
            color: Rgba {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 255,
            },
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(source),
                &crate::NeverCancel,
            )
            .unwrap();

        // The translated mask pixel is centered just outside x = 0. Its blur
        // halo still reaches x = 1 before the final result is clipped.
        assert_eq!(
            rendered.pixels(),
            &[100, 110, 120, 255, 1, 2, 3, 10, 0, 0, 0, 0]
        );
    }

    #[test]
    fn hard_shadow_uses_alpha_mask_and_straight_alpha_source_over() {
        let source = RgbaSurface::new(
            PhysicalSize::new(2, 1).unwrap(),
            vec![200, 100, 50, 128, 250, 1, 2, 0],
        )
        .unwrap();
        let effect = Effect::Shadow {
            offset_x: 1,
            offset_y: 0,
            blur_radius: 0,
            color: Rgba {
                red: 20,
                green: 40,
                blue: 60,
                alpha: 128,
            },
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(source),
                &crate::NeverCancel,
            )
            .unwrap();
        assert_eq!(rendered.pixels(), &[200, 100, 50, 128, 20, 40, 60, 64]);

        let overlap =
            RgbaSurface::new(PhysicalSize::new(1, 1).unwrap(), vec![200, 100, 50, 128]).unwrap();
        let effect = Effect::Shadow {
            offset_x: 0,
            offset_y: 0,
            blur_radius: 0,
            color: Rgba {
                red: 20,
                green: 40,
                blue: 60,
                alpha: 128,
            },
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(overlap),
                &crate::NeverCancel,
            )
            .unwrap();
        assert_eq!(rendered.pixels(), &[164, 88, 52, 160]);
    }

    #[test]
    fn shadow_canonicalizes_fully_transparent_composition() {
        let source =
            RgbaSurface::new(PhysicalSize::new(1, 1).unwrap(), vec![250, 100, 50, 0]).unwrap();
        let effect = Effect::Shadow {
            offset_x: 0,
            offset_y: 0,
            blur_radius: 0,
            color: Rgba {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 255,
            },
        };
        let rendered = CpuRenderer::new()
            .render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(source),
                &crate::NeverCancel,
            )
            .unwrap();
        assert_eq!(rendered.pixels(), &[0, 0, 0, 0]);
    }

    #[test]
    fn shadow_offsets_cover_the_full_i32_range_without_overflow() {
        for (offset_x, offset_y) in [(i32::MIN, i32::MAX), (i32::MAX, i32::MIN)] {
            let effect = Effect::Shadow {
                offset_x,
                offset_y,
                blur_radius: MAX_BLUR_RADIUS,
                color: Rgba {
                    red: 1,
                    green: 2,
                    blue: 3,
                    alpha: 255,
                },
            };
            let rendered = CpuRenderer::new()
                .render_clip(
                    &clip(ClipTransform::default(), vec![effect]),
                    &provider(opaque_red_surface(1, 1, &[73])),
                    &crate::NeverCancel,
                )
                .unwrap();
            assert_eq!(rendered.pixels(), &[73, 0, 0, 255]);
        }
    }

    #[test]
    fn invalid_geometry_and_effect_parameters_are_rejected() {
        let bad_crop = ClipTransform {
            crop: Some(PhysicalRect::new(3, 0, 2, 1).unwrap()),
            ..ClipTransform::default()
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(bad_crop, Vec::new()),
                &provider(labelled_surface(4, 3)),
                &crate::NeverCancel
            ),
            Err(RenderError::InvalidCrop { .. })
        ));

        let bad_pixelate = Effect::Pixelate {
            region: PhysicalRect::new(0, 0, 1, 1).unwrap(),
            block_size: 0,
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(ClipTransform::default(), vec![bad_pixelate]),
                &provider(labelled_surface(4, 3)),
                &crate::NeverCancel
            ),
            Err(RenderError::InvalidEffectParameter {
                effect: "pixelate",
                ..
            })
        ));

        for invalid_radius in [0, MAX_BLUR_RADIUS + 1] {
            let bad_blur = Effect::Blur {
                region: PhysicalRect::new(0, 0, 1, 1).unwrap(),
                radius: invalid_radius,
            };
            assert!(matches!(
                CpuRenderer::new().render_clip(
                    &clip(ClipTransform::default(), vec![bad_blur]),
                    &provider(labelled_surface(1, 1)),
                    &crate::NeverCancel
                ),
                Err(RenderError::InvalidEffectParameter {
                    effect: "blur",
                    parameter: "radius",
                    value,
                }) if value == u64::from(invalid_radius)
            ));
        }

        let invalid_shadow = Effect::Shadow {
            offset_x: 0,
            offset_y: 0,
            blur_radius: MAX_BLUR_RADIUS + 1,
            color: Rgba::TRANSPARENT,
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(ClipTransform::default(), vec![invalid_shadow]),
                &provider(labelled_surface(1, 1)),
                &crate::NeverCancel
            ),
            Err(RenderError::InvalidEffectParameter {
                effect: "shadow",
                parameter: "blur_radius",
                value,
            }) if value == u64::from(MAX_BLUR_RADIUS + 1)
        ));
    }

    #[test]
    fn unsupported_effect_is_explicit() {
        let effect = Effect::Cinemagraph {
            mask_asset: AssetId::from_digest([8; 32]),
            invert_mask: false,
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(labelled_surface(1, 1)),
                &crate::NeverCancel
            ),
            Err(RenderError::UnsupportedEffect(
                UnsupportedEffect::Cinemagraph
            ))
        ));
    }

    struct CancelAfterChecks(AtomicUsize, usize);

    impl CancellationToken for CancelAfterChecks {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed) >= self.1
        }
    }

    #[test]
    fn cancellation_is_observed_inside_row_processing() {
        let cancellation = CancelAfterChecks(AtomicUsize::new(0), 2);
        let transform = ClipTransform {
            output_size: Some(PhysicalSize::new(8, 8).unwrap()),
            ..ClipTransform::default()
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(transform, Vec::new()),
                &provider(labelled_surface(2, 2)),
                &cancellation
            ),
            Err(RenderError::Cancelled)
        ));
    }

    #[test]
    fn cancellation_is_observed_during_blur_row_processing() {
        // Five checks occur before the first horizontal row finishes. Delaying
        // cancellation until check six proves the blur loops observe the token.
        let cancellation = CancelAfterChecks(AtomicUsize::new(0), 6);
        let effect = Effect::Blur {
            region: PhysicalRect::new(0, 0, 8, 8).unwrap(),
            radius: 2,
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(labelled_surface(8, 8)),
                &cancellation
            ),
            Err(RenderError::Cancelled)
        ));
    }

    #[test]
    fn cancellation_is_observed_during_shadow_mask_processing() {
        // Checks zero through five reach the first integral-image row. Check six
        // fires on the second row, after the shadow working buffer was allocated.
        let cancellation = CancelAfterChecks(AtomicUsize::new(0), 6);
        let effect = Effect::Shadow {
            offset_x: 1,
            offset_y: 1,
            blur_radius: 2,
            color: Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 128,
            },
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(labelled_surface(8, 8)),
                &cancellation
            ),
            Err(RenderError::Cancelled)
        ));
    }

    #[test]
    fn cancellation_is_observed_during_shadow_compositing() {
        // Integral construction consumes checks five through twelve; check 13
        // is the boundary before composition and check 15 reaches its row loop.
        let cancellation = CancelAfterChecks(AtomicUsize::new(0), 15);
        let effect = Effect::Shadow {
            offset_x: 1,
            offset_y: 1,
            blur_radius: 2,
            color: Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 128,
            },
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(labelled_surface(8, 8)),
                &cancellation
            ),
            Err(RenderError::Cancelled)
        ));
    }

    #[test]
    fn blur_working_memory_is_limited_before_allocation() {
        let renderer = CpuRenderer::with_limits(RenderLimits {
            max_surface_bytes: 32,
        });
        let effect = Effect::Blur {
            region: PhysicalRect::new(0, 0, 2, 2).unwrap(),
            radius: 1,
        };
        assert!(matches!(
            renderer.render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(labelled_surface(2, 2)),
                &crate::NeverCancel
            ),
            Err(RenderError::EffectWorkingMemoryLimitExceeded {
                effect: "blur",
                requested: 64,
                limit: 32,
            })
        ));
    }

    #[test]
    fn shadow_working_memory_is_limited_before_allocation() {
        let renderer = CpuRenderer::with_limits(RenderLimits {
            max_surface_bytes: 64,
        });
        let effect = Effect::Shadow {
            offset_x: 1,
            offset_y: 1,
            blur_radius: 1,
            color: Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 128,
            },
        };
        assert!(matches!(
            renderer.render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(labelled_surface(2, 2)),
                &crate::NeverCancel
            ),
            Err(RenderError::EffectWorkingMemoryLimitExceeded {
                effect: "shadow",
                requested: 72,
                limit: 64,
            })
        ));
    }

    #[test]
    fn surface_limit_is_checked_before_allocating_intermediate() {
        let renderer = CpuRenderer::with_limits(RenderLimits {
            max_surface_bytes: 64,
        });
        let transform = ClipTransform {
            output_size: Some(PhysicalSize::new(5, 5).unwrap()),
            ..ClipTransform::default()
        };
        assert!(matches!(
            renderer.render_clip(
                &clip(transform, Vec::new()),
                &provider(labelled_surface(2, 2)),
                &crate::NeverCancel
            ),
            Err(RenderError::SurfaceLimitExceeded {
                requested: 100,
                limit: 64
            })
        ));
    }

    #[test]
    fn overflowing_effect_region_is_rejected_without_panicking() {
        let invalid_region = PhysicalRect {
            origin: PhysicalPoint {
                x: PhysicalPx::new(u32::MAX),
                y: PhysicalPx::ZERO,
            },
            size: PhysicalSize::new(1, 1).unwrap(),
        };
        let effect = Effect::Darken {
            region: invalid_region,
            amount_percent: 20,
        };
        assert!(matches!(
            CpuRenderer::new().render_clip(
                &clip(ClipTransform::default(), vec![effect]),
                &provider(labelled_surface(1, 1)),
                &crate::NeverCancel
            ),
            Err(RenderError::InvalidEffectRegion {
                effect: "darken",
                ..
            })
        ));
    }

    #[test]
    fn blur_rejects_empty_and_out_of_bounds_regions() {
        let empty_region = PhysicalRect {
            origin: PhysicalPoint::default(),
            size: PhysicalSize {
                width: PhysicalPx::ZERO,
                height: PhysicalPx::new(1),
            },
        };
        for region in [
            empty_region,
            PhysicalRect::new(1, 0, 1, 1).expect("the rectangle itself is valid"),
        ] {
            let effect = Effect::Blur { region, radius: 1 };
            assert!(matches!(
                CpuRenderer::new().render_clip(
                    &clip(ClipTransform::default(), vec![effect]),
                    &provider(labelled_surface(1, 1)),
                    &crate::NeverCancel
                ),
                Err(RenderError::InvalidEffectRegion { effect: "blur", .. })
            ));
        }
    }
}
