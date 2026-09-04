use std::sync::Arc;

use crate::CaptureError;

/// A timestamp in microseconds from the beginning of one capture session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureTimestamp(u64);

impl CaptureTimestamp {
    /// Creates a session-relative timestamp.
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }

    /// Returns the session-relative microseconds.
    pub const fn as_micros(self) -> u64 {
        self.0
    }
}

/// A physical-pixel position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PhysicalPosition {
    /// Horizontal coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
}

/// A non-empty physical-pixel size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalSize {
    /// Width in physical pixels.
    width: u32,
    /// Height in physical pixels.
    height: u32,
}

impl PhysicalSize {
    /// Creates a non-empty size.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if either dimension is zero.
    pub fn new(width: u32, height: u32) -> Result<Self, CaptureError> {
        if width == 0 || height == 0 {
            return Err(CaptureError::invalid_request(
                "capture dimensions must both be greater than zero",
            ));
        }
        Ok(Self { width, height })
    }

    /// Width in physical pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Height in physical pixels.
    pub const fn height(self) -> u32 {
        self.height
    }
}

/// A non-empty rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalRect {
    /// Top-left position.
    origin: PhysicalPosition,
    /// Rectangle size.
    size: PhysicalSize,
}

impl PhysicalRect {
    /// Creates a non-empty rectangle.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] if either dimension is zero.
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self, CaptureError> {
        Ok(Self {
            origin: PhysicalPosition { x, y },
            size: PhysicalSize::new(width, height)?,
        })
    }

    /// Top-left physical-pixel position.
    pub const fn origin(self) -> PhysicalPosition {
        self.origin
    }

    /// Rectangle dimensions.
    pub const fn size(self) -> PhysicalSize {
        self.size
    }

    /// Returns true when this rectangle is fully contained in a zero-origin size.
    pub fn fits_within(self, size: PhysicalSize) -> bool {
        if self.origin.x < 0 || self.origin.y < 0 {
            return false;
        }
        let Ok(x) = u32::try_from(self.origin.x) else {
            return false;
        };
        let Ok(y) = u32::try_from(self.origin.y) else {
            return false;
        };
        x.checked_add(self.size.width)
            .is_some_and(|right| right <= size.width)
            && y.checked_add(self.size.height)
                .is_some_and(|bottom| bottom <= size.height)
    }
}

/// Packed pixel format used at the native-adapter boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelFormat {
    /// Red, green, blue, alpha byte order.
    Rgba8,
    /// Blue, green, red, alpha byte order, common for native capture APIs.
    Bgra8,
}

impl PixelFormat {
    /// Number of bytes occupied by one pixel.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 | Self::Bgra8 => 4,
        }
    }
}

/// State of a keyboard key at the time of an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyState {
    /// Key-down event.
    Pressed,
    /// Key-up event.
    Released,
}

/// State of a pointer button at the time of an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ButtonState {
    /// Button-down event.
    Pressed,
    /// Button-up event.
    Released,
}

/// A portable pointer button identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PointerButton {
    /// Primary/left button.
    Primary,
    /// Secondary/right button.
    Secondary,
    /// Middle button.
    Middle,
    /// Additional native button number.
    Other(u16),
}

/// Input metadata observed between captured frames.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputEvent {
    /// A keyboard transition. `native_code` is adapter-specific and optional
    /// text is the interpreted text at event time.
    Key {
        /// Session-relative event timestamp.
        at: CaptureTimestamp,
        /// Native scan/key code.
        native_code: u32,
        /// Interpreted text, if any.
        text: Option<String>,
        /// Press or release state.
        state: KeyState,
    },
    /// A pointer-button transition.
    PointerButton {
        /// Session-relative event timestamp.
        at: CaptureTimestamp,
        /// Button identity.
        button: PointerButton,
        /// Press or release state.
        state: ButtonState,
        /// Position in captured-frame coordinates, when known.
        position: Option<PhysicalPosition>,
    },
}

/// Editable pointer information returned separately from frame pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorMetadata {
    /// Pointer position in captured-frame coordinates.
    pub position: PhysicalPosition,
    /// Hotspot in cursor-image coordinates.
    pub hotspot: PhysicalPosition,
    /// Whether the pointer is visible in this frame.
    pub visible: bool,
    /// Optional stable adapter-provided cursor shape identifier.
    pub shape_id: Option<String>,
}

/// One immutable native capture frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame {
    sequence: u64,
    captured_at: CaptureTimestamp,
    size: PhysicalSize,
    stride: usize,
    format: PixelFormat,
    pixels: Arc<[u8]>,
    damage: Vec<PhysicalRect>,
    cursor: Option<CursorMetadata>,
    input_events: Vec<InputEvent>,
}

impl CapturedFrame {
    /// Creates and validates an immutable captured frame.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when the stride is too short, a size
    /// calculation overflows, or the pixel buffer does not cover every row.
    pub fn new(
        sequence: u64,
        captured_at: CaptureTimestamp,
        size: PhysicalSize,
        stride: usize,
        format: PixelFormat,
        pixels: impl Into<Arc<[u8]>>,
    ) -> Result<Self, CaptureError> {
        let minimum_stride = usize::try_from(size.width)
            .ok()
            .and_then(|width| width.checked_mul(format.bytes_per_pixel()))
            .ok_or_else(|| CaptureError::invalid_frame("frame row size overflowed usize"))?;
        if stride < minimum_stride {
            return Err(CaptureError::invalid_frame(format!(
                "frame stride {stride} is smaller than the required {minimum_stride} bytes"
            )));
        }
        let required_length = usize::try_from(size.height)
            .ok()
            .and_then(|height| stride.checked_mul(height))
            .ok_or_else(|| CaptureError::invalid_frame("frame buffer size overflowed usize"))?;
        let pixels = pixels.into();
        if pixels.len() < required_length {
            return Err(CaptureError::invalid_frame(format!(
                "frame buffer has {} bytes but {required_length} are required",
                pixels.len()
            )));
        }

        Ok(Self {
            sequence,
            captured_at,
            size,
            stride,
            format,
            pixels,
            damage: Vec::new(),
            cursor: None,
            input_events: Vec::new(),
        })
    }

    /// Adds native damage rectangles after validating their bounds.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError`] when any damage rectangle falls outside the
    /// frame's physical bounds.
    pub fn with_damage(mut self, damage: Vec<PhysicalRect>) -> Result<Self, CaptureError> {
        if let Some(out_of_bounds) = damage
            .iter()
            .copied()
            .find(|rectangle| !rectangle.fits_within(self.size))
        {
            return Err(CaptureError::invalid_frame(format!(
                "damage rectangle {out_of_bounds:?} is outside frame bounds {:?}",
                self.size
            )));
        }
        self.damage = damage;
        Ok(self)
    }

    /// Adds separately captured cursor metadata.
    #[must_use]
    pub fn with_cursor(mut self, cursor: CursorMetadata) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Adds input events associated with this frame interval.
    #[must_use]
    pub fn with_input_events(mut self, input_events: Vec<InputEvent>) -> Self {
        self.input_events = input_events;
        self
    }

    /// Monotonically increasing sequence number within a session.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Session-relative capture time.
    pub const fn captured_at(&self) -> CaptureTimestamp {
        self.captured_at
    }

    /// Frame dimensions.
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// Bytes between adjacent rows.
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// Pixel byte order.
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Immutable pixel buffer. It may contain native row padding.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Native damage rectangles, or an empty slice when unavailable.
    pub fn damage(&self) -> &[PhysicalRect] {
        &self.damage
    }

    /// Separately captured pointer metadata.
    pub const fn cursor(&self) -> Option<&CursorMetadata> {
        self.cursor.as_ref()
    }

    /// Input events observed since the preceding frame.
    pub fn input_events(&self) -> &[InputEvent] {
        &self.input_events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_short_stride_and_buffer() {
        let size = PhysicalSize::new(2, 2).unwrap();
        let stride_error = CapturedFrame::new(
            0,
            CaptureTimestamp::default(),
            size,
            7,
            PixelFormat::Rgba8,
            vec![0; 16],
        )
        .unwrap_err();
        assert!(stride_error.message().contains("stride"));

        let buffer_error = CapturedFrame::new(
            0,
            CaptureTimestamp::default(),
            size,
            8,
            PixelFormat::Rgba8,
            vec![0; 15],
        )
        .unwrap_err();
        assert!(buffer_error.message().contains("15"));
    }

    #[test]
    fn validates_damage_bounds() {
        let size = PhysicalSize::new(4, 4).unwrap();
        let frame = CapturedFrame::new(
            0,
            CaptureTimestamp::default(),
            size,
            16,
            PixelFormat::Bgra8,
            vec![0; 64],
        )
        .unwrap();
        assert!(
            frame
                .clone()
                .with_damage(vec![PhysicalRect::new(1, 1, 3, 3).unwrap()])
                .is_ok()
        );
        assert!(
            frame
                .with_damage(vec![PhysicalRect::new(2, 2, 3, 3).unwrap()])
                .is_err()
        );
    }
}
