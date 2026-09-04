//! Safe, bounded media import for `GifFromScreen`.
//!
//! GIF animations and common static raster images decode into bounded,
//! full-canvas straight-alpha sRGB RGBA8 frames.

#![forbid(unsafe_code)]

mod decode;
mod error;
mod model;
mod static_image;

pub use decode::decode_gif;
pub use error::GifDecodeError;
pub use model::{
    DEFAULT_MAX_FRAMES, DEFAULT_MAX_HEIGHT, DEFAULT_MAX_TOTAL_RGBA_BYTES, DEFAULT_MAX_WIDTH,
    DEFAULT_ZERO_DELAY_US, DecodeLimits, DecodedAnimation, DecodedFrame, GifDecodeOptions,
    LoopBehavior, ZeroDelayPolicy,
};
pub use static_image::{
    DecodedStaticImage, DecodedStaticImageSequence, StaticImageDecodeError,
    StaticImageDecodeOptions, StaticImageFormat, StaticImageSequenceDurationPolicy,
    StaticImageSequenceError, StaticImageSequenceOptions, assemble_static_image_sequence,
    decode_static_image, decode_static_image_with_format,
};
