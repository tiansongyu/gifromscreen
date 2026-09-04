use gif_from_screen_domain::{AssetId, PhysicalRect};
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
    /// The caller requested cancellation.
    #[error("render cancelled")]
    Cancelled,
    /// The source asset provider could not return normalized RGBA8 pixels.
    #[error("could not load RGBA8 frame asset {asset_id}: {source}")]
    AssetLoad {
        /// Asset that failed to load.
        asset_id: AssetId,
        /// Provider-specific cause.
        #[source]
        source: AssetProviderError,
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
