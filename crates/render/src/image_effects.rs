//! Expanding image effects are separate from the unchanged legacy effects.
//!
//! Geometry/paint order follows `ScreenToGif` a4d0a67 `Editor.BorderAsync` and
//! `ShadowAsync` at 96 DPI. Shadow follows dotnet/wpf v9.0.0 a04736ac software
//! `DropShadowEffect.cpp` / `BlurEffect.cpp`, not the GPU Quality path or box blur.
//! We have not established bit equality against Windows RenderTargetBitmap/WIC.
//! Fractional axis-aligned strokes use deterministic pixel-area coverage, rather
//! than claiming WPF rasterizer-specific antialiasing or DPI rounding parity.

use gif_from_screen_domain::{
    CanvasPlacement, ImageBorderStyle, ImageShadowStyle, PhysicalSize, Rgba,
};

use crate::{
    CancellationToken, RenderError, RenderLimits, RgbaSurface, surface::checked_byte_len,
    wpf_pixels,
};

const PIXEL_CHECK_INTERVAL: usize = 1_024;
const MILLIS_PER_PIXEL: i64 = 1_000;
const PIXEL_AREA: u64 = 1_000_000;
const SHADOW_NAME: &str = "image shadow";

fn check_cancelled<C: CancellationToken + ?Sized>(cancel: &C) -> Result<(), RenderError> {
    if cancel.is_cancelled() {
        Err(RenderError::Cancelled)
    } else {
        Ok(())
    }
}

fn allocate_surface(size: PhysicalSize, limits: RenderLimits) -> Result<RgbaSurface, RenderError> {
    let requested = checked_byte_len(size)?;
    if requested > limits.max_surface_bytes {
        return Err(RenderError::SurfaceLimitExceeded {
            requested,
            limit: limits.max_surface_bytes,
        });
    }
    Ok(RgbaSurface::try_zeroed(size)?)
}

pub(crate) fn border<C: CancellationToken + ?Sized>(
    source: &RgbaSurface,
    style: &ImageBorderStyle,
    limits: RenderLimits,
    cancel: &C,
) -> Result<RgbaSurface, RenderError> {
    check_cancelled(cancel)?;
    let placement =
        style
            .placement(source.size())
            .map_err(|reason| RenderError::InvalidImageEffect {
                effect: "image border",
                reason,
            })?;
    let mut output = allocate_surface(placement.output_size, limits)?;
    let geometry = BorderGeometry::new(source.size(), style);
    paint_rectangle(&mut output, geometry.background, style.background, cancel)?;
    composite_source(&mut output, source, placement, cancel)?;
    // Separate strokes in upstream order. Oversized/reversed endpoints retain
    // their geometric meaning, including repeated alpha at genuine overlaps.
    for rectangle in geometry.strokes {
        paint_rectangle(&mut output, rectangle, style.color, cancel)?;
    }
    // Border paint passes share an 8-bit premultiplied surface, just like one
    // RenderTargetBitmap; quantize to WIC straight RGBA only at the boundary.
    for row in output
        .pixels_mut()
        .as_chunks_mut::<4>()
        .0
        .chunks_mut(PIXEL_CHECK_INTERVAL)
    {
        check_cancelled(cancel)?;
        for pixel in row {
            *pixel = wpf_pixels::unpremultiply(*pixel);
        }
    }
    check_cancelled(cancel)?;
    Ok(output)
}

#[derive(Clone, Copy)]
struct MilliRect {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl MilliRect {
    fn new(left: i64, top: i64, right: i64, bottom: i64) -> Self {
        Self {
            left: left.min(right),
            top: top.min(bottom),
            right: left.max(right),
            bottom: top.max(bottom),
        }
    }
}

struct BorderGeometry {
    background: MilliRect,
    strokes: [MilliRect; 4],
}

impl BorderGeometry {
    fn new(input: PhysicalSize, style: &ImageBorderStyle) -> Self {
        let left = i64::from(style.widths.left_milli);
        let top = i64::from(style.widths.top_milli);
        let right = i64::from(style.widths.right_milli);
        let bottom = i64::from(style.widths.bottom_milli);
        let width = i64::from(input.width.get()) * MILLIS_PER_PIXEL;
        let height = i64::from(input.height.get()) * MILLIS_PER_PIXEL;
        let outer_left = (-left).max(0);
        let outer_top = (-top).max(0);
        let outer_right = (-right).max(0);
        let outer_bottom = (-bottom).max(0);
        let source_left = outer_left / MILLIS_PER_PIXEL * MILLIS_PER_PIXEL;
        let source_top = outer_top / MILLIS_PER_PIXEL * MILLIS_PER_PIXEL;
        // Upstream casts only negative left/top before adding the opposite
        // side. This background extent is NOT the rounded output canvas.
        let painted_width = width + source_left + outer_right;
        let painted_height = height + source_top + outer_bottom;
        let right_edge = width + outer_left;
        let bottom_edge = height + outer_top;
        let horizontal_end = width + source_left - right.max(0);
        Self {
            background: MilliRect::new(0, 0, painted_width, painted_height),
            strokes: [
                MilliRect::new(0, 0, left.abs(), painted_height),
                MilliRect::new(
                    right_edge - right.max(0),
                    0,
                    right_edge + outer_right,
                    painted_height,
                ),
                MilliRect::new(left.abs(), 0, horizontal_end, top.abs()),
                MilliRect::new(
                    left.abs(),
                    bottom_edge - bottom.max(0),
                    horizontal_end,
                    bottom_edge + outer_bottom,
                ),
            ],
        }
    }
}

fn paint_rectangle<C: CancellationToken + ?Sized>(
    output: &mut RgbaSurface,
    rect: MilliRect,
    color: Rgba,
    cancel: &C,
) -> Result<(), RenderError> {
    check_cancelled(cancel)?;
    if color.alpha == 0 || rect.left == rect.right || rect.top == rect.bottom {
        return Ok(());
    }
    let start_x = clipped_floor(rect.left, output.width());
    let start_y = clipped_floor(rect.top, output.height());
    let end_x = clipped_ceil(rect.right, output.width());
    let end_y = clipped_ceil(rect.bottom, output.height());
    for y in start_y..end_y {
        check_cancelled(cancel)?;
        let vertical = overlap(rect.top, rect.bottom, y);
        for x in start_x..end_x {
            if x % 1_024 == 0 {
                check_cancelled(cancel)?;
            }
            let coverage = vertical * overlap(rect.left, rect.right, x);
            let alpha =
                u8::try_from((u64::from(color.alpha) * coverage + PIXEL_AREA / 2) / PIXEL_AREA)
                    .expect("unit-area coverage cannot increase alpha");
            let offset = output.byte_offset(x, y);
            source_over(
                &mut output.pixels_mut()[offset..offset + 4],
                [color.red, color.green, color.blue, alpha],
            );
        }
    }
    Ok(())
}

fn clipped_floor(value: i64, bound: u32) -> u32 {
    u32::try_from((value.div_euclid(MILLIS_PER_PIXEL)).clamp(0, i64::from(bound)))
        .expect("clipped pixel coordinate fits u32")
}

fn clipped_ceil(value: i64, bound: u32) -> u32 {
    clipped_floor(value + MILLIS_PER_PIXEL - 1, bound)
}

fn overlap(start: i64, end: i64, pixel: u32) -> u64 {
    let position = i64::from(pixel) * MILLIS_PER_PIXEL;
    u64::try_from((end.min(position + MILLIS_PER_PIXEL) - start.max(position)).max(0))
        .expect("nonnegative coverage is at most one pixel")
}

fn composite_source<C: CancellationToken + ?Sized>(
    output: &mut RgbaSurface,
    source: &RgbaSurface,
    placement: CanvasPlacement,
    cancel: &C,
) -> Result<(), RenderError> {
    let left = placement.source_origin.x.get();
    let top = placement.source_origin.y.get();
    let width = source.width().min(output.width().saturating_sub(left));
    let height = source.height().min(output.height().saturating_sub(top));
    for y in 0..height {
        check_cancelled(cancel)?;
        for x in 0..width {
            if x % 1_024 == 0 {
                check_cancelled(cancel)?;
            }
            let source_offset = source.byte_offset(x, y);
            let destination_offset = output.byte_offset(x + left, y + top);
            let pixel = source.pixels()[source_offset..source_offset + 4]
                .try_into()
                .expect("RGBA surface has four bytes per pixel");
            source_over(
                &mut output.pixels_mut()[destination_offset..destination_offset + 4],
                pixel,
            );
        }
    }
    Ok(())
}

// Destination is premultiplied for this entire new image-effect operation;
// source is a straight pixel or brush color. No legacy blend path calls this.
fn source_over(destination: &mut [u8], source: [u8; 4]) {
    let previous = destination.try_into().expect("validated RGBA pixel");
    destination.copy_from_slice(&wpf_pixels::over(wpf_pixels::premultiply(source), previous));
}

pub(crate) fn shadow<C: CancellationToken + ?Sized>(
    source: &RgbaSurface,
    style: &ImageShadowStyle,
    limits: RenderLimits,
    cancel: &C,
) -> Result<RgbaSurface, RenderError> {
    check_cancelled(cancel)?;
    let placement = style.placement(source.size()).map_err(shadow_error)?;
    let offset = style.pixel_offset().map_err(shadow_error)?;
    let mut output = allocate_surface(placement.output_size, limits)?;
    let kernel = GaussianKernel::new(style.blur_radius_hundredths / 100);
    let vertical = if style.opacity_basis_points == 0 {
        Vec::new()
    } else {
        vertical_alpha(source, placement, offset.1, &kernel, limits, cancel)?
    };
    let raster = ShadowRaster {
        source,
        style,
        placement,
        offset,
        kernel: &kernel,
        vertical: &vertical,
    };
    raster.paint(&mut output, cancel)?;
    check_cancelled(cancel)?;
    Ok(output)
}

fn shadow_error(reason: String) -> RenderError {
    RenderError::InvalidImageEffect {
        effect: SHADOW_NAME,
        reason,
    }
}

struct GaussianKernel {
    radius: usize,
    weights: [f32; 201],
}

impl GaussianKernel {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "WPF deliberately computes weights in f64 then stores and corrects f32 weights"
    )]
    fn new(radius: u16) -> Self {
        debug_assert!(
            radius <= 100,
            "domain validation bounds the software kernel"
        );
        let mut result = Self {
            radius: usize::from(radius),
            weights: [0.0; 201],
        };
        if radius == 0 {
            result.weights[0] = 1.0;
            return result;
        }
        let deviation = f64::from(radius) / 3.0;
        let mut sum = 0.0;
        for index in 0..=radius {
            let distance = f64::from(index);
            let weight = ((1.0 / (deviation * std::f64::consts::TAU.sqrt()))
                * (-(distance * distance) / (2.0 * deviation * deviation)).exp())
                as f32;
            result.weights[usize::from(radius + index)] = weight;
            result.weights[usize::from(radius - index)] = weight;
            sum += f64::from(weight);
            if index != 0 {
                sum += f64::from(weight);
            }
        }
        let correction = ((1.0 - sum) / f64::from(radius * 2 + 1)) as f32;
        for weight in &mut result.weights[..usize::from(radius * 2 + 1)] {
            *weight += correction;
        }
        result
    }

    fn weights(&self) -> &[f32] {
        &self.weights[..=self.radius * 2]
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "explicit nearest-even and u8 clamping reproduce each software blur pass"
)]
fn alpha_from_float(value: f32) -> u8 {
    value.round_ties_even().clamp(0.0, 255.0) as u8
}

fn allocate_alpha(length: usize, limits: RenderLimits) -> Result<Vec<u8>, RenderError> {
    if length > limits.max_surface_bytes {
        return Err(RenderError::EffectWorkingMemoryLimitExceeded {
            effect: SHADOW_NAME,
            requested: length,
            limit: limits.max_surface_bytes,
        });
    }
    let mut values = Vec::new();
    values.try_reserve_exact(length).map_err(|_| {
        RenderError::EffectWorkingMemoryAllocationFailed {
            effect: SHADOW_NAME,
            requested: length,
        }
    })?;
    values.resize(length, 0);
    Ok(values)
}

fn vertical_alpha<C: CancellationToken + ?Sized>(
    source: &RgbaSurface,
    placement: CanvasPlacement,
    offset_y: i32,
    kernel: &GaussianKernel,
    limits: RenderLimits,
    cancel: &C,
) -> Result<Vec<u8>, RenderError> {
    let width = usize::try_from(source.width()).expect("surface width fits usize");
    let length = u64::from(source.width()) * u64::from(placement.output_size.height.get());
    let length =
        usize::try_from(length).map_err(|_| RenderError::EffectWorkingMemorySizeOverflow {
            effect: SHADOW_NAME,
        })?;
    let mut vertical = allocate_alpha(length, limits)?;
    let radius = i64::try_from(kernel.radius).expect("kernel radius <=100");
    for (y, row) in vertical.chunks_exact_mut(width).enumerate() {
        check_cancelled(cancel)?;
        let center = i64::try_from(y).expect("surface height fits i64")
            - i64::from(placement.source_origin.y.get())
            - i64::from(offset_y);
        if center + radius < 0 || center - radius >= i64::from(source.height()) {
            continue;
        }
        for (x, alpha) in row.iter_mut().enumerate() {
            if x % PIXEL_CHECK_INTERVAL == 0 {
                check_cancelled(cancel)?;
            }
            let mut sum = 0.0_f32;
            for (index, weight) in kernel.weights().iter().enumerate() {
                let sample_y = center + i64::try_from(index).expect("kernel length <=201") - radius;
                if let Ok(sample_y) = u32::try_from(sample_y)
                    && sample_y < source.height()
                {
                    let byte_offset = source
                        .byte_offset(u32::try_from(x).expect("source width fits u32"), sample_y);
                    sum += weight * f32::from(source.pixels()[byte_offset + 3]);
                }
            }
            *alpha = alpha_from_float(sum);
        }
    }
    Ok(vertical)
}

struct ShadowRaster<'a> {
    source: &'a RgbaSurface,
    style: &'a ImageShadowStyle,
    placement: CanvasPlacement,
    offset: (i32, i32),
    kernel: &'a GaussianKernel,
    vertical: &'a [u8],
}

impl ShadowRaster<'_> {
    fn paint<C: CancellationToken + ?Sized>(
        &self,
        output: &mut RgbaSurface,
        cancel: &C,
    ) -> Result<(), RenderError> {
        let width = usize::try_from(output.width()).expect("surface width fits usize");
        let opacity = u32::from(self.style.opacity_basis_points) * 255 / 10_000;
        let shadow_color = software_shadow_color(self.style.color);
        for (y, row) in output.pixels_mut().chunks_exact_mut(width * 4).enumerate() {
            check_cancelled(cancel)?;
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                if x % PIXEL_CHECK_INTERVAL == 0 {
                    check_cancelled(cancel)?;
                }
                let original = self.original(x, y);
                if original[3] == 255 {
                    *pixel = original;
                    continue;
                }
                let blurred = if opacity == 0 {
                    0
                } else {
                    self.horizontal_alpha(x, y)
                };
                *pixel = shadow_pixel(
                    original,
                    blurred,
                    opacity,
                    shadow_color,
                    self.style.background,
                );
            }
        }
        Ok(())
    }

    fn original(&self, x: usize, y: usize) -> [u8; 4] {
        let source_x = i64::try_from(x).expect("surface width fits i64")
            - i64::from(self.placement.source_origin.x.get());
        let source_y = i64::try_from(y).expect("surface height fits i64")
            - i64::from(self.placement.source_origin.y.get());
        if let (Ok(x), Ok(y)) = (u32::try_from(source_x), u32::try_from(source_y))
            && x < self.source.width()
            && y < self.source.height()
        {
            let offset = self.source.byte_offset(x, y);
            self.source.pixels()[offset..offset + 4]
                .try_into()
                .expect("validated RGBA pixel")
        } else {
            [0; 4]
        }
    }

    fn horizontal_alpha(&self, x: usize, y: usize) -> u8 {
        let width = usize::try_from(self.source.width()).expect("surface width fits usize");
        let center = i64::try_from(x).expect("surface width fits i64")
            - i64::from(self.placement.source_origin.x.get())
            - i64::from(self.offset.0);
        let radius = i64::try_from(self.kernel.radius).expect("kernel radius <=100");
        let mut sum = 0.0_f32;
        for (index, weight) in self.kernel.weights().iter().enumerate() {
            let sample_x = center + i64::try_from(index).expect("kernel length <=201") - radius;
            if let Ok(source_x) = usize::try_from(sample_x)
                && source_x < width
            {
                sum += weight * f32::from(self.vertical[y * width + source_x]);
            }
        }
        alpha_from_float(sum)
    }
}

// WPF ColorToMilColorF sends ScR/ScG/ScB, while the software shadow's
// ConvertColor truncates those linear values directly to bytes. It does not
// convert them back to sRGB as an ordinary SolidColorBrush does.
fn software_shadow_color(color: Rgba) -> Rgba {
    Rgba {
        red: software_shadow_channel(color.red),
        green: software_shadow_channel(color.green),
        blue: software_shadow_channel(color.blue),
        alpha: 255,
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "matches WPF's bounded byte-to-f32 scRGB conversion then native f64-to-byte truncation"
)]
fn software_shadow_channel(channel: u8) -> u8 {
    let value = f32::from(channel) / 255.0;
    let linear = if value == 0.0 {
        0.0
    } else if f64::from(value) <= 0.040_45 {
        value / 12.92
    } else if value < 1.0 {
        ((f64::from(value) + 0.055) / 1.055).powf(2.4) as f32
    } else {
        1.0
    };
    (f64::from(linear) * 255.0) as u8
}

fn shadow_pixel(
    source: [u8; 4],
    blurred_alpha: u8,
    opacity: u32,
    color: Rgba,
    background: Rgba,
) -> [u8; 4] {
    let alpha = u32::from(source[3]);
    let extra = u32::from(blurred_alpha) * (255 - alpha) * opacity / 65_536;
    let mut result = wpf_pixels::premultiply(source);
    let shadow_channels = [color.red, color.green, color.blue];
    for channel in 0..3 {
        let shadow_premultiplied = extra * u32::from(shadow_channels[channel]) / 255;
        result[channel] = u8::try_from(u32::from(result[channel]) + shadow_premultiplied)
            .expect("source plus occluded shadow remains within its combined alpha");
    }
    result[3] = u8::try_from(alpha + extra).expect("source-over alpha remains u8");
    let background = wpf_pixels::premultiply([
        background.red,
        background.green,
        background.blue,
        background.alpha,
    ]);
    wpf_pixels::unpremultiply(wpf_pixels::over(result, background))
}

#[cfg(test)]
#[path = "image_effects_tests.rs"]
mod tests;
