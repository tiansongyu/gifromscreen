use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// Stable categories that callers can use for error handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaptureErrorKind {
    /// A native adapter exists but has not completed initialization.
    BackendUninitialized,
    /// No usable backend exists in the current environment.
    BackendUnavailable,
    /// The operation needs an operating-system permission.
    PermissionRequired,
    /// The selected backend cannot provide a requested capability.
    UnsupportedCapability,
    /// A request violates an invariant or contains invalid values.
    InvalidRequest,
    /// A native frame violates the portable frame contract.
    InvalidFrame,
    /// A session command is not valid in its current state.
    InvalidStateTransition,
    /// The requested source is no longer present or was never enumerated.
    SourceNotFound,
    /// A previously valid source disappeared during capture.
    SourceLost,
    /// No frame arrived before a native deadline.
    Timeout,
    /// A platform adapter reported an otherwise uncategorized failure.
    Platform,
}

/// A machine-readable suggestion for presenting recovery actions in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryHint {
    /// Retrying the same operation may succeed.
    Retry,
    /// Initialize or reinitialize the selected backend.
    InitializeBackend,
    /// Ask the user to grant a platform permission.
    RequestPermission,
    /// Ask the user to select a different source.
    ChooseDifferentSource,
    /// Change the requested options to ones supported by the backend.
    ChangeRequest,
    /// There is no known in-process recovery action.
    None,
}

/// A capture failure with a stable category and a user-actionable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError {
    kind: CaptureErrorKind,
    message: String,
    recovery: RecoveryHint,
}

impl CaptureError {
    /// Creates a capture error.
    pub fn new(kind: CaptureErrorKind, message: impl Into<String>, recovery: RecoveryHint) -> Self {
        Self {
            kind,
            message: message.into(),
            recovery,
        }
    }

    /// Creates an error for an adapter that has not been initialized.
    pub fn backend_uninitialized(message: impl Into<String>) -> Self {
        Self::new(
            CaptureErrorKind::BackendUninitialized,
            message,
            RecoveryHint::InitializeBackend,
        )
    }

    /// Creates an invalid request error.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(
            CaptureErrorKind::InvalidRequest,
            message,
            RecoveryHint::ChangeRequest,
        )
    }

    /// Creates an invalid frame error.
    pub fn invalid_frame(message: impl Into<String>) -> Self {
        Self::new(CaptureErrorKind::InvalidFrame, message, RecoveryHint::None)
    }

    /// Returns the stable category.
    pub const fn kind(&self) -> CaptureErrorKind {
        self.kind
    }

    /// Returns the diagnostic message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the recommended recovery action.
    pub const fn recovery(&self) -> RecoveryHint {
        self.recovery
    }
}

impl Display for CaptureError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CaptureError {}
