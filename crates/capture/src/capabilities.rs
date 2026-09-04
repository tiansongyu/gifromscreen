use crate::{CaptureError, CaptureErrorKind, RecoveryHint};

/// A capture feature that may differ by backend, compositor, and permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum CaptureCapability {
    /// Capture a complete monitor.
    Monitor,
    /// Capture an individual window.
    Window,
    /// Capture an arbitrary rectangular region.
    ArbitraryRegion,
    /// Composite the pointer into pixels in the native capture stream.
    CursorEmbedded,
    /// Return editable pointer position/shape metadata separately from pixels.
    CursorMetadata,
    /// Passively observe global mouse-button events.
    PassiveMouseButtons,
    /// Passively observe global keyboard events.
    PassiveKeyboard,
    /// Register global start/pause/stop shortcuts.
    GlobalShortcuts,
    /// Capture a camera as a source.
    Camera,
}

/// Runtime availability of one capability.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CapabilityStatus {
    /// The operation is ready without a known limitation.
    Available,
    /// The operation works with the documented limitation.
    Limited(String),
    /// The backend supports the operation after an OS/user authorization step.
    PermissionRequired(String),
    /// The backend cannot provide the operation.
    Unavailable(String),
}

impl CapabilityStatus {
    /// Returns true if this backend can ever provide the capability.
    pub const fn is_supported(&self) -> bool {
        !matches!(self, Self::Unavailable(_))
    }

    /// Returns true if a request can use the capability immediately.
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Available | Self::Limited(_))
    }

    /// Returns the explanation associated with a non-plain status.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::Limited(reason)
            | Self::PermissionRequired(reason)
            | Self::Unavailable(reason) => Some(reason),
        }
    }
}

/// Runtime capture capabilities for a concrete backend instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCapabilities {
    /// Monitor capture availability.
    pub monitor: CapabilityStatus,
    /// Window capture availability.
    pub window: CapabilityStatus,
    /// Arbitrary rectangular region availability.
    pub arbitrary_region: CapabilityStatus,
    /// Embedded cursor availability.
    pub cursor_embedded: CapabilityStatus,
    /// Separate cursor metadata availability.
    pub cursor_metadata: CapabilityStatus,
    /// Passive mouse-button event availability.
    pub passive_mouse_buttons: CapabilityStatus,
    /// Passive keyboard event availability.
    pub passive_keyboard: CapabilityStatus,
    /// Global shortcut availability.
    pub global_shortcuts: CapabilityStatus,
    /// Camera availability.
    pub camera: CapabilityStatus,
}

impl CaptureCapabilities {
    /// Returns a capability set in which every operation is unavailable.
    pub fn all_unavailable(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            monitor: CapabilityStatus::Unavailable(reason.clone()),
            window: CapabilityStatus::Unavailable(reason.clone()),
            arbitrary_region: CapabilityStatus::Unavailable(reason.clone()),
            cursor_embedded: CapabilityStatus::Unavailable(reason.clone()),
            cursor_metadata: CapabilityStatus::Unavailable(reason.clone()),
            passive_mouse_buttons: CapabilityStatus::Unavailable(reason.clone()),
            passive_keyboard: CapabilityStatus::Unavailable(reason.clone()),
            global_shortcuts: CapabilityStatus::Unavailable(reason.clone()),
            camera: CapabilityStatus::Unavailable(reason),
        }
    }

    /// Returns the status of a named capability.
    pub const fn status(&self, capability: CaptureCapability) -> &CapabilityStatus {
        match capability {
            CaptureCapability::Monitor => &self.monitor,
            CaptureCapability::Window => &self.window,
            CaptureCapability::ArbitraryRegion => &self.arbitrary_region,
            CaptureCapability::CursorEmbedded => &self.cursor_embedded,
            CaptureCapability::CursorMetadata => &self.cursor_metadata,
            CaptureCapability::PassiveMouseButtons => &self.passive_mouse_buttons,
            CaptureCapability::PassiveKeyboard => &self.passive_keyboard,
            CaptureCapability::GlobalShortcuts => &self.global_shortcuts,
            CaptureCapability::Camera => &self.camera,
        }
    }

    /// Checks that a capability is immediately usable.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] with a permission or unsupported-capability
    /// category when the capability is not ready.
    pub fn require_ready(&self, capability: CaptureCapability) -> Result<(), CaptureError> {
        match self.status(capability) {
            CapabilityStatus::Available | CapabilityStatus::Limited(_) => Ok(()),
            CapabilityStatus::PermissionRequired(reason) => Err(CaptureError::new(
                CaptureErrorKind::PermissionRequired,
                reason.clone(),
                RecoveryHint::RequestPermission,
            )),
            CapabilityStatus::Unavailable(reason) => Err(CaptureError::new(
                CaptureErrorKind::UnsupportedCapability,
                reason.clone(),
                RecoveryHint::ChangeRequest,
            )),
        }
    }
}

impl Default for CaptureCapabilities {
    fn default() -> Self {
        Self::all_unavailable("the backend did not report this capability")
    }
}
