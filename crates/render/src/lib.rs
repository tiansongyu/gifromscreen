//! Deterministic CPU rendering for `GifFromScreen`.
//!
//! Source assets and rendered surfaces use straight-alpha sRGB RGBA8 pixels.
//! A clip is rendered in a fixed order: crop, nearest-neighbor resize, rotation,
//! horizontal/vertical flips, then effects in their stored order. Blur uses an
//! edge-clamped separable box filter in alpha-weighted integer space and writes
//! straight-alpha pixels back. Shadow keeps the canvas size fixed, translates
//! the current surface's alpha mask, box-blurs it with transparent samples
//! outside the canvas, clips the result to the canvas, and composites the
//! original pixels over the colored mask. A zero shadow radius is a valid hard
//! shadow, radii above [`MAX_BLUR_RADIUS`] are rejected, and every `i32` offset
//! is accepted with overflow-free clipping. Keeping this pipeline CPU-only
//! gives exports a stable reference implementation across Linux machines and
//! graphics drivers.

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
