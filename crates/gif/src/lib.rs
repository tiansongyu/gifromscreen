//! GIF encoding boundary and built-in encoder for GifFromScreen.
//!
//! The public port deals only in owned, full-canvas, straight-alpha sRGB RGBA8
//! frames. The built-in adapter supports deterministic median-cut, grayscale,
//! most-used-color, bounded octree, variance-minimizing Wu, and bounded-sample
//! NeuQuant quantizers with local or global palettes, plus validated custom and
//! predefined fixed palettes through [`FixedPaletteQuantizer`]. Palette mapping
//! includes ordered Bayer and Floyd-Steinberg, Atkinson, Burkes, and Sierra-family,
//! Jarvis–Judice–Ninke, Stucki, and Stevenson–Arce error diffusion through
//! [`DitherMode`]. More sophisticated palette and delta-frame pipelines can be
//! added without exposing the underlying `gif` crate.

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
    ColorPalette, DitherMode, FixedPaletteQuantizer, FrameQuantizer, GrayscaleQuantizer,
    IndexedFrame, MedianCutQuantizer, MostUsedQuantizer, NeuQuantQuantizer, OctreeQuantizer,
    PredefinedPalette, QuantizationSettings, QuantizerStrategy, WuQuantizer,
};
pub use source::{IteratorFrameSource, RgbaFrameSource};
pub use timing::{GIF_TICK_US, GifTimingQuantizer};
