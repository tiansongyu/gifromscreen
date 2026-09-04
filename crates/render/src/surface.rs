use gif_from_screen_domain::PhysicalSize;

use crate::SurfaceError;

/// An owned, tightly packed, row-major straight-alpha sRGB RGBA8 surface.
///
/// The constructor enforces exactly four bytes per pixel, so renderer code can
/// safely use the dimensions as the buffer's shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbaSurface {
    size: PhysicalSize,
    pixels: Vec<u8>,
}

impl RgbaSurface {
    /// Creates a surface from normalized RGBA8 bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for zero dimensions, byte-length overflow, or a buffer
    /// whose length does not exactly match `width * height * 4`.
    pub fn new(size: PhysicalSize, pixels: Vec<u8>) -> Result<Self, SurfaceError> {
        let expected = checked_byte_len(size)?;
        if pixels.len() != expected {
            return Err(SurfaceError::InvalidBufferLength {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self { size, pixels })
    }

    /// Returns the physical dimensions of the surface.
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// Returns the width in physical pixels.
    pub const fn width(&self) -> u32 {
        self.size.width.get()
    }

    /// Returns the height in physical pixels.
    pub const fn height(&self) -> u32 {
        self.size.height.get()
    }

    /// Borrows the tightly packed RGBA8 bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Mutably borrows the RGBA8 bytes without allowing their length to change.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    /// Consumes the surface and returns its packed RGBA8 bytes.
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    pub(crate) fn try_zeroed(size: PhysicalSize) -> Result<Self, SurfaceError> {
        let byte_len = checked_byte_len(size)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(byte_len)
            .map_err(|_| SurfaceError::AllocationFailed {
                requested: byte_len,
            })?;
        pixels.resize(byte_len, 0);
        Ok(Self { size, pixels })
    }

    pub(crate) fn byte_offset(&self, x: u32, y: u32) -> usize {
        debug_assert!(x < self.width());
        debug_assert!(y < self.height());
        let pixel_index = u64::from(y) * u64::from(self.width()) + u64::from(x);
        usize::try_from(pixel_index * 4)
            .expect("validated surface byte length guarantees a representable offset")
    }
}

pub(crate) fn checked_byte_len(size: PhysicalSize) -> Result<usize, SurfaceError> {
    let width = size.width.get();
    let height = size.height.get();
    if width == 0 || height == 0 {
        return Err(SurfaceError::EmptyDimensions { width, height });
    }

    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|area| area.checked_mul(4))
        .ok_or(SurfaceError::BufferSizeOverflow { width, height })?;
    usize::try_from(bytes).map_err(|_| SurfaceError::BufferSizeOverflow { width, height })
}

#[cfg(test)]
mod tests {
    use gif_from_screen_domain::{PhysicalPx, PhysicalSize};

    use super::*;

    #[test]
    fn validates_dimensions_and_exact_rgba_length() {
        let invalid_size = PhysicalSize {
            width: PhysicalPx::ZERO,
            height: PhysicalPx::new(2),
        };
        assert_eq!(
            RgbaSurface::new(invalid_size, Vec::new()),
            Err(SurfaceError::EmptyDimensions {
                width: 0,
                height: 2
            })
        );

        let size = PhysicalSize::new(2, 2).unwrap();
        assert_eq!(
            RgbaSurface::new(size, vec![0; 15]),
            Err(SurfaceError::InvalidBufferLength {
                expected: 16,
                actual: 15
            })
        );
        assert!(RgbaSurface::new(size, vec![0; 16]).is_ok());
    }

    #[test]
    fn detects_byte_length_overflow_before_allocation() {
        let size = PhysicalSize {
            width: PhysicalPx::new(u32::MAX),
            height: PhysicalPx::new(u32::MAX),
        };
        assert_eq!(
            checked_byte_len(size),
            Err(SurfaceError::BufferSizeOverflow {
                width: u32::MAX,
                height: u32::MAX
            })
        );
    }
}
