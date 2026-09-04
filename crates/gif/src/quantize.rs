use crate::{CancellationToken, QuantizationError, RgbaFrame};

const HISTOGRAM_CHANNEL_BITS: usize = 5;
const HISTOGRAM_CHANNEL_SIZE: usize = 1 << HISTOGRAM_CHANNEL_BITS;
const HISTOGRAM_LEN: usize =
    HISTOGRAM_CHANNEL_SIZE * HISTOGRAM_CHANNEL_SIZE * HISTOGRAM_CHANNEL_SIZE;
const CANCELLATION_CHECK_INTERVAL: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuantizationSettings {
    /// Total palette entries, including a transparency entry when one is used.
    pub max_colors: u16,
    /// Pixels whose alpha is less than this value become transparent. `None`
    /// makes every source pixel opaque.
    pub alpha_threshold: Option<u8>,
    /// Reserve a transparent palette entry even when this frame contains no
    /// transparent pixels. Disposal-to-background needs this on the preceding
    /// opaque frame when the following target introduces transparency.
    pub reserve_transparency: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DitherMode {
    #[default]
    None,
    /// Ordered 4x4 Bayer dithering with a deterministic, moderate amplitude.
    Bayer4x4,
    /// Left-to-right Floyd-Steinberg error diffusion.
    FloydSteinberg,
}

/// RGB palette shared by every image descriptor in a GIF.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ColorPalette {
    colors: Vec<u8>,
    transparent_index: Option<u8>,
}

impl ColorPalette {
    pub fn new(colors: Vec<u8>, transparent_index: Option<u8>) -> Result<Self, QuantizationError> {
        validate_palette(&colors, transparent_index)?;
        Ok(Self {
            colors,
            transparent_index,
        })
    }

    pub fn colors(&self) -> &[u8] {
        &self.colors
    }

    pub const fn transparent_index(&self) -> Option<u8> {
        self.transparent_index
    }

    pub fn color_count(&self) -> usize {
        self.colors.len() / 3
    }
}

/// Palette and index data ready for a GIF frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedFrame {
    palette: Vec<u8>,
    indices: Vec<u8>,
    transparent_index: Option<u8>,
}

impl IndexedFrame {
    pub fn new(
        palette: Vec<u8>,
        indices: Vec<u8>,
        transparent_index: Option<u8>,
        expected_pixels: usize,
    ) -> Result<Self, QuantizationError> {
        validate_palette(&palette, transparent_index)?;
        let colors = palette.len() / 3;
        if indices.len() != expected_pixels {
            return Err(QuantizationError::InvalidIndexBuffer {
                expected: expected_pixels,
                actual: indices.len(),
            });
        }
        if let Some(&index) = indices.iter().find(|&&index| usize::from(index) >= colors) {
            return Err(QuantizationError::PaletteIndexOutOfBounds { index, colors });
        }
        Ok(Self {
            palette,
            indices,
            transparent_index,
        })
    }

    pub fn palette(&self) -> &[u8] {
        &self.palette
    }

    pub fn indices(&self) -> &[u8] {
        &self.indices
    }

    pub const fn transparent_index(&self) -> Option<u8> {
        self.transparent_index
    }

    pub(crate) fn into_parts(self) -> (Vec<u8>, Vec<u8>, Option<u8>) {
        (self.palette, self.indices, self.transparent_index)
    }
}

/// Replaceable per-frame quantization boundary.
pub trait FrameQuantizer: Send + Sync {
    fn quantize(
        &self,
        frame: &RgbaFrame,
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<IndexedFrame, QuantizationError>;

    /// Analyze a buffered sequence and create one palette for all frames.
    ///
    /// Custom per-frame quantizers remain source-compatible: the default
    /// implementation reports that global analysis is unsupported.
    fn build_global_palette(
        &self,
        _frames: &[RgbaFrame],
        _settings: QuantizationSettings,
        _cancellation: &dyn CancellationToken,
    ) -> Result<ColorPalette, QuantizationError> {
        Err(QuantizationError::Unsupported(
            "global palette analysis".to_owned(),
        ))
    }
}

/// Deterministic, frequency-weighted median-cut quantizer.
///
/// A fixed 5-bit/channel histogram bounds analysis memory and makes output
/// independent of hash-map iteration order. Palette lookup uses a weighted RGB
/// distance and currently performs no dithering.
#[derive(Clone, Copy, Debug, Default)]
pub struct MedianCutQuantizer;

#[derive(Clone, Copy, Debug, Default)]
struct HistogramBin {
    red_sum: u64,
    green_sum: u64,
    blue_sum: u64,
    count: u64,
}

impl HistogramBin {
    fn add(&mut self, red: u8, green: u8, blue: u8) {
        self.red_sum += u64::from(red);
        self.green_sum += u64::from(green);
        self.blue_sum += u64::from(blue);
        self.count += 1;
    }

    fn average(self) -> [u8; 3] {
        debug_assert_ne!(self.count, 0);
        [
            (self.red_sum / self.count) as u8,
            (self.green_sum / self.count) as u8,
            (self.blue_sum / self.count) as u8,
        ]
    }
}

#[derive(Clone, Copy, Debug)]
struct ColorPoint {
    rgb: [u8; 3],
    count: u64,
}

impl FrameQuantizer for MedianCutQuantizer {
    fn quantize(
        &self,
        frame: &RgbaFrame,
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<IndexedFrame, QuantizationError> {
        let palette =
            self.build_global_palette(std::slice::from_ref(frame), settings, cancellation)?;
        let indices = map_frame_to_palette(
            frame,
            &palette,
            settings.alpha_threshold,
            DitherMode::None,
            cancellation,
        )?;
        IndexedFrame::new(
            palette.colors,
            indices,
            palette.transparent_index,
            usize::from(frame.width()) * usize::from(frame.height()),
        )
    }

    fn build_global_palette(
        &self,
        frames: &[RgbaFrame],
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<ColorPalette, QuantizationError> {
        validate_settings(settings)?;
        check_now(cancellation)?;

        let mut histogram = vec![HistogramBin::default(); HISTOGRAM_LEN];
        let mut has_transparency = settings.reserve_transparency;
        let mut visited_pixels = 0_usize;
        for frame in frames {
            for pixel in frame.pixels().chunks_exact(4) {
                check_cancellation(visited_pixels, cancellation)?;
                visited_pixels = visited_pixels.wrapping_add(1);
                if is_transparent(pixel[3], settings.alpha_threshold) {
                    has_transparency = true;
                    continue;
                }
                let index = histogram_index(pixel[0], pixel[1], pixel[2]);
                histogram[index].add(pixel[0], pixel[1], pixel[2]);
            }
        }

        let points = histogram_points(&histogram);
        let reserved_colors = usize::from(has_transparency);
        let opaque_limit = usize::from(settings.max_colors).saturating_sub(reserved_colors);
        let mut opaque_palette = make_palette(&points, opaque_limit, cancellation)?;
        opaque_palette.sort_unstable();

        let transparent_index = has_transparency.then_some(0);
        let mut colors = Vec::with_capacity((opaque_palette.len() + reserved_colors).max(2) * 3);
        if has_transparency {
            // RGB values at a transparent index are irrelevant, but a stable
            // black entry makes byte output deterministic.
            colors.extend_from_slice(&[0, 0, 0]);
        }
        for color in opaque_palette {
            colors.extend_from_slice(&color);
        }
        while colors.len() < 6 {
            colors.extend_from_slice(&[0, 0, 0]);
        }
        ColorPalette::new(colors, transparent_index)
    }
}

pub(crate) fn map_frame_to_palette(
    frame: &RgbaFrame,
    palette: &ColorPalette,
    alpha_threshold: Option<u8>,
    dither: DitherMode,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<u8>, QuantizationError> {
    let opaque_colors = opaque_palette_entries(palette)?;
    match dither {
        DitherMode::None => map_without_dither(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            cancellation,
        ),
        DitherMode::Bayer4x4 => map_with_bayer(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            cancellation,
        ),
        DitherMode::FloydSteinberg => map_with_floyd_steinberg(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            cancellation,
        ),
    }
}

fn validate_settings(settings: QuantizationSettings) -> Result<(), QuantizationError> {
    if !(2..=256).contains(&settings.max_colors) {
        return Err(QuantizationError::InvalidPalette(format!(
            "maximum color count must be 2..=256, got {}",
            settings.max_colors
        )));
    }
    Ok(())
}

fn validate_palette(
    palette: &[u8],
    transparent_index: Option<u8>,
) -> Result<(), QuantizationError> {
    if !palette.len().is_multiple_of(3) {
        return Err(QuantizationError::InvalidPalette(
            "RGB palette length is not divisible by three".to_owned(),
        ));
    }
    let colors = palette.len() / 3;
    if !(2..=256).contains(&colors) {
        return Err(QuantizationError::InvalidPalette(format!(
            "expected 2..=256 RGB entries, got {colors}"
        )));
    }
    if let Some(index) = transparent_index
        && usize::from(index) >= colors
    {
        return Err(QuantizationError::PaletteIndexOutOfBounds { index, colors });
    }
    Ok(())
}

fn histogram_points(histogram: &[HistogramBin]) -> Vec<ColorPoint> {
    histogram
        .iter()
        .copied()
        .filter(|bin| bin.count != 0)
        .map(|bin| ColorPoint {
            rgb: bin.average(),
            count: bin.count,
        })
        .collect()
}

fn opaque_palette_entries(palette: &ColorPalette) -> Result<Vec<(u8, [u8; 3])>, QuantizationError> {
    let entries: Vec<_> = palette
        .colors
        .chunks_exact(3)
        .enumerate()
        .filter(|(index, _)| Some(*index as u8) != palette.transparent_index)
        .map(|(index, color)| (index as u8, [color[0], color[1], color[2]]))
        .collect();
    if entries.is_empty() {
        return Err(QuantizationError::InvalidPalette(
            "palette has no opaque entry".to_owned(),
        ));
    }
    Ok(entries)
}

fn build_color_lookup(
    palette: &[(u8, [u8; 3])],
    cancellation: &dyn CancellationToken,
) -> Result<Vec<u8>, QuantizationError> {
    let mut lookup = Vec::with_capacity(HISTOGRAM_LEN);
    for index in 0..HISTOGRAM_LEN {
        check_cancellation(index, cancellation)?;
        let red = (((index / (HISTOGRAM_CHANNEL_SIZE * HISTOGRAM_CHANNEL_SIZE)) << 3) | 4) as u8;
        let green = ((((index / HISTOGRAM_CHANNEL_SIZE) % HISTOGRAM_CHANNEL_SIZE) << 3) | 4) as u8;
        let blue = (((index % HISTOGRAM_CHANNEL_SIZE) << 3) | 4) as u8;
        lookup.push(nearest_palette_index([red, green, blue], palette));
    }
    Ok(lookup)
}

fn map_without_dither(
    frame: &RgbaFrame,
    transparent_index: Option<u8>,
    palette: &[(u8, [u8; 3])],
    alpha_threshold: Option<u8>,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<u8>, QuantizationError> {
    let lookup = build_color_lookup(palette, cancellation)?;
    let mut indices = Vec::with_capacity(usize::from(frame.width()) * usize::from(frame.height()));
    for (pixel_index, pixel) in frame.pixels().chunks_exact(4).enumerate() {
        check_cancellation(pixel_index, cancellation)?;
        if is_transparent(pixel[3], alpha_threshold) {
            indices.push(required_transparent_index(transparent_index)?);
        } else {
            indices.push(lookup[histogram_index(pixel[0], pixel[1], pixel[2])]);
        }
    }
    Ok(indices)
}

fn map_with_bayer(
    frame: &RgbaFrame,
    transparent_index: Option<u8>,
    palette: &[(u8, [u8; 3])],
    alpha_threshold: Option<u8>,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<u8>, QuantizationError> {
    const BAYER_4X4: [[i16; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

    let lookup = build_color_lookup(palette, cancellation)?;
    let width = usize::from(frame.width());
    let mut indices = Vec::with_capacity(width * usize::from(frame.height()));
    for (pixel_index, pixel) in frame.pixels().chunks_exact(4).enumerate() {
        check_cancellation(pixel_index, cancellation)?;
        if is_transparent(pixel[3], alpha_threshold) {
            indices.push(required_transparent_index(transparent_index)?);
            continue;
        }
        let x = pixel_index % width;
        let y = pixel_index / width;
        // Values cover [-30, 30] in steps of four. This is deliberately
        // moderate: it exposes gradients without overwhelming UI captures.
        let adjustment = (BAYER_4X4[y % 4][x % 4] * 2 + 1 - 16) * 2;
        let adjusted = [
            adjust_channel(pixel[0], adjustment),
            adjust_channel(pixel[1], adjustment),
            adjust_channel(pixel[2], adjustment),
        ];
        indices.push(lookup[histogram_index(adjusted[0], adjusted[1], adjusted[2])]);
    }
    Ok(indices)
}

fn map_with_floyd_steinberg(
    frame: &RgbaFrame,
    transparent_index: Option<u8>,
    palette: &[(u8, [u8; 3])],
    alpha_threshold: Option<u8>,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<u8>, QuantizationError> {
    let lookup = build_color_lookup(palette, cancellation)?;
    let palette_by_index = palette_colors_by_index(palette);
    let width = usize::from(frame.width());
    let height = usize::from(frame.height());
    let mut current_error = vec![[0_i32; 3]; width + 2];
    let mut next_error = vec![[0_i32; 3]; width + 2];
    let mut indices = Vec::with_capacity(width * height);

    for y in 0..height {
        for x in 0..width {
            let pixel_index = y * width + x;
            check_cancellation(pixel_index, cancellation)?;
            let pixel = &frame.pixels()[pixel_index * 4..pixel_index * 4 + 4];
            if is_transparent(pixel[3], alpha_threshold) {
                indices.push(required_transparent_index(transparent_index)?);
                continue;
            }

            let mut adjusted = [0_u8; 3];
            let mut scaled = [0_i32; 3];
            for channel in 0..3 {
                scaled[channel] = (i32::from(pixel[channel]) * 16 + current_error[x + 1][channel])
                    .clamp(0, 255 * 16);
                adjusted[channel] = ((scaled[channel] + 8) / 16) as u8;
            }
            let palette_index = lookup[histogram_index(adjusted[0], adjusted[1], adjusted[2])];
            indices.push(palette_index);
            let chosen = palette_by_index[usize::from(palette_index)];

            for channel in 0..3 {
                let error = scaled[channel] - i32::from(chosen[channel]) * 16;
                current_error[x + 2][channel] += error * 7 / 16;
                next_error[x][channel] += error * 3 / 16;
                next_error[x + 1][channel] += error * 5 / 16;
                next_error[x + 2][channel] += error / 16;
            }
        }
        std::mem::swap(&mut current_error, &mut next_error);
        next_error.fill([0; 3]);
    }
    Ok(indices)
}

fn palette_colors_by_index(palette: &[(u8, [u8; 3])]) -> Vec<[u8; 3]> {
    let len = palette
        .iter()
        .map(|(index, _)| usize::from(*index))
        .max()
        .unwrap_or(0)
        + 1;
    let mut colors = vec![[0; 3]; len];
    for &(index, color) in palette {
        colors[usize::from(index)] = color;
    }
    colors
}

fn required_transparent_index(index: Option<u8>) -> Result<u8, QuantizationError> {
    index.ok_or_else(|| {
        QuantizationError::InvalidPalette(
            "transparent source pixels require a transparent palette entry".to_owned(),
        )
    })
}

fn adjust_channel(channel: u8, adjustment: i16) -> u8 {
    (i16::from(channel) + adjustment).clamp(0, 255) as u8
}

fn nearest_palette_index(color: [u8; 3], palette: &[(u8, [u8; 3])]) -> u8 {
    palette
        .iter()
        .min_by_key(|&&(index, candidate)| (color_distance(color, candidate), index))
        .map(|&(index, _)| index)
        .unwrap_or(0)
}

pub(crate) fn indexed_to_rgba(
    indices: &[u8],
    palette: &[u8],
    transparent_index: Option<u8>,
) -> Result<Vec<u8>, QuantizationError> {
    validate_palette(palette, transparent_index)?;
    let colors = palette.len() / 3;
    let mut rgba = Vec::with_capacity(indices.len() * 4);
    for &index in indices {
        if usize::from(index) >= colors {
            return Err(QuantizationError::PaletteIndexOutOfBounds { index, colors });
        }
        if Some(index) == transparent_index {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let offset = usize::from(index) * 3;
            rgba.extend_from_slice(&[
                palette[offset],
                palette[offset + 1],
                palette[offset + 2],
                255,
            ]);
        }
    }
    Ok(rgba)
}

fn check_cancellation(
    index: usize,
    cancellation: &dyn CancellationToken,
) -> Result<(), QuantizationError> {
    if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) && cancellation.is_cancelled() {
        Err(QuantizationError::Cancelled)
    } else {
        Ok(())
    }
}

fn check_now(cancellation: &dyn CancellationToken) -> Result<(), QuantizationError> {
    if cancellation.is_cancelled() {
        Err(QuantizationError::Cancelled)
    } else {
        Ok(())
    }
}

fn is_transparent(alpha: u8, alpha_threshold: Option<u8>) -> bool {
    alpha_threshold.is_some_and(|threshold| alpha < threshold)
}

fn histogram_index(red: u8, green: u8, blue: u8) -> usize {
    (usize::from(red) >> (8 - HISTOGRAM_CHANNEL_BITS))
        * HISTOGRAM_CHANNEL_SIZE
        * HISTOGRAM_CHANNEL_SIZE
        + (usize::from(green) >> (8 - HISTOGRAM_CHANNEL_BITS)) * HISTOGRAM_CHANNEL_SIZE
        + (usize::from(blue) >> (8 - HISTOGRAM_CHANNEL_BITS))
}

fn make_palette(
    points: &[ColorPoint],
    limit: usize,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<[u8; 3]>, QuantizationError> {
    if points.is_empty() {
        return Ok(Vec::new());
    }

    let limit = limit.max(1).min(points.len());
    let mut boxes = vec![(0..points.len()).collect::<Vec<_>>()];

    while boxes.len() < limit {
        if cancellation.is_cancelled() {
            return Err(QuantizationError::Cancelled);
        }

        let Some((box_index, channel)) = best_box_to_split(&boxes, points) else {
            break;
        };
        let mut color_box = boxes.swap_remove(box_index);
        color_box.sort_unstable_by_key(|&index| {
            let rgb = points[index].rgb;
            (rgb[channel], rgb[(channel + 1) % 3], rgb[(channel + 2) % 3])
        });

        let total_weight: u64 = color_box.iter().map(|&index| points[index].count).sum();
        let mut left_weight = 0_u64;
        let mut split_at = 1;
        for (index, &point_index) in color_box[..color_box.len() - 1].iter().enumerate() {
            left_weight += points[point_index].count;
            split_at = index + 1;
            if u128::from(left_weight) * 2 >= u128::from(total_weight) {
                break;
            }
        }

        let right = color_box.split_off(split_at);
        boxes.push(color_box);
        boxes.push(right);
    }

    let mut palette = Vec::with_capacity(boxes.len());
    for color_box in boxes {
        let mut red_sum = 0_u128;
        let mut green_sum = 0_u128;
        let mut blue_sum = 0_u128;
        let mut count = 0_u128;
        for point_index in color_box {
            let point = points[point_index];
            let weight = u128::from(point.count);
            red_sum += u128::from(point.rgb[0]) * weight;
            green_sum += u128::from(point.rgb[1]) * weight;
            blue_sum += u128::from(point.rgb[2]) * weight;
            count += weight;
        }
        palette.push([
            (red_sum / count) as u8,
            (green_sum / count) as u8,
            (blue_sum / count) as u8,
        ]);
    }
    Ok(palette)
}

fn best_box_to_split(boxes: &[Vec<usize>], points: &[ColorPoint]) -> Option<(usize, usize)> {
    boxes
        .iter()
        .enumerate()
        .filter_map(|(box_index, color_box)| {
            if color_box.len() < 2 {
                return None;
            }
            let mut min = [u8::MAX; 3];
            let mut max = [u8::MIN; 3];
            let mut weight = 0_u64;
            for &point_index in color_box {
                let point = points[point_index];
                weight += point.count;
                for channel in 0..3 {
                    min[channel] = min[channel].min(point.rgb[channel]);
                    max[channel] = max[channel].max(point.rgb[channel]);
                }
            }
            let (channel, range) = (0..3)
                .map(|channel| (channel, max[channel] - min[channel]))
                .max_by_key(|&(channel, range)| (range, std::cmp::Reverse(channel)))?;
            (range != 0).then_some((box_index, channel, u128::from(range) * u128::from(weight)))
        })
        .max_by_key(|&(box_index, channel, score)| {
            (
                score,
                std::cmp::Reverse(box_index),
                std::cmp::Reverse(channel),
            )
        })
        .map(|(box_index, channel, _)| (box_index, channel))
}

fn color_distance(left: [u8; 3], right: [u8; 3]) -> i32 {
    let red = i32::from(left[0]) - i32::from(right[0]);
    let green = i32::from(left[1]) - i32::from(right[1]);
    let blue = i32::from(left[2]) - i32::from(right[2]);
    30 * red * red + 59 * green * green + 11 * blue * blue
}

#[cfg(test)]
mod tests {
    use crate::NeverCancel;

    use super::*;

    #[test]
    fn respects_two_color_limit() {
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let frame = RgbaFrame::new(4, 1, pixels, 10_000).unwrap();
        let indexed = MedianCutQuantizer
            .quantize(
                &frame,
                QuantizationSettings {
                    max_colors: 2,
                    alpha_threshold: Some(1),
                    reserve_transparency: false,
                },
                &NeverCancel,
            )
            .unwrap();

        assert_eq!(indexed.palette().len(), 2 * 3);
        assert!(indexed.indices().iter().all(|&index| index < 2));
    }

    #[test]
    fn reserves_transparent_palette_entry() {
        let pixels = vec![255, 0, 0, 0, 0, 255, 0, 255];
        let frame = RgbaFrame::new(2, 1, pixels, 10_000).unwrap();
        let indexed = MedianCutQuantizer
            .quantize(
                &frame,
                QuantizationSettings {
                    max_colors: 2,
                    alpha_threshold: Some(1),
                    reserve_transparency: false,
                },
                &NeverCancel,
            )
            .unwrap();

        assert_eq!(indexed.transparent_index(), Some(0));
        assert_eq!(indexed.indices()[0], 0);
        assert_eq!(indexed.indices()[1], 1);
        assert_eq!(indexed.palette().len(), 6);
    }

    #[test]
    fn bayer_and_floyd_steinberg_dither_a_midpoint() {
        let frame = RgbaFrame::new(8, 8, [128, 128, 128, 255].repeat(64), 10_000).unwrap();
        let palette = ColorPalette::new(vec![0, 0, 0, 255, 255, 255], None).unwrap();
        let none =
            map_frame_to_palette(&frame, &palette, None, DitherMode::None, &NeverCancel).unwrap();
        let bayer =
            map_frame_to_palette(&frame, &palette, None, DitherMode::Bayer4x4, &NeverCancel)
                .unwrap();
        let floyd = map_frame_to_palette(
            &frame,
            &palette,
            None,
            DitherMode::FloydSteinberg,
            &NeverCancel,
        )
        .unwrap();

        assert!(none.iter().all(|&index| index == none[0]));
        assert!(bayer.contains(&0) && bayer.contains(&1));
        assert!(floyd.contains(&0) && floyd.contains(&1));
        assert_ne!(bayer, none);
        assert_ne!(floyd, none);
    }
}
