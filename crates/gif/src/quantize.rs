use std::collections::{BTreeMap, BTreeSet};

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
/// [`MostUsed`](Self::MostUsed) favors the most frequent source colors, and
/// [`Octree`](Self::Octree) prunes a bounded RGB octree, [`Wu`](Self::Wu)
/// optimizes variance over a fixed 33³ moment lattice, and
/// [`NeuQuant`](Self::NeuQuant) trains a bounded Kohonen network over a
/// deterministic sample. The remaining variants use immutable predefined
/// palettes with a reserved transparency slot.
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
    /// A deterministic, population-pruned RGB octree.
    Octree,
    /// Variance-minimizing Wu quantization over a bounded RGB moment lattice.
    Wu,
    /// Bounded deterministic NeuQuant neural-network color reduction.
    NeuQuant,
    /// Fixed 216-color web-safe cube plus a reserved transparent entry.
    WebSafe216,
    /// Fixed black-and-white palette plus a reserved transparent entry.
    Monochrome,
    /// Fixed classic Windows 16-color palette plus a reserved transparent entry.
    Windows16,
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

/// Built-in fixed RGB palettes with publicly documented definitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PredefinedPalette {
    /// The 216-color web-safe cube, ordered RGB with channel levels
    /// `00`, `33`, `66`, `99`, `CC`, and `FF`.
    WebSafe216,
    /// Two-color black (`#000000`) and white (`#FFFFFF`).
    Monochrome,
    /// The classic 16-color Windows/HTML palette in conventional index order.
    Windows16,
}

impl PredefinedPalette {
    /// Returns the number of RGB entries in this palette.
    pub const fn color_count(self) -> usize {
        match self {
            Self::WebSafe216 => 216,
            Self::Monochrome => 2,
            Self::Windows16 => 16,
        }
    }

    /// Materializes tightly packed RGB entries in stable palette order.
    pub fn colors(self) -> Vec<u8> {
        match self {
            Self::WebSafe216 => {
                const LEVELS: [u8; 6] = [0x00, 0x33, 0x66, 0x99, 0xcc, 0xff];
                let mut colors = Vec::with_capacity(self.color_count() * 3);
                for red in LEVELS {
                    for green in LEVELS {
                        for blue in LEVELS {
                            colors.extend_from_slice(&[red, green, blue]);
                        }
                    }
                }
                colors
            }
            Self::Monochrome => vec![0, 0, 0, 255, 255, 255],
            Self::Windows16 => vec![
                0x00, 0x00, 0x00, // black
                0x80, 0x00, 0x00, // maroon
                0x00, 0x80, 0x00, // green
                0x80, 0x80, 0x00, // olive
                0x00, 0x00, 0x80, // navy
                0x80, 0x00, 0x80, // purple
                0x00, 0x80, 0x80, // teal
                0xc0, 0xc0, 0xc0, // silver
                0x80, 0x80, 0x80, // gray
                0xff, 0x00, 0x00, // red
                0x00, 0xff, 0x00, // lime
                0xff, 0xff, 0x00, // yellow
                0x00, 0x00, 0xff, // blue
                0xff, 0x00, 0xff, // fuchsia
                0x00, 0xff, 0xff, // aqua
                0xff, 0xff, 0xff, // white
            ],
        }
    }
}

/// Quantizer that maps every frame to one immutable caller-selected palette.
///
/// A designated transparent entry stays in the returned palette for every
/// local and global frame, even when a particular frame is opaque. Opaque
/// pixels never map to that entry. If transparency is required but no entry is
/// designated, quantization fails explicitly. Likewise, a palette larger than
/// [`QuantizationSettings::max_colors`] is rejected rather than truncated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedPaletteQuantizer {
    palette: ColorPalette,
}

impl FixedPaletteQuantizer {
    /// Validates and constructs a custom tightly packed RGB palette.
    ///
    /// `transparent_index` designates an existing entry; duplicate RGB values
    /// are allowed because an opaque entry may intentionally share its visible
    /// color with the transparent entry.
    ///
    /// # Errors
    ///
    /// Returns [`QuantizationError`] unless there are 2–256 complete RGB
    /// entries and the optional transparent index is in range.
    pub fn new(colors: Vec<u8>, transparent_index: Option<u8>) -> Result<Self, QuantizationError> {
        Ok(Self {
            palette: ColorPalette::new(colors, transparent_index)?,
        })
    }

    /// Creates an opaque built-in palette.
    pub fn from_predefined(predefined: PredefinedPalette) -> Self {
        Self {
            palette: ColorPalette {
                colors: predefined.colors(),
                transparent_index: None,
            },
        }
    }

    /// Prepends a designated transparent color to a built-in palette.
    ///
    /// # Errors
    ///
    /// Returns [`QuantizationError`] if adding the entry would exceed GIF's
    /// 256-color limit.
    pub fn from_predefined_with_transparency(
        predefined: PredefinedPalette,
        transparent_rgb: [u8; 3],
    ) -> Result<Self, QuantizationError> {
        let mut colors = Vec::with_capacity((predefined.color_count() + 1) * 3);
        colors.extend_from_slice(&transparent_rgb);
        colors.extend_from_slice(&predefined.colors());
        Self::new(colors, Some(0))
    }

    /// Borrows the immutable validated palette.
    pub const fn palette(&self) -> &ColorPalette {
        &self.palette
    }

    fn palette_for(
        &self,
        frames: &[RgbaFrame],
        settings: QuantizationSettings,
        cancellation: &dyn CancellationToken,
    ) -> Result<ColorPalette, QuantizationError> {
        validate_settings(settings)?;
        check_now(cancellation)?;
        if self.palette.color_count() > usize::from(settings.max_colors) {
            return Err(QuantizationError::FixedPaletteExceedsColorLimit {
                palette_colors: self.palette.color_count(),
                max_colors: settings.max_colors,
            });
        }
        if self.palette.transparent_index.is_none() {
            if settings.reserve_transparency {
                return Err(QuantizationError::FixedPaletteMissingTransparency);
            }
            let mut visited = 0_usize;
            for frame in frames {
                for pixel in frame.pixels().as_chunks::<4>().0 {
                    check_cancellation(visited, cancellation)?;
                    visited = visited.wrapping_add(1);
                    if is_transparent(pixel[3], settings.alpha_threshold) {
                        return Err(QuantizationError::FixedPaletteMissingTransparency);
                    }
                }
            }
        }
        check_now(cancellation)?;
        Ok(self.palette.clone())
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

/// Deterministic, bounded-memory RGB octree quantizer.
///
/// Input first enters the crate's fixed 5-bit/channel histogram. Its occupied
/// bins populate a depth-5 octree, whose complete shape is bounded by 37,449
/// nodes (`1 + 8 + … + 8^5`) regardless of frame dimensions or count. Starting
/// at depth 4 and moving toward the root, reduction collapses sibling leaves
/// into their population-weighted parent centroid. At a given depth, the least
/// populated parent is collapsed first; equal populations use the node's RGB
/// Morton path as a stable tie-break. The final frontier is ordered by its
/// left-aligned Morton prefix, so palette indices are reproducible as well.
#[derive(Clone, Copy, Debug, Default)]
pub struct OctreeQuantizer;

/// Deterministic, bounded-memory Wu color quantizer.
///
/// The source first enters the shared 5-bit/channel histogram. Five integral
/// moments (population, RGB sums, and squared magnitude) are then accumulated
/// over a 33×33×33 lattice. Palette boxes are split by the greatest reduction
/// in within-box variance; ties retain stable box, R/G/B axis, and ascending-cut
/// order. The histogram and moment lattice have fixed size independent of input
/// frame dimensions and count.
#[derive(Clone, Copy, Debug, Default)]
pub struct WuQuantizer;

/// Deterministic NeuQuant adapter with bounded training memory.
///
/// Transparent pixels are excluded from training and reserve palette index
/// zero through the shared palette finalizer. Opaque inputs are sampled evenly
/// in presentation order to at most 65,536 pixels. The permissive
/// `color_quant` implementation is always called with its documented color
/// range of 64–256 and sample factor range of 1–30; smaller requested palettes
/// are produced by frequency-pruning the trained codebook with RGB ordering as
/// a stable tie-break. Final opaque colors are sorted lexicographically, so
/// palette indices are deterministic.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeuQuantQuantizer;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct HistogramBin {
    red_sum: u64,
    green_sum: u64,
    blue_sum: u64,
    squared_sum: u64,
    count: u64,
}

impl HistogramBin {
    fn add(&mut self, red: u8, green: u8, blue: u8) -> Result<(), QuantizationError> {
        let squared = u64::from(red) * u64::from(red)
            + u64::from(green) * u64::from(green)
            + u64::from(blue) * u64::from(blue);
        let red_sum = self
            .red_sum
            .checked_add(u64::from(red))
            .ok_or_else(histogram_moment_overflow)?;
        let green_sum = self
            .green_sum
            .checked_add(u64::from(green))
            .ok_or_else(histogram_moment_overflow)?;
        let blue_sum = self
            .blue_sum
            .checked_add(u64::from(blue))
            .ok_or_else(histogram_moment_overflow)?;
        let squared_sum = self
            .squared_sum
            .checked_add(squared)
            .ok_or_else(histogram_moment_overflow)?;
        let count = self
            .count
            .checked_add(1)
            .ok_or_else(histogram_moment_overflow)?;
        self.red_sum = red_sum;
        self.green_sum = green_sum;
        self.blue_sum = blue_sum;
        self.squared_sum = squared_sum;
        self.count = count;
        Ok(())
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

fn histogram_moment_overflow() -> QuantizationError {
    QuantizationError::InvalidPalette(
        "RGB histogram moment overflowed its u64 accumulator".to_owned(),
    )
}

#[derive(Clone, Copy, Debug)]
struct ColorPoint {
    rgb: [u8; 3],
    count: u64,
}

const OCTREE_DEPTH: u8 = HISTOGRAM_CHANNEL_BITS as u8;
// A complete 8-way tree through depth five has
// (8^(5 + 1) - 1) / (8 - 1) nodes.
const OCTREE_MAX_NODES: usize = 37_449;
const OCTREE_REDUCTION_CANCELLATION_INTERVAL: usize = 256;

#[derive(Clone, Debug)]
struct OctreeNode {
    children: [Option<usize>; 8],
    red_sum: u64,
    green_sum: u64,
    blue_sum: u64,
    count: u64,
    depth: u8,
    path: u32,
}

impl OctreeNode {
    const fn new(depth: u8, path: u32) -> Self {
        Self {
            children: [None; 8],
            red_sum: 0,
            green_sum: 0,
            blue_sum: 0,
            count: 0,
            depth,
            path,
        }
    }

    fn add_bin(&mut self, bin: HistogramBin) {
        self.red_sum += bin.red_sum;
        self.green_sum += bin.green_sum;
        self.blue_sum += bin.blue_sum;
        self.count += bin.count;
    }

    fn centroid(&self) -> [u8; 3] {
        debug_assert_ne!(self.count, 0);
        [
            (self.red_sum / self.count) as u8,
            (self.green_sum / self.count) as u8,
            (self.blue_sum / self.count) as u8,
        ]
    }

    fn palette_order_key(&self) -> u32 {
        self.path << (3 * u32::from(OCTREE_DEPTH - self.depth))
    }
}

fn build_octree(
    histogram: &[HistogramBin],
    cancellation: &dyn CancellationToken,
) -> Result<Vec<OctreeNode>, QuantizationError> {
    debug_assert_eq!(histogram.len(), HISTOGRAM_LEN);
    check_now(cancellation)?;

    let mut nodes = Vec::with_capacity(OCTREE_MAX_NODES);
    nodes.push(OctreeNode::new(0, 0));

    for (histogram_index, &bin) in histogram.iter().enumerate() {
        check_cancellation(histogram_index, cancellation)?;
        if bin.count == 0 {
            continue;
        }

        nodes[0].add_bin(bin);
        let color = bin.average();
        let mut node_index = 0;
        for depth in 1..=OCTREE_DEPTH {
            let branch = octree_branch(color, depth);
            let child_index = match nodes[node_index].children[branch] {
                Some(index) => index,
                None => {
                    if nodes.len() >= OCTREE_MAX_NODES {
                        return Err(QuantizationError::InvalidPalette(
                            "octree exceeded its fixed node bound".to_owned(),
                        ));
                    }
                    let index = nodes.len();
                    let path = (nodes[node_index].path << 3) | branch as u32;
                    nodes.push(OctreeNode::new(depth, path));
                    nodes[node_index].children[branch] = Some(index);
                    index
                }
            };
            nodes[child_index].add_bin(bin);
            node_index = child_index;
        }
    }

    debug_assert!(nodes.len() <= OCTREE_MAX_NODES);
    Ok(nodes)
}

fn reduce_octree(
    nodes: &[OctreeNode],
    color_limit: usize,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<[u8; 3]>, QuantizationError> {
    let palette_nodes = reduce_octree_frontier(nodes, color_limit, cancellation)?;
    Ok(palette_nodes
        .into_iter()
        .map(|index| nodes[index].centroid())
        .collect())
}

fn reduce_octree_frontier(
    nodes: &[OctreeNode],
    color_limit: usize,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<usize>, QuantizationError> {
    check_now(cancellation)?;
    if nodes[0].count == 0 {
        return Ok(Vec::new());
    }

    let mut active = vec![false; nodes.len()];
    let mut active_count = 0_usize;
    for (index, node) in nodes.iter().enumerate() {
        if node.depth == OCTREE_DEPTH {
            active[index] = true;
            active_count += 1;
        }
    }

    let mut visited_candidates = 0_usize;
    'depths: for depth in (0..OCTREE_DEPTH).rev() {
        if active_count <= color_limit {
            break;
        }
        check_now(cancellation)?;
        let mut candidates = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.depth == depth)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|&index| (nodes[index].count, nodes[index].path));

        for parent_index in candidates {
            if visited_candidates.is_multiple_of(OCTREE_REDUCTION_CANCELLATION_INTERVAL) {
                check_now(cancellation)?;
            }
            visited_candidates = visited_candidates.wrapping_add(1);
            let active_children = nodes[parent_index]
                .children
                .iter()
                .flatten()
                .copied()
                .filter(|&child_index| active[child_index])
                .collect::<Vec<_>>();
            if active_children.is_empty() {
                continue;
            }
            debug_assert!(!active[parent_index]);
            debug_assert!(
                nodes[parent_index]
                    .children
                    .iter()
                    .flatten()
                    .all(|&child_index| active[child_index])
            );
            debug_assert_eq!(
                active_children
                    .iter()
                    .map(|&child_index| nodes[child_index].count)
                    .sum::<u64>(),
                nodes[parent_index].count
            );

            for child_index in &active_children {
                active[*child_index] = false;
            }
            active[parent_index] = true;
            active_count = active_count + 1 - active_children.len();
            if active_count <= color_limit {
                break 'depths;
            }
        }
    }

    check_now(cancellation)?;
    let mut palette_nodes = active
        .iter()
        .enumerate()
        .filter(|(_, is_active)| **is_active)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    palette_nodes.sort_unstable_by_key(|&index| {
        let node = &nodes[index];
        (node.palette_order_key(), node.depth, node.path)
    });
    debug_assert!(palette_nodes.len() <= color_limit);

    Ok(palette_nodes)
}

fn octree_branch(color: [u8; 3], depth: u8) -> usize {
    debug_assert!((1..=OCTREE_DEPTH).contains(&depth));
    let shift = 8 - depth;
    usize::from((color[0] >> shift) & 1) << 2
        | usize::from((color[1] >> shift) & 1) << 1
        | usize::from((color[2] >> shift) & 1)
}

const WU_SIDE: usize = HISTOGRAM_CHANNEL_SIZE + 1;
const WU_MOMENT_LEN: usize = WU_SIDE * WU_SIDE * WU_SIDE;
const WU_CANCELLATION_INTERVAL: usize = 256;

#[derive(Clone, Copy, Debug, Default)]
struct WuMoment {
    weight: u64,
    red: u64,
    green: u64,
    blue: u64,
    squared: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WuCube {
    red_min: u8,
    red_max: u8,
    green_min: u8,
    green_max: u8,
    blue_min: u8,
    blue_max: u8,
}

impl WuCube {
    const FULL: Self = Self {
        red_min: 0,
        red_max: HISTOGRAM_CHANNEL_SIZE as u8,
        green_min: 0,
        green_max: HISTOGRAM_CHANNEL_SIZE as u8,
        blue_min: 0,
        blue_max: HISTOGRAM_CHANNEL_SIZE as u8,
    };
}

#[derive(Clone, Copy, Debug)]
enum WuAxis {
    Red,
    Green,
    Blue,
}

#[derive(Clone, Copy, Debug)]
struct WuSplit {
    cube_index: usize,
    left: WuCube,
    right: WuCube,
    gain: f64,
}

fn build_wu_moments(
    histogram: &[HistogramBin],
    cancellation: &dyn CancellationToken,
) -> Result<Vec<WuMoment>, QuantizationError> {
    if histogram.len() != HISTOGRAM_LEN {
        return Err(QuantizationError::InvalidPalette(format!(
            "Wu histogram has {} bins, expected {HISTOGRAM_LEN}",
            histogram.len()
        )));
    }
    let mut moments = Vec::new();
    moments.try_reserve_exact(WU_MOMENT_LEN).map_err(|_| {
        QuantizationError::InvalidPalette(format!(
            "could not allocate bounded {WU_MOMENT_LEN}-cell Wu moment lattice"
        ))
    })?;
    moments.resize(WU_MOMENT_LEN, WuMoment::default());

    let mut visited = 0_usize;
    for red in 1..WU_SIDE {
        for green in 1..WU_SIDE {
            for blue in 1..WU_SIDE {
                if visited.is_multiple_of(WU_CANCELLATION_INTERVAL) {
                    check_now(cancellation)?;
                }
                visited = visited.wrapping_add(1);
                let bin = histogram[histogram_cell(red - 1, green - 1, blue - 1)];
                let raw = WuMoment {
                    weight: bin.count,
                    red: bin.red_sum,
                    green: bin.green_sum,
                    blue: bin.blue_sum,
                    squared: bin.squared_sum,
                };
                let prefix = combine_wu_moments(&[
                    (raw, 1),
                    (moments[wu_cell(red - 1, green, blue)], 1),
                    (moments[wu_cell(red, green - 1, blue)], 1),
                    (moments[wu_cell(red, green, blue - 1)], 1),
                    (moments[wu_cell(red - 1, green - 1, blue)], -1),
                    (moments[wu_cell(red - 1, green, blue - 1)], -1),
                    (moments[wu_cell(red, green - 1, blue - 1)], -1),
                    (moments[wu_cell(red - 1, green - 1, blue - 1)], 1),
                ])?;
                moments[wu_cell(red, green, blue)] = prefix;
            }
        }
    }
    check_now(cancellation)?;
    Ok(moments)
}

fn combine_wu_moments(terms: &[(WuMoment, i8)]) -> Result<WuMoment, QuantizationError> {
    let component = |read: fn(WuMoment) -> u64| {
        let value = terms.iter().fold(0_i128, |sum, (moment, sign)| {
            sum + i128::from(read(*moment)) * i128::from(*sign)
        });
        u64::try_from(value).map_err(|_| {
            QuantizationError::InvalidPalette(
                "Wu integral-moment arithmetic exceeded its unsigned range".to_owned(),
            )
        })
    };
    Ok(WuMoment {
        weight: component(|moment| moment.weight)?,
        red: component(|moment| moment.red)?,
        green: component(|moment| moment.green)?,
        blue: component(|moment| moment.blue)?,
        squared: component(|moment| moment.squared)?,
    })
}

fn wu_volume(moments: &[WuMoment], cube: WuCube) -> Result<WuMoment, QuantizationError> {
    // Wu's 33³ prefix lattice represents each quantized box as
    // (minimum, maximum] on every axis. This is the eight-corner volume
    // formula from Xiaolin Wu, "Efficient Statistical Computations for
    // Optimal Color Quantization" (Graphics Gems II).
    let r0 = usize::from(cube.red_min);
    let r1 = usize::from(cube.red_max);
    let g0 = usize::from(cube.green_min);
    let g1 = usize::from(cube.green_max);
    let b0 = usize::from(cube.blue_min);
    let b1 = usize::from(cube.blue_max);
    combine_wu_moments(&[
        (moments[wu_cell(r1, g1, b1)], 1),
        (moments[wu_cell(r1, g1, b0)], -1),
        (moments[wu_cell(r1, g0, b1)], -1),
        (moments[wu_cell(r0, g1, b1)], -1),
        (moments[wu_cell(r1, g0, b0)], 1),
        (moments[wu_cell(r0, g1, b0)], 1),
        (moments[wu_cell(r0, g0, b1)], 1),
        (moments[wu_cell(r0, g0, b0)], -1),
    ])
}

fn best_wu_split(
    cubes: &[WuCube],
    moments: &[WuMoment],
    cancellation: &dyn CancellationToken,
) -> Result<Option<WuSplit>, QuantizationError> {
    let mut best = None;
    let mut visited = 0_usize;
    // Strict `>` updates make exact ties deterministic: the existing box is
    // chosen first, then R/G/B here, then the ascending cut.
    for (cube_index, &cube) in cubes.iter().enumerate() {
        let parent_variance = wu_variance(wu_volume(moments, cube)?);
        for axis in [WuAxis::Red, WuAxis::Green, WuAxis::Blue] {
            let (minimum, maximum) = wu_axis_bounds(cube, axis);
            for cut in minimum + 1..maximum {
                if visited.is_multiple_of(WU_CANCELLATION_INTERVAL) {
                    check_now(cancellation)?;
                }
                visited = visited.wrapping_add(1);
                let (left, right) = split_wu_cube(cube, axis, cut);
                let left_moment = wu_volume(moments, left)?;
                let right_moment = wu_volume(moments, right)?;
                if left_moment.weight == 0 || right_moment.weight == 0 {
                    continue;
                }
                let gain = parent_variance - wu_variance(left_moment) - wu_variance(right_moment);
                if gain > 0.0 && best.is_none_or(|current: WuSplit| gain > current.gain) {
                    best = Some(WuSplit {
                        cube_index,
                        left,
                        right,
                        gain,
                    });
                }
            }
        }
    }
    Ok(best)
}

fn make_wu_palette(
    histogram: &[HistogramBin],
    color_limit: usize,
    cancellation: &dyn CancellationToken,
) -> Result<Vec<[u8; 3]>, QuantizationError> {
    check_now(cancellation)?;
    let moments = build_wu_moments(histogram, cancellation)?;
    if wu_volume(&moments, WuCube::FULL)?.weight == 0 {
        return Ok(Vec::new());
    }
    let mut cubes = Vec::with_capacity(color_limit);
    cubes.push(WuCube::FULL);
    while cubes.len() < color_limit {
        check_now(cancellation)?;
        let Some(split) = best_wu_split(&cubes, &moments, cancellation)? else {
            break;
        };
        cubes[split.cube_index] = split.left;
        cubes.push(split.right);
    }
    let mut palette = Vec::with_capacity(cubes.len());
    for (index, cube) in cubes.into_iter().enumerate() {
        check_cancellation(index, cancellation)?;
        let moment = wu_volume(&moments, cube)?;
        if moment.weight != 0 {
            palette.push(wu_centroid(moment));
        }
    }
    palette.sort_unstable();
    palette.dedup();
    check_now(cancellation)?;
    Ok(palette)
}

const fn histogram_cell(red: usize, green: usize, blue: usize) -> usize {
    red * HISTOGRAM_CHANNEL_SIZE * HISTOGRAM_CHANNEL_SIZE + green * HISTOGRAM_CHANNEL_SIZE + blue
}

const fn wu_cell(red: usize, green: usize, blue: usize) -> usize {
    red * WU_SIDE * WU_SIDE + green * WU_SIDE + blue
}

const fn wu_axis_bounds(cube: WuCube, axis: WuAxis) -> (u8, u8) {
    match axis {
        WuAxis::Red => (cube.red_min, cube.red_max),
        WuAxis::Green => (cube.green_min, cube.green_max),
        WuAxis::Blue => (cube.blue_min, cube.blue_max),
    }
}

const fn split_wu_cube(mut cube: WuCube, axis: WuAxis, cut: u8) -> (WuCube, WuCube) {
    let mut right = cube;
    match axis {
        WuAxis::Red => {
            cube.red_max = cut;
            right.red_min = cut;
        }
        WuAxis::Green => {
            cube.green_max = cut;
            right.green_min = cut;
        }
        WuAxis::Blue => {
            cube.blue_max = cut;
            right.blue_min = cut;
        }
    }
    (cube, right)
}

fn wu_mean_score(moment: WuMoment) -> f64 {
    if moment.weight == 0 {
        return 0.0;
    }
    let red = moment.red as f64;
    let green = moment.green as f64;
    let blue = moment.blue as f64;
    (red * red + green * green + blue * blue) / moment.weight as f64
}

fn wu_variance(moment: WuMoment) -> f64 {
    if moment.weight == 0 {
        0.0
    } else {
        (moment.squared as f64 - wu_mean_score(moment)).max(0.0)
    }
}

fn wu_centroid(moment: WuMoment) -> [u8; 3] {
    debug_assert_ne!(moment.weight, 0);
    let channel = |sum: u64| u8::try_from((sum / moment.weight).min(255)).unwrap_or(255);
    [
        channel(moment.red),
        channel(moment.green),
        channel(moment.blue),
    ]
}

const NEUQUANT_MIN_TRAINING_COLORS: usize = 64;
const NEUQUANT_MAX_TRAINING_PIXELS: usize = 65_536;
const NEUQUANT_DEFAULT_SAMPLE_FACTOR: i32 = 10;
const NEUQUANT_SMALL_INPUT_PIXELS: usize = 1_000;

struct NeuQuantInput {
    training_rgba: Vec<u8>,
    exact_colors: Option<Vec<[u8; 3]>>,
    has_transparency: bool,
}

fn collect_neuquant_input(
    frames: &[RgbaFrame],
    settings: QuantizationSettings,
    cancellation: &dyn CancellationToken,
) -> Result<NeuQuantInput, QuantizationError> {
    validate_settings(settings)?;
    check_now(cancellation)?;
    let exact_color_cap = usize::from(settings.max_colors);
    let mut exact_colors = Some(BTreeSet::new());
    let mut has_transparency = settings.reserve_transparency;
    let mut opaque_pixels = 0_u64;
    let mut visited = 0_usize;
    for frame in frames {
        for pixel in frame.pixels().as_chunks::<4>().0 {
            check_cancellation(visited, cancellation)?;
            visited = visited.wrapping_add(1);
            if is_transparent(pixel[3], settings.alpha_threshold) {
                has_transparency = true;
                continue;
            }
            opaque_pixels = opaque_pixels.checked_add(1).ok_or_else(|| {
                QuantizationError::InvalidPalette("opaque pixel count overflowed u64".to_owned())
            })?;
            if let Some(colors) = &mut exact_colors {
                colors.insert([pixel[0], pixel[1], pixel[2]]);
                if colors.len() > exact_color_cap {
                    exact_colors = None;
                }
            }
        }
    }

    let opaque_limit = opaque_color_limit(settings.max_colors, has_transparency);
    if exact_colors
        .as_ref()
        .is_some_and(|colors| colors.len() > opaque_limit)
    {
        exact_colors = None;
    }
    if let Some(colors) = exact_colors {
        return Ok(NeuQuantInput {
            training_rgba: Vec::new(),
            exact_colors: Some(colors.into_iter().collect()),
            has_transparency,
        });
    }

    let sample_pixels = usize::try_from(
        opaque_pixels.min(u64::try_from(NEUQUANT_MAX_TRAINING_PIXELS).unwrap_or(u64::MAX)),
    )
    .map_err(|_| {
        QuantizationError::InvalidPalette("NeuQuant sample length exceeds usize".to_owned())
    })?;
    let sample_bytes = sample_pixels.checked_mul(4).ok_or_else(|| {
        QuantizationError::InvalidPalette("NeuQuant sample byte length overflowed".to_owned())
    })?;
    let mut training_rgba = Vec::new();
    training_rgba.try_reserve_exact(sample_bytes).map_err(|_| {
        QuantizationError::InvalidPalette(format!(
            "could not allocate bounded {sample_bytes}-byte NeuQuant sample"
        ))
    })?;
    let mut opaque_index = 0_u64;
    let sample_pixels_u128 = u128::try_from(sample_pixels).map_err(|_| {
        QuantizationError::InvalidPalette("NeuQuant sample length exceeds u128".to_owned())
    })?;
    visited = 0;
    for frame in frames {
        for pixel in frame.pixels().as_chunks::<4>().0 {
            check_cancellation(visited, cancellation)?;
            visited = visited.wrapping_add(1);
            if is_transparent(pixel[3], settings.alpha_threshold) {
                continue;
            }
            let before = u128::from(opaque_index) * sample_pixels_u128 / u128::from(opaque_pixels);
            opaque_index += 1;
            let after = u128::from(opaque_index) * sample_pixels_u128 / u128::from(opaque_pixels);
            if after != before {
                training_rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
    }
    debug_assert_eq!(training_rgba.len(), sample_bytes);
    Ok(NeuQuantInput {
        training_rgba,
        exact_colors: None,
        has_transparency,
    })
}

fn make_neuquant_palette(
    frames: &[RgbaFrame],
    settings: QuantizationSettings,
    cancellation: &dyn CancellationToken,
) -> Result<ColorPalette, QuantizationError> {
    let input = collect_neuquant_input(frames, settings, cancellation)?;
    if let Some(colors) = input.exact_colors {
        return finish_palette(colors, input.has_transparency);
    }

    let opaque_limit = opaque_color_limit(settings.max_colors, input.has_transparency);
    let training_colors = opaque_limit.clamp(NEUQUANT_MIN_TRAINING_COLORS, 256);
    let training_pixels = input.training_rgba.len() / 4;
    let sample_factor = if training_pixels < NEUQUANT_SMALL_INPUT_PIXELS {
        1
    } else {
        NEUQUANT_DEFAULT_SAMPLE_FACTOR
    };
    debug_assert!((1..=30).contains(&sample_factor));
    debug_assert!((NEUQUANT_MIN_TRAINING_COLORS..=256).contains(&training_colors));
    check_now(cancellation)?;
    let quantizer =
        color_quant::NeuQuant::new(sample_factor, training_colors, &input.training_rgba);
    check_now(cancellation)?;

    let color_map = quantizer.color_map_rgb();
    if color_map.len() != training_colors * 3 {
        return Err(QuantizationError::InvalidPalette(format!(
            "NeuQuant produced {} RGB bytes for {training_colors} colors",
            color_map.len()
        )));
    }
    let mut usage = BTreeMap::<[u8; 3], u64>::new();
    for color in color_map.as_chunks::<3>().0 {
        usage.entry([color[0], color[1], color[2]]).or_default();
    }
    for (pixel_index, pixel) in input.training_rgba.as_chunks::<4>().0.iter().enumerate() {
        check_cancellation(pixel_index, cancellation)?;
        let color_index = quantizer.index_of(pixel);
        let offset = color_index.checked_mul(3).ok_or_else(|| {
            QuantizationError::InvalidPalette("NeuQuant palette index overflowed".to_owned())
        })?;
        let color = color_map.get(offset..offset + 3).ok_or_else(|| {
            QuantizationError::InvalidPalette(format!(
                "NeuQuant returned out-of-range color index {color_index}"
            ))
        })?;
        *usage.entry([color[0], color[1], color[2]]).or_default() += 1;
    }
    let mut candidates = usage.into_iter().collect::<Vec<_>>();
    candidates.sort_unstable_by(|(left_color, left_count), (right_color, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_color.cmp(right_color))
    });
    let mut colors = candidates
        .into_iter()
        .take(opaque_limit)
        .map(|(color, _)| color)
        .collect::<Vec<_>>();
    colors.sort_unstable();
    check_now(cancellation)?;
    finish_palette(colors, input.has_transparency)
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

impl FrameQuantizer for OctreeQuantizer {
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
        let opaque_limit = opaque_color_limit(settings.max_colors, has_transparency);
        let nodes = build_octree(&histogram, cancellation)?;
        let opaque_palette = reduce_octree(&nodes, opaque_limit, cancellation)?;

        finish_palette(opaque_palette, has_transparency)
    }
}

impl FrameQuantizer for WuQuantizer {
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
        let opaque_limit = opaque_color_limit(settings.max_colors, has_transparency);
        let opaque_palette = make_wu_palette(&histogram, opaque_limit, cancellation)?;
        finish_palette(opaque_palette, has_transparency)
    }
}

impl FrameQuantizer for NeuQuantQuantizer {
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
        make_neuquant_palette(frames, settings, cancellation)
    }
}

impl FrameQuantizer for FixedPaletteQuantizer {
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
        self.palette_for(frames, settings, cancellation)
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
            Self::Octree => OctreeQuantizer.quantize(frame, settings, cancellation),
            Self::Wu => WuQuantizer.quantize(frame, settings, cancellation),
            Self::NeuQuant => NeuQuantQuantizer.quantize(frame, settings, cancellation),
            Self::WebSafe216 => predefined_strategy_quantizer(PredefinedPalette::WebSafe216)?
                .quantize(frame, settings, cancellation),
            Self::Monochrome => predefined_strategy_quantizer(PredefinedPalette::Monochrome)?
                .quantize(frame, settings, cancellation),
            Self::Windows16 => predefined_strategy_quantizer(PredefinedPalette::Windows16)?
                .quantize(frame, settings, cancellation),
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
            Self::Octree => OctreeQuantizer.build_global_palette(frames, settings, cancellation),
            Self::Wu => WuQuantizer.build_global_palette(frames, settings, cancellation),
            Self::NeuQuant => {
                NeuQuantQuantizer.build_global_palette(frames, settings, cancellation)
            }
            Self::WebSafe216 => predefined_strategy_quantizer(PredefinedPalette::WebSafe216)?
                .build_global_palette(frames, settings, cancellation),
            Self::Monochrome => predefined_strategy_quantizer(PredefinedPalette::Monochrome)?
                .build_global_palette(frames, settings, cancellation),
            Self::Windows16 => predefined_strategy_quantizer(PredefinedPalette::Windows16)?
                .build_global_palette(frames, settings, cancellation),
        }
    }
}

fn predefined_strategy_quantizer(
    palette: PredefinedPalette,
) -> Result<FixedPaletteQuantizer, QuantizationError> {
    FixedPaletteQuantizer::from_predefined_with_transparency(palette, [0, 0, 0])
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
            histogram[index].add(pixel[0], pixel[1], pixel[2])?;
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

    const STRATEGIES: [QuantizerStrategy; 6] = [
        QuantizerStrategy::MedianCut,
        QuantizerStrategy::Grayscale,
        QuantizerStrategy::MostUsed,
        QuantizerStrategy::Octree,
        QuantizerStrategy::Wu,
        QuantizerStrategy::NeuQuant,
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

    fn photo_like_frame(phase: u32) -> RgbaFrame {
        const WIDTH: u16 = 48;
        const HEIGHT: u16 = 32;
        let mut pixels = Vec::with_capacity(usize::from(WIDTH) * usize::from(HEIGHT) * 4);
        for y in 0..u32::from(HEIGHT) {
            for x in 0..u32::from(WIDTH) {
                let red = (x * 255 / (u32::from(WIDTH) - 1) + y * 3 + phase * 17) % 256;
                let green = (y * 255 / (u32::from(HEIGHT) - 1) + x * 2 + phase * 29) % 256;
                let blue = ((x + y) * 255 / (u32::from(WIDTH) + u32::from(HEIGHT) - 2)
                    + (x * y + phase * 31) % 97)
                    % 256;
                pixels.extend_from_slice(&[
                    u8::try_from(red).unwrap(),
                    u8::try_from(green).unwrap(),
                    u8::try_from(blue).unwrap(),
                    255,
                ]);
            }
        }
        RgbaFrame::new(WIDTH, HEIGHT, pixels, 10_000).unwrap()
    }

    fn complete_octree_histogram() -> Vec<HistogramBin> {
        let mut histogram = vec![HistogramBin::default(); HISTOGRAM_LEN];
        for (index, bin) in histogram.iter_mut().enumerate() {
            let red =
                (((index / (HISTOGRAM_CHANNEL_SIZE * HISTOGRAM_CHANNEL_SIZE)) << 3) | 4) as u8;
            let green =
                ((((index / HISTOGRAM_CHANNEL_SIZE) % HISTOGRAM_CHANNEL_SIZE) << 3) | 4) as u8;
            let blue = (((index % HISTOGRAM_CHANNEL_SIZE) << 3) | 4) as u8;
            bin.add(red, green, blue).unwrap();
        }
        histogram
    }

    fn assert_valid_octree_frontier(nodes: &[OctreeNode], frontier: &[usize]) {
        let mut active = vec![false; nodes.len()];
        for &index in frontier {
            assert!(!active[index], "frontier contains node {index} twice");
            assert_ne!(nodes[index].count, 0);
            active[index] = true;
        }

        fn visit(
            node_index: usize,
            nodes: &[OctreeNode],
            active: &[bool],
            has_active_ancestor: bool,
        ) {
            assert!(
                !(has_active_ancestor && active[node_index]),
                "frontier contains an ancestor and descendant"
            );
            let is_covered = has_active_ancestor || active[node_index];
            let children = nodes[node_index]
                .children
                .iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            if children.is_empty() {
                assert!(is_covered, "terminal octree leaf is not covered");
            } else {
                for child_index in children {
                    visit(child_index, nodes, active, is_covered);
                }
            }
        }

        visit(0, nodes, &active, false);
        assert_eq!(
            frontier
                .iter()
                .map(|&index| nodes[index].count)
                .sum::<u64>(),
            nodes[0].count,
            "frontier population does not equal root population"
        );
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
    fn predefined_palettes_match_their_public_color_definitions() {
        assert_eq!(
            PredefinedPalette::Monochrome.colors(),
            [0, 0, 0, 255, 255, 255]
        );
        assert_eq!(
            PredefinedPalette::Windows16.colors(),
            [
                0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x80, 0x80, 0x00, 0x00, 0x00,
                0x80, 0x80, 0x00, 0x80, 0x00, 0x80, 0x80, 0xc0, 0xc0, 0xc0, 0x80, 0x80, 0x80, 0xff,
                0x00, 0x00, 0x00, 0xff, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0xff,
                0x00, 0xff, 0xff, 0xff, 0xff, 0xff,
            ]
        );
        let web_safe = PredefinedPalette::WebSafe216.colors();
        assert_eq!(web_safe.len(), 216 * 3);
        assert_eq!(&web_safe[0..3], &[0x00, 0x00, 0x00]);
        assert_eq!(&web_safe[3..6], &[0x00, 0x00, 0x33]);
        assert_eq!(&web_safe[18..21], &[0x00, 0x33, 0x00]);
        assert_eq!(&web_safe[108..111], &[0x33, 0x00, 0x00]);
        assert_eq!(&web_safe[645..648], &[0xff, 0xff, 0xff]);
    }

    #[test]
    fn fixed_palette_constructor_strictly_validates_entries_and_transparency() {
        assert!(matches!(
            FixedPaletteQuantizer::new(vec![0, 0, 0], None),
            Err(QuantizationError::InvalidPalette(_))
        ));
        assert!(matches!(
            FixedPaletteQuantizer::new(vec![0; 7], None),
            Err(QuantizationError::InvalidPalette(_))
        ));
        assert!(matches!(
            FixedPaletteQuantizer::new(vec![0; 257 * 3], None),
            Err(QuantizationError::InvalidPalette(_))
        ));
        assert_eq!(
            FixedPaletteQuantizer::new(vec![0, 0, 0, 255, 255, 255], Some(2)),
            Err(QuantizationError::PaletteIndexOutOfBounds {
                index: 2,
                colors: 2,
            })
        );
    }

    #[test]
    fn fixed_palette_keeps_transparent_and_identical_opaque_entries_distinct() {
        let quantizer = FixedPaletteQuantizer::new(
            vec![
                0, 0, 0, // transparent black
                0, 0, 0, // opaque black
                255, 255, 255,
            ],
            Some(0),
        )
        .unwrap();
        let frame = frame_from_pixels(&[[77, 88, 99, 0], [0, 0, 0, 255], [255, 255, 255, 255]]);
        let indexed = quantizer
            .quantize(&frame, settings(3), &NeverCancel)
            .unwrap();
        assert_eq!(indexed.palette(), quantizer.palette().colors());
        assert_eq!(indexed.transparent_index(), Some(0));
        assert_eq!(indexed.indices(), [0, 1, 2]);
        assert_eq!(
            quantizer
                .build_global_palette(&[frame], settings(3), &NeverCancel)
                .unwrap(),
            quantizer.palette().clone()
        );
    }

    #[test]
    fn fixed_palette_rejects_limit_conflicts_and_missing_transparency() {
        let windows = FixedPaletteQuantizer::from_predefined(PredefinedPalette::Windows16);
        let opaque = frame_from_pixels(&[[12, 34, 56, 255]]);
        assert_eq!(
            windows.build_global_palette(&[opaque], settings(15), &NeverCancel),
            Err(QuantizationError::FixedPaletteExceedsColorLimit {
                palette_colors: 16,
                max_colors: 15,
            })
        );

        let monochrome = FixedPaletteQuantizer::from_predefined(PredefinedPalette::Monochrome);
        let transparent = frame_from_pixels(&[[1, 2, 3, 0]]);
        assert_eq!(
            monochrome.quantize(&transparent, settings(2), &NeverCancel),
            Err(QuantizationError::FixedPaletteMissingTransparency)
        );
        let mut reserved = settings(2);
        reserved.reserve_transparency = true;
        assert_eq!(
            monochrome.build_global_palette(&[], reserved, &NeverCancel),
            Err(QuantizationError::FixedPaletteMissingTransparency)
        );
    }

    #[test]
    fn fixed_palette_uses_existing_ordered_and_error_diffusion_mappers() {
        let quantizer = FixedPaletteQuantizer::from_predefined(PredefinedPalette::Monochrome);
        let frame = RgbaFrame::new(8, 8, [128, 128, 128, 255].repeat(64), 10_000).unwrap();
        let none = map_frame_to_palette(
            &frame,
            quantizer.palette(),
            None,
            DitherMode::None,
            &NeverCancel,
        )
        .unwrap();
        for dither in [DitherMode::Bayer4x4, DitherMode::FloydSteinberg] {
            let mapped =
                map_frame_to_palette(&frame, quantizer.palette(), None, dither, &NeverCancel)
                    .unwrap();
            assert!(mapped.contains(&0) && mapped.contains(&1));
            assert_ne!(mapped, none);
        }
    }

    #[test]
    fn fixed_palette_honors_cancellation() {
        let quantizer = FixedPaletteQuantizer::from_predefined(PredefinedPalette::Monochrome);
        let cancellation = CancellationFlag::default();
        cancellation.cancel();
        let frame = frame_from_pixels(&[[128, 128, 128, 255]]);
        assert_eq!(
            quantizer.quantize(&frame, settings(2), &cancellation),
            Err(QuantizationError::Cancelled)
        );
        assert_eq!(
            quantizer.build_global_palette(&[frame], settings(2), &cancellation),
            Err(QuantizationError::Cancelled)
        );

        let large = RgbaFrame::new(128, 64, [128, 128, 128, 255].repeat(8_192), 10_000).unwrap();
        let during_scan = CancelAfterChecks::new(2);
        assert_eq!(
            quantizer.build_global_palette(&[large], settings(2), &during_scan),
            Err(QuantizationError::Cancelled)
        );
        assert!(during_scan.checks.load(Ordering::Relaxed) >= 3);
    }

    #[test]
    fn octree_palette_is_golden_stable_and_preserves_transparency() {
        let first_frame = frame_from_pixels(&[
            [10, 20, 240, 255],
            [10, 240, 20, 255],
            [240, 10, 20, 255],
            [240, 240, 10, 255],
            [250, 250, 250, 0],
        ]);
        let second_frame = frame_from_pixels(&[
            [20, 30, 250, 255],
            [20, 250, 30, 255],
            [250, 20, 30, 255],
            [250, 250, 20, 255],
        ]);
        let forward = OctreeQuantizer
            .build_global_palette(
                &[first_frame.clone(), second_frame.clone()],
                settings(5),
                &NeverCancel,
            )
            .unwrap();
        let reversed = OctreeQuantizer
            .build_global_palette(&[second_frame, first_frame], settings(5), &NeverCancel)
            .unwrap();

        assert_eq!(forward, reversed);
        assert_eq!(forward.transparent_index(), Some(0));
        assert_eq!(
            palette_entries(&forward),
            vec![
                [0, 0, 0],
                [15, 25, 245],
                [15, 245, 25],
                [245, 15, 25],
                [245, 245, 15],
            ]
        );
    }

    #[test]
    fn octree_color_corpus_has_bounded_error_and_independent_selection() {
        let mut pixels = Vec::with_capacity(16 * 16 * 4);
        for y in 0_u8..16 {
            for x in 0_u8..16 {
                pixels.extend_from_slice(&[x * 17, y * 17, ((x * 5 + y * 11) % 16) * 17, 255]);
            }
        }
        let frame = RgbaFrame::new(16, 16, pixels, 10_000).unwrap();
        let settings = settings(16);
        let octree = OctreeQuantizer
            .build_global_palette(std::slice::from_ref(&frame), settings, &NeverCancel)
            .unwrap();
        let median = MedianCutQuantizer
            .build_global_palette(std::slice::from_ref(&frame), settings, &NeverCancel)
            .unwrap();

        let palette_by_index = palette_entries(&octree);
        let mut octree_colors = palette_by_index.clone();
        let mut median_colors = palette_entries(&median);
        octree_colors.sort_unstable();
        median_colors.sort_unstable();
        assert_ne!(octree_colors, median_colors);
        assert!((2..=16).contains(&octree.color_count()));

        let indices =
            map_frame_to_palette(&frame, &octree, None, DitherMode::None, &NeverCancel).unwrap();
        let squared_error = frame
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(indices)
            .map(|(pixel, palette_index)| {
                let color = palette_by_index[usize::from(palette_index)];
                (0..3)
                    .map(|channel| i64::from(pixel[channel]) - i64::from(color[channel]))
                    .map(|error| (error * error) as u64)
                    .sum::<u64>()
            })
            .sum::<u64>();
        let mean_squared_channel_error = squared_error / (16 * 16 * 3);
        assert!(mean_squared_channel_error <= 1_500);
    }

    #[test]
    fn octree_has_a_fixed_complete_tree_bound() {
        let histogram = complete_octree_histogram();
        let nodes = build_octree(&histogram, &NeverCancel).unwrap();
        assert_eq!(nodes.len(), OCTREE_MAX_NODES);
        let palette = reduce_octree(&nodes, 256, &NeverCancel).unwrap();
        assert!(!palette.is_empty());
        assert!(palette.len() <= 256);
    }

    #[test]
    fn octree_frontier_always_partitions_the_root_population() {
        let nodes = build_octree(&complete_octree_histogram(), &NeverCancel).unwrap();
        for color_limit in [2, 16, 256, 4_094, 30_000, 32_768] {
            let frontier = reduce_octree_frontier(&nodes, color_limit, &NeverCancel).unwrap();
            assert!(frontier.len() <= color_limit);
            assert_valid_octree_frontier(&nodes, &frontier);

            if color_limit == 256 {
                let first_depth = nodes[frontier[0]].depth;
                assert!(
                    frontier
                        .iter()
                        .any(|&index| nodes[index].depth != first_depth),
                    "the regression case must stop partway through a depth"
                );
            }
        }
    }

    #[test]
    fn octree_checks_cancellation_during_build_and_reduction() {
        let histogram = complete_octree_histogram();
        let build_cancellation = CancelAfterChecks::new(3);
        assert!(matches!(
            build_octree(&histogram, &build_cancellation),
            Err(QuantizationError::Cancelled)
        ));

        let nodes = build_octree(&histogram, &NeverCancel).unwrap();
        let reduction_cancellation = CancelAfterChecks::new(3);
        assert!(matches!(
            reduce_octree(&nodes, 2, &reduction_cancellation),
            Err(QuantizationError::Cancelled)
        ));
        assert!(reduction_cancellation.checks.load(Ordering::Relaxed) >= 4);
    }

    #[test]
    fn wu_prefix_boundaries_include_each_histogram_bin_exactly_once() {
        let mut histogram = vec![HistogramBin::default(); HISTOGRAM_LEN];
        histogram[histogram_cell(0, 0, 0)].add(1, 2, 3).unwrap();
        histogram[histogram_cell(1, 0, 0)].add(9, 10, 11).unwrap();
        histogram[histogram_cell(31, 31, 31)]
            .add(255, 254, 253)
            .unwrap();
        let moments = build_wu_moments(&histogram, &NeverCancel).unwrap();
        assert_eq!(moments.len(), WU_MOMENT_LEN);

        let first_red_bin = WuCube {
            red_min: 0,
            red_max: 1,
            green_min: 0,
            green_max: 1,
            blue_min: 0,
            blue_max: 1,
        };
        assert_eq!(
            wu_volume(&moments, first_red_bin).unwrap().weight,
            1,
            "lower-exclusive/upper-inclusive moment coordinates are off by one"
        );
        let first_two_red_bins = WuCube {
            red_max: 2,
            ..first_red_bin
        };
        assert_eq!(wu_volume(&moments, first_two_red_bins).unwrap().weight, 2);
        let total = wu_volume(&moments, WuCube::FULL).unwrap();
        assert_eq!(total.weight, 3);
        assert_eq!(total.red, 265);
        assert_eq!(total.green, 266);
        assert_eq!(total.blue, 267);
        assert_eq!(
            total.squared,
            1 + 4 + 9 + 81 + 100 + 121 + 65_025 + 64_516 + 64_009
        );
    }

    #[test]
    fn wu_split_ties_use_box_then_rgb_axis_then_ascending_cut() {
        let mut histogram = vec![HistogramBin::default(); HISTOGRAM_LEN];
        for red in [0, 255] {
            for green in [0, 255] {
                for blue in [0, 255] {
                    histogram[histogram_index(red, green, blue)]
                        .add(red, green, blue)
                        .unwrap();
                }
            }
        }
        let moments = build_wu_moments(&histogram, &NeverCancel).unwrap();
        let split = best_wu_split(&[WuCube::FULL, WuCube::FULL], &moments, &NeverCancel)
            .unwrap()
            .unwrap();
        assert_eq!(split.cube_index, 0);
        assert_eq!(split.left.red_max, 1);
        assert_eq!(split.left.green_max, HISTOGRAM_CHANNEL_SIZE as u8);
        assert_eq!(split.left.blue_max, HISTOGRAM_CHANNEL_SIZE as u8);
    }

    #[test]
    fn wu_palette_is_golden_stable_and_reserves_transparency_zero() {
        let first_frame = frame_from_pixels(&[
            [10, 20, 240, 255],
            [10, 240, 20, 255],
            [240, 10, 20, 255],
            [240, 240, 10, 255],
            [250, 250, 250, 0],
        ]);
        let second_frame = frame_from_pixels(&[
            [20, 30, 250, 255],
            [20, 250, 30, 255],
            [250, 20, 30, 255],
            [250, 250, 20, 255],
        ]);
        let forward = WuQuantizer
            .build_global_palette(
                &[first_frame.clone(), second_frame.clone()],
                settings(5),
                &NeverCancel,
            )
            .unwrap();
        let reversed = WuQuantizer
            .build_global_palette(&[second_frame, first_frame], settings(5), &NeverCancel)
            .unwrap();
        assert_eq!(forward, reversed);
        assert_eq!(forward.transparent_index(), Some(0));
        assert_eq!(
            palette_entries(&forward),
            vec![
                [0, 0, 0],
                [15, 25, 245],
                [15, 245, 25],
                [245, 15, 25],
                [245, 245, 15],
            ]
        );
    }

    #[test]
    fn wu_photo_corpus_has_bounded_error_and_independent_palette() {
        let frames = [photo_like_frame(0), photo_like_frame(1)];
        let settings = settings(32);
        let wu = WuQuantizer
            .build_global_palette(&frames, settings, &NeverCancel)
            .unwrap();
        let repeated = WuQuantizer
            .build_global_palette(&frames, settings, &NeverCancel)
            .unwrap();
        assert_eq!(wu, repeated);
        assert!(wu.color_count() <= 32);

        let median = MedianCutQuantizer
            .build_global_palette(&frames, settings, &NeverCancel)
            .unwrap();
        let octree = OctreeQuantizer
            .build_global_palette(&frames, settings, &NeverCancel)
            .unwrap();
        assert_ne!(palette_entries(&wu), palette_entries(&median));
        assert_ne!(palette_entries(&wu), palette_entries(&octree));

        let palette = palette_entries(&wu);
        let mut squared_error = 0_u64;
        let mut channels = 0_u64;
        for frame in &frames {
            let indices =
                map_frame_to_palette(frame, &wu, None, DitherMode::None, &NeverCancel).unwrap();
            for (pixel, index) in frame.pixels().as_chunks::<4>().0.iter().zip(indices) {
                let color = palette[usize::from(index)];
                for channel in 0..3 {
                    let error = i64::from(pixel[channel]) - i64::from(color[channel]);
                    squared_error += u64::try_from(error * error).unwrap();
                    channels += 1;
                }
            }
        }
        let mean_squared_channel_error = squared_error / channels;
        assert!(
            mean_squared_channel_error <= 700,
            "Wu color corpus MSE was {mean_squared_channel_error}"
        );
    }

    #[test]
    fn wu_memory_bound_empty_single_color_and_overflow_are_explicit() {
        assert_eq!(WU_MOMENT_LEN, 33 * 33 * 33);
        assert!(std::mem::size_of::<WuMoment>() * WU_MOMENT_LEN < 2 * 1024 * 1024);

        let empty = WuQuantizer
            .build_global_palette(&[], settings(2), &NeverCancel)
            .unwrap();
        assert_eq!(empty.color_count(), 2);
        let single = frame_from_pixels(&[[12, 34, 56, 255]; 4]);
        let palette = WuQuantizer
            .build_global_palette(&[single], settings(256), &NeverCancel)
            .unwrap();
        assert_eq!(palette.color_count(), 2);
        assert!(palette_entries(&palette).contains(&[12, 34, 56]));

        let maximum = make_wu_palette(&complete_octree_histogram(), 256, &NeverCancel).unwrap();
        assert_eq!(maximum.len(), 256);

        let mut overflowing = HistogramBin {
            count: u64::MAX,
            ..HistogramBin::default()
        };
        assert!(matches!(
            overflowing.add(1, 2, 3),
            Err(QuantizationError::InvalidPalette(message)) if message.contains("overflowed")
        ));
        assert_eq!(
            overflowing,
            HistogramBin {
                count: u64::MAX,
                ..HistogramBin::default()
            }
        );
    }

    #[test]
    fn wu_checks_cancellation_during_moments_and_split_search() {
        let histogram = complete_octree_histogram();
        let during_moments = CancelAfterChecks::new(2);
        assert!(matches!(
            build_wu_moments(&histogram, &during_moments),
            Err(QuantizationError::Cancelled)
        ));

        let moments = build_wu_moments(&histogram, &NeverCancel).unwrap();
        let during_split = CancelAfterChecks::new(0);
        assert!(matches!(
            best_wu_split(&[WuCube::FULL], &moments, &during_split),
            Err(QuantizationError::Cancelled)
        ));
    }

    #[test]
    fn neuquant_is_deterministic_bounded_and_selects_independently() {
        let frames = [photo_like_frame(0), photo_like_frame(1)];
        let settings = settings(32);
        let first = NeuQuantQuantizer
            .build_global_palette(&frames, settings, &NeverCancel)
            .unwrap();
        let second = NeuQuantQuantizer
            .build_global_palette(&frames, settings, &NeverCancel)
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.transparent_index(), None);
        assert!((2..=32).contains(&first.color_count()));
        assert!(
            palette_entries(&first)
                .windows(2)
                .all(|colors| colors[0] <= colors[1])
        );

        let median = MedianCutQuantizer
            .build_global_palette(&frames, settings, &NeverCancel)
            .unwrap();
        let octree = OctreeQuantizer
            .build_global_palette(&frames, settings, &NeverCancel)
            .unwrap();
        let mut neuquant_colors = palette_entries(&first);
        let mut median_colors = palette_entries(&median);
        let mut octree_colors = palette_entries(&octree);
        neuquant_colors.sort_unstable();
        median_colors.sort_unstable();
        octree_colors.sort_unstable();
        assert_ne!(neuquant_colors, median_colors);
        assert_ne!(neuquant_colors, octree_colors);

        let palette_by_index = palette_entries(&first);
        let mut squared_error = 0_u64;
        let mut channel_count = 0_u64;
        for frame in &frames {
            let indices =
                map_frame_to_palette(frame, &first, None, DitherMode::None, &NeverCancel).unwrap();
            for (pixel, palette_index) in frame.pixels().as_chunks::<4>().0.iter().zip(indices) {
                let color = palette_by_index[usize::from(palette_index)];
                for channel in 0..3 {
                    let error = i64::from(pixel[channel]) - i64::from(color[channel]);
                    squared_error += u64::try_from(error * error).unwrap();
                    channel_count += 1;
                }
            }
        }
        let mean_squared_channel_error = squared_error / channel_count;
        assert!(
            mean_squared_channel_error <= 800,
            "NeuQuant color corpus MSE was {mean_squared_channel_error}"
        );
    }

    #[test]
    fn neuquant_reserves_zero_for_transparency_without_opaque_collisions() {
        const WIDTH: u16 = 40;
        const HEIGHT: u16 = 20;
        let mut pixels = Vec::with_capacity(usize::from(WIDTH) * usize::from(HEIGHT) * 4);
        for index in 0..usize::from(WIDTH) * usize::from(HEIGHT) {
            let value = u8::try_from(index % 256).unwrap();
            let transparent = index.is_multiple_of(11);
            pixels.extend_from_slice(&[
                value,
                value.wrapping_mul(3),
                value.wrapping_mul(7),
                if transparent { 0 } else { 255 },
            ]);
        }
        let frame = RgbaFrame::new(WIDTH, HEIGHT, pixels, 10_000).unwrap();
        let indexed = NeuQuantQuantizer
            .quantize(&frame, settings(16), &NeverCancel)
            .unwrap();
        assert_eq!(indexed.transparent_index(), Some(0));
        assert!(indexed.palette().len() / 3 <= 16);
        for (pixel_index, (&palette_index, pixel)) in indexed
            .indices()
            .iter()
            .zip(frame.pixels().as_chunks::<4>().0)
            .enumerate()
        {
            if pixel[3] == 0 {
                assert_eq!(palette_index, 0, "transparent pixel {pixel_index}");
            } else {
                assert_ne!(palette_index, 0, "opaque pixel {pixel_index}");
                assert!(usize::from(palette_index) < indexed.palette().len() / 3);
            }
        }
    }

    #[test]
    fn neuquant_sampling_is_bounded_and_checks_cancellation_mid_scan() {
        const WIDTH: u16 = 257;
        const HEIGHT: u16 = 256;
        let pixels = (0..usize::from(WIDTH) * usize::from(HEIGHT))
            .flat_map(|index| {
                let value = u8::try_from(index % 256).unwrap();
                [value, value.wrapping_mul(17), value.wrapping_mul(31), 255]
            })
            .collect();
        let frame = RgbaFrame::new(WIDTH, HEIGHT, pixels, 10_000).unwrap();
        let input =
            collect_neuquant_input(std::slice::from_ref(&frame), settings(16), &NeverCancel)
                .unwrap();
        assert_eq!(input.training_rgba.len(), NEUQUANT_MAX_TRAINING_PIXELS * 4);
        assert!(input.exact_colors.is_none());
        assert!((1..=30).contains(&NEUQUANT_DEFAULT_SAMPLE_FACTOR));
        assert!((64..=256).contains(&NEUQUANT_MIN_TRAINING_COLORS));

        let cancellation = CancelAfterChecks::new(2);
        assert!(matches!(
            collect_neuquant_input(std::slice::from_ref(&frame), settings(16), &cancellation),
            Err(QuantizationError::Cancelled)
        ));
        assert!(cancellation.checks.load(Ordering::Relaxed) >= 3);
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
