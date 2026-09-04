//! Deterministic CPU rendering for `GifFromScreen`.
//!
//! Source assets and rendered surfaces use straight-alpha sRGB RGBA8 pixels.
//! A clip is rendered in a fixed order: crop, nearest-neighbor resize, rotation,
//! horizontal/vertical flips, then effects in their stored order. Blur uses an
//! edge-clamped separable box filter in alpha-weighted integer space and writes
//! straight-alpha pixels back. Keeping this pipeline CPU-only gives exports a
//! stable reference implementation across Linux machines and graphics drivers.

#![forbid(unsafe_code)]

mod control;
mod error;
mod renderer;
mod surface;

pub use control::{CancellationToken, NeverCancel};
pub use error::{RenderError, SurfaceError, UnsupportedEffect};
pub use renderer::{
    AssetProviderError, CpuRenderer, FrameAssetProvider, MAX_BLUR_RADIUS, RenderLimits,
};
pub use surface::RgbaSurface;
