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
//! allocation-free vector rasterization. Legacy stages use straight-alpha blend
//! modes; explicit WPF stages share one premultiplied RGBA8 surface and one
//! PNG/WIC-compatible fixed-reciprocal boundary. Keeping
//! this pipeline CPU-only gives exports a stable reference implementation across
//! Linux machines and graphics drivers.
//!
//! New `ImageBorder`/`ImageShadow` steps expand the current composed canvas without
//! changing legacy effects. `ImageShadow` uses the WPF software-reference Gaussian
//! kernel, separate per-pass 8-bit quantization and independent shadow opacity;
//! its chosen background is composited last. Fractional border strokes use
//! pixel-area coverage. The first independent Windows `RenderTargetBitmap`/WIC
//! corpus matches exactly; this bounded coverage is not a claim of equality for
//! every parameter, vector antialiasing or historical fractional-DPI behavior.

#![forbid(unsafe_code)]

mod control;
mod error;
mod image_effects;
mod ink;
mod ink_outline;
mod ink_raster;
mod overlay;
mod premultiplied;
mod renderer;
mod surface;
mod transition;
mod wpf_pixels;

#[cfg(test)]
mod ordered_tests;

pub use control::{CancellationToken, NeverCancel};
pub use error::{RenderError, SurfaceError, UnsupportedEffect};
pub use ink::{
    InkAttributes, InkError, InkFigure, InkFillRule, InkLimits, InkPath, InkPoint, InkSample,
    InkSegment, InkStroke, InkTip,
};
pub use ink_outline::{fitted_ink_samples, outline_ink_stroke, outline_ink_strokes};
pub use ink_raster::{clip_ink_reference, rasterize_ink_paths};
pub use overlay::{
    OverlayRenderPlan, RasterOverlayAsset, active_raster_overlay_assets,
    active_raster_overlay_assets_for_frame, freeze_timed_overlay_content,
};
pub use premultiplied::{PremultipliedRgbaSurface, PremultipliedSnapshotError};
pub use renderer::{
    AssetProviderError, CpuRenderer, FrameAssetProvider, MAX_BLUR_RADIUS, RenderLimits,
};
pub use surface::RgbaSurface;
pub use transition::{TransitionProgress, render_transition};
