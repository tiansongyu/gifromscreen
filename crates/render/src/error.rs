use gif_from_screen_domain::{AssetId, FrameId, OverlayId, PhysicalRect};
use thiserror::Error;

use crate::AssetProviderError;

/// Failures while constructing an RGBA8 surface.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SurfaceError {
    /// Width and height must both be positive.
    #[error("RGBA surface dimensions must be non-zero, got {width}x{height}")]
    EmptyDimensions {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// The number of required RGBA bytes cannot be represented on this target.
    #[error("RGBA surface byte length overflows for {width}x{height}")]
    BufferSizeOverflow {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// The supplied byte slice does not contain exactly four bytes per pixel.
    #[error("invalid RGBA buffer length: expected {expected} bytes, got {actual}")]
    InvalidBufferLength {
        /// Required number of bytes.
        expected: usize,
        /// Supplied number of bytes.
        actual: usize,
    },
    /// The allocator could not reserve the requested surface buffer.
    #[error("could not allocate {requested} bytes for an RGBA surface")]
    AllocationFailed {
        /// Number of bytes the allocator was asked to reserve.
        requested: usize,
    },
}

/// Effects intentionally deferred beyond the current CPU-renderer milestone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedEffect {
    /// Masked cinemagraph composition.
    Cinemagraph,
}

impl std::fmt::Display for UnsupportedEffect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Cinemagraph => "cinemagraph",
        })
    }
}

/// Errors produced by the deterministic CPU renderer.
#[derive(Debug, Error)]
pub enum RenderError {
    /// Legacy time-only planning cannot decide which frame-owned marks to draw.
    #[error("frame-owned overlays require frame identity; use frame-aware overlay planning")]
    OverlayFrameIdentityRequired,
    /// A detached drawing plan must not be applied to a different frame.
    #[error("overlay plan belongs to frame {expected}, not {actual}")]
    OverlayPlanFrameMismatch {
        /// Frame whose marks were resolved.
        expected: FrameId,
        /// Frame the caller requested rendering.
        actual: FrameId,
    },
    /// An active overlay cannot yet be reproduced by the renderer.
    #[error(
        "overlay {overlay_id} uses unsupported {kind} content; hide or remove it before exporting"
    )]
    UnsupportedOverlay {
        /// Item that would otherwise be silently lost from the output.
        overlay_id: OverlayId,
        /// Stable name of the unsupported content variant.
        kind: &'static str,
    },
    /// The caller requested cancellation.
    #[error("render cancelled")]
    Cancelled,
    /// Both transition endpoints must use the same canvas.
    #[error(
        "transition endpoint dimensions differ: {from_width}x{from_height} versus {to_width}x{to_height}"
    )]
    TransitionDimensionMismatch {
        /// Width of the outgoing surface.
        from_width: u32,
        /// Height of the outgoing surface.
        from_height: u32,
        /// Width of the incoming surface.
        to_width: u32,
        /// Height of the incoming surface.
        to_height: u32,
    },
    /// A transition step lies outside its non-empty inclusive range.
    #[error("transition step {step} is outside 0..={steps}; steps must be greater than zero")]
    InvalidTransitionProgress {
        /// Requested current step.
        step: u32,
        /// Requested number of steps.
        steps: u32,
    },
    /// The source asset provider could not return normalized RGBA8 pixels.
    #[error("could not load RGBA8 frame asset {asset_id}: {source}")]
    AssetLoad {
        /// Asset that failed to load.
        asset_id: AssetId,
        /// Provider-specific cause.
        #[source]
        source: AssetProviderError,
    },
    /// A raster overlay asset provider failed.
    #[error("could not load RGBA8 raster overlay {overlay_id} asset {asset_id}: {source}")]
    OverlayAssetLoad {
        /// Overlay item whose immutable pixels were requested.
        overlay_id: OverlayId,
        /// Referenced raster asset.
        asset_id: AssetId,
        /// Provider-specific cause.
        #[source]
        source: AssetProviderError,
    },
    /// A supported overlay span cannot represent its exclusive endpoint.
    #[error("overlay {overlay_id} time span overflows the project clock")]
    OverlaySpanOverflow {
        /// Overlay with an overflowing span.
        overlay_id: OverlayId,
    },
    /// The active supported-overlay sorting plan exceeded the platform address space.
    #[error("active overlay plan item count overflowed")]
    OverlayPlanSizeOverflow,
    /// Reserving the active supported-overlay sorting plan failed.
    #[error("could not allocate an active overlay plan for {requested} items")]
    OverlayPlanAllocationFailed {
        /// Number of plan entries requested at the failed growth point.
        requested: usize,
    },
    /// Shape or drawing geometry is malformed and cannot be rasterized deterministically.
    #[error("overlay {overlay_id} has invalid geometry: {reason}")]
    InvalidOverlayGeometry {
        /// Overlay item with malformed geometry.
        overlay_id: OverlayId,
        /// Stable validation reason.
        reason: &'static str,
    },
    /// A drawing pressure exceeds the normalized 0..=1000 range.
    #[error(
        "drawing overlay {overlay_id} point {point_index} has pressure {pressure_milli}, above {maximum}"
    )]
    InvalidDrawingPressure {
        /// Drawing overlay item.
        overlay_id: OverlayId,
        /// Zero-based point position.
        point_index: usize,
        /// Rejected pressure value.
        pressure_milli: u16,
        /// Maximum normalized pressure.
        maximum: u16,
    },
    /// A requested crop is empty, overflows, or extends outside the source.
    #[error("crop {crop:?} does not fit within the {source_width}x{source_height} source surface")]
    InvalidCrop {
        /// Rejected crop rectangle.
        crop: PhysicalRect,
        /// Width of the loaded source.
        source_width: u32,
        /// Height of the loaded source.
        source_height: u32,
    },
    /// An effect region is empty, overflows, or extends outside the transformed surface.
    #[error(
        "{effect} region {region:?} does not fit within the {surface_width}x{surface_height} surface"
    )]
    InvalidEffectRegion {
        /// Stable effect name.
        effect: &'static str,
        /// Rejected effect rectangle.
        region: PhysicalRect,
        /// Width of the transformed surface.
        surface_width: u32,
        /// Height of the transformed surface.
        surface_height: u32,
    },
    /// An effect parameter is outside the renderer's supported range.
    #[error("invalid {effect} parameter {parameter}={value}")]
    InvalidEffectParameter {
        /// Stable effect name.
        effect: &'static str,
        /// Stable parameter name.
        parameter: &'static str,
        /// Rejected numeric value.
        value: u64,
    },
    /// A surface exceeded the renderer's configured per-surface byte budget.
    #[error("surface requires {requested} bytes, exceeding the {limit}-byte render limit")]
    SurfaceLimitExceeded {
        /// Required packed RGBA byte length.
        requested: usize,
        /// Configured maximum packed RGBA byte length.
        limit: usize,
    },
    /// An effect's temporary working buffer cannot be represented on this target.
    #[error("{effect} working-memory size overflows for the selected region")]
    EffectWorkingMemorySizeOverflow {
        /// Stable effect name.
        effect: &'static str,
    },
    /// An effect's temporary working buffer exceeded the configured byte budget.
    #[error(
        "{effect} requires {requested} bytes of working memory, exceeding the {limit}-byte render limit"
    )]
    EffectWorkingMemoryLimitExceeded {
        /// Stable effect name.
        effect: &'static str,
        /// Required temporary-buffer byte length.
        requested: usize,
        /// Configured byte limit.
        limit: usize,
    },
    /// The allocator could not reserve an effect's temporary working buffer.
    #[error("could not allocate {requested} bytes of working memory for {effect}")]
    EffectWorkingMemoryAllocationFailed {
        /// Stable effect name.
        effect: &'static str,
        /// Number of bytes requested from the allocator.
        requested: usize,
    },
    /// This milestone does not yet implement the requested effect.
    #[error("unsupported effect: {0}")]
    UnsupportedEffect(UnsupportedEffect),
    /// An RGBA surface was invalid or could not be allocated.
    #[error(transparent)]
    Surface(#[from] SurfaceError),
}
