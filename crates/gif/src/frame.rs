use std::sync::Arc;

use crate::FrameError;

/// Keeps owned render buffers movable while allowing capture buffers to be shared.
#[derive(Clone, Debug, Eq)]
enum FramePixels {
    Owned(Vec<u8>),
    Shared(Arc<[u8]>),
}

impl FramePixels {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(pixels) => pixels,
            Self::Shared(pixels) => pixels,
        }
    }
}

impl PartialEq for FramePixels {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

/// A physical-pixel rectangle on a frame canvas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl DirtyRect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub(crate) fn fits_within(self, width: u16, height: u16) -> bool {
        self.width != 0
            && self.height != 0
            && u32::from(self.x) + u32::from(self.width) <= u32::from(width)
            && u32::from(self.y) + u32::from(self.height) <= u32::from(height)
    }
}

/// An owned, full-canvas RGBA8 frame.
///
/// Pixels are straight-alpha sRGB in row-major RGBA order. `dirty_rect` is a
/// renderer hint; the built-in M1 encoder currently writes the full canvas.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbaFrame {
    width: u16,
    height: u16,
    pixels: FramePixels,
    duration_us: u64,
    dirty_rect: Option<DirtyRect>,
}

impl RgbaFrame {
    pub fn new(
        width: u16,
        height: u16,
        pixels: Vec<u8>,
        duration_us: u64,
    ) -> Result<Self, FrameError> {
        Self::from_pixels(width, height, FramePixels::Owned(pixels), duration_us)
    }

    /// Creates a frame that shares tightly packed immutable RGBA8 pixels.
    ///
    /// No pixel copy is made, including when this frame is cloned. Dimensions,
    /// buffer length and duration are checked exactly as in [`Self::new`].
    pub fn from_shared_pixels(
        width: u16,
        height: u16,
        pixels: Arc<[u8]>,
        duration_us: u64,
    ) -> Result<Self, FrameError> {
        Self::from_pixels(width, height, FramePixels::Shared(pixels), duration_us)
    }

    fn from_pixels(
        width: u16,
        height: u16,
        pixels: FramePixels,
        duration_us: u64,
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 {
            return Err(FrameError::EmptyDimensions);
        }
        if duration_us == 0 {
            return Err(FrameError::ZeroDuration);
        }

        let expected = usize::from(width)
            .checked_mul(usize::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(FrameError::BufferSizeOverflow { width, height })?;
        if pixels.as_slice().len() != expected {
            return Err(FrameError::InvalidBufferLength {
                expected,
                actual: pixels.as_slice().len(),
            });
        }

        Ok(Self {
            width,
            height,
            pixels,
            duration_us,
            dirty_rect: None,
        })
    }

    pub fn with_dirty_rect(mut self, dirty_rect: DirtyRect) -> Result<Self, FrameError> {
        if !dirty_rect.fits_within(self.width, self.height) {
            return Err(FrameError::InvalidDirtyRect {
                width: self.width,
                height: self.height,
            });
        }
        self.dirty_rect = Some(dirty_rect);
        Ok(self)
    }

    pub const fn width(&self) -> u16 {
        self.width
    }

    pub const fn height(&self) -> u16 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        self.pixels.as_slice()
    }

    pub const fn duration_us(&self) -> u64 {
        self.duration_us
    }

    pub const fn dirty_rect(&self) -> Option<DirtyRect> {
        self.dirty_rect
    }

    pub(crate) fn merge_duration(&mut self, duration_us: u64) -> Result<(), ()> {
        self.duration_us = self.duration_us.checked_add(duration_us).ok_or(())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_buffer_and_duration() {
        assert_eq!(
            RgbaFrame::new(2, 1, vec![0; 7], 10_000),
            Err(FrameError::InvalidBufferLength {
                expected: 8,
                actual: 7
            })
        );
        assert_eq!(
            RgbaFrame::new(1, 1, vec![0; 4], 0),
            Err(FrameError::ZeroDuration)
        );
    }

    #[test]
    fn shared_pixels_keep_the_allocation_and_match_owned_frames() {
        let pixels: Arc<[u8]> = vec![1, 2, 3, 255, 4, 5, 6, 255].into();
        let shared = RgbaFrame::from_shared_pixels(2, 1, Arc::clone(&pixels), 10_000).unwrap();
        let owned_pixels = pixels.to_vec();
        let owned_address = owned_pixels.as_ptr();
        let owned = RgbaFrame::new(2, 1, owned_pixels, 10_000).unwrap();

        assert_eq!(shared, owned);
        assert_eq!(owned.pixels().as_ptr(), owned_address);
        assert_eq!(shared.pixels().as_ptr(), pixels.as_ptr());
        let cloned = shared.clone();
        assert_eq!(cloned.pixels().as_ptr(), pixels.as_ptr());
        drop(pixels);
        drop(shared);
        assert_eq!(cloned.pixels(), owned.pixels());
    }

    #[test]
    fn shared_pixels_validate_the_complete_buffer() {
        assert_eq!(
            RgbaFrame::from_shared_pixels(1, 1, vec![0; 8].into(), 10_000),
            Err(FrameError::InvalidBufferLength {
                expected: 4,
                actual: 8,
            })
        );
        assert_eq!(
            RgbaFrame::from_shared_pixels(1, 1, vec![0; 4].into(), 0),
            Err(FrameError::ZeroDuration)
        );
    }

    #[test]
    fn validates_dirty_rectangle() {
        let frame = RgbaFrame::new(4, 3, vec![0; 4 * 3 * 4], 10_000).unwrap();
        assert!(
            frame
                .clone()
                .with_dirty_rect(DirtyRect::new(1, 1, 3, 2))
                .is_ok()
        );
        assert_eq!(
            frame.with_dirty_rect(DirtyRect::new(3, 2, 2, 1)),
            Err(FrameError::InvalidDirtyRect {
                width: 4,
                height: 3
            })
        );
    }
}
