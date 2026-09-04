use std::io::Cursor;

use gif_from_screen_gif::{
    BuiltinGifEncoder, CancellationFlag, DeltaMode, DitherMode, EncodeOptions, EncodePhase,
    EncodeProgress, FixedPaletteQuantizer, GifEncodeError, GifEncoder, IteratorFrameSource,
    LoopBehavior, PaletteMode, QuantizationError, QuantizerStrategy, RgbaFrame, Transparency,
};

fn solid_frame(color: [u8; 4], duration_us: u64) -> RgbaFrame {
    let pixels = color
        .into_iter()
        .cycle()
        .take(3 * 2 * 4)
        .collect::<Vec<_>>();
    RgbaFrame::new(3, 2, pixels, duration_us).unwrap()
}

fn decode_summary(bytes: &[u8]) -> (gif::Repeat, Vec<u16>, u16, u16) {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options.read_info(Cursor::new(bytes)).unwrap();
    let repeat = decoder.repeat();
    let width = decoder.width();
    let height = decoder.height();
    let mut delays = Vec::new();
    while let Some(frame) = decoder.read_next_frame().unwrap() {
        delays.push(frame.delay);
    }
    (repeat, delays, width, height)
}

fn quantizer_roundtrip(strategy: QuantizerStrategy) {
    let frames = [
        RgbaFrame::new(
            4,
            1,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 12, 34, 56, 0, 0, 0, 255, 255,
            ],
            10_000,
        )
        .unwrap(),
        RgbaFrame::new(
            4,
            1,
            vec![
                255, 255, 0, 255, 65, 43, 21, 64, 0, 255, 255, 255, 255, 0, 255, 255,
            ],
            20_000,
        )
        .unwrap(),
    ];

    for palette_mode in [PaletteMode::LocalPerFrame, PaletteMode::Global] {
        let options = EncodeOptions {
            // Force NeuQuant's learned-codebook path (rather than its exact
            // small-color shortcut) in both local and global modes.
            max_colors: if strategy == QuantizerStrategy::NeuQuant {
                3
            } else {
                4
            },
            merge_duplicate_frames: false,
            transparency: Transparency::AlphaThreshold(128),
            palette_mode,
            quantizer: strategy,
            dither: if strategy == QuantizerStrategy::Wu {
                DitherMode::FloydSteinberg
            } else {
                DitherMode::None
            },
            ..EncodeOptions::default()
        };
        let mut first_bytes = Vec::new();
        let mut second_bytes = Vec::new();

        BuiltinGifEncoder::default()
            .encode_frames(frames.clone(), &mut first_bytes, &options)
            .unwrap();
        BuiltinGifEncoder::default()
            .encode_frames(frames.clone(), &mut second_bytes, &options)
            .unwrap();
        assert_eq!(first_bytes, second_bytes);

        let mut indexed_decoder = gif::DecodeOptions::new()
            .read_info(Cursor::new(&first_bytes))
            .unwrap();
        if palette_mode == PaletteMode::Global {
            assert!(indexed_decoder.global_palette().is_some());
        }
        while let Some(frame) = indexed_decoder.read_next_frame().unwrap() {
            assert_eq!(
                frame.palette.is_some(),
                palette_mode == PaletteMode::LocalPerFrame
            );
            assert!(frame.transparent.is_some());
        }

        let mut rgba_options = gif::DecodeOptions::new();
        rgba_options.set_color_output(gif::ColorOutput::RGBA);
        let mut rgba_decoder = rgba_options.read_info(Cursor::new(&first_bytes)).unwrap();
        let first = rgba_decoder.read_next_frame().unwrap().unwrap().clone();
        let second = rgba_decoder.read_next_frame().unwrap().unwrap().clone();
        assert!(rgba_decoder.read_next_frame().unwrap().is_none());
        assert_eq!(first.delay, 1);
        assert_eq!(second.delay, 2);
        assert_eq!(first.buffer[11], 0);
        assert_eq!(second.buffer[7], 0);

        if strategy == QuantizerStrategy::Grayscale {
            for frame in [&first, &second] {
                for pixel in frame.buffer.as_chunks::<4>().0 {
                    if pixel[3] != 0 {
                        assert_eq!(pixel[0], pixel[1]);
                        assert_eq!(pixel[1], pixel[2]);
                    }
                }
            }
        }
    }
}

#[test]
fn median_cut_roundtrips_with_local_and_global_palettes() {
    quantizer_roundtrip(QuantizerStrategy::MedianCut);
}

#[test]
fn grayscale_roundtrips_with_local_and_global_palettes() {
    quantizer_roundtrip(QuantizerStrategy::Grayscale);
}

#[test]
fn most_used_roundtrips_with_local_and_global_palettes() {
    quantizer_roundtrip(QuantizerStrategy::MostUsed);
}

#[test]
fn octree_roundtrips_with_local_and_global_palettes() {
    quantizer_roundtrip(QuantizerStrategy::Octree);
}

#[test]
fn wu_roundtrips_local_global_dither_and_transparency() {
    quantizer_roundtrip(QuantizerStrategy::Wu);
}

#[test]
fn neuquant_roundtrips_with_local_and_global_palettes() {
    quantizer_roundtrip(QuantizerStrategy::NeuQuant);
}

#[test]
fn predefined_palette_strategies_encode_local_and_global_with_transparency() {
    let frames = [
        RgbaFrame::new(
            3,
            1,
            vec![255, 0, 0, 255, 0, 0, 0, 0, 0, 255, 0, 255],
            10_000,
        )
        .unwrap(),
        RgbaFrame::new(
            3,
            1,
            vec![0, 0, 255, 255, 255, 255, 255, 255, 12, 34, 56, 0],
            20_000,
        )
        .unwrap(),
    ];
    for (strategy, required_colors) in [
        (QuantizerStrategy::WebSafe216, 217),
        (QuantizerStrategy::Monochrome, 3),
        (QuantizerStrategy::Windows16, 17),
    ] {
        for palette_mode in [PaletteMode::LocalPerFrame, PaletteMode::Global] {
            let options = EncodeOptions {
                max_colors: required_colors,
                merge_duplicate_frames: false,
                transparency: Transparency::AlphaThreshold(1),
                palette_mode,
                quantizer: strategy,
                dither: DitherMode::StevensonArce,
                ..EncodeOptions::default()
            };
            let mut first = Vec::new();
            let mut second = Vec::new();
            BuiltinGifEncoder::default()
                .encode_frames(frames.clone(), &mut first, &options)
                .unwrap();
            BuiltinGifEncoder::default()
                .encode_frames(frames.clone(), &mut second, &options)
                .unwrap();
            assert_eq!(first, second);

            let mut decoder = gif::DecodeOptions::new()
                .read_info(Cursor::new(first))
                .unwrap();
            if palette_mode == PaletteMode::Global {
                assert!(decoder.global_palette().is_some());
            }
            let mut decoded_frames = 0;
            while let Some(frame) = decoder.read_next_frame().unwrap() {
                assert!(frame.transparent.is_some());
                decoded_frames += 1;
            }
            assert_eq!(decoded_frames, 2);
        }
    }
}

#[test]
fn fixed_palette_roundtrips_local_global_bayer_and_floyd() {
    let mut first_pixels = [128, 128, 128, 255].repeat(64);
    first_pixels[3] = 0;
    let mut second_pixels = [160, 160, 160, 255].repeat(64);
    second_pixels[7] = 0;
    let frames = [
        RgbaFrame::new(8, 8, first_pixels, 10_000).unwrap(),
        RgbaFrame::new(8, 8, second_pixels, 20_000).unwrap(),
    ];

    for palette_mode in [PaletteMode::LocalPerFrame, PaletteMode::Global] {
        for dither in [DitherMode::Bayer4x4, DitherMode::FloydSteinberg] {
            let options = EncodeOptions {
                max_colors: 3,
                merge_duplicate_frames: false,
                transparency: Transparency::AlphaThreshold(128),
                palette_mode,
                dither,
                ..EncodeOptions::default()
            };
            let quantizer =
                FixedPaletteQuantizer::new(vec![0, 0, 0, 0, 0, 0, 255, 255, 255], Some(0)).unwrap();
            let mut bytes = Vec::new();
            BuiltinGifEncoder::new(Box::new(quantizer))
                .encode_frames(frames.clone(), &mut bytes, &options)
                .unwrap();

            let mut indexed = gif::DecodeOptions::new()
                .read_info(Cursor::new(&bytes))
                .unwrap();
            if palette_mode == PaletteMode::Global {
                assert!(indexed.global_palette().is_some());
            }
            while let Some(frame) = indexed.read_next_frame().unwrap() {
                assert_eq!(
                    frame.palette.is_some(),
                    palette_mode == PaletteMode::LocalPerFrame
                );
                assert_eq!(frame.transparent, Some(0));
            }

            let mut rgba_options = gif::DecodeOptions::new();
            rgba_options.set_color_output(gif::ColorOutput::RGBA);
            let mut rgba = rgba_options.read_info(Cursor::new(bytes)).unwrap();
            assert_eq!(rgba.read_next_frame().unwrap().unwrap().buffer[3], 0);
            assert_eq!(rgba.read_next_frame().unwrap().unwrap().buffer[7], 0);
        }
    }
}

#[test]
fn fixed_palette_encoder_rejects_max_color_conflict() {
    for palette_mode in [PaletteMode::LocalPerFrame, PaletteMode::Global] {
        let quantizer =
            FixedPaletteQuantizer::new(vec![0, 0, 0, 128, 128, 128, 255, 255, 255], None).unwrap();
        let options = EncodeOptions {
            max_colors: 2,
            palette_mode,
            ..EncodeOptions::default()
        };
        let mut bytes = Vec::new();
        assert!(matches!(
            BuiltinGifEncoder::new(Box::new(quantizer)).encode_frames(
                [solid_frame([128, 128, 128, 255], 10_000)],
                &mut bytes,
                &options,
            ),
            Err(GifEncodeError::Quantization(
                QuantizationError::FixedPaletteExceedsColorLimit {
                    palette_colors: 3,
                    max_colors: 2,
                }
            ))
        ));
    }
}

#[test]
fn roundtrip_merges_duplicates_and_preserves_total_duration() {
    let red = solid_frame([255, 0, 0, 255], 15_000);
    let same_red = solid_frame([255, 0, 0, 255], 25_000);
    let blue = solid_frame([0, 0, 255, 255], 20_000);
    let options = EncodeOptions {
        max_colors: 2,
        loop_behavior: LoopBehavior::Infinite,
        ..EncodeOptions::default()
    };
    let mut bytes = Vec::new();

    let report = BuiltinGifEncoder::default()
        .encode_frames([red, same_red, blue], &mut bytes, &options)
        .unwrap();

    let (repeat, delays, width, height) = decode_summary(&bytes);
    assert_eq!(repeat, gif::Repeat::Infinite);
    assert_eq!(delays, [4, 2]);
    assert_eq!(u64::from(delays.iter().sum::<u16>()), 6);
    assert_eq!((width, height), (3, 2));
    assert_eq!(report.input_frames, 3);
    assert_eq!(report.encoded_frames, 2);
    assert_eq!(report.duplicate_frames_merged, 1);
    assert_eq!(report.input_duration_us, 60_000);
    assert_eq!(report.encoded_duration_ticks, 6);
}

#[test]
fn roundtrip_writes_finite_loop_and_cumulative_tick_distribution() {
    let frames = [
        solid_frame([255, 0, 0, 255], 16_667),
        solid_frame([0, 255, 0, 255], 16_667),
        solid_frame([0, 0, 255, 255], 16_667),
    ];
    let options = EncodeOptions {
        max_colors: 16,
        loop_behavior: LoopBehavior::Finite(3),
        ..EncodeOptions::default()
    };
    let mut bytes = Vec::new();

    let report = BuiltinGifEncoder::default()
        .encode_frames(frames, &mut bytes, &options)
        .unwrap();

    let (repeat, delays, _, _) = decode_summary(&bytes);
    assert_eq!(repeat, gif::Repeat::Finite(3));
    assert_eq!(delays, [2, 1, 2]);
    assert_eq!(report.encoded_frames, 3);
    assert_eq!(report.input_duration_us, 50_001);
    assert_eq!(report.encoded_duration_ticks, 5);
    let encoded_us = report.encoded_duration_ticks * 10_000;
    assert!(report.input_duration_us.abs_diff(encoded_us) < 10_000);
}

#[test]
fn reports_progress_and_supports_cancellation() {
    let frames = vec![solid_frame([255, 255, 255, 255], 10_000)];
    let mut source = IteratorFrameSource::new(frames.into_iter());
    let mut bytes = Vec::new();
    let cancellation = CancellationFlag::default();
    cancellation.cancel();
    let mut phases = Vec::new();

    let result = BuiltinGifEncoder::default().encode(
        &mut source,
        &mut bytes,
        &EncodeOptions::default(),
        &cancellation,
        &mut |progress: EncodeProgress| phases.push(progress.phase),
    );

    assert!(matches!(result, Err(GifEncodeError::Cancelled)));
    assert!(bytes.is_empty());
    assert!(phases.is_empty());

    cancellation.reset();
    let frames = vec![solid_frame([255, 255, 255, 255], 10_000)];
    let mut source = IteratorFrameSource::new(frames.into_iter());
    BuiltinGifEncoder::default()
        .encode(
            &mut source,
            &mut bytes,
            &EncodeOptions::default(),
            &cancellation,
            &mut |progress: EncodeProgress| phases.push(progress.phase),
        )
        .unwrap();
    assert_eq!(phases.last(), Some(&EncodePhase::Complete));
}

#[test]
fn global_palette_roundtrip_uses_one_color_table() {
    let frames = [
        solid_frame([255, 0, 0, 255], 10_000),
        solid_frame([0, 255, 0, 255], 20_000),
        solid_frame([0, 0, 255, 255], 30_000),
    ];
    let options = EncodeOptions {
        max_colors: 4,
        palette_mode: PaletteMode::Global,
        ..EncodeOptions::default()
    };
    let mut bytes = Vec::new();
    let report = BuiltinGifEncoder::default()
        .encode_frames(frames, &mut bytes, &options)
        .unwrap();

    let mut decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(&bytes))
        .unwrap();
    assert!(decoder.global_palette().is_some());
    let mut delays = Vec::new();
    while let Some(frame) = decoder.read_next_frame().unwrap() {
        assert!(frame.palette.is_none());
        delays.push(frame.delay);
    }
    assert_eq!(delays, [1, 2, 3]);
    assert_eq!(report.encoded_duration_ticks, 6);
}

#[test]
fn global_palette_honors_frame_buffer_limit_before_writing() {
    let options = EncodeOptions {
        palette_mode: PaletteMode::Global,
        global_palette_buffer_limit_bytes: 23,
        ..EncodeOptions::default()
    };
    let mut bytes = Vec::new();
    let error = BuiltinGifEncoder::default()
        .encode_frames([solid_frame([0, 0, 0, 255], 10_000)], &mut bytes, &options)
        .unwrap_err();

    assert!(matches!(
        error,
        GifEncodeError::GlobalPaletteMemoryLimitExceeded {
            required_bytes: 24,
            limit_bytes: 23
        }
    ));
    assert!(bytes.is_empty());
}

#[test]
fn delta_mode_writes_changed_bounding_rectangle() {
    let first = RgbaFrame::new(4, 3, [0, 0, 0, 255].repeat(12), 10_000).unwrap();
    let mut second_pixels = [0, 0, 0, 255].repeat(12);
    second_pixels[24..28].copy_from_slice(&[255, 255, 255, 255]);
    let second = RgbaFrame::new(4, 3, second_pixels, 10_000).unwrap();
    let options = EncodeOptions {
        max_colors: 2,
        palette_mode: PaletteMode::Global,
        delta_mode: DeltaMode::ChangedRectangles,
        ..EncodeOptions::default()
    };
    let mut bytes = Vec::new();
    let report = BuiltinGifEncoder::default()
        .encode_frames([first, second], &mut bytes, &options)
        .unwrap();

    let mut decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(&bytes))
        .unwrap();
    let first = decoder.read_next_frame().unwrap().unwrap().clone();
    let second = decoder.read_next_frame().unwrap().unwrap().clone();
    assert_eq!(
        (first.left, first.top, first.width, first.height),
        (0, 0, 4, 3)
    );
    assert_eq!(
        (second.left, second.top, second.width, second.height),
        (2, 1, 1, 1)
    );
    assert_eq!(second.dispose, gif::DisposalMethod::Keep);
    assert_eq!(report.delta_frames, 1);
}

#[test]
fn opaque_to_transparent_transition_clears_the_previous_canvas() {
    let first = RgbaFrame::new(3, 1, [255, 0, 0, 255].repeat(3), 10_000).unwrap();
    let second = RgbaFrame::new(
        3,
        1,
        vec![255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255],
        10_000,
    )
    .unwrap();
    let options = EncodeOptions {
        max_colors: 2,
        palette_mode: PaletteMode::Global,
        delta_mode: DeltaMode::ChangedRectangles,
        ..EncodeOptions::default()
    };
    let mut bytes = Vec::new();
    BuiltinGifEncoder::default()
        .encode_frames([first, second], &mut bytes, &options)
        .unwrap();

    let mut decoder_options = gif::DecodeOptions::new();
    decoder_options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = decoder_options.read_info(Cursor::new(&bytes)).unwrap();
    let first = decoder.read_next_frame().unwrap().unwrap().clone();
    let second = decoder.read_next_frame().unwrap().unwrap().clone();
    assert_eq!(first.dispose, gif::DisposalMethod::Background);
    assert_eq!((first.left, first.width), (0, 3));
    assert_eq!((second.left, second.width), (0, 3));
    assert_eq!(first.transparent, Some(0));
    assert_eq!(second.transparent, Some(0));
    assert_eq!(&second.buffer[4..8], &[0, 0, 0, 0]);
}
