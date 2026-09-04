//! Platform-independent capture contracts.
//!
//! This crate deliberately contains no operating-system or UI types. Native
//! adapters implement [`CaptureBackend`], while application code consumes a
//! [`CaptureSession`] as a small explicit state machine.

#![forbid(unsafe_code)]

mod backend;
mod capabilities;
mod error;
mod frame;
mod synthetic;

pub use backend::{
    BackendDescriptor, BackendStatus, CaptureBackend, CaptureCadence, CaptureRequest,
    CaptureSession, CaptureSessionState, CaptureSource, CaptureSourceId, CaptureSourceKind,
    CaptureTarget, CursorCaptureMode, FramePoll,
};
pub use capabilities::{CapabilityStatus, CaptureCapabilities, CaptureCapability};
pub use error::{CaptureError, CaptureErrorKind, RecoveryHint};
pub use frame::{
    ButtonState, CaptureTimestamp, CapturedFrame, CursorMetadata, InputEvent, KeyState,
    PhysicalPosition, PhysicalRect, PhysicalSize, PixelFormat, PointerButton,
};
pub use synthetic::{SyntheticCaptureBackend, SyntheticCaptureSession};
