//! Safe, bounded media import for `GifFromScreen`.
//!
//! The crate currently imports GIF animations only. Decoded pixels are
//! straight-alpha sRGB RGBA8 and every returned frame covers the complete
//! logical GIF canvas.

#![forbid(unsafe_code)]

mod decode;
mod error;
mod model;

pub use decode::decode_gif;
pub use error::GifDecodeError;
pub use model::{
    DEFAULT_MAX_FRAMES, DEFAULT_MAX_HEIGHT, DEFAULT_MAX_TOTAL_RGBA_BYTES, DEFAULT_MAX_WIDTH,
    DEFAULT_ZERO_DELAY_US, DecodeLimits, DecodedAnimation, DecodedFrame, GifDecodeOptions,
    LoopBehavior, ZeroDelayPolicy,
};
