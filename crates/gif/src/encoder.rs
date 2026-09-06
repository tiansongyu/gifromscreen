use std::io::Write;

use crate::quantize::{indexed_to_rgba, map_frame_to_palette};
use crate::{
    CancellationToken, ColorPalette, DitherMode, EncodePhase, EncodeProgress, FrameQuantizer,
    GifEncodeError, GifTimingQuantizer, IteratorFrameSource, NeverCancel, NoopProgress,
    ProgressSink, QuantizationError, QuantizationSettings, QuantizerStrategy, RgbaFrame,
    RgbaFrameSource,
};

pub const DEFAULT_GLOBAL_PALETTE_BUFFER_LIMIT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LoopBehavior {
    /// Play the animation once; no Netscape loop extension is written.
    Once,
    /// Repeat forever.
    #[default]
    Infinite,
    /// Netscape repeat count. This mirrors `gif::Repeat::Finite`; zero is
    /// rejected because GIF represents infinite repetition separately.
    Finite(u16),
}

/// Palette ownership strategy.
///
/// This is non-exhaustive so global/custom palette modes can be introduced
/// without leaking the container crate through the public interface.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PaletteMode {
    #[default]
    LocalPerFrame,
    /// Analyze all retained frames and write one GIF global color table.
    /// One-shot sources buffer up to `EncodeOptions::global_palette_buffer_limit_bytes`.
    /// A precomputed global palette allows bounded streaming instead.
    Global,
}

/// Frame rectangle/disposal optimization strategy.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeltaMode {
    /// Encode every frame as a full-canvas image and use Keep disposal.
    #[default]
    FullFrames,
    /// Write the bounding rectangle of pixels that changed after quantization.
    ChangedRectangles,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transparency {
    /// Ignore source alpha and encode every pixel as opaque.
    Opaque,
    /// Encode pixels with alpha strictly below the threshold as transparent.
    AlphaThreshold(u8),
}

impl Default for Transparency {
    fn default() -> Self {
        Self::AlphaThreshold(1)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodeOptions {
    /// Maximum number of palette entries, including transparency.
    pub max_colors: u16,
    pub loop_behavior: LoopBehavior,
    pub merge_duplicate_frames: bool,
    pub transparency: Transparency,
    pub palette_mode: PaletteMode,
    /// Built-in palette quantizer used when the encoder has no custom
    /// [`FrameQuantizer`].
    pub quantizer: QuantizerStrategy,
    pub delta_mode: DeltaMode,
    pub dither: DitherMode,
    /// Maximum RGBA pixel bytes retained by a one-shot global encoder source.
    /// Precomputed palettes and replay-based project exports do not use this
    /// full-sequence buffer; their analysis workspaces are independently bounded.
    pub global_palette_buffer_limit_bytes: u64,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            max_colors: 256,
            loop_behavior: LoopBehavior::Infinite,
            merge_duplicate_frames: true,
            transparency: Transparency::default(),
            palette_mode: PaletteMode::default(),
            quantizer: QuantizerStrategy::default(),
            delta_mode: DeltaMode::default(),
            dither: DitherMode::default(),
            global_palette_buffer_limit_bytes: DEFAULT_GLOBAL_PALETTE_BUFFER_LIMIT_BYTES,
        }
    }
}

impl EncodeOptions {
    /// Validates color and loop settings before rendering or analysis starts.
    ///
    /// # Errors
    ///
    /// Returns an invalid-color-count or invalid-repeat-count error.
    pub fn validate(&self) -> Result<(), GifEncodeError> {
        if !(2..=256).contains(&self.max_colors) {
            return Err(GifEncodeError::InvalidColorCount(self.max_colors));
        }
        if matches!(self.loop_behavior, LoopBehavior::Finite(0)) {
            return Err(GifEncodeError::InvalidRepeatCount);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EncodeReport {
    pub input_frames: u64,
    /// Number of image descriptors written. This can exceed the number of
    /// unique input frames when a delay must be split at GIF's `u16` limit.
    pub encoded_frames: u64,
    pub duplicate_frames_merged: u64,
    pub input_duration_us: u64,
    pub encoded_duration_ticks: u64,
    /// Logical frames encoded as a rectangle smaller than the canvas.
    pub delta_frames: u64,
}

/// Object-safe application port for GIF encoding.
pub trait GifEncoder: Send + Sync {
    fn encode(
        &self,
        source: &mut dyn RgbaFrameSource,
        output: &mut dyn Write,
        options: &EncodeOptions,
        cancellation: &dyn CancellationToken,
        progress: &mut dyn ProgressSink,
    ) -> Result<EncodeReport, GifEncodeError>;
}

/// Built-in permissive GIF89a/LZW adapter backed by `image-gif`.
#[derive(Default)]
pub struct BuiltinGifEncoder {
    custom_quantizer: Option<Box<dyn FrameQuantizer>>,
    prepared_global_palette: Option<ColorPalette>,
}

impl std::fmt::Debug for BuiltinGifEncoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BuiltinGifEncoder")
            .field(
                "quantizer",
                &if self.custom_quantizer.is_some() {
                    "custom FrameQuantizer"
                } else {
                    "EncodeOptions::quantizer"
                },
            )
            .finish()
    }
}

impl BuiltinGifEncoder {
    /// Create an encoder whose custom quantizer overrides
    /// [`EncodeOptions::quantizer`].
    pub fn new(quantizer: Box<dyn FrameQuantizer>) -> Self {
        Self {
            custom_quantizer: Some(quantizer),
            prepared_global_palette: None,
        }
    }

    /// Uses a precomputed global table and streams source frames without the
    /// one-shot global RGBA buffer. The caller is responsible for analyzing an
    /// identical immutable sequence. Local mode retains its normal behavior.
    pub fn with_global_palette(palette: ColorPalette) -> Self {
        Self {
            custom_quantizer: None,
            prepared_global_palette: Some(palette),
        }
    }

    fn quantizer<'a>(&'a self, options: &'a EncodeOptions) -> &'a dyn FrameQuantizer {
        self.custom_quantizer
            .as_deref()
            .unwrap_or(&options.quantizer)
    }

    /// Convenience entry point for an owned, infallible frame iterator.
    pub fn encode_frames<I, W>(
        &self,
        frames: I,
        output: &mut W,
        options: &EncodeOptions,
    ) -> Result<EncodeReport, GifEncodeError>
    where
        I: IntoIterator<Item = RgbaFrame>,
        I::IntoIter: Send,
        W: Write,
    {
        let mut source = IteratorFrameSource::new(frames.into_iter());
        let mut progress = NoopProgress;
        self.encode(&mut source, output, options, &NeverCancel, &mut progress)
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_streaming(
        &self,
        source: &mut dyn RgbaFrameSource,
        output: &mut dyn Write,
        options: &EncodeOptions,
        cancellation: &dyn CancellationToken,
        progress: &mut dyn ProgressSink,
        mut report: EncodeReport,
        total_frames_hint: Option<u64>,
        global_palette: Option<&ColorPalette>,
    ) -> Result<EncodeReport, GifEncodeError> {
        if let Some(palette) = global_palette {
            validate_palette_limit(palette, options.max_colors)?;
        }
        let first = read_frame(
            source,
            cancellation,
            progress,
            &mut report,
            total_frames_hint,
        )?
        .ok_or(GifEncodeError::NoFrames)?;
        let width = first.width();
        let height = first.height();
        let loop_first = first.clone();
        let mut writer = gif::Encoder::new(
            output,
            width,
            height,
            global_palette.map_or(&[], ColorPalette::colors),
        )?;
        let storage = if global_palette.is_some() {
            PaletteStorage::Global
        } else {
            PaletteStorage::Local
        };
        configure_loop(&mut writer, options.loop_behavior)?;

        let mut timing = GifTimingQuantizer::new();
        let mut pending = first;
        let mut previous_target = None;
        let mut force_full = true;

        while let Some(frame) = read_frame(
            source,
            cancellation,
            progress,
            &mut report,
            total_frames_hint,
        )? {
            validate_dimensions(&frame, report.input_frames, width, height)?;
            if options.merge_duplicate_frames && pending.pixels() == frame.pixels() {
                pending
                    .merge_duration(frame.duration_us())
                    .map_err(|()| GifEncodeError::DurationOverflow)?;
                report.duplicate_frames_merged += 1;
                continue;
            }

            let clear_after = transition_needs_background_clear(
                &pending,
                &frame,
                alpha_threshold(options.transparency),
            );
            let prepared = self.prepare_streaming_frame(
                &pending,
                options,
                clear_after,
                cancellation,
                progress,
                report,
                total_frames_hint,
                global_palette,
            )?;
            let target = prepared.target_rgba.clone();
            write_prepared_frame(
                &mut writer,
                pending,
                prepared,
                previous_target.as_deref(),
                force_full || clear_after,
                clear_after,
                options,
                cancellation,
                progress,
                &mut report,
                total_frames_hint,
                &mut timing,
                storage,
            )?;
            previous_target = Some(target);
            force_full = clear_after;
            pending = frame;
        }

        let loop_next = repeats(options.loop_behavior).then_some(&loop_first);
        let clear_after = loop_next.is_some_and(|next| {
            transition_needs_background_clear(&pending, next, alpha_threshold(options.transparency))
        });
        let prepared = self.prepare_streaming_frame(
            &pending,
            options,
            clear_after,
            cancellation,
            progress,
            report,
            total_frames_hint,
            global_palette,
        )?;
        write_prepared_frame(
            &mut writer,
            pending,
            prepared,
            previous_target.as_deref(),
            force_full || clear_after,
            clear_after,
            options,
            cancellation,
            progress,
            &mut report,
            total_frames_hint,
            &mut timing,
            storage,
        )?;
        finish(
            writer,
            report,
            timing,
            cancellation,
            progress,
            total_frames_hint,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_streaming_frame(
        &self,
        frame: &RgbaFrame,
        options: &EncodeOptions,
        reserve_transparency: bool,
        cancellation: &dyn CancellationToken,
        progress: &mut dyn ProgressSink,
        report: EncodeReport,
        total_frames_hint: Option<u64>,
        global_palette: Option<&ColorPalette>,
    ) -> Result<PreparedFrame, GifEncodeError> {
        if let Some(palette) = global_palette {
            prepare_global_frame(
                frame,
                palette,
                options,
                cancellation,
                progress,
                report,
                total_frames_hint,
            )
        } else {
            prepare_local_frame(
                self.quantizer(options),
                frame,
                options,
                reserve_transparency,
                cancellation,
                progress,
                report,
                total_frames_hint,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_global(
        &self,
        source: &mut dyn RgbaFrameSource,
        output: &mut dyn Write,
        options: &EncodeOptions,
        cancellation: &dyn CancellationToken,
        progress: &mut dyn ProgressSink,
        mut report: EncodeReport,
        total_frames_hint: Option<u64>,
    ) -> Result<EncodeReport, GifEncodeError> {
        let frames = collect_global_frames(
            source,
            options,
            cancellation,
            progress,
            &mut report,
            total_frames_hint,
        )?;
        let width = frames[0].width();
        let height = frames[0].height();
        let threshold = alpha_threshold(options.transparency);
        let reserve_transparency = frames
            .iter()
            .any(|frame| frame_has_transparency(frame, threshold));

        report_progress(
            progress,
            EncodePhase::AnalyzingPalette,
            report,
            total_frames_hint,
        );
        let palette = self
            .quantizer(options)
            .build_global_palette(
                &frames,
                QuantizationSettings {
                    max_colors: options.max_colors,
                    alpha_threshold: threshold,
                    reserve_transparency,
                },
                cancellation,
            )
            .map_err(map_quantization_error)?;
        validate_palette_limit(&palette, options.max_colors)?;

        let mut writer = gif::Encoder::new(output, width, height, palette.colors())?;
        configure_loop(&mut writer, options.loop_behavior)?;
        let mut timing = GifTimingQuantizer::new();
        let mut previous_target = None;
        let mut force_full = true;

        for (index, frame) in frames.iter().enumerate() {
            let next = frames.get(index + 1).or_else(|| {
                repeats(options.loop_behavior)
                    .then(|| frames.first())
                    .flatten()
            });
            let clear_after =
                next.is_some_and(|next| transition_needs_background_clear(frame, next, threshold));
            let prepared = prepare_global_frame(
                frame,
                &palette,
                options,
                cancellation,
                progress,
                report,
                total_frames_hint,
            )?;
            let target = prepared.target_rgba.clone();
            write_prepared_frame(
                &mut writer,
                frame.clone(),
                prepared,
                previous_target.as_deref(),
                force_full || clear_after,
                clear_after,
                options,
                cancellation,
                progress,
                &mut report,
                total_frames_hint,
                &mut timing,
                PaletteStorage::Global,
            )?;
            previous_target = Some(target);
            force_full = clear_after;
        }
        finish(
            writer,
            report,
            timing,
            cancellation,
            progress,
            total_frames_hint,
        )
    }
}

impl GifEncoder for BuiltinGifEncoder {
    fn encode(
        &self,
        source: &mut dyn RgbaFrameSource,
        output: &mut dyn Write,
        options: &EncodeOptions,
        cancellation: &dyn CancellationToken,
        progress: &mut dyn ProgressSink,
    ) -> Result<EncodeReport, GifEncodeError> {
        options.validate()?;
        check_cancelled(cancellation)?;

        let total_frames_hint = source.frame_count_hint();
        let report = EncodeReport::default();
        report_progress(progress, EncodePhase::Reading, report, total_frames_hint);
        match options.palette_mode {
            PaletteMode::LocalPerFrame => self.encode_streaming(
                source,
                output,
                options,
                cancellation,
                progress,
                report,
                total_frames_hint,
                None,
            ),
            PaletteMode::Global if self.prepared_global_palette.is_some() => self.encode_streaming(
                source,
                output,
                options,
                cancellation,
                progress,
                report,
                total_frames_hint,
                self.prepared_global_palette.as_ref(),
            ),
            PaletteMode::Global => self.encode_global(
                source,
                output,
                options,
                cancellation,
                progress,
                report,
                total_frames_hint,
            ),
        }
    }
}

fn read_frame(
    source: &mut dyn RgbaFrameSource,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn ProgressSink,
    report: &mut EncodeReport,
    total_frames_hint: Option<u64>,
) -> Result<Option<RgbaFrame>, GifEncodeError> {
    check_cancelled(cancellation)?;
    let frame = source.next_frame()?;
    if let Some(frame) = &frame {
        report.input_frames = report
            .input_frames
            .checked_add(1)
            .ok_or(GifEncodeError::DurationOverflow)?;
        report.input_duration_us = report
            .input_duration_us
            .checked_add(frame.duration_us())
            .ok_or(GifEncodeError::DurationOverflow)?;
        report_progress(progress, EncodePhase::Reading, *report, total_frames_hint);
    }
    Ok(frame)
}

fn validate_dimensions(
    frame: &RgbaFrame,
    frame_index_after_read: u64,
    expected_width: u16,
    expected_height: u16,
) -> Result<(), GifEncodeError> {
    if frame.width() == expected_width && frame.height() == expected_height {
        return Ok(());
    }
    Err(GifEncodeError::DimensionMismatch {
        frame_index: frame_index_after_read - 1,
        expected_width,
        expected_height,
        actual_width: frame.width(),
        actual_height: frame.height(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaletteStorage {
    Local,
    Global,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameRect {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
}

impl FrameRect {
    const fn full(width: u16, height: u16) -> Self {
        Self {
            left: 0,
            top: 0,
            width,
            height,
        }
    }

    const fn is_full(self, width: u16, height: u16) -> bool {
        self.left == 0 && self.top == 0 && self.width == width && self.height == height
    }
}

struct PreparedFrame {
    palette: Vec<u8>,
    indices: Vec<u8>,
    transparent_index: Option<u8>,
    target_rgba: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
fn prepare_local_frame(
    quantizer: &dyn FrameQuantizer,
    frame: &RgbaFrame,
    options: &EncodeOptions,
    clear_after: bool,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn ProgressSink,
    report: EncodeReport,
    total_frames_hint: Option<u64>,
) -> Result<PreparedFrame, GifEncodeError> {
    check_cancelled(cancellation)?;
    report_progress(progress, EncodePhase::Quantizing, report, total_frames_hint);
    let threshold = alpha_threshold(options.transparency);
    let reserve_transparency = clear_after || frame_has_transparency(frame, threshold);
    let indexed = quantizer
        .quantize(
            frame,
            QuantizationSettings {
                max_colors: options.max_colors,
                alpha_threshold: threshold,
                reserve_transparency,
            },
            cancellation,
        )
        .map_err(map_quantization_error)?;
    let (palette, mut indices, transparent_index) = indexed.into_parts();
    validate_raw_palette_limit(&palette, options.max_colors)?;
    if reserve_transparency && transparent_index.is_none() {
        return Err(GifEncodeError::Quantization(
            QuantizationError::InvalidPalette(
                "this frame needs a reserved transparent entry for disposal".to_owned(),
            ),
        ));
    }
    if options.dither != DitherMode::None {
        let color_palette = ColorPalette::new(palette.clone(), transparent_index)
            .map_err(map_quantization_error)?;
        indices = map_frame_to_palette(
            frame,
            &color_palette,
            threshold,
            options.dither,
            cancellation,
        )
        .map_err(map_quantization_error)?;
    }
    let target_rgba =
        indexed_to_rgba(&indices, &palette, transparent_index).map_err(map_quantization_error)?;
    Ok(PreparedFrame {
        palette,
        indices,
        transparent_index,
        target_rgba,
    })
}

#[allow(clippy::too_many_arguments)]
fn prepare_global_frame(
    frame: &RgbaFrame,
    palette: &ColorPalette,
    options: &EncodeOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn ProgressSink,
    report: EncodeReport,
    total_frames_hint: Option<u64>,
) -> Result<PreparedFrame, GifEncodeError> {
    check_cancelled(cancellation)?;
    report_progress(progress, EncodePhase::Quantizing, report, total_frames_hint);
    let indices = map_frame_to_palette(
        frame,
        palette,
        alpha_threshold(options.transparency),
        options.dither,
        cancellation,
    )
    .map_err(map_quantization_error)?;
    let target_rgba = indexed_to_rgba(&indices, palette.colors(), palette.transparent_index())
        .map_err(map_quantization_error)?;
    Ok(PreparedFrame {
        palette: palette.colors().to_vec(),
        indices,
        transparent_index: palette.transparent_index(),
        target_rgba,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_prepared_frame<W: Write>(
    writer: &mut gif::Encoder<W>,
    frame: RgbaFrame,
    prepared: PreparedFrame,
    previous_target: Option<&[u8]>,
    force_full: bool,
    clear_after: bool,
    options: &EncodeOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn ProgressSink,
    report: &mut EncodeReport,
    total_frames_hint: Option<u64>,
    timing: &mut GifTimingQuantizer,
    palette_storage: PaletteStorage,
) -> Result<(), GifEncodeError> {
    let full_rect = FrameRect::full(frame.width(), frame.height());
    let rect = if force_full || options.delta_mode == DeltaMode::FullFrames {
        full_rect
    } else {
        previous_target.map_or(full_rect, |previous| {
            changed_rectangle(
                previous,
                &prepared.target_rgba,
                frame.width(),
                frame.height(),
            )
        })
    };
    if !rect.is_full(frame.width(), frame.height()) {
        report.delta_frames = report
            .delta_frames
            .checked_add(1)
            .ok_or(GifEncodeError::DurationOverflow)?;
    }
    let cropped = crop_indices(&prepared.indices, frame.width(), rect);
    let disposal = if clear_after {
        gif::DisposalMethod::Background
    } else {
        gif::DisposalMethod::Keep
    };
    let mut remaining_ticks = timing.quantize(frame.duration_us());

    loop {
        check_cancelled(cancellation)?;
        let frame_ticks = remaining_ticks.min(u64::from(u16::MAX)) as u16;
        let mut gif_frame = match palette_storage {
            PaletteStorage::Local => gif::Frame::from_palette_pixels(
                rect.width,
                rect.height,
                cropped.clone(),
                prepared.palette.clone(),
                prepared.transparent_index,
            ),
            PaletteStorage::Global => gif::Frame::from_indexed_pixels(
                rect.width,
                rect.height,
                cropped.clone(),
                prepared.transparent_index,
            ),
        };
        gif_frame.left = rect.left;
        gif_frame.top = rect.top;
        gif_frame.delay = frame_ticks;
        gif_frame.dispose = disposal;

        report_progress(progress, EncodePhase::Writing, *report, total_frames_hint);
        writer.write_frame(&gif_frame)?;
        report.encoded_frames = report
            .encoded_frames
            .checked_add(1)
            .ok_or(GifEncodeError::DurationOverflow)?;
        if remaining_ticks <= u64::from(u16::MAX) {
            break;
        }
        remaining_ticks -= u64::from(u16::MAX);
    }
    Ok(())
}

fn crop_indices(indices: &[u8], canvas_width: u16, rect: FrameRect) -> Vec<u8> {
    let canvas_width = usize::from(canvas_width);
    let left = usize::from(rect.left);
    let row_width = usize::from(rect.width);
    let top = usize::from(rect.top);
    let bottom = top + usize::from(rect.height);
    let mut cropped = Vec::with_capacity(row_width * usize::from(rect.height));
    for row in top..bottom {
        let start = row * canvas_width + left;
        cropped.extend_from_slice(&indices[start..start + row_width]);
    }
    cropped
}

fn changed_rectangle(previous: &[u8], current: &[u8], width: u16, height: u16) -> FrameRect {
    debug_assert_eq!(previous.len(), current.len());
    let width_usize = usize::from(width);
    let mut min_x = width_usize;
    let mut min_y = usize::from(height);
    let mut max_x = 0_usize;
    let mut max_y = 0_usize;
    let mut changed = false;
    for (pixel_index, (old, new)) in previous
        .as_chunks::<4>()
        .0
        .iter()
        .zip(current.as_chunks::<4>().0.iter())
        .enumerate()
    {
        if old == new {
            continue;
        }
        changed = true;
        let x = pixel_index % width_usize;
        let y = pixel_index / width_usize;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    if !changed {
        return FrameRect {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
        };
    }
    FrameRect {
        left: min_x as u16,
        top: min_y as u16,
        width: (max_x - min_x + 1) as u16,
        height: (max_y - min_y + 1) as u16,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_global_frames(
    source: &mut dyn RgbaFrameSource,
    options: &EncodeOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn ProgressSink,
    report: &mut EncodeReport,
    total_frames_hint: Option<u64>,
) -> Result<Vec<RgbaFrame>, GifEncodeError> {
    let first = read_frame(source, cancellation, progress, report, total_frames_hint)?
        .ok_or(GifEncodeError::NoFrames)?;
    let width = first.width();
    let height = first.height();
    let mut retained_bytes = u64::try_from(first.pixels().len()).unwrap_or(u64::MAX);
    enforce_global_memory_limit(retained_bytes, options.global_palette_buffer_limit_bytes)?;
    let mut frames = vec![first];

    while let Some(frame) = read_frame(source, cancellation, progress, report, total_frames_hint)? {
        validate_dimensions(&frame, report.input_frames, width, height)?;
        let last = frames.last_mut().expect("the first frame was retained");
        if options.merge_duplicate_frames && last.pixels() == frame.pixels() {
            last.merge_duration(frame.duration_us())
                .map_err(|()| GifEncodeError::DurationOverflow)?;
            report.duplicate_frames_merged += 1;
            continue;
        }
        retained_bytes =
            retained_bytes.saturating_add(u64::try_from(frame.pixels().len()).unwrap_or(u64::MAX));
        enforce_global_memory_limit(retained_bytes, options.global_palette_buffer_limit_bytes)?;
        frames.push(frame);
    }
    Ok(frames)
}

fn enforce_global_memory_limit(required: u64, limit: u64) -> Result<(), GifEncodeError> {
    if required > limit {
        Err(GifEncodeError::GlobalPaletteMemoryLimitExceeded {
            required_bytes: required,
            limit_bytes: limit,
        })
    } else {
        Ok(())
    }
}

fn frame_has_transparency(frame: &RgbaFrame, threshold: Option<u8>) -> bool {
    threshold.is_some_and(|threshold| {
        frame
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] < threshold)
    })
}

fn transition_needs_background_clear(
    current: &RgbaFrame,
    next: &RgbaFrame,
    threshold: Option<u8>,
) -> bool {
    let Some(threshold) = threshold else {
        return false;
    };
    current
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(next.pixels().as_chunks::<4>().0.iter())
        .any(|(current, next)| current[3] >= threshold && next[3] < threshold)
}

const fn alpha_threshold(transparency: Transparency) -> Option<u8> {
    match transparency {
        Transparency::Opaque => None,
        Transparency::AlphaThreshold(threshold) => Some(threshold),
    }
}

const fn repeats(loop_behavior: LoopBehavior) -> bool {
    !matches!(loop_behavior, LoopBehavior::Once)
}

fn configure_loop<W: Write>(
    writer: &mut gif::Encoder<W>,
    loop_behavior: LoopBehavior,
) -> Result<(), GifEncodeError> {
    match loop_behavior {
        LoopBehavior::Once => Ok(()),
        LoopBehavior::Infinite => writer
            .set_repeat(gif::Repeat::Infinite)
            .map_err(GifEncodeError::from),
        LoopBehavior::Finite(repeats) => writer
            .set_repeat(gif::Repeat::Finite(repeats))
            .map_err(GifEncodeError::from),
    }
}

fn validate_palette_limit(palette: &ColorPalette, max_colors: u16) -> Result<(), GifEncodeError> {
    validate_raw_palette_limit(palette.colors(), max_colors)
}

fn validate_raw_palette_limit(palette: &[u8], max_colors: u16) -> Result<(), GifEncodeError> {
    let actual = palette.len() / 3;
    if actual > usize::from(max_colors) {
        Err(GifEncodeError::Quantization(
            QuantizationError::InvalidPalette(format!(
                "quantizer returned {actual} colors above the requested {max_colors}"
            )),
        ))
    } else {
        Ok(())
    }
}

fn map_quantization_error(error: QuantizationError) -> GifEncodeError {
    match error {
        QuantizationError::Cancelled => GifEncodeError::Cancelled,
        other => GifEncodeError::Quantization(other),
    }
}

fn finish<W: Write>(
    writer: gif::Encoder<W>,
    mut report: EncodeReport,
    timing: GifTimingQuantizer,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn ProgressSink,
    total_frames_hint: Option<u64>,
) -> Result<EncodeReport, GifEncodeError> {
    check_cancelled(cancellation)?;
    report_progress(progress, EncodePhase::Finalizing, report, total_frames_hint);
    let _ = writer.into_inner()?;
    report.encoded_duration_ticks =
        u64::try_from(timing.assigned_ticks()).map_err(|_| GifEncodeError::DurationOverflow)?;
    report_progress(progress, EncodePhase::Complete, report, total_frames_hint);
    Ok(report)
}

fn check_cancelled(cancellation: &dyn CancellationToken) -> Result<(), GifEncodeError> {
    if cancellation.is_cancelled() {
        Err(GifEncodeError::Cancelled)
    } else {
        Ok(())
    }
}

fn report_progress(
    progress: &mut dyn ProgressSink,
    phase: EncodePhase,
    report: EncodeReport,
    total_frames_hint: Option<u64>,
) {
    progress.report(EncodeProgress {
        phase,
        frames_read: report.input_frames,
        frames_written: report.encoded_frames,
        total_frames_hint,
    });
}
