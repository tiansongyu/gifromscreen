use thiserror::Error;

/// Failure produced while decoding an untrusted GIF stream.
#[derive(Debug, Error)]
pub enum GifDecodeError {
    /// The logical GIF canvas has a zero width or height.
    #[error("GIF canvas dimensions must be non-zero, got {width}x{height}")]
    EmptyCanvas {
        /// Logical canvas width from the GIF header.
        width: u16,
        /// Logical canvas height from the GIF header.
        height: u16,
    },

    /// The configured canvas-width limit was exceeded.
    #[error("GIF canvas width {actual} exceeds the configured limit of {limit}")]
    WidthLimitExceeded {
        /// Width declared by the input.
        actual: u16,
        /// Maximum accepted width.
        limit: u16,
    },

    /// The configured canvas-height limit was exceeded.
    #[error("GIF canvas height {actual} exceeds the configured limit of {limit}")]
    HeightLimitExceeded {
        /// Height declared by the input.
        actual: u16,
        /// Maximum accepted height.
        limit: u16,
    },

    /// A frame declares an empty image rectangle.
    #[error("GIF frame {frame_index} has empty dimensions {width}x{height}")]
    EmptyFrame {
        /// Zero-based frame index.
        frame_index: usize,
        /// Frame rectangle width.
        width: u16,
        /// Frame rectangle height.
        height: u16,
    },

    /// A frame rectangle is not fully contained by the logical canvas.
    #[error(
        "GIF frame {frame_index} rectangle ({left}, {top}, {width}x{height}) is outside canvas {canvas_width}x{canvas_height}"
    )]
    FrameOutOfBounds {
        /// Zero-based frame index.
        frame_index: usize,
        /// Frame rectangle left offset.
        left: u16,
        /// Frame rectangle top offset.
        top: u16,
        /// Frame rectangle width.
        width: u16,
        /// Frame rectangle height.
        height: u16,
        /// Logical canvas width.
        canvas_width: u16,
        /// Logical canvas height.
        canvas_height: u16,
    },

    /// The configured frame-count limit was exceeded.
    #[error("GIF contains more than the configured limit of {limit} frames")]
    FrameLimitExceeded {
        /// Maximum number of accepted frames.
        limit: usize,
    },

    /// Retaining another full-canvas RGBA frame would exceed the output limit.
    #[error(
        "decoded GIF frames require {required_bytes} RGBA bytes, above the configured limit of {limit_bytes}"
    )]
    TotalRgbaBytesLimitExceeded {
        /// Bytes required after accepting the current frame.
        required_bytes: u64,
        /// Maximum bytes allowed across returned frame buffers.
        limit_bytes: u64,
    },

    /// A zero-delay frame was rejected by policy.
    #[error("GIF frame {frame_index} declares a zero delay")]
    ZeroFrameDelay {
        /// Zero-based frame index.
        frame_index: usize,
    },

    /// A required buffer length cannot be represented by this platform.
    #[error("RGBA buffer length {bytes} cannot be represented on this platform")]
    AddressSpaceExceeded {
        /// Required buffer length.
        bytes: u64,
    },

    /// A bounded allocation failed.
    #[error("failed to allocate a bounded {bytes}-byte RGBA buffer")]
    AllocationFailed {
        /// Requested allocation size.
        bytes: usize,
    },

    /// The stream contains no GIF image frames.
    #[error("GIF stream contains no image frames")]
    NoFrames,

    /// The underlying GIF parser rejected the stream.
    #[error("GIF decoding failed: {0}")]
    Codec(#[from] gif::DecodingError),
}
