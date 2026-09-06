//! Deterministic CPU rendering for `GifFromScreen`.
//!
//! Source assets and rendered surfaces use straight-alpha sRGB RGBA8 pixels.
//! A clip retains its legacy prefix: crop, nearest-neighbor resize, rotation,
//! horizontal/vertical flips, then effects in their stored order. Ordered render
//! steps follow that prefix, alternating full-surface geometry/effects with
//! explicit Composite stages. Empty steps preserve legacy pixels. Blur uses an
//! edge-clamped separable box filter in alpha-weighted integer space and writes
//! straight-alpha pixels back. Legacy Shadow keeps the canvas size fixed, translates
//! the current surface's alpha mask, box-blurs it with transparent samples
//! outside the canvas, clips the result to the canvas, and composites the
//! original pixels over the colored mask. A zero shadow radius is a valid hard
//! shadow, radii above [`MAX_BLUR_RADIUS`] are rejected, and every `i32` offset
//! is accepted with overflow-free clipping. Active timed items join the first
//! Composite stage, stage-anchored marks join their named stage, and unanchored
//! marks draw at the tail. Each stage uses stable z/track/item order with hard-edged,
//! allocation-free vector rasterization and straight-alpha blend modes. Keeping
//! this pipeline CPU-only gives exports a stable reference implementation across
//! Linux machines and graphics drivers.
//!
//! New `ImageBorder`/`ImageShadow` steps expand the current composed canvas without
//! changing legacy effects. `ImageShadow` uses the WPF software-reference Gaussian
//! kernel, separate per-pass 8-bit quantization and independent shadow opacity;
//! its chosen background is composited last. Fractional border strokes use
//! pixel-area coverage. These are explicit reference algorithms, not a claim of
//! bit equality with Windows `RenderTargetBitmap`/WIC output.

#![forbid(unsafe_code)]

mod control;
mod error;
mod image_effects;
mod overlay;
mod renderer;
mod surface;
mod transition;

#[cfg(test)]
mod ordered_tests;

pub use control::{CancellationToken, NeverCancel};
pub use error::{RenderError, SurfaceError, UnsupportedEffect};
pub use overlay::{
    OverlayRenderPlan, RasterOverlayAsset, active_raster_overlay_assets,
    active_raster_overlay_assets_for_frame, freeze_timed_overlay_content,
};
pub use renderer::{
    AssetProviderError, CpuRenderer, FrameAssetProvider, MAX_BLUR_RADIUS, RenderLimits,
};
pub use surface::RgbaSurface;
pub use transition::{TransitionProgress, render_transition};
