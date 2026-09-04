//! GIF encoding boundary and built-in encoder for GifFromScreen.
//!
//! The public port deals only in owned, full-canvas, straight-alpha sRGB RGBA8
//! frames. The built-in adapter currently emits full-size frames with a local,
//! deterministic median-cut palette. More sophisticated palette and delta-frame
//! pipelines can be added without exposing the underlying `gif` crate.

#![forbid(unsafe_code)]

mod control;
mod encoder;
mod error;
mod frame;
mod quantize;
mod source;
mod timing;

pub use control::{
    CancellationFlag, CancellationToken, EncodePhase, EncodeProgress, NeverCancel, NoopProgress,
    ProgressSink,
};
pub use encoder::{
    BuiltinGifEncoder, DEFAULT_GLOBAL_PALETTE_BUFFER_LIMIT_BYTES, DeltaMode, EncodeOptions,
    EncodeReport, GifEncoder, LoopBehavior, PaletteMode, Transparency,
};
pub use error::{FrameError, FrameSourceError, GifEncodeError, QuantizationError};
pub use frame::{DirtyRect, RgbaFrame};
pub use quantize::{
    ColorPalette, DitherMode, FrameQuantizer, IndexedFrame, MedianCutQuantizer,
    QuantizationSettings,
};
pub use source::{IteratorFrameSource, RgbaFrameSource};
pub use timing::{GIF_TICK_US, GifTimingQuantizer};
