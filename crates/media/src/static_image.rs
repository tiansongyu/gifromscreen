use std::{
    fmt,
    io::{self, BufRead, Seek, SeekFrom},
    num::NonZeroU64,
};

use image::{
    DynamicImage, ImageDecoder as _, ImageError, ImageFormat, ImageReader, Limits,
    metadata::Orientation,
};
use thiserror::Error;

use crate::{DEFAULT_ZERO_DELAY_US, DecodeLimits, DecodedAnimation, DecodedFrame, LoopBehavior};

const RGBA_BYTES_PER_PIXEL: u64 = 4;

/// Static raster formats intentionally accepted by [`decode_static_image`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StaticImageFormat {
    /// Portable Network Graphics, including alpha.
    Png,
    /// JPEG/JFIF image.
    Jpeg,
    /// Windows bitmap.
    Bmp,
    /// Static WebP image, including alpha when present.
    WebP,
}

impl StaticImageFormat {
    /// Canonical media type for source provenance, independent of filenames.
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Bmp => "image/bmp",
            Self::WebP => "image/webp",
        }
    }
}

impl fmt::Display for StaticImageFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Bmp => "BMP",
            Self::WebP => "WebP",
        })
    }
}

/// A decoded one-frame animation together with its content-detected format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedStaticImage {
    format: StaticImageFormat,
    animation: DecodedAnimation,
}

impl DecodedStaticImage {
    /// Returns the format detected from the input signature.
    pub const fn format(&self) -> StaticImageFormat {
        self.format
    }

    /// Borrows the normalized one-frame animation.
    pub const fn animation(&self) -> &DecodedAnimation {
        &self.animation
    }

    /// Consumes the result and returns its normalized animation.
    pub fn into_animation(self) -> DecodedAnimation {
        self.animation
    }
}

/// Limits and timeline duration applied to one decoded static image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticImageDecodeOptions {
    /// Dimension, frame-count, and retained RGBA byte limits.
    pub limits: DecodeLimits,
    /// Positive duration assigned to the returned single frame.
    pub frame_duration_us: NonZeroU64,
}

impl Default for StaticImageDecodeOptions {
    fn default() -> Self {
        Self {
            limits: DecodeLimits::default(),
            frame_duration_us: NonZeroU64::new(DEFAULT_ZERO_DELAY_US)
                .expect("the default static-frame duration is non-zero"),
        }
    }
}

/// Failure while probing or decoding an untrusted static raster image.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StaticImageDecodeError {
    /// Content signatures did not identify a known image format.
    #[error("image format could not be identified from its content")]
    UnknownFormat,
    /// A known format is outside this API's static-image allowlist.
    #[error("detected image format {format} is not supported by the static-image decoder")]
    UnsupportedFormat {
        /// Debug name reported by the format sniffer.
        format: String,
    },
    /// The selected format's codec or one of its file features is unavailable.
    #[error("{format} decoding is unavailable or does not support this image feature: {source}")]
    CodecUnsupported {
        /// Detected supported format.
        format: StaticImageFormat,
        /// Underlying codec diagnostic.
        #[source]
        source: ImageError,
    },
    /// Seeking or reading the supplied stream failed.
    #[error("could not {operation} static image stream: {source}")]
    Io {
        /// Stream operation that failed.
        operation: &'static str,
        /// Underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// The codec rejected the header before pixel decoding began.
    #[error("could not inspect {format} image header: {source}")]
    Header {
        /// Detected supported format.
        format: StaticImageFormat,
        /// Underlying header diagnostic.
        #[source]
        source: ImageError,
    },
    /// The source declares an empty image.
    #[error("static image dimensions must be non-zero, got {width}x{height}")]
    EmptyDimensions {
        /// Oriented image width.
        width: u32,
        /// Oriented image height.
        height: u32,
    },
    /// The configured width bound was exceeded after applying orientation.
    #[error("static image width {actual} exceeds the configured limit of {limit}")]
    WidthLimitExceeded {
        /// Oriented source width.
        actual: u32,
        /// Maximum accepted width.
        limit: u16,
    },
    /// The configured height bound was exceeded after applying orientation.
    #[error("static image height {actual} exceeds the configured limit of {limit}")]
    HeightLimitExceeded {
        /// Oriented source height.
        actual: u32,
        /// Maximum accepted height.
        limit: u16,
    },
    /// The shared frame-count policy does not permit the one returned frame.
    #[error("static image requires one frame, above the configured limit of {limit}")]
    FrameLimitExceeded {
        /// Configured maximum frame count.
        limit: usize,
    },
    /// Pixel count exceeds what the retained RGBA byte limit can represent.
    #[error(
        "static image has {actual_pixels} pixels, above the configured RGBA capacity of {limit_pixels} pixels"
    )]
    PixelLimitExceeded {
        /// Oriented image pixel count.
        actual_pixels: u64,
        /// Maximum pixels representable as four-byte RGBA within the configured limit.
        limit_pixels: u64,
    },
    /// The codec's native decoded buffer exceeds the configured memory bound.
    #[error(
        "{format} decoder requires {required_bytes} bytes before RGBA conversion, above the configured limit of {limit_bytes}"
    )]
    DecodeMemoryLimitExceeded {
        /// Detected supported format.
        format: StaticImageFormat,
        /// Codec-advertised native buffer length.
        required_bytes: u64,
        /// Configured decoder allocation bound.
        limit_bytes: u64,
    },
    /// A required RGBA byte length overflowed `u64`.
    #[error("RGBA byte length overflowed for {width}x{height} static image")]
    RgbaByteLengthOverflow {
        /// Oriented image width.
        width: u32,
        /// Oriented image height.
        height: u32,
    },
    /// The bounded output cannot be addressed on this platform.
    #[error("RGBA buffer length {bytes} cannot be represented on this platform")]
    AddressSpaceExceeded {
        /// Required tightly packed output bytes.
        bytes: u64,
    },
    /// The image crate refused the configured decoder limit.
    #[error("{format} decoder could not honor the configured memory limit: {source}")]
    DecoderLimit {
        /// Detected supported format.
        format: StaticImageFormat,
        /// Underlying limit diagnostic.
        #[source]
        source: ImageError,
    },
    /// Full pixel decoding or RGBA conversion failed.
    #[error("{format} pixel decoding failed: {source}")]
    Decode {
        /// Detected supported format.
        format: StaticImageFormat,
        /// Underlying codec diagnostic.
        #[source]
        source: ImageError,
    },
    /// Codec output did not match the preflight dimensions.
    #[error(
        "{format} decoded to {actual_width}x{actual_height}, expected oriented dimensions {expected_width}x{expected_height}"
    )]
    DecodedDimensionMismatch {
        /// Detected supported format.
        format: StaticImageFormat,
        /// Expected oriented width.
        expected_width: u32,
        /// Expected oriented height.
        expected_height: u32,
        /// Actual decoded width.
        actual_width: u32,
        /// Actual decoded height.
        actual_height: u32,
    },
    /// Codec output did not contain exactly one packed RGBA value per pixel.
    #[error("decoded RGBA buffer has {actual} bytes, expected {expected}")]
    InvalidRgbaLength {
        /// Preflight output byte length.
        expected: usize,
        /// Actual converted buffer length.
        actual: usize,
    },
}

/// Decodes one PNG, JPEG, BMP, or static WebP into a one-frame animation.
///
/// This compatibility wrapper delegates once to
/// [`decode_static_image_with_format`] and discards only the detected-format
/// field; it never probes or decodes the stream a second time.
///
/// # Errors
///
/// Returns [`StaticImageDecodeError`] under the same conditions as
/// [`decode_static_image_with_format`].
pub fn decode_static_image<R: BufRead + Seek>(
    reader: R,
    options: &StaticImageDecodeOptions,
) -> Result<DecodedAnimation, StaticImageDecodeError> {
    decode_static_image_with_format(reader, options).map(DecodedStaticImage::into_animation)
}

/// Decodes a static raster and retains its content-detected format.
///
/// The function identifies content rather than trusting a filename. A header
/// pass obtains dimensions, native decoded bytes, and EXIF orientation before
/// full pixel decoding. Oriented dimensions, one-frame capacity, output pixel
/// count, native decoder bytes, and platform addressability must all fit
/// [`DecodeLimits`] first. Decoder allocation limits are also forwarded to the
/// `image` crate.
///
/// JPEG and WebP EXIF orientation is normalized when exposed by their active
/// codecs; rotated results therefore report their post-orientation dimensions.
/// PNG/BMP orientation is applied if a codec exposes it. Embedded ICC profiles
/// are currently not color-converted; channel values are treated as sRGB.
/// Animated GIF belongs to [`crate::decode_gif`] and is deliberately rejected
/// by this static-image entry point.
///
/// # Errors
///
/// Returns [`StaticImageDecodeError`] for unidentified or disallowed formats,
/// stream failures, malformed headers/pixels, unsupported codec features, or
/// any configured dimension, frame, pixel, memory, or address-space limit.
pub fn decode_static_image_with_format<R: BufRead + Seek>(
    mut reader: R,
    options: &StaticImageDecodeOptions,
) -> Result<DecodedStaticImage, StaticImageDecodeError> {
    let start = reader
        .stream_position()
        .map_err(|source| StaticImageDecodeError::Io {
            operation: "read the initial position of",
            source,
        })?;
    let guessed = ImageReader::new(&mut reader)
        .with_guessed_format()
        .map_err(|source| StaticImageDecodeError::Io {
            operation: "identify",
            source,
        })?;
    let image_format = guessed
        .format()
        .ok_or(StaticImageDecodeError::UnknownFormat)?;
    drop(guessed);
    let format = supported_format(image_format)?;
    rewind(&mut reader, start, "rewind")?;

    // The first decoder only parses metadata, but still receives strict edge
    // and allocation bounds because codec construction itself handles
    // untrusted chunks. A common edge bound permits EXIF width/height swaps;
    // the oriented dimensions are checked against each axis below.
    let mut header_reader = ImageReader::with_format(&mut reader, image_format);
    let mut header_limits = Limits::default();
    let max_oriented_edge = u32::from(options.limits.max_width.max(options.limits.max_height));
    header_limits.max_image_width = Some(max_oriented_edge);
    header_limits.max_image_height = Some(max_oriented_edge);
    header_limits.max_alloc = Some(options.limits.max_total_rgba_bytes);
    header_reader.limits(header_limits);
    let mut header_decoder = header_reader
        .into_decoder()
        .map_err(|source| classify_image_error(format, "inspect", source))?;
    let raw_dimensions = header_decoder.dimensions();
    let orientation = header_decoder
        .orientation()
        .map_err(|source| classify_image_error(format, "inspect", source))?;
    let native_bytes = header_decoder.total_bytes();
    let dimensions = oriented_dimensions(raw_dimensions, orientation);
    validate_preflight(format, dimensions, native_bytes, options.limits)?;
    let rgba_bytes = checked_rgba_bytes(dimensions)?;
    let rgba_len = usize::try_from(rgba_bytes)
        .map_err(|_| StaticImageDecodeError::AddressSpaceExceeded { bytes: rgba_bytes })?;
    drop(header_decoder);

    rewind(&mut reader, start, "rewind after header inspection")?;
    let mut decode_reader = ImageReader::with_format(reader, image_format);
    let mut decoder_limits = Limits::default();
    decoder_limits.max_image_width = Some(raw_dimensions.0);
    decoder_limits.max_image_height = Some(raw_dimensions.1);
    decoder_limits.max_alloc = Some(options.limits.max_total_rgba_bytes);
    decode_reader.limits(decoder_limits);
    let pixel_decoder = decode_reader
        .into_decoder()
        .map_err(|source| classify_image_error(format, "decode", source))?;
    let mut dynamic_image = DynamicImage::from_decoder(pixel_decoder)
        .map_err(|source| classify_image_error(format, "decode", source))?;
    dynamic_image.apply_orientation(orientation);
    let actual_dimensions = (dynamic_image.width(), dynamic_image.height());
    if actual_dimensions != dimensions {
        return Err(StaticImageDecodeError::DecodedDimensionMismatch {
            format,
            expected_width: dimensions.0,
            expected_height: dimensions.1,
            actual_width: actual_dimensions.0,
            actual_height: actual_dimensions.1,
        });
    }
    let rgba = dynamic_image.into_rgba8().into_raw();
    if rgba.len() != rgba_len {
        return Err(StaticImageDecodeError::InvalidRgbaLength {
            expected: rgba_len,
            actual: rgba.len(),
        });
    }

    let width =
        u16::try_from(dimensions.0).map_err(|_| StaticImageDecodeError::WidthLimitExceeded {
            actual: dimensions.0,
            limit: options.limits.max_width,
        })?;
    let height =
        u16::try_from(dimensions.1).map_err(|_| StaticImageDecodeError::HeightLimitExceeded {
            actual: dimensions.1,
            limit: options.limits.max_height,
        })?;
    Ok(DecodedStaticImage {
        format,
        animation: DecodedAnimation {
            width,
            height,
            frames: vec![DecodedFrame {
                rgba,
                duration_us: options.frame_duration_us.get(),
            }],
            loop_behavior: LoopBehavior::Once,
        },
    })
}

fn rewind(
    reader: &mut (impl Seek + ?Sized),
    position: u64,
    operation: &'static str,
) -> Result<(), StaticImageDecodeError> {
    reader
        .seek(SeekFrom::Start(position))
        .map(|_| ())
        .map_err(|source| StaticImageDecodeError::Io { operation, source })
}

fn supported_format(format: ImageFormat) -> Result<StaticImageFormat, StaticImageDecodeError> {
    match format {
        ImageFormat::Png => Ok(StaticImageFormat::Png),
        ImageFormat::Jpeg => Ok(StaticImageFormat::Jpeg),
        ImageFormat::Bmp => Ok(StaticImageFormat::Bmp),
        ImageFormat::WebP => Ok(StaticImageFormat::WebP),
        unsupported => Err(StaticImageDecodeError::UnsupportedFormat {
            format: format!("{unsupported:?}"),
        }),
    }
}

fn validate_preflight(
    format: StaticImageFormat,
    dimensions: (u32, u32),
    native_bytes: u64,
    limits: DecodeLimits,
) -> Result<(), StaticImageDecodeError> {
    if dimensions.0 == 0 || dimensions.1 == 0 {
        return Err(StaticImageDecodeError::EmptyDimensions {
            width: dimensions.0,
            height: dimensions.1,
        });
    }
    if dimensions.0 > u32::from(limits.max_width) {
        return Err(StaticImageDecodeError::WidthLimitExceeded {
            actual: dimensions.0,
            limit: limits.max_width,
        });
    }
    if dimensions.1 > u32::from(limits.max_height) {
        return Err(StaticImageDecodeError::HeightLimitExceeded {
            actual: dimensions.1,
            limit: limits.max_height,
        });
    }
    if limits.max_frames == 0 {
        return Err(StaticImageDecodeError::FrameLimitExceeded { limit: 0 });
    }
    let pixels = u64::from(dimensions.0) * u64::from(dimensions.1);
    let pixel_limit = limits.max_total_rgba_bytes / RGBA_BYTES_PER_PIXEL;
    if pixels > pixel_limit {
        return Err(StaticImageDecodeError::PixelLimitExceeded {
            actual_pixels: pixels,
            limit_pixels: pixel_limit,
        });
    }
    if native_bytes > limits.max_total_rgba_bytes {
        return Err(StaticImageDecodeError::DecodeMemoryLimitExceeded {
            format,
            required_bytes: native_bytes,
            limit_bytes: limits.max_total_rgba_bytes,
        });
    }
    Ok(())
}

fn checked_rgba_bytes(dimensions: (u32, u32)) -> Result<u64, StaticImageDecodeError> {
    u64::from(dimensions.0)
        .checked_mul(u64::from(dimensions.1))
        .and_then(|pixels| pixels.checked_mul(RGBA_BYTES_PER_PIXEL))
        .ok_or(StaticImageDecodeError::RgbaByteLengthOverflow {
            width: dimensions.0,
            height: dimensions.1,
        })
}

const fn oriented_dimensions(dimensions: (u32, u32), orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (dimensions.1, dimensions.0),
        _ => dimensions,
    }
}

fn classify_image_error(
    format: StaticImageFormat,
    stage: &'static str,
    source: ImageError,
) -> StaticImageDecodeError {
    match source {
        ImageError::IoError(source) if source.kind() == io::ErrorKind::UnexpectedEof => {
            let source = ImageError::IoError(source);
            if stage == "inspect" {
                StaticImageDecodeError::Header { format, source }
            } else {
                StaticImageDecodeError::Decode { format, source }
            }
        }
        ImageError::IoError(source) => StaticImageDecodeError::Io {
            operation: stage,
            source,
        },
        source @ ImageError::Unsupported(_) => {
            StaticImageDecodeError::CodecUnsupported { format, source }
        }
        source @ ImageError::Limits(_) => StaticImageDecodeError::DecoderLimit { format, source },
        source if stage == "inspect" => StaticImageDecodeError::Header { format, source },
        source => StaticImageDecodeError::Decode { format, source },
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use gif::{Encoder as GifEncoder, Frame as GifFrame};
    use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb, Rgba};

    use super::*;

    const RGBA_FIXTURE: [u8; 24] = [
        255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 12, 34, 56, 64, 240, 180, 20, 200, 90, 45,
        180, 255,
    ];

    fn encode_rgba(format: ImageFormat) -> Vec<u8> {
        let buffer = ImageBuffer::<Rgba<u8>, _>::from_raw(3, 2, RGBA_FIXTURE.to_vec()).unwrap();
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(buffer)
            .write_to(&mut encoded, format)
            .unwrap();
        encoded.into_inner()
    }

    fn encode_rgb_jpeg() -> Vec<u8> {
        let pixels = [42, 101, 179].repeat(6);
        let buffer = ImageBuffer::<Rgb<u8>, _>::from_raw(3, 2, pixels).unwrap();
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(buffer)
            .write_to(&mut encoded, ImageFormat::Jpeg)
            .unwrap();
        encoded.into_inner()
    }

    fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        let mut tiff = vec![
            b'I', b'I', 42, 0, // little-endian TIFF header
            8, 0, 0, 0, // first IFD offset
            1, 0, // one directory entry
            0x12, 0x01, // Orientation tag
            3, 0, // SHORT
            1, 0, 0, 0, // one value
        ];
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);
        let segment_len = u16::try_from(payload.len() + 2).unwrap();
        let mut oriented = Vec::with_capacity(jpeg.len() + payload.len() + 4);
        oriented.extend_from_slice(&jpeg[..2]);
        oriented.extend_from_slice(&[0xff, 0xe1]);
        oriented.extend_from_slice(&segment_len.to_be_bytes());
        oriented.extend_from_slice(&payload);
        oriented.extend_from_slice(&jpeg[2..]);
        oriented
    }

    fn options() -> StaticImageDecodeOptions {
        StaticImageDecodeOptions {
            frame_duration_us: NonZeroU64::new(42_000).unwrap(),
            ..StaticImageDecodeOptions::default()
        }
    }

    fn decode_fixture(bytes: Vec<u8>) -> DecodedAnimation {
        decode_static_image(Cursor::new(bytes), &options()).unwrap()
    }

    fn assert_lossless_rgba(format: ImageFormat) {
        let decoded = decode_fixture(encode_rgba(format));
        assert_eq!((decoded.width(), decoded.height()), (3, 2));
        assert_eq!(decoded.loop_behavior(), LoopBehavior::Once);
        assert_eq!(decoded.frames().len(), 1);
        assert_eq!(decoded.frames()[0].duration_us(), 42_000);
        assert_eq!(decoded.frames()[0].rgba(), RGBA_FIXTURE);
    }

    #[test]
    fn with_format_result_exposes_borrowed_and_owned_animation() {
        let fixtures = [
            (
                StaticImageFormat::Png,
                "image/png",
                encode_rgba(ImageFormat::Png),
            ),
            (StaticImageFormat::Jpeg, "image/jpeg", encode_rgb_jpeg()),
            (
                StaticImageFormat::Bmp,
                "image/bmp",
                encode_rgba(ImageFormat::Bmp),
            ),
            (
                StaticImageFormat::WebP,
                "image/webp",
                encode_rgba(ImageFormat::WebP),
            ),
        ];
        for (expected_format, expected_media_type, bytes) in fixtures {
            let decoded = decode_static_image_with_format(Cursor::new(bytes), &options()).unwrap();
            assert_eq!(decoded.format(), expected_format);
            assert_eq!(decoded.format().media_type(), expected_media_type);
            assert_eq!(decoded.animation().frames().len(), 1);
            let borrowed = decoded.animation().clone();
            assert_eq!(decoded.into_animation(), borrowed);
        }
    }

    #[test]
    fn png_memory_fixture_roundtrips_rgba_and_alpha() {
        assert_lossless_rgba(ImageFormat::Png);
    }

    #[test]
    fn bmp_memory_fixture_roundtrips_rgba_and_alpha() {
        assert_lossless_rgba(ImageFormat::Bmp);
    }

    #[test]
    fn webp_memory_fixture_roundtrips_rgba_and_alpha() {
        assert_lossless_rgba(ImageFormat::WebP);
    }

    #[test]
    fn jpeg_memory_fixture_decodes_as_opaque_rgba() {
        let decoded = decode_fixture(encode_rgb_jpeg());
        assert_eq!((decoded.width(), decoded.height()), (3, 2));
        assert_eq!(decoded.loop_behavior(), LoopBehavior::Once);
        assert_eq!(decoded.frames()[0].duration_us(), 42_000);
        for pixel in decoded.frames()[0].rgba().as_chunks::<4>().0 {
            assert_eq!(pixel[3], 255);
            for (actual, expected) in pixel[..3].iter().zip([42, 101, 179]) {
                assert!(actual.abs_diff(expected) <= 3);
            }
        }
    }

    #[test]
    fn jpeg_exif_orientation_is_applied_before_dimension_validation() {
        // EXIF value 6 is a clockwise quarter turn.
        let jpeg = encode_rgb_jpeg();
        let decoded = decode_fixture(with_exif_orientation(&jpeg, 6));
        assert_eq!((decoded.width(), decoded.height()), (2, 3));

        let oriented_width_limit = StaticImageDecodeOptions {
            limits: DecodeLimits {
                max_width: 1,
                max_height: 3,
                ..DecodeLimits::default()
            },
            ..options()
        };
        assert!(matches!(
            decode_static_image(
                Cursor::new(with_exif_orientation(&jpeg, 6)),
                &oriented_width_limit,
            ),
            Err(StaticImageDecodeError::WidthLimitExceeded {
                actual: 2,
                limit: 1,
            })
        ));
    }

    #[test]
    fn rejects_unknown_disallowed_and_truncated_inputs_distinctly() {
        assert!(matches!(
            decode_static_image(Cursor::new(b"not an image"), &options()),
            Err(StaticImageDecodeError::UnknownFormat)
        ));

        let mut gif_bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut gif_bytes, 1, 1, &[0, 0, 0, 255, 0, 0]).unwrap();
            encoder.write_frame(&GifFrame::default()).unwrap();
        }
        assert!(matches!(
            decode_static_image(Cursor::new(gif_bytes), &options()),
            Err(StaticImageDecodeError::UnsupportedFormat { ref format }) if format == "Gif"
        ));

        let truncated =
            decode_static_image(Cursor::new([137, 80, 78, 71, 13, 10, 26, 10]), &options())
                .unwrap_err();
        assert!(
            matches!(
                truncated,
                StaticImageDecodeError::Header {
                    format: StaticImageFormat::Png,
                    ..
                }
            ),
            "{truncated:?}"
        );
    }

    #[test]
    fn rejects_dimensions_frame_count_pixels_and_native_memory_before_decode() {
        let png = encode_rgba(ImageFormat::Png);
        let width_limited = StaticImageDecodeOptions {
            limits: DecodeLimits {
                max_width: 2,
                ..DecodeLimits::default()
            },
            ..options()
        };
        assert!(matches!(
            decode_static_image(Cursor::new(&png), &width_limited),
            Err(StaticImageDecodeError::WidthLimitExceeded {
                actual: 3,
                limit: 2,
            })
        ));

        let height_limited = StaticImageDecodeOptions {
            limits: DecodeLimits {
                max_height: 1,
                ..DecodeLimits::default()
            },
            ..options()
        };
        assert!(matches!(
            decode_static_image(Cursor::new(&png), &height_limited),
            Err(StaticImageDecodeError::HeightLimitExceeded {
                actual: 2,
                limit: 1,
            })
        ));

        let no_frames = StaticImageDecodeOptions {
            limits: DecodeLimits {
                max_frames: 0,
                ..DecodeLimits::default()
            },
            ..options()
        };
        assert!(matches!(
            decode_static_image(Cursor::new(&png), &no_frames),
            Err(StaticImageDecodeError::FrameLimitExceeded { limit: 0 })
        ));

        let pixel_limited = StaticImageDecodeOptions {
            limits: DecodeLimits {
                max_total_rgba_bytes: 20,
                ..DecodeLimits::default()
            },
            ..options()
        };
        assert!(matches!(
            decode_static_image(Cursor::new(png), &pixel_limited),
            Err(StaticImageDecodeError::PixelLimitExceeded {
                actual_pixels: 6,
                limit_pixels: 5,
            })
        ));

        assert!(matches!(
            validate_preflight(
                StaticImageFormat::Png,
                (1, 1),
                8,
                DecodeLimits {
                    max_total_rgba_bytes: 4,
                    ..DecodeLimits::default()
                },
            ),
            Err(StaticImageDecodeError::DecodeMemoryLimitExceeded {
                required_bytes: 8,
                limit_bytes: 4,
                ..
            })
        ));
    }
}
