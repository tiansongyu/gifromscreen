use std::io;
use std::path::PathBuf;

use gif_from_screen_capture::CaptureError;
use gif_from_screen_gif::{FrameError, GifEncodeError};
use thiserror::Error;

/// A typed failure from capture collection or atomic GIF export.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WorkflowError {
    /// Capture setup, polling, or shutdown failed.
    #[error("capture failed: {0}")]
    Capture(#[from] CaptureError),

    /// The job was cooperatively cancelled.
    #[error("recording was cancelled")]
    Cancelled,

    /// The capture stream ended before producing a usable frame.
    #[error("the capture stream produced no frames")]
    EmptyCapture,

    /// A collection option cannot be represented or would make no progress.
    #[error("invalid collection option: {0}")]
    InvalidCollectionOption(String),

    /// A frame timestamp did not strictly advance.
    #[error(
        "frame {frame_index} has non-monotonic timestamp {actual_micros} us; expected a value greater than {previous_micros} us"
    )]
    NonMonotonicTimestamp {
        /// Zero-based index in the native capture stream.
        frame_index: u64,
        /// Timestamp of the preceding frame.
        previous_micros: u64,
        /// Timestamp of the rejected frame.
        actual_micros: u64,
    },

    /// A frame sequence number did not strictly advance.
    #[error(
        "frame {frame_index} has non-monotonic sequence {actual}; expected a value greater than {previous}"
    )]
    NonMonotonicSequence {
        /// Zero-based index in the native capture stream.
        frame_index: u64,
        /// Sequence number of the preceding frame.
        previous: u64,
        /// Sequence number of the rejected frame.
        actual: u64,
    },

    /// A captured frame cannot fit in GIF's 16-bit canvas dimensions.
    #[error(
        "frame {frame_index} dimensions {width}x{height} exceed the GIF canvas limit of 65535x65535"
    )]
    DimensionsOutOfRange {
        /// Zero-based index in the native capture stream.
        frame_index: u64,
        /// Captured width.
        width: u32,
        /// Captured height.
        height: u32,
    },

    /// A frame's dimensions differ from the first retained frame.
    #[error(
        "frame {frame_index} dimensions {actual_width}x{actual_height} do not match capture canvas {expected_width}x{expected_height}"
    )]
    DimensionMismatch {
        /// Zero-based index in the native capture stream.
        frame_index: u64,
        /// Width established by the first frame.
        expected_width: u32,
        /// Height established by the first frame.
        expected_height: u32,
        /// Width of the rejected frame.
        actual_width: u32,
        /// Height of the rejected frame.
        actual_height: u32,
    },

    /// A native frame row is shorter than its declared pixel format requires.
    #[error("frame {frame_index} stride {actual} is shorter than its required {minimum} bytes")]
    InvalidStride {
        /// Zero-based index in the native capture stream.
        frame_index: u64,
        /// Required number of bytes in one row.
        minimum: usize,
        /// Declared number of bytes in one row.
        actual: usize,
    },

    /// A native pixel buffer does not contain all declared rows.
    #[error(
        "frame {frame_index} pixel buffer contains {actual} bytes; at least {minimum} are required"
    )]
    InvalidPixelBuffer {
        /// Zero-based index in the native capture stream.
        frame_index: u64,
        /// Required buffer size.
        minimum: usize,
        /// Actual buffer size.
        actual: usize,
    },

    /// A future capture pixel format is not supported by this workflow yet.
    #[error("frame {frame_index} uses unsupported pixel format {format}")]
    UnsupportedPixelFormat {
        /// Zero-based index in the native capture stream.
        frame_index: u64,
        /// Debug name of the format reported by the capture adapter.
        format: String,
    },

    /// Retaining another normalized frame would exceed the configured bound.
    #[error(
        "captured RGBA frames need {required_bytes} bytes, above the configured {limit_bytes}-byte limit"
    )]
    FrameBufferLimitExceeded {
        /// Bytes required after accepting the current frame.
        required_bytes: u64,
        /// Configured collection bound.
        limit_bytes: u64,
    },

    /// Building an encoder frame failed after capture validation.
    #[error("captured frame could not be converted for GIF encoding: {0}")]
    Frame(#[from] FrameError),

    /// GIF encoding failed.
    #[error("GIF encoding failed: {0}")]
    Encode(#[from] GifEncodeError),

    /// The requested target does not identify a file name.
    #[error("output path must identify a GIF file: {0}")]
    InvalidOutputPath(PathBuf),

    /// Creating, syncing, inspecting, or renaming an output failed.
    #[error("could not {operation} '{}': {source}", path.display())]
    OutputIo {
        /// Operation being attempted.
        operation: &'static str,
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },

    /// Export failed and its temporary output could not be removed.
    #[error(
        "{original}; additionally, temporary output '{}' could not be removed: {cleanup}",
        path.display()
    )]
    PartialCleanup {
        /// Temporary output that remains on disk.
        path: PathBuf,
        /// The original workflow failure.
        original: Box<Self>,
        /// Failure returned while removing the temporary file.
        cleanup: io::Error,
    },
}

impl WorkflowError {
    pub(crate) fn output_io(
        operation: &'static str,
        path: impl Into<PathBuf>,
        source: io::Error,
    ) -> Self {
        Self::OutputIo {
            operation,
            path: path.into(),
            source,
        }
    }
}
