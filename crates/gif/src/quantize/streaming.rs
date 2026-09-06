//! Exact bounded-memory global analysis; frames are supplied in retained order.

use super::{
    BTreeSet, CancellationToken, ColorPalette, HISTOGRAM_LEN, HistogramBin,
    NEUQUANT_MAX_TRAINING_PIXELS, NeuQuantInput, PredefinedPalette, QuantizationError,
    QuantizationSettings, QuantizerStrategy, RgbaFrame, build_octree, check_cancellation,
    check_now, finish_palette, grayscale_luma, grayscale_points, histogram_index,
    histogram_moment_overflow, histogram_points, is_transparent, make_neuquant_palette_from_input,
    make_palette, make_wu_palette, opaque_color_limit, predefined_strategy_quantizer,
    reduce_octree, validate_settings,
};

enum AnalysisState {
    Rgb {
        strategy: QuantizerStrategy,
        histogram: Vec<HistogramBin>,
    },
    Grayscale(Box<[u64; 256]>),
    Fixed(ColorPalette),
    CountNeuQuant {
        opaque_pixels: u64,
        exact_colors: Option<BTreeSet<[u8; 3]>>,
    },
    SampleNeuQuant {
        opaque_pixels: u64,
        opaque_index: u64,
        sample_pixels: u64,
        rgba: Vec<u8>,
    },
    Finished,
}

/// Incremental analysis with the same palette choices as buffered quantizers.
///
/// RGB statistics use exactly 32,768 bins; grayscale uses 256. NeuQuant first
/// counts opaque pixels and tracks at most `max_colors + 1` exact colors, then
/// requests one replay to reproduce the existing evenly spaced sample of at
/// most 65,536 pixels. No frame pixels or per-frame metadata are retained.
pub struct GlobalPaletteBuilder {
    settings: QuantizationSettings,
    has_transparency: bool,
    state: AnalysisState,
}

impl GlobalPaletteBuilder {
    /// Starts bounded analysis using one of the built-in quantizers.
    ///
    /// # Errors
    ///
    /// Rejects invalid color limits or a fixed palette larger than the limit.
    pub fn new(
        strategy: QuantizerStrategy,
        settings: QuantizationSettings,
    ) -> Result<Self, QuantizationError> {
        validate_settings(settings)?;
        let state = match strategy {
            QuantizerStrategy::Grayscale => AnalysisState::Grayscale(Box::new([0; 256])),
            QuantizerStrategy::NeuQuant => AnalysisState::CountNeuQuant {
                opaque_pixels: 0,
                exact_colors: Some(BTreeSet::new()),
            },
            QuantizerStrategy::WebSafe216
            | QuantizerStrategy::Monochrome
            | QuantizerStrategy::Windows16 => {
                let predefined = match strategy {
                    QuantizerStrategy::WebSafe216 => PredefinedPalette::WebSafe216,
                    QuantizerStrategy::Monochrome => PredefinedPalette::Monochrome,
                    _ => PredefinedPalette::Windows16,
                };
                return Self::from_palette(
                    predefined_strategy_quantizer(predefined)?.palette().clone(),
                    settings,
                );
            }
            _ => AnalysisState::Rgb {
                strategy,
                histogram: vec![HistogramBin::default(); HISTOGRAM_LEN],
            },
        };
        Ok(Self {
            settings,
            has_transparency: settings.reserve_transparency,
            state,
        })
    }

    /// Analyzes transparency without changing a caller-provided palette.
    ///
    /// # Errors
    ///
    /// Rejects invalid color limits or a palette larger than the limit.
    pub fn from_palette(
        palette: ColorPalette,
        settings: QuantizationSettings,
    ) -> Result<Self, QuantizationError> {
        validate_settings(settings)?;
        if palette.color_count() > usize::from(settings.max_colors) {
            return Err(QuantizationError::FixedPaletteExceedsColorLimit {
                palette_colors: palette.color_count(),
                max_colors: settings.max_colors,
            });
        }
        Ok(Self {
            settings,
            has_transparency: settings.reserve_transparency,
            state: AnalysisState::Fixed(palette),
        })
    }

    /// Adds one retained frame. Callers apply their existing duplicate policy
    /// before this boundary, and replay exactly the same sequence if requested.
    ///
    /// # Errors
    ///
    /// Returns cancellation, checked-statistic overflow, or invalid pass state.
    pub fn push_frame(
        &mut self,
        frame: &RgbaFrame,
        cancellation: &dyn CancellationToken,
    ) -> Result<(), QuantizationError> {
        check_now(cancellation)?;
        if matches!(self.state, AnalysisState::Finished) {
            return Err(invalid_state());
        }
        for (index, pixel) in frame.pixels().as_chunks::<4>().0.iter().enumerate() {
            check_cancellation(index, cancellation)?;
            if is_transparent(pixel[3], self.settings.alpha_threshold) {
                self.has_transparency = true;
                continue;
            }
            let rgb = [pixel[0], pixel[1], pixel[2]];
            match &mut self.state {
                AnalysisState::Rgb { histogram, .. } => histogram
                    [histogram_index(rgb[0], rgb[1], rgb[2])]
                .add(rgb[0], rgb[1], rgb[2])?,
                AnalysisState::Grayscale(histogram) => {
                    let bin = &mut histogram[usize::from(grayscale_luma(rgb[0], rgb[1], rgb[2]))];
                    *bin = bin.checked_add(1).ok_or_else(histogram_moment_overflow)?;
                }
                AnalysisState::CountNeuQuant {
                    opaque_pixels,
                    exact_colors,
                } => {
                    *opaque_pixels = opaque_pixels
                        .checked_add(1)
                        .ok_or_else(histogram_moment_overflow)?;
                    if let Some(colors) = exact_colors {
                        colors.insert(rgb);
                        if colors.len() > usize::from(self.settings.max_colors) {
                            *exact_colors = None;
                        }
                    }
                }
                AnalysisState::SampleNeuQuant {
                    opaque_pixels,
                    opaque_index,
                    sample_pixels,
                    rgba,
                } => {
                    if *opaque_index >= *opaque_pixels {
                        return Err(changed_replay());
                    }
                    let before = u128::from(*opaque_index) * u128::from(*sample_pixels)
                        / u128::from(*opaque_pixels);
                    *opaque_index += 1;
                    let after = u128::from(*opaque_index) * u128::from(*sample_pixels)
                        / u128::from(*opaque_pixels);
                    if after != before {
                        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                    }
                }
                AnalysisState::Fixed(_) => {}
                AnalysisState::Finished => return Err(invalid_state()),
            }
        }
        Ok(())
    }

    /// Finishes a complete pass. `Some(palette)` completes analysis; `None`
    /// requests one exact replay for NeuQuant's bounded, evenly spaced sample.
    ///
    /// # Errors
    ///
    /// Rejects cancellation, missing fixed-palette transparency, changed sample
    /// counts on replay, or an attempt to finish an already-finished builder.
    pub fn finish_pass(
        &mut self,
        cancellation: &dyn CancellationToken,
    ) -> Result<Option<ColorPalette>, QuantizationError> {
        check_now(cancellation)?;
        let state = std::mem::replace(&mut self.state, AnalysisState::Finished);
        let opaque_limit = opaque_color_limit(self.settings.max_colors, self.has_transparency);
        let palette = match state {
            AnalysisState::Rgb {
                strategy,
                histogram,
            } => finish_rgb(
                &histogram,
                strategy,
                opaque_limit,
                self.has_transparency,
                cancellation,
            )?,
            AnalysisState::Grayscale(histogram) => {
                let mut colors =
                    make_palette(&grayscale_points(&histogram), opaque_limit, cancellation)?;
                colors.sort_unstable();
                finish_palette(colors, self.has_transparency)?
            }
            AnalysisState::Fixed(palette) => {
                if self.has_transparency && palette.transparent_index().is_none() {
                    return Err(QuantizationError::FixedPaletteMissingTransparency);
                }
                palette
            }
            AnalysisState::CountNeuQuant {
                opaque_pixels,
                exact_colors,
            } => {
                if let Some(colors) = exact_colors.filter(|colors| colors.len() <= opaque_limit) {
                    finish_palette(colors.into_iter().collect(), self.has_transparency)?
                } else {
                    let sample_pixels = opaque_pixels.min(NEUQUANT_MAX_TRAINING_PIXELS as u64);
                    let sample_bytes =
                        usize::try_from(sample_pixels).map_err(|_| invalid_state())? * 4;
                    let mut rgba = Vec::new();
                    rgba.try_reserve_exact(sample_bytes).map_err(|_| {
                        QuantizationError::InvalidPalette(
                            "could not allocate bounded NeuQuant sample".to_owned(),
                        )
                    })?;
                    self.state = AnalysisState::SampleNeuQuant {
                        opaque_pixels,
                        opaque_index: 0,
                        sample_pixels,
                        rgba,
                    };
                    return Ok(None);
                }
            }
            AnalysisState::SampleNeuQuant {
                opaque_pixels,
                opaque_index,
                sample_pixels,
                rgba,
            } => {
                if opaque_index != opaque_pixels || rgba.len() as u64 != sample_pixels * 4 {
                    return Err(changed_replay());
                }
                make_neuquant_palette_from_input(
                    NeuQuantInput {
                        training_rgba: rgba,
                        exact_colors: None,
                        has_transparency: self.has_transparency,
                    },
                    self.settings,
                    cancellation,
                )?
            }
            AnalysisState::Finished => return Err(invalid_state()),
        };
        Ok(Some(palette))
    }
}

pub(super) fn finish_rgb(
    histogram: &[HistogramBin],
    strategy: QuantizerStrategy,
    limit: usize,
    transparent: bool,
    cancellation: &dyn CancellationToken,
) -> Result<ColorPalette, QuantizationError> {
    let colors = match strategy {
        QuantizerStrategy::MedianCut => {
            let mut colors = make_palette(&histogram_points(histogram), limit, cancellation)?;
            colors.sort_unstable();
            colors
        }
        QuantizerStrategy::MostUsed => {
            let mut points = histogram_points(histogram);
            points.sort_unstable_by(|left, right| {
                right
                    .count
                    .cmp(&left.count)
                    .then_with(|| left.rgb.cmp(&right.rgb))
            });
            points
                .into_iter()
                .take(limit)
                .map(|point| point.rgb)
                .collect()
        }
        QuantizerStrategy::Octree => {
            reduce_octree(&build_octree(histogram, cancellation)?, limit, cancellation)?
        }
        QuantizerStrategy::Wu => make_wu_palette(histogram, limit, cancellation)?,
        _ => return Err(invalid_state()),
    };
    check_now(cancellation)?;
    finish_palette(colors, transparent)
}

fn invalid_state() -> QuantizationError {
    QuantizationError::InvalidPalette("invalid global analysis pass state".to_owned())
}

fn changed_replay() -> QuantizationError {
    QuantizationError::InvalidPalette(
        "opaque pixel count changed during global analysis replay".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameQuantizer, NeuQuantQuantizer, NeverCancel};

    fn settings(colors: u16) -> QuantizationSettings {
        QuantizationSettings {
            max_colors: colors,
            alpha_threshold: Some(128),
            reserve_transparency: false,
        }
    }

    fn corpus() -> Vec<RgbaFrame> {
        let pixels: Vec<u8> = (0_u32..70_000)
            .flat_map(|index| {
                [
                    (index % 256) as u8,
                    ((index / 256) % 256) as u8,
                    ((index * 31) % 251) as u8,
                    if index % 13 == 0 { 0 } else { 255 },
                ]
            })
            .collect();
        vec![
            RgbaFrame::new(350, 200, pixels.clone(), 10_000).unwrap(),
            RgbaFrame::new(350, 200, pixels, 20_000).unwrap(),
        ]
    }

    #[test]
    fn neuquant_replay_collects_exact_legacy_sample_under_its_fixed_memory_bound() {
        let frames = corpus();
        let mut builder =
            GlobalPaletteBuilder::new(QuantizerStrategy::NeuQuant, settings(16)).unwrap();
        for frame in &frames {
            builder.push_frame(frame, &NeverCancel).unwrap();
        }
        assert_eq!(builder.finish_pass(&NeverCancel).unwrap(), None);
        for frame in &frames {
            builder.push_frame(frame, &NeverCancel).unwrap();
        }
        let AnalysisState::SampleNeuQuant { rgba, .. } = &builder.state else {
            panic!("sample pass expected")
        };
        assert_eq!(rgba.len(), NEUQUANT_MAX_TRAINING_PIXELS * 4);
        assert_eq!(
            *rgba,
            super::super::collect_neuquant_input(&frames, settings(16), &NeverCancel)
                .unwrap()
                .training_rgba
        );
        let expected = NeuQuantQuantizer
            .build_global_palette(&frames, settings(16), &NeverCancel)
            .unwrap();
        assert_eq!(builder.finish_pass(&NeverCancel).unwrap(), Some(expected));
        assert!(builder.push_frame(&frames[0], &NeverCancel).is_err());
        assert!(builder.finish_pass(&NeverCancel).is_err());
    }

    #[test]
    fn every_strategy_retains_exact_buffered_palette_bytes() {
        let frames = corpus();
        for strategy in [
            QuantizerStrategy::MedianCut,
            QuantizerStrategy::Grayscale,
            QuantizerStrategy::MostUsed,
            QuantizerStrategy::Octree,
            QuantizerStrategy::Wu,
            QuantizerStrategy::WebSafe216,
            QuantizerStrategy::Windows16,
            QuantizerStrategy::Monochrome,
        ] {
            let mut builder = GlobalPaletteBuilder::new(strategy, settings(256)).unwrap();
            for frame in &frames {
                builder.push_frame(frame, &NeverCancel).unwrap();
            }
            assert_eq!(
                builder.finish_pass(&NeverCancel).unwrap(),
                Some(
                    strategy
                        .build_global_palette(&frames, settings(256), &NeverCancel)
                        .unwrap()
                ),
                "{strategy:?}"
            );
        }
    }

    #[test]
    fn histograms_do_not_grow_with_frame_count() {
        let frame = RgbaFrame::new(1, 1, vec![17, 29, 43, 255], 1).unwrap();
        let mut builder =
            GlobalPaletteBuilder::new(QuantizerStrategy::MedianCut, settings(4)).unwrap();
        for _ in 0..100_000 {
            builder.push_frame(&frame, &NeverCancel).unwrap();
        }
        let AnalysisState::Rgb { histogram, .. } = &builder.state else {
            panic!("RGB analysis expected")
        };
        assert_eq!(histogram.len(), HISTOGRAM_LEN);
        assert_eq!(histogram.iter().map(|bin| bin.count).sum::<u64>(), 100_000);
        assert!(builder.finish_pass(&NeverCancel).unwrap().is_some());
    }

    #[test]
    fn cancellation_and_changed_sample_counts_fail_explicitly() {
        struct Cancelled;
        impl CancellationToken for Cancelled {
            fn is_cancelled(&self) -> bool {
                true
            }
        }
        let frames = corpus();
        let mut builder =
            GlobalPaletteBuilder::new(QuantizerStrategy::NeuQuant, settings(4)).unwrap();
        assert_eq!(
            builder.push_frame(&frames[0], &Cancelled),
            Err(QuantizationError::Cancelled)
        );
        for frame in &frames {
            builder.push_frame(frame, &NeverCancel).unwrap();
        }
        assert_eq!(
            builder.finish_pass(&Cancelled),
            Err(QuantizationError::Cancelled)
        );
        assert!(builder.finish_pass(&NeverCancel).unwrap().is_none());
        builder.push_frame(&frames[0], &NeverCancel).unwrap();
        assert!(builder.finish_pass(&NeverCancel).is_err());
    }

    #[test]
    fn small_exact_neuquant_palettes_do_not_request_an_extra_pass() {
        let frame = RgbaFrame::new(2, 1, vec![17, 29, 43, 255, 0, 0, 0, 0], 1).unwrap();
        let mut builder =
            GlobalPaletteBuilder::new(QuantizerStrategy::NeuQuant, settings(2)).unwrap();
        builder.push_frame(&frame, &NeverCancel).unwrap();
        assert_eq!(
            builder.finish_pass(&NeverCancel).unwrap(),
            Some(
                NeuQuantQuantizer
                    .build_global_palette(&[frame], settings(2), &NeverCancel)
                    .unwrap()
            )
        );
    }

    #[test]
    fn fixed_palette_limits_and_required_transparency_are_not_bypassed() {
        let palette = ColorPalette::new(vec![0, 0, 0, 255, 255, 255, 1, 2, 3], None).unwrap();
        assert!(matches!(
            GlobalPaletteBuilder::from_palette(palette.clone(), settings(2)),
            Err(QuantizationError::FixedPaletteExceedsColorLimit { .. })
        ));
        let mut builder = GlobalPaletteBuilder::from_palette(palette, settings(3)).unwrap();
        let frame = RgbaFrame::new(1, 1, vec![255, 255, 255, 0], 1).unwrap();
        builder.push_frame(&frame, &NeverCancel).unwrap();
        assert_eq!(
            builder.finish_pass(&NeverCancel),
            Err(QuantizationError::FixedPaletteMissingTransparency)
        );
    }
}
