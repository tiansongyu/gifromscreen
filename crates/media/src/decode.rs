use std::{io::Read, num::NonZeroU64};

use gif::{ColorOutput, DisposalMethod, MemoryLimit, Repeat};

use crate::{
    DecodeLimits, DecodedAnimation, DecodedFrame, GifDecodeError, GifDecodeOptions, LoopBehavior,
    ZeroDelayPolicy,
};

const RGBA_CHANNELS: usize = 4;
const RGBA_BYTES_PER_PIXEL: u64 = 4;
const GIF_TICK_US: u64 = 10_000;

#[derive(Clone, Copy, Debug)]
struct FrameInfo {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    delay: u16,
    disposal: DisposalMethod,
}

impl FrameInfo {
    const fn from_gif(frame: &gif::Frame<'_>) -> Self {
        Self {
            left: frame.left,
            top: frame.top,
            width: frame.width,
            height: frame.height,
            delay: frame.delay,
            disposal: frame.dispose,
        }
    }
}

/// Decode a GIF stream into bounded, full-canvas RGBA8 frames.
///
/// Local image rectangles are composited in playback order. Transparent source
/// pixels leave the current canvas unchanged. `Background` disposal clears the
/// affected rectangle to transparent, matching common animated-GIF rendering,
/// while `Previous` restores the rectangle as it was before the frame.
///
/// # Errors
///
/// Returns [`GifDecodeError`] for malformed data, out-of-bounds frame
/// rectangles, rejected zero delays, configured limit violations, or bounded
/// allocation failures.
pub fn decode_gif<R: Read>(
    reader: R,
    options: &GifDecodeOptions,
) -> Result<DecodedAnimation, GifDecodeError> {
    let mut decoder_options = gif::DecodeOptions::new();
    decoder_options.set_color_output(ColorOutput::RGBA);
    decoder_options.set_memory_limit(MemoryLimit::Bytes(decoder_memory_limit(options.limits)));

    let mut decoder = decoder_options.read_info(reader)?;
    let width = decoder.width();
    let height = decoder.height();
    validate_canvas(width, height, options.limits)?;

    let canvas_bytes_u64 = rgba_len(width, height);
    enforce_total_bytes(canvas_bytes_u64, options.limits.max_total_rgba_bytes)?;
    let canvas_bytes = platform_len(canvas_bytes_u64)?;
    let mut canvas = zeroed_buffer(canvas_bytes)?;
    let mut frames = Vec::new();
    let mut retained_bytes = 0_u64;

    loop {
        let Some(frame) = decoder.next_frame_info()? else {
            break;
        };
        let frame_info = FrameInfo::from_gif(frame);
        let frame_index = frames.len();

        validate_frame(frame_info, frame_index, width, height)?;
        if frame_index >= options.limits.max_frames {
            return Err(GifDecodeError::FrameLimitExceeded {
                limit: options.limits.max_frames,
            });
        }

        let next_retained_bytes = retained_bytes.checked_add(canvas_bytes_u64).ok_or(
            GifDecodeError::TotalRgbaBytesLimitExceeded {
                required_bytes: u64::MAX,
                limit_bytes: options.limits.max_total_rgba_bytes,
            },
        )?;
        enforce_total_bytes(next_retained_bytes, options.limits.max_total_rgba_bytes)?;

        frames
            .try_reserve(1)
            .map_err(|_| GifDecodeError::AllocationFailed {
                bytes: std::mem::size_of::<DecodedFrame>(),
            })?;

        let frame_bytes_u64 = rgba_len(frame_info.width, frame_info.height);
        let frame_bytes = platform_len(frame_bytes_u64)?;
        let mut frame_rgba = zeroed_buffer(frame_bytes)?;
        decoder.read_into_buffer(&mut frame_rgba)?;

        let previous = if frame_info.disposal == DisposalMethod::Previous {
            copy_region(&canvas, width, frame_info)?
        } else {
            Vec::new()
        };

        composite_region(&mut canvas, width, &frame_rgba, frame_info);
        let displayed = copy_buffer(&canvas)?;
        let duration_us = frame_duration_us(frame_info.delay, frame_index, options.zero_delay)?;
        frames.push(DecodedFrame {
            rgba: displayed,
            duration_us,
        });
        retained_bytes = next_retained_bytes;

        match frame_info.disposal {
            DisposalMethod::Any | DisposalMethod::Keep => {}
            DisposalMethod::Background => clear_region(&mut canvas, width, frame_info),
            DisposalMethod::Previous => {
                restore_region(&mut canvas, width, frame_info, &previous);
            }
        }
    }

    if frames.is_empty() {
        return Err(GifDecodeError::NoFrames);
    }

    Ok(DecodedAnimation {
        width,
        height,
        frames,
        loop_behavior: map_repeat(decoder.repeat()),
    })
}

fn decoder_memory_limit(limits: DecodeLimits) -> NonZeroU64 {
    let configured_canvas_bytes = rgba_len(limits.max_width, limits.max_height);
    let limit = limits.max_total_rgba_bytes.min(configured_canvas_bytes);
    NonZeroU64::new(limit).unwrap_or(NonZeroU64::MIN)
}

fn validate_canvas(width: u16, height: u16, limits: DecodeLimits) -> Result<(), GifDecodeError> {
    if width == 0 || height == 0 {
        return Err(GifDecodeError::EmptyCanvas { width, height });
    }
    if width > limits.max_width {
        return Err(GifDecodeError::WidthLimitExceeded {
            actual: width,
            limit: limits.max_width,
        });
    }
    if height > limits.max_height {
        return Err(GifDecodeError::HeightLimitExceeded {
            actual: height,
            limit: limits.max_height,
        });
    }
    Ok(())
}

fn validate_frame(
    frame: FrameInfo,
    frame_index: usize,
    canvas_width: u16,
    canvas_height: u16,
) -> Result<(), GifDecodeError> {
    if frame.width == 0 || frame.height == 0 {
        return Err(GifDecodeError::EmptyFrame {
            frame_index,
            width: frame.width,
            height: frame.height,
        });
    }

    let right = u32::from(frame.left) + u32::from(frame.width);
    let bottom = u32::from(frame.top) + u32::from(frame.height);
    if right > u32::from(canvas_width) || bottom > u32::from(canvas_height) {
        return Err(GifDecodeError::FrameOutOfBounds {
            frame_index,
            left: frame.left,
            top: frame.top,
            width: frame.width,
            height: frame.height,
            canvas_width,
            canvas_height,
        });
    }
    Ok(())
}

fn rgba_len(width: u16, height: u16) -> u64 {
    u64::from(width) * u64::from(height) * RGBA_BYTES_PER_PIXEL
}

fn platform_len(bytes: u64) -> Result<usize, GifDecodeError> {
    usize::try_from(bytes).map_err(|_| GifDecodeError::AddressSpaceExceeded { bytes })
}

fn enforce_total_bytes(required_bytes: u64, limit_bytes: u64) -> Result<(), GifDecodeError> {
    if required_bytes > limit_bytes {
        Err(GifDecodeError::TotalRgbaBytesLimitExceeded {
            required_bytes,
            limit_bytes,
        })
    } else {
        Ok(())
    }
}

fn zeroed_buffer(bytes: usize) -> Result<Vec<u8>, GifDecodeError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(bytes)
        .map_err(|_| GifDecodeError::AllocationFailed { bytes })?;
    buffer.resize(bytes, 0);
    Ok(buffer)
}

fn copy_buffer(source: &[u8]) -> Result<Vec<u8>, GifDecodeError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(source.len())
        .map_err(|_| GifDecodeError::AllocationFailed {
            bytes: source.len(),
        })?;
    copy.extend_from_slice(source);
    Ok(copy)
}

fn copy_region(
    canvas: &[u8],
    canvas_width: u16,
    frame: FrameInfo,
) -> Result<Vec<u8>, GifDecodeError> {
    let row_bytes = usize::from(frame.width) * RGBA_CHANNELS;
    let region_bytes = row_bytes * usize::from(frame.height);
    let mut region = Vec::new();
    region
        .try_reserve_exact(region_bytes)
        .map_err(|_| GifDecodeError::AllocationFailed {
            bytes: region_bytes,
        })?;

    for row in 0..usize::from(frame.height) {
        let canvas_start = pixel_offset(
            canvas_width,
            usize::from(frame.left),
            usize::from(frame.top) + row,
        );
        region.extend_from_slice(&canvas[canvas_start..canvas_start + row_bytes]);
    }
    Ok(region)
}

fn composite_region(canvas: &mut [u8], canvas_width: u16, rgba: &[u8], frame: FrameInfo) {
    let source_row_bytes = usize::from(frame.width) * RGBA_CHANNELS;
    for row in 0..usize::from(frame.height) {
        let source_start = row * source_row_bytes;
        let destination_start = pixel_offset(
            canvas_width,
            usize::from(frame.left),
            usize::from(frame.top) + row,
        );
        let source_row = &rgba[source_start..source_start + source_row_bytes];
        let destination_row = &mut canvas[destination_start..destination_start + source_row_bytes];

        for (source, destination) in source_row.as_chunks::<RGBA_CHANNELS>().0.iter().zip(
            destination_row
                .as_chunks_mut::<RGBA_CHANNELS>()
                .0
                .iter_mut(),
        ) {
            if source[3] != 0 {
                destination.copy_from_slice(source);
            }
        }
    }
}

fn clear_region(canvas: &mut [u8], canvas_width: u16, frame: FrameInfo) {
    let row_bytes = usize::from(frame.width) * RGBA_CHANNELS;
    for row in 0..usize::from(frame.height) {
        let start = pixel_offset(
            canvas_width,
            usize::from(frame.left),
            usize::from(frame.top) + row,
        );
        canvas[start..start + row_bytes].fill(0);
    }
}

fn restore_region(canvas: &mut [u8], canvas_width: u16, frame: FrameInfo, previous: &[u8]) {
    let row_bytes = usize::from(frame.width) * RGBA_CHANNELS;
    for row in 0..usize::from(frame.height) {
        let canvas_start = pixel_offset(
            canvas_width,
            usize::from(frame.left),
            usize::from(frame.top) + row,
        );
        let previous_start = row * row_bytes;
        canvas[canvas_start..canvas_start + row_bytes]
            .copy_from_slice(&previous[previous_start..previous_start + row_bytes]);
    }
}

fn pixel_offset(canvas_width: u16, x: usize, y: usize) -> usize {
    (y * usize::from(canvas_width) + x) * RGBA_CHANNELS
}

fn frame_duration_us(
    delay: u16,
    frame_index: usize,
    policy: ZeroDelayPolicy,
) -> Result<u64, GifDecodeError> {
    if delay != 0 {
        return Ok(u64::from(delay) * GIF_TICK_US);
    }

    match policy {
        ZeroDelayPolicy::UseMinimum(duration) => Ok(duration.get()),
        ZeroDelayPolicy::Reject => Err(GifDecodeError::ZeroFrameDelay { frame_index }),
    }
}

const fn map_repeat(repeat: Repeat) -> LoopBehavior {
    match repeat {
        Repeat::Finite(0) => LoopBehavior::Once,
        Repeat::Finite(count) => LoopBehavior::Finite(count),
        Repeat::Infinite => LoopBehavior::Infinite,
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, io::Cursor, num::NonZeroU64};

    use gif::{DisposalMethod, Encoder, Frame, Repeat};

    use super::*;

    const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];
    const GLOBAL_PALETTE: &[u8] = &[
        0, 0, 0, // transparent/black
        255, 0, 0, // red
        0, 255, 0, // green
        0, 0, 255, // blue
    ];

    #[derive(Clone)]
    struct InputFrame {
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        indices: Vec<u8>,
        palette: Option<Vec<u8>>,
        transparent: Option<u8>,
        delay: u16,
        disposal: DisposalMethod,
    }

    impl InputFrame {
        fn solid(width: u16, height: u16, index: u8) -> Self {
            Self {
                left: 0,
                top: 0,
                width,
                height,
                indices: vec![index; usize::from(width) * usize::from(height)],
                palette: None,
                transparent: None,
                delay: 1,
                disposal: DisposalMethod::Keep,
            }
        }
    }

    fn make_gif(
        width: u16,
        height: u16,
        palette: &[u8],
        repeat: Option<Repeat>,
        frames: &[InputFrame],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, width, height, palette).unwrap();
            if let Some(repeat) = repeat {
                encoder.set_repeat(repeat).unwrap();
            }
            for input in frames {
                let frame = Frame {
                    delay: input.delay,
                    dispose: input.disposal,
                    transparent: input.transparent,
                    top: input.top,
                    left: input.left,
                    width: input.width,
                    height: input.height,
                    palette: input.palette.clone(),
                    buffer: Cow::Owned(input.indices.clone()),
                    ..Frame::default()
                };
                encoder.write_frame(&frame).unwrap();
            }
        }
        bytes
    }

    fn pixels(colors: &[[u8; 4]]) -> Vec<u8> {
        colors.iter().flatten().copied().collect()
    }

    fn decode(bytes: &[u8]) -> DecodedAnimation {
        decode_gif(Cursor::new(bytes), &GifDecodeOptions::default()).unwrap()
    }

    #[test]
    fn composites_offsets_palettes_transparency_and_all_disposals() {
        let frames = vec![
            InputFrame {
                delay: 2,
                ..InputFrame::solid(3, 2, 1)
            },
            InputFrame {
                left: 1,
                top: 0,
                width: 1,
                height: 1,
                indices: vec![1],
                palette: Some(vec![0, 0, 0, 255, 255, 0]),
                transparent: None,
                delay: 0,
                disposal: DisposalMethod::Previous,
            },
            InputFrame {
                left: 0,
                top: 1,
                width: 1,
                height: 1,
                indices: vec![2],
                palette: None,
                transparent: None,
                delay: 3,
                disposal: DisposalMethod::Background,
            },
            InputFrame {
                left: 0,
                top: 1,
                width: 2,
                height: 1,
                indices: vec![0, 3],
                palette: None,
                transparent: Some(0),
                delay: 4,
                disposal: DisposalMethod::Keep,
            },
        ];
        let bytes = make_gif(3, 2, GLOBAL_PALETTE, Some(Repeat::Infinite), &frames);

        let animation = decode(&bytes);

        assert_eq!((animation.width(), animation.height()), (3, 2));
        assert_eq!(animation.loop_behavior(), LoopBehavior::Infinite);
        assert_eq!(animation.frames().len(), 4);
        assert_eq!(animation.frames()[0].duration_us(), 20_000);
        assert_eq!(animation.frames()[1].duration_us(), 10_000);
        assert_eq!(animation.frames()[2].duration_us(), 30_000);
        assert_eq!(animation.frames()[3].duration_us(), 40_000);
        assert_eq!(animation.frames()[0].rgba(), pixels(&[RED; 6]));
        assert_eq!(
            animation.frames()[1].rgba(),
            pixels(&[RED, YELLOW, RED, RED, RED, RED])
        );
        assert_eq!(
            animation.frames()[2].rgba(),
            pixels(&[RED, RED, RED, GREEN, RED, RED])
        );
        assert_eq!(
            animation.frames()[3].rgba(),
            pixels(&[RED, RED, RED, TRANSPARENT, BLUE, RED])
        );
    }

    #[test]
    fn preserves_once_and_finite_loop_metadata() {
        let frame = InputFrame::solid(1, 1, 1);
        let once = make_gif(1, 1, GLOBAL_PALETTE, None, std::slice::from_ref(&frame));
        let finite = make_gif(1, 1, GLOBAL_PALETTE, Some(Repeat::Finite(7)), &[frame]);

        assert_eq!(decode(&once).loop_behavior(), LoopBehavior::Once);
        assert_eq!(decode(&finite).loop_behavior(), LoopBehavior::Finite(7));
    }

    #[test]
    fn zero_delay_policy_is_configurable_or_strict() {
        let frame = InputFrame {
            delay: 0,
            ..InputFrame::solid(1, 1, 1)
        };
        let bytes = make_gif(1, 1, GLOBAL_PALETTE, None, &[frame]);
        let custom = GifDecodeOptions {
            zero_delay: ZeroDelayPolicy::UseMinimum(NonZeroU64::new(25_000).unwrap()),
            ..GifDecodeOptions::default()
        };
        assert_eq!(
            decode_gif(Cursor::new(&bytes), &custom).unwrap().frames()[0].duration_us(),
            25_000
        );

        let strict = GifDecodeOptions {
            zero_delay: ZeroDelayPolicy::Reject,
            ..GifDecodeOptions::default()
        };
        assert!(matches!(
            decode_gif(Cursor::new(&bytes), &strict),
            Err(GifDecodeError::ZeroFrameDelay { frame_index: 0 })
        ));
    }

    #[test]
    fn rejects_canvas_dimension_limits_before_decoding_pixels() {
        let bytes = make_gif(3, 2, GLOBAL_PALETTE, None, &[InputFrame::solid(3, 2, 1)]);
        let options = GifDecodeOptions {
            limits: DecodeLimits {
                max_width: 2,
                ..DecodeLimits::default()
            },
            ..GifDecodeOptions::default()
        };

        assert!(matches!(
            decode_gif(Cursor::new(bytes), &options),
            Err(GifDecodeError::WidthLimitExceeded {
                actual: 3,
                limit: 2
            })
        ));
    }

    #[test]
    fn rejects_frame_count_and_total_output_byte_limits() {
        let frames = [InputFrame::solid(2, 1, 1), InputFrame::solid(2, 1, 2)];
        let bytes = make_gif(2, 1, GLOBAL_PALETTE, None, &frames);
        let one_frame = GifDecodeOptions {
            limits: DecodeLimits {
                max_frames: 1,
                ..DecodeLimits::default()
            },
            ..GifDecodeOptions::default()
        };
        assert!(matches!(
            decode_gif(Cursor::new(&bytes), &one_frame),
            Err(GifDecodeError::FrameLimitExceeded { limit: 1 })
        ));

        let eight_bytes = GifDecodeOptions {
            limits: DecodeLimits {
                max_total_rgba_bytes: 8,
                ..DecodeLimits::default()
            },
            ..GifDecodeOptions::default()
        };
        assert!(matches!(
            decode_gif(Cursor::new(&bytes), &eight_bytes),
            Err(GifDecodeError::TotalRgbaBytesLimitExceeded {
                required_bytes: 16,
                limit_bytes: 8
            })
        ));
    }

    #[test]
    fn rejects_frame_rectangles_outside_the_canvas() {
        let frame = InputFrame {
            left: 2,
            top: 0,
            ..InputFrame::solid(1, 1, 1)
        };
        let bytes = make_gif(2, 2, GLOBAL_PALETTE, None, &[frame]);

        assert!(matches!(
            decode_gif(Cursor::new(bytes), &GifDecodeOptions::default()),
            Err(GifDecodeError::FrameOutOfBounds { frame_index: 0, .. })
        ));
    }
}
