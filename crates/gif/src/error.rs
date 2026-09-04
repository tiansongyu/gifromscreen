use std::error::Error;

use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum FrameError {
    #[error("GIF frame dimensions must be non-zero")]
    EmptyDimensions,

    #[error("RGBA buffer length overflow for {width}x{height}")]
    BufferSizeOverflow { width: u16, height: u16 },

    #[error("invalid RGBA buffer length: expected {expected} bytes, got {actual}")]
    InvalidBufferLength { expected: usize, actual: usize },

    #[error("frame duration must be greater than zero microseconds")]
    ZeroDuration,

    #[error("dirty rectangle is empty or falls outside the {width}x{height} canvas")]
    InvalidDirtyRect { width: u16, height: u16 },
}

/// Error supplied by an application-owned frame stream.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct FrameSourceError {
    message: String,
    #[source]
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl FrameSourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(
        message: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum QuantizationError {
    #[error("quantization was cancelled")]
    Cancelled,

    #[error("quantizer produced an invalid palette: {0}")]
    InvalidPalette(String),

    #[error("quantizer produced {actual} indices, expected {expected}")]
    InvalidIndexBuffer { expected: usize, actual: usize },

    #[error("quantizer produced palette index {index} for a {colors}-color palette")]
    PaletteIndexOutOfBounds { index: u8, colors: usize },

    /// A fixed palette cannot be truncated to satisfy the encoder color limit.
    #[error(
        "fixed palette contains {palette_colors} colors, above the configured maximum of {max_colors}"
    )]
    FixedPaletteExceedsColorLimit {
        /// Number of entries in the caller-supplied palette.
        palette_colors: usize,
        /// Maximum entries allowed by the current encoding options.
        max_colors: u16,
    },

    /// Transparent pixels or disposal require a designated fixed entry.
    #[error("fixed palette has no transparent entry required by the frame or disposal policy")]
    FixedPaletteMissingTransparency,

    #[error("the selected quantizer does not support {0}")]
    Unsupported(String),
}

#[derive(Debug, Error)]
pub enum GifEncodeError {
    #[error("encoding was cancelled")]
    Cancelled,

    #[error("the frame source was empty")]
    NoFrames,

    #[error("maximum colors must be in the range 2..=256, got {0}")]
    InvalidColorCount(u16),

    #[error("finite GIF repeat count must be at least one")]
    InvalidRepeatCount,

    #[error(
        "frame {frame_index} dimensions {actual_width}x{actual_height} do not match canvas {expected_width}x{expected_height}"
    )]
    DimensionMismatch {
        frame_index: u64,
        expected_width: u16,
        expected_height: u16,
        actual_width: u16,
        actual_height: u16,
    },

    #[error("animation duration overflowed the microsecond time representation")]
    DurationOverflow,

    #[error(
        "global-palette frame buffer needs at least {required_bytes} bytes, above its {limit_bytes}-byte limit"
    )]
    GlobalPaletteMemoryLimitExceeded {
        required_bytes: u64,
        limit_bytes: u64,
    },

    #[error("frame source failed: {0}")]
    FrameSource(#[from] FrameSourceError),

    #[error("frame quantization failed: {0}")]
    Quantization(#[from] QuantizationError),

    #[error("GIF writer failed: {0}")]
    Encoding(#[from] gif::EncodingError),
}
