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

/// Deterministic palette-mapping strategy used after color quantization.
///
/// Error-diffusion modes scan rows from left to right. Transparent pixels are
/// assigned the palette's transparent entry without consuming, producing, or
/// forwarding color error, so hidden RGB data cannot affect opaque output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DitherMode {
    /// Map every pixel directly to its nearest palette entry.
    #[default]
    None,
    /// Ordered 4x4 Bayer dithering with a deterministic, moderate amplitude.
    Bayer4x4,
    /// Left-to-right Floyd-Steinberg error diffusion.
    FloydSteinberg,
    /// Atkinson error diffusion, which intentionally diffuses only 3/4 of the
    /// quantization error and therefore preserves stronger local contrast.
    Atkinson,
    /// Burkes two-row error diffusion.
    Burkes,
    /// Sierra Lite two-row error diffusion using three neighboring pixels.
    SierraLite,
    /// Two-row Sierra error diffusion.
    TwoRowSierra,
    /// Full three-row Sierra error diffusion.
    Sierra,
    /// Three-row Jarvis–Judice–Ninke error diffusion.
    JarvisJudiceNinke,
    /// Three-row Stucki error diffusion.
    Stucki,
    /// Four-row Stevenson–Arce error diffusion with a sparse seven-column
    /// neighborhood.
    StevensonArce,
}

/// Deterministic quantizer provided by the built-in encoder.
///
/// The selected strategy is used for both local per-frame palettes and a
/// shared global palette. [`MedianCut`](Self::MedianCut) is the general-purpose
/// default, [`Grayscale`](Self::Grayscale) deliberately removes hue, and
/// [`MostUsed`](Self::MostUsed) favors the most frequent source colors.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QuantizerStrategy {
    /// Frequency-weighted median cut over a bounded RGB histogram.
    #[default]
    MedianCut,
    /// Frequency-weighted median cut after conversion to grayscale.
    Grayscale,
    /// The most frequent colors in a bounded RGB histogram.
    MostUsed,
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

/// Deterministic grayscale quantizer.
///
/// Source pixels are converted to integer BT.601 luma before a
/// frequency-weighted one-dimensional median cut is applied.
#[derive(Clone, Copy, Debug, Default)]
pub struct GrayscaleQuantizer;

/// Deterministic high-frequency-color quantizer.
///
/// Colors are selected by descending frequency from the same bounded
/// 5-bit/channel histogram used by [`MedianCutQuantizer`]. Equal-frequency
/// colors are ordered lexicographically, so output never depends on hash-map
/// iteration order.
#[derive(Clone, Copy, Debug, Default)]
pub struct MostUsedQuantizer;

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
        quantize_frame(self, frame, settings, cancellation)
    }

    fn build_global_palette(
        &self,
        frames: &[RgbaFrame],
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<ColorPalette, QuantizationError> {
        let (histogram, has_transparency) = build_rgb_histogram(frames, settings, cancellation)?;

        let points = histogram_points(&histogram);
        let opaque_limit = opaque_color_limit(settings.max_colors, has_transparency);
        let mut opaque_palette = make_palette(&points, opaque_limit, cancellation)?;
        opaque_palette.sort_unstable();

        finish_palette(opaque_palette, has_transparency)
    }
}

impl FrameQuantizer for GrayscaleQuantizer {
    fn quantize(
        &self,
        frame: &RgbaFrame,
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<IndexedFrame, QuantizationError> {
        quantize_frame(self, frame, settings, cancellation)
    }

    fn build_global_palette(
        &self,
        frames: &[RgbaFrame],
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<ColorPalette, QuantizationError> {
        let (histogram, has_transparency) =
            build_grayscale_histogram(frames, settings, cancellation)?;
        let points = grayscale_points(&histogram);
        let opaque_limit = opaque_color_limit(settings.max_colors, has_transparency);
        let mut opaque_palette = make_palette(&points, opaque_limit, cancellation)?;
        opaque_palette.sort_unstable();

        finish_palette(opaque_palette, has_transparency)
    }
}

impl FrameQuantizer for MostUsedQuantizer {
    fn quantize(
        &self,
        frame: &RgbaFrame,
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<IndexedFrame, QuantizationError> {
        quantize_frame(self, frame, settings, cancellation)
    }

    fn build_global_palette(
        &self,
        frames: &[RgbaFrame],
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<ColorPalette, QuantizationError> {
        let (histogram, has_transparency) = build_rgb_histogram(frames, settings, cancellation)?;
        let mut points = histogram_points(&histogram);
        points.sort_unstable_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.rgb.cmp(&right.rgb))
        });
        check_now(cancellation)?;

        let opaque_limit = opaque_color_limit(settings.max_colors, has_transparency);
        let opaque_palette = points
            .into_iter()
            .take(opaque_limit)
            .map(|point| point.rgb)
            .collect();

        finish_palette(opaque_palette, has_transparency)
    }
}

impl FrameQuantizer for QuantizerStrategy {
    fn quantize(
        &self,
        frame: &RgbaFrame,
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<IndexedFrame, QuantizationError> {
        match self {
            Self::MedianCut => MedianCutQuantizer.quantize(frame, settings, cancellation),
            Self::Grayscale => GrayscaleQuantizer.quantize(frame, settings, cancellation),
            Self::MostUsed => MostUsedQuantizer.quantize(frame, settings, cancellation),
        }
    }

    fn build_global_palette(
        &self,
        frames: &[RgbaFrame],
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<ColorPalette, QuantizationError> {
        match self {
            Self::MedianCut => {
                MedianCutQuantizer.build_global_palette(frames, settings, cancellation)
            }
            Self::Grayscale => {
                GrayscaleQuantizer.build_global_palette(frames, settings, cancellation)
            }
            Self::MostUsed => {
                MostUsedQuantizer.build_global_palette(frames, settings, cancellation)
            }
        }
    }
}

fn quantize_frame(
    quantizer: &dyn FrameQuantizer,
    frame: &RgbaFrame,
    settings: QuantizationSettings,
    cancellation: &dyn CancellationToken,
) -> Result<IndexedFrame, QuantizationError> {
    let palette =
        quantizer.build_global_palette(std::slice::from_ref(frame), settings, cancellation)?;
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

fn build_rgb_histogram(
    frames: &[RgbaFrame],
    settings: QuantizationSettings,
    cancellation: &dyn CancellationToken,
) -> Result<(Vec<HistogramBin>, bool), QuantizationError> {
    validate_settings(settings)?;
    check_now(cancellation)?;

    let mut histogram = vec![HistogramBin::default(); HISTOGRAM_LEN];
    let mut has_transparency = settings.reserve_transparency;
    let mut visited_pixels = 0_usize;
    for frame in frames {
        for pixel in frame.pixels().as_chunks::<4>().0 {
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
    Ok((histogram, has_transparency))
}

fn build_grayscale_histogram(
    frames: &[RgbaFrame],
    settings: QuantizationSettings,
    cancellation: &dyn CancellationToken,
) -> Result<([u64; 256], bool), QuantizationError> {
    validate_settings(settings)?;
    check_now(cancellation)?;

    let mut histogram = [0_u64; 256];
    let mut has_transparency = settings.reserve_transparency;
    let mut visited_pixels = 0_usize;
    for frame in frames {
        for pixel in frame.pixels().as_chunks::<4>().0 {
            check_cancellation(visited_pixels, cancellation)?;
            visited_pixels = visited_pixels.wrapping_add(1);
            if is_transparent(pixel[3], settings.alpha_threshold) {
                has_transparency = true;
                continue;
            }
            let gray = grayscale_luma(pixel[0], pixel[1], pixel[2]);
            histogram[usize::from(gray)] += 1;
        }
    }
    Ok((histogram, has_transparency))
}

fn finish_palette(
    opaque_palette: Vec<[u8; 3]>,
    has_transparency: bool,
) -> Result<ColorPalette, QuantizationError> {
    let reserved_colors = usize::from(has_transparency);
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

const fn opaque_color_limit(max_colors: u16, has_transparency: bool) -> usize {
    (max_colors as usize).saturating_sub(has_transparency as usize)
}

const fn grayscale_luma(red: u8, green: u8, blue: u8) -> u8 {
    let weighted = red as u32 * 77 + green as u32 * 150 + blue as u32 * 29 + 128;
    (weighted >> 8) as u8
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
        DitherMode::Atkinson => map_with_error_diffusion(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            ATKINSON,
            cancellation,
        ),
        DitherMode::Burkes => map_with_error_diffusion(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            BURKES,
            cancellation,
        ),
        DitherMode::SierraLite => map_with_error_diffusion(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            SIERRA_LITE,
            cancellation,
        ),
        DitherMode::TwoRowSierra => map_with_error_diffusion(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            TWO_ROW_SIERRA,
            cancellation,
        ),
        DitherMode::Sierra => map_with_error_diffusion(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            SIERRA,
            cancellation,
        ),
        DitherMode::JarvisJudiceNinke => map_with_error_diffusion(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            JARVIS_JUDICE_NINKE,
            cancellation,
        ),
        DitherMode::Stucki => map_with_error_diffusion(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            STUCKI,
            cancellation,
        ),
        DitherMode::StevensonArce => map_with_error_diffusion(
            frame,
            palette.transparent_index,
            &opaque_colors,
            alpha_threshold,
            STEVENSON_ARCE,
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

fn grayscale_points(histogram: &[u64; 256]) -> Vec<ColorPoint> {
    histogram
        .iter()
        .enumerate()
        .filter(|(_, count)| **count != 0)
        .map(|(gray, &count)| {
            let gray = gray as u8;
            ColorPoint {
                rgb: [gray, gray, gray],
                count,
            }
        })
        .collect()
}

fn opaque_palette_entries(palette: &ColorPalette) -> Result<Vec<(u8, [u8; 3])>, QuantizationError> {
    let entries: Vec<_> = palette
        .colors
        .as_chunks::<3>()
        .0
        .iter()
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
    for (pixel_index, pixel) in frame.pixels().as_chunks::<4>().0.iter().enumerate() {
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
    for (pixel_index, pixel) in frame.pixels().as_chunks::<4>().0.iter().enumerate() {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiffusionTap {
    x: isize,
    row: usize,
    weight: i32,
}

#[derive(Clone, Copy, Debug)]
struct DiffusionKernel {
    divisor: i32,
    taps: &'static [DiffusionTap],
}

const fn tap(x: isize, row: usize, weight: i32) -> DiffusionTap {
    DiffusionTap { x, row, weight }
}

const FLOYD_STEINBERG: DiffusionKernel = DiffusionKernel {
    divisor: 16,
    taps: &[tap(1, 0, 7), tap(-1, 1, 3), tap(0, 1, 5), tap(1, 1, 1)],
};

const ATKINSON: DiffusionKernel = DiffusionKernel {
    divisor: 8,
    taps: &[
        tap(1, 0, 1),
        tap(2, 0, 1),
        tap(-1, 1, 1),
        tap(0, 1, 1),
        tap(1, 1, 1),
        tap(0, 2, 1),
    ],
};

const BURKES: DiffusionKernel = DiffusionKernel {
    divisor: 32,
    taps: &[
        tap(1, 0, 8),
        tap(2, 0, 4),
        tap(-2, 1, 2),
        tap(-1, 1, 4),
        tap(0, 1, 8),
        tap(1, 1, 4),
        tap(2, 1, 2),
    ],
};

const SIERRA_LITE: DiffusionKernel = DiffusionKernel {
    divisor: 4,
    taps: &[tap(1, 0, 2), tap(-1, 1, 1), tap(0, 1, 1)],
};

const TWO_ROW_SIERRA: DiffusionKernel = DiffusionKernel {
    divisor: 16,
    taps: &[
        tap(1, 0, 4),
        tap(2, 0, 3),
        tap(-2, 1, 1),
        tap(-1, 1, 2),
        tap(0, 1, 3),
        tap(1, 1, 2),
        tap(2, 1, 1),
    ],
};

const SIERRA: DiffusionKernel = DiffusionKernel {
    divisor: 32,
    taps: &[
        tap(1, 0, 5),
        tap(2, 0, 3),
        tap(-2, 1, 2),
        tap(-1, 1, 4),
        tap(0, 1, 5),
        tap(1, 1, 4),
        tap(2, 1, 2),
        tap(-1, 2, 2),
        tap(0, 2, 3),
        tap(1, 2, 2),
    ],
};

// Jarvis, Judice, and Ninke, “A Survey of Techniques for the Display of
// Continuous Tone Pictures on Bi-Level Displays”, Computer Graphics and Image
// Processing 5 (1976), pp. 13–40. `X` is the current pixel:
//
//           X   7   5
//   3   5   7   5   3
//   1   3   5   3   1   × 1/48
const JARVIS_JUDICE_NINKE: DiffusionKernel = DiffusionKernel {
    divisor: 48,
    taps: &[
        tap(1, 0, 7),
        tap(2, 0, 5),
        tap(-2, 1, 3),
        tap(-1, 1, 5),
        tap(0, 1, 7),
        tap(1, 1, 5),
        tap(2, 1, 3),
        tap(-2, 2, 1),
        tap(-1, 2, 3),
        tap(0, 2, 5),
        tap(1, 2, 3),
        tap(2, 2, 1),
    ],
};

// Stucki, “MECCA — A Multiple-Error Correcting Computation Algorithm for
// Bi-Level Image Hardcopy Reproduction”, IBM Research Report RZ1060 (1981):
//
//           X   8   4
//   2   4   8   4   2
//   1   2   4   2   1   × 1/42
const STUCKI: DiffusionKernel = DiffusionKernel {
    divisor: 42,
    taps: &[
        tap(1, 0, 8),
        tap(2, 0, 4),
        tap(-2, 1, 2),
        tap(-1, 1, 4),
        tap(0, 1, 8),
        tap(1, 1, 4),
        tap(2, 1, 2),
        tap(-2, 2, 1),
        tap(-1, 2, 2),
        tap(0, 2, 4),
        tap(1, 2, 2),
        tap(2, 2, 1),
    ],
};

// Stevenson and Arce, “Binary Display of Hexagonally Sampled Continuous-Tone
// Images”, JOSA A 2(7) (1985), pp. 1009–1013, doi:10.1364/JOSAA.2.001009.
// The sparse rectangular-lattice coefficient matrix is (`.` means zero):
//
//               X   .  32
//  12   .  26   .  30   .  16
//   .  12   .  26   .  12   .
//   5   .  12   .  12   .   5   × 1/200
const STEVENSON_ARCE: DiffusionKernel = DiffusionKernel {
    divisor: 200,
    taps: &[
        tap(2, 0, 32),
        tap(-3, 1, 12),
        tap(-1, 1, 26),
        tap(1, 1, 30),
        tap(3, 1, 16),
        tap(-2, 2, 12),
        tap(0, 2, 26),
        tap(2, 2, 12),
        tap(-3, 3, 5),
        tap(-1, 3, 12),
        tap(1, 3, 12),
        tap(3, 3, 5),
    ],
};

fn map_with_floyd_steinberg(
    frame: &RgbaFrame,
    transparent_index: Option<u8>,
    palette: &[(u8, [u8; 3])],
    alpha_threshold: Option<u8>,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<u8>, QuantizationError> {
    map_with_error_diffusion(
        frame,
        transparent_index,
        palette,
        alpha_threshold,
        FLOYD_STEINBERG,
        cancellation,
    )
}

fn map_with_error_diffusion(
    frame: &RgbaFrame,
    transparent_index: Option<u8>,
    palette: &[(u8, [u8; 3])],
    alpha_threshold: Option<u8>,
    kernel: DiffusionKernel,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<u8>, QuantizationError> {
    const ERROR_SCALE: i32 = 256;

    let lookup = build_color_lookup(palette, cancellation)?;
    let palette_by_index = palette_colors_by_index(palette);
    let width = usize::from(frame.width());
    let height = usize::from(frame.height());
    let row_count = kernel.taps.iter().map(|tap| tap.row).max().unwrap_or(0) + 1;
    let padding = kernel
        .taps
        .iter()
        .map(|tap| tap.x.unsigned_abs())
        .max()
        .unwrap_or(0);
    let mut error_rows = vec![vec![[0_i32; 3]; width + padding * 2]; row_count];
    let mut indices = Vec::with_capacity(width * height);

    for y in 0..height {
        for x in 0..width {
            let pixel_index = y * width + x;
            check_cancellation(pixel_index, cancellation)?;
            let pixel = &frame.pixels()[pixel_index * 4..pixel_index * 4 + 4];
            if is_transparent(pixel[3], alpha_threshold) {
                // Discard any error aimed at a transparent pixel. Its hidden
                // RGB values must neither influence output nor relay error to
                // opaque neighbors.
                error_rows[0][x + padding] = [0; 3];
                indices.push(required_transparent_index(transparent_index)?);
                continue;
            }

            let mut adjusted = [0_u8; 3];
            let mut scaled = [0_i32; 3];
            for channel in 0..3 {
                scaled[channel] = (i32::from(pixel[channel]) * ERROR_SCALE
                    + error_rows[0][x + padding][channel])
                    .clamp(0, 255 * ERROR_SCALE);
                adjusted[channel] = ((scaled[channel] + ERROR_SCALE / 2) / ERROR_SCALE) as u8;
            }
            let palette_index = lookup[histogram_index(adjusted[0], adjusted[1], adjusted[2])];
            indices.push(palette_index);
            let chosen = palette_by_index[usize::from(palette_index)];

            for tap in kernel.taps {
                let target_x = x as isize + tap.x;
                let target_y = y + tap.row;
                if target_x < 0 || target_x >= width as isize || target_y >= height {
                    continue;
                }
                let target_x = target_x as usize;
                let target_alpha = frame.pixels()[(target_y * width + target_x) * 4 + 3];
                if is_transparent(target_alpha, alpha_threshold) {
                    continue;
                }
                for channel in 0..3 {
                    let error = scaled[channel] - i32::from(chosen[channel]) * ERROR_SCALE;
                    error_rows[tap.row][target_x + padding][channel] +=
                        error * tap.weight / kernel.divisor;
                }
            }
        }
        error_rows.rotate_left(1);
        error_rows[row_count - 1].fill([0; 3]);
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::{CancellationFlag, NeverCancel};

    use super::*;

    const STRATEGIES: [QuantizerStrategy; 3] = [
        QuantizerStrategy::MedianCut,
        QuantizerStrategy::Grayscale,
        QuantizerStrategy::MostUsed,
    ];

    const ERROR_DIFFUSION_MODES: [DitherMode; 9] = [
        DitherMode::FloydSteinberg,
        DitherMode::Atkinson,
        DitherMode::Burkes,
        DitherMode::SierraLite,
        DitherMode::TwoRowSierra,
        DitherMode::Sierra,
        DitherMode::JarvisJudiceNinke,
        DitherMode::Stucki,
        DitherMode::StevensonArce,
    ];

    #[derive(Debug)]
    struct CancelAfterChecks {
        checks: AtomicUsize,
        allowed_checks: usize,
    }

    impl CancelAfterChecks {
        const fn new(allowed_checks: usize) -> Self {
            Self {
                checks: AtomicUsize::new(0),
                allowed_checks,
            }
        }
    }

    impl CancellationToken for CancelAfterChecks {
        fn is_cancelled(&self) -> bool {
            self.checks.fetch_add(1, Ordering::Relaxed) >= self.allowed_checks
        }
    }

    fn frame_from_pixels(pixels: &[[u8; 4]]) -> RgbaFrame {
        RgbaFrame::new(
            u16::try_from(pixels.len()).unwrap(),
            1,
            pixels.iter().flatten().copied().collect(),
            10_000,
        )
        .unwrap()
    }

    const fn settings(max_colors: u16) -> QuantizationSettings {
        QuantizationSettings {
            max_colors,
            alpha_threshold: Some(128),
            reserve_transparency: false,
        }
    }

    fn palette_entries(palette: &ColorPalette) -> Vec<[u8; 3]> {
        palette.colors().as_chunks::<3>().0.to_vec()
    }

    fn assert_conservative_kernel(
        kernel: DiffusionKernel,
        expected_divisor: i32,
        expected_taps: &[DiffusionTap],
    ) {
        assert_eq!(kernel.divisor, expected_divisor);
        assert_eq!(kernel.taps, expected_taps);
        assert_eq!(
            kernel.taps.iter().map(|tap| tap.weight).sum::<i32>(),
            kernel.divisor
        );
        for (index, current) in kernel.taps.iter().enumerate() {
            assert!(current.weight > 0);
            assert!(current.row > 0 || current.x > 0);
            assert!(
                kernel.taps[index + 1..]
                    .iter()
                    .all(|other| (current.x, current.row) != (other.x, other.row)),
                "duplicate diffusion coordinate ({}, {})",
                current.x,
                current.row
            );
        }
    }

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
    fn median_cut_palette_is_deterministic_and_preserves_transparency() {
        let frame = frame_from_pixels(&[
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 0, 0, 255],
            [9, 8, 7, 0],
        ]);

        let first = MedianCutQuantizer
            .build_global_palette(std::slice::from_ref(&frame), settings(4), &NeverCancel)
            .unwrap();
        let second = MedianCutQuantizer
            .build_global_palette(std::slice::from_ref(&frame), settings(4), &NeverCancel)
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.transparent_index(), Some(0));
        assert_eq!(
            palette_entries(&first),
            vec![[0, 0, 0], [0, 0, 255], [0, 255, 0], [255, 0, 0]]
        );
    }

    #[test]
    fn grayscale_palette_is_deterministic_and_preserves_transparency() {
        let frame = frame_from_pixels(&[
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
            [9, 8, 7, 0],
        ]);

        let first = GrayscaleQuantizer
            .build_global_palette(std::slice::from_ref(&frame), settings(5), &NeverCancel)
            .unwrap();
        let second = GrayscaleQuantizer
            .build_global_palette(std::slice::from_ref(&frame), settings(5), &NeverCancel)
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.transparent_index(), Some(0));
        assert_eq!(
            palette_entries(&first),
            vec![
                [0, 0, 0],
                [29, 29, 29],
                [77, 77, 77],
                [149, 149, 149],
                [255, 255, 255]
            ]
        );
    }

    #[test]
    fn most_used_palette_is_deterministic_and_frequency_ordered() {
        let frame = frame_from_pixels(&[
            [255, 0, 0, 255],
            [255, 0, 0, 255],
            [255, 0, 0, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 255, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [0, 0, 255, 255],
            [9, 8, 7, 0],
        ]);

        let first = MostUsedQuantizer
            .build_global_palette(std::slice::from_ref(&frame), settings(3), &NeverCancel)
            .unwrap();
        let second = MostUsedQuantizer
            .build_global_palette(std::slice::from_ref(&frame), settings(3), &NeverCancel)
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.transparent_index(), Some(0));
        assert_eq!(
            palette_entries(&first),
            vec![[0, 0, 0], [255, 0, 0], [0, 255, 0]]
        );
    }

    #[test]
    fn every_strategy_supports_two_and_256_color_limits() {
        let transparent = frame_from_pixels(&[[255, 0, 0, 255], [0, 0, 0, 0]]);
        let gradient = frame_from_pixels(
            &(0_u8..=u8::MAX)
                .map(|gray| [gray, gray, gray, 255])
                .collect::<Vec<_>>(),
        );

        for strategy in STRATEGIES {
            let minimum = strategy
                .quantize(&transparent, settings(2), &NeverCancel)
                .unwrap();
            assert_eq!(minimum.palette().len() / 3, 2);
            assert_eq!(minimum.transparent_index(), Some(0));
            assert_eq!(minimum.indices(), [1, 0]);

            let maximum = strategy
                .quantize(&gradient, settings(256), &NeverCancel)
                .unwrap();
            assert!((2..=256).contains(&(maximum.palette().len() / 3)));
            assert!(
                maximum
                    .indices()
                    .iter()
                    .all(|&index| usize::from(index) < maximum.palette().len() / 3)
            );
        }
    }

    #[test]
    fn every_strategy_honors_cancellation() {
        let frame = frame_from_pixels(&[[255, 0, 0, 255], [0, 255, 0, 255]]);
        let cancellation = CancellationFlag::default();
        cancellation.cancel();

        for strategy in STRATEGIES {
            assert_eq!(
                strategy.quantize(&frame, settings(2), &cancellation),
                Err(QuantizationError::Cancelled)
            );
            assert_eq!(
                strategy.build_global_palette(
                    std::slice::from_ref(&frame),
                    settings(2),
                    &cancellation
                ),
                Err(QuantizationError::Cancelled)
            );
        }
    }

    #[test]
    fn every_strategy_rejects_color_limits_outside_gif_range() {
        let frame = frame_from_pixels(&[[255, 0, 0, 255]]);

        for strategy in STRATEGIES {
            for max_colors in [1, 257] {
                assert!(matches!(
                    strategy.quantize(&frame, settings(max_colors), &NeverCancel),
                    Err(QuantizationError::InvalidPalette(_))
                ));
            }
        }
    }

    #[test]
    fn bayer_dithers_a_midpoint() {
        let frame = RgbaFrame::new(8, 8, [128, 128, 128, 255].repeat(64), 10_000).unwrap();
        let palette = ColorPalette::new(vec![0, 0, 0, 255, 255, 255], None).unwrap();
        let none =
            map_frame_to_palette(&frame, &palette, None, DitherMode::None, &NeverCancel).unwrap();
        let bayer =
            map_frame_to_palette(&frame, &palette, None, DitherMode::Bayer4x4, &NeverCancel)
                .unwrap();

        assert!(none.iter().all(|&index| index == none[0]));
        assert!(bayer.contains(&0) && bayer.contains(&1));
        assert_ne!(bayer, none);
    }

    #[test]
    fn extended_kernel_layouts_match_their_published_matrices() {
        assert_conservative_kernel(
            JARVIS_JUDICE_NINKE,
            48,
            &[
                tap(1, 0, 7),
                tap(2, 0, 5),
                tap(-2, 1, 3),
                tap(-1, 1, 5),
                tap(0, 1, 7),
                tap(1, 1, 5),
                tap(2, 1, 3),
                tap(-2, 2, 1),
                tap(-1, 2, 3),
                tap(0, 2, 5),
                tap(1, 2, 3),
                tap(2, 2, 1),
            ],
        );
        assert_conservative_kernel(
            STUCKI,
            42,
            &[
                tap(1, 0, 8),
                tap(2, 0, 4),
                tap(-2, 1, 2),
                tap(-1, 1, 4),
                tap(0, 1, 8),
                tap(1, 1, 4),
                tap(2, 1, 2),
                tap(-2, 2, 1),
                tap(-1, 2, 2),
                tap(0, 2, 4),
                tap(1, 2, 2),
                tap(2, 2, 1),
            ],
        );
        assert_conservative_kernel(
            STEVENSON_ARCE,
            200,
            &[
                tap(2, 0, 32),
                tap(-3, 1, 12),
                tap(-1, 1, 26),
                tap(1, 1, 30),
                tap(3, 1, 16),
                tap(-2, 2, 12),
                tap(0, 2, 26),
                tap(2, 2, 12),
                tap(-3, 3, 5),
                tap(-1, 3, 12),
                tap(1, 3, 12),
                tap(3, 3, 5),
            ],
        );
    }

    #[test]
    fn extended_modes_match_golden_indices() {
        let grays = [
            60, 117, 48, 189, 183, 30, 52, 120, 134, 178, 180, 27, 145, 201, 118, 114, 3, 87, 251,
            107, 232, 170, 189, 99, 63, 38, 46, 37, 18, 94, 132, 85, 76, 150, 201, 120, 82, 208,
            198, 219, 176, 221, 182, 237, 74, 41, 227, 234,
        ];
        let pixels = grays
            .into_iter()
            .flat_map(|gray| [gray, gray, gray, 255])
            .collect();
        let frame = RgbaFrame::new(8, 6, pixels, 10_000).unwrap();
        let palette = ColorPalette::new(
            vec![0, 0, 0, 85, 85, 85, 170, 170, 170, 255, 255, 255],
            None,
        )
        .unwrap();

        let expected = [
            (
                DitherMode::JarvisJudiceNinke,
                [
                    1, 1, 1, 2, 2, 0, 1, 1, 2, 2, 2, 0, 2, 2, 1, 2, 0, 1, 3, 1, 3, 2, 2, 1, 1, 0,
                    1, 1, 0, 1, 2, 1, 1, 2, 2, 1, 1, 2, 2, 3, 2, 3, 2, 3, 1, 1, 3, 3,
                ],
            ),
            (
                DitherMode::Stucki,
                [
                    1, 1, 1, 2, 2, 0, 1, 1, 2, 2, 2, 0, 2, 2, 1, 2, 0, 1, 3, 1, 3, 2, 2, 1, 1, 0,
                    1, 1, 0, 1, 2, 1, 1, 2, 2, 1, 1, 3, 2, 3, 2, 3, 2, 3, 1, 0, 3, 3,
                ],
            ),
            (
                DitherMode::StevensonArce,
                [
                    1, 1, 1, 2, 2, 0, 1, 1, 2, 2, 2, 0, 2, 2, 2, 1, 0, 1, 3, 1, 3, 2, 2, 1, 1, 0,
                    1, 1, 0, 1, 2, 1, 1, 2, 2, 1, 1, 2, 2, 3, 2, 3, 2, 3, 1, 1, 3, 3,
                ],
            ),
        ];

        for (mode, expected_indices) in expected {
            let actual = map_frame_to_palette(&frame, &palette, None, mode, &NeverCancel).unwrap();
            assert_eq!(
                actual.as_slice(),
                expected_indices.as_slice(),
                "{mode:?} golden indices changed"
            );
        }
    }

    #[test]
    fn every_error_diffusion_mode_is_deterministic_bounded_and_dithers() {
        let frame = RgbaFrame::new(16, 16, [128, 128, 128, 255].repeat(256), 10_000).unwrap();
        let palette = ColorPalette::new(vec![0, 0, 0, 255, 255, 255], None).unwrap();
        let none =
            map_frame_to_palette(&frame, &palette, None, DitherMode::None, &NeverCancel).unwrap();

        for mode in ERROR_DIFFUSION_MODES {
            let first = map_frame_to_palette(&frame, &palette, None, mode, &NeverCancel).unwrap();
            let second = map_frame_to_palette(&frame, &palette, None, mode, &NeverCancel).unwrap();

            assert_eq!(first, second, "{mode:?} output must be deterministic");
            assert!(
                first.iter().all(|&index| index < 2),
                "{mode:?} emitted an out-of-range palette index"
            );
            assert!(
                first.contains(&0) && first.contains(&1),
                "{mode:?} did not use both colors"
            );
            assert_ne!(first, none, "{mode:?} matched non-dithered output");
        }
    }

    #[test]
    fn transparent_pixels_do_not_contaminate_error_diffusion() {
        let width = 16_u16;
        let height = 8_u16;
        let mut dark_hidden = Vec::with_capacity(usize::from(width * height) * 4);
        let mut light_hidden = Vec::with_capacity(usize::from(width * height) * 4);
        for index in 0..usize::from(width * height) {
            let x = index % usize::from(width);
            let transparent = x == 3 || x == 9;
            let alpha = if transparent { 0 } else { 255 };
            dark_hidden.extend_from_slice(&[if transparent { 0 } else { 128 }; 3]);
            dark_hidden.push(alpha);
            light_hidden.extend_from_slice(&[if transparent { 255 } else { 128 }; 3]);
            light_hidden.push(alpha);
        }
        let dark_hidden = RgbaFrame::new(width, height, dark_hidden, 10_000).unwrap();
        let light_hidden = RgbaFrame::new(width, height, light_hidden, 10_000).unwrap();
        let palette = ColorPalette::new(vec![0, 0, 0, 0, 0, 0, 255, 255, 255], Some(0)).unwrap();

        for mode in ERROR_DIFFUSION_MODES {
            let first = map_frame_to_palette(&dark_hidden, &palette, Some(128), mode, &NeverCancel)
                .unwrap();
            let second =
                map_frame_to_palette(&light_hidden, &palette, Some(128), mode, &NeverCancel)
                    .unwrap();

            assert_eq!(
                first, second,
                "{mode:?} leaked hidden transparent RGB into opaque pixels"
            );
            for (index, &palette_index) in first.iter().enumerate() {
                let x = index % usize::from(width);
                if x == 3 || x == 9 {
                    assert_eq!(palette_index, 0, "{mode:?} lost transparency");
                } else {
                    assert!(
                        (1..=2).contains(&palette_index),
                        "{mode:?} emitted an invalid opaque index"
                    );
                }
            }
        }
    }

    #[test]
    fn every_error_diffusion_mode_checks_cancellation_during_mapping() {
        let frame = RgbaFrame::new(128, 64, [128, 128, 128, 255].repeat(8_192), 10_000).unwrap();
        let palette = ColorPalette::new(vec![0, 0, 0, 255, 255, 255], None).unwrap();

        for mode in ERROR_DIFFUSION_MODES {
            // Building the fixed lookup performs eight checks. Allow its
            // checks plus the first mapping check, then cancel at pixel 4096.
            let cancellation = CancelAfterChecks::new(9);
            assert_eq!(
                map_frame_to_palette(&frame, &palette, None, mode, &cancellation),
                Err(QuantizationError::Cancelled),
                "{mode:?} ignored periodic cancellation"
            );
            assert!(cancellation.checks.load(Ordering::Relaxed) >= 10);
        }
    }
}
