//! Typed PBGRA-equivalent pixels in RGBA byte order, never a straight-alpha image.

use gif_from_screen_domain::{
    PREMULTIPLIED_SNAPSHOT_FORMAT_VERSION, PREMULTIPLIED_SNAPSHOT_HEADER_LEN, PhysicalSize,
};
use thiserror::Error;

use crate::{SurfaceError, surface::checked_byte_len};

const MAGIC: &[u8; 7] = b"GFSPM8\0";

/// Immutable, validated, tightly packed premultiplied sRGB channels in RGBA order.
/// This type deliberately has no implicit conversion to [`crate::RgbaSurface`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PremultipliedRgbaSurface {
    size: PhysicalSize,
    pixels: Vec<u8>,
}

impl PremultipliedRgbaSurface {
    /// Checks dimensions, byte count and the premultiplied `RGB <= alpha` invariant.
    ///
    /// # Errors
    /// Rejects malformed dimensions/buffers or any channel greater than alpha.
    pub fn new(size: PhysicalSize, pixels: Vec<u8>) -> Result<Self, PremultipliedSnapshotError> {
        let expected = checked_byte_len(size)?;
        if pixels.len() != expected {
            return Err(SurfaceError::InvalidBufferLength {
                expected,
                actual: pixels.len(),
            }
            .into());
        }
        if let Some(index) = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .position(|pixel| pixel[..3].iter().any(|color| *color > pixel[3]))
        {
            return Err(PremultipliedSnapshotError::InvalidPixel { index });
        }
        Ok(Self { size, pixels })
    }

    /// Physical dimensions of the premultiplied pixels.
    pub const fn size(&self) -> PhysicalSize {
        self.size
    }

    /// Premultiplied RGB channels followed by their unscaled alpha, in row-major order.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Encodes the distinct v1 container without any color or alpha conversion.
    /// Its 17-byte header prevents collision with valid tightly packed RGBA assets.
    ///
    /// # Errors
    /// Rejects byte-count overflow, the supplied encoded-byte limit, or allocation failure.
    pub fn encode(&self, max_bytes: usize) -> Result<Vec<u8>, PremultipliedSnapshotError> {
        let length = self
            .pixels
            .len()
            .checked_add(PREMULTIPLIED_SNAPSHOT_HEADER_LEN)
            .ok_or(PremultipliedSnapshotError::LengthOverflow)?;
        if length > max_bytes {
            return Err(PremultipliedSnapshotError::LimitExceeded {
                requested: length,
                limit: max_bytes,
            });
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| SurfaceError::AllocationFailed { requested: length })?;
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&PREMULTIPLIED_SNAPSHOT_FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&self.size.width.get().to_le_bytes());
        bytes.extend_from_slice(&self.size.height.get().to_le_bytes());
        bytes.extend_from_slice(&self.pixels);
        Ok(bytes)
    }

    /// Decodes an owned container using its exact declared dimensions, without resizing.
    /// Header removal reuses the allocation; no second full image is allocated.
    ///
    /// # Errors
    /// Rejects unsupported magic/version, mismatched shape/length, oversized payloads,
    /// or invalid premultiplied pixels, before returning any surface.
    pub fn decode(
        mut bytes: Vec<u8>,
        expected_size: PhysicalSize,
        max_payload_bytes: usize,
    ) -> Result<Self, PremultipliedSnapshotError> {
        Self::validate_encoded(&bytes, expected_size, max_payload_bytes)?;
        bytes.drain(..PREMULTIPLIED_SNAPSHOT_HEADER_LEN);
        Ok(Self {
            size: expected_size,
            pixels: bytes,
        })
    }

    /// Validates a borrowed container without copying or allocating pixel data.
    ///
    /// # Errors
    /// Rejects the same malformed header, payload, size and limit cases as [`Self::decode`].
    pub fn validate_encoded(
        bytes: &[u8],
        expected_size: PhysicalSize,
        max_payload_bytes: usize,
    ) -> Result<(), PremultipliedSnapshotError> {
        let expected = Self::validate_encoded_header(bytes, expected_size, max_payload_bytes)?;
        if bytes.len() != expected {
            return Err(PremultipliedSnapshotError::InvalidContainerLength {
                expected,
                actual: bytes.len(),
            });
        }
        if let Some(index) = bytes[PREMULTIPLIED_SNAPSHOT_HEADER_LEN..]
            .as_chunks::<4>()
            .0
            .iter()
            .position(|pixel| pixel[..3].iter().any(|color| *color > pixel[3]))
        {
            return Err(PremultipliedSnapshotError::InvalidPixel { index });
        }
        Ok(())
    }

    /// Validates the first 17 bytes and returns the required total encoded length.
    /// Streaming callers must additionally validate every payload pixel and its length.
    ///
    /// # Errors
    /// Rejects incomplete or unknown headers, mismatched dimensions and oversized payloads.
    /// It deliberately does not claim to validate payload bytes absent from the header.
    pub fn validate_encoded_header(
        bytes: &[u8],
        expected_size: PhysicalSize,
        max_payload_bytes: usize,
    ) -> Result<usize, PremultipliedSnapshotError> {
        if bytes.len() < PREMULTIPLIED_SNAPSHOT_HEADER_LEN || &bytes[..7] != MAGIC {
            return Err(PremultipliedSnapshotError::InvalidHeader);
        }
        let version = u16::from_le_bytes([bytes[7], bytes[8]]);
        if version != PREMULTIPLIED_SNAPSHOT_FORMAT_VERSION {
            return Err(PremultipliedSnapshotError::UnsupportedVersion { version });
        }
        let width = u32::from_le_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]);
        let height = u32::from_le_bytes([bytes[13], bytes[14], bytes[15], bytes[16]]);
        let size = PhysicalSize::new(width, height)
            .map_err(|_| SurfaceError::EmptyDimensions { width, height })?;
        if size != expected_size {
            return Err(PremultipliedSnapshotError::SizeMismatch {
                expected: expected_size,
                actual: size,
            });
        }
        let payload_length = checked_byte_len(size)?;
        if payload_length > max_payload_bytes {
            return Err(PremultipliedSnapshotError::LimitExceeded {
                requested: payload_length,
                limit: max_payload_bytes,
            });
        }
        payload_length
            .checked_add(PREMULTIPLIED_SNAPSHOT_HEADER_LEN)
            .ok_or(PremultipliedSnapshotError::LengthOverflow)
    }
}

/// A malformed or resource-limited premultiplied snapshot.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PremultipliedSnapshotError {
    /// The ordinary pixel dimensions or byte count are invalid.
    #[error(transparent)]
    Surface(#[from] SurfaceError),
    /// A channel exceeds its pixel alpha, including hidden RGB beneath zero alpha.
    #[error("snapshot pixel {index} is not premultiplied RGBA8")]
    InvalidPixel {
        /// Zero-based row-major pixel.
        index: usize,
    },
    /// The byte sequence does not identify this explicit container.
    #[error("invalid premultiplied snapshot header")]
    InvalidHeader,
    /// Future versions must never be decoded under v1 assumptions.
    #[error("unsupported premultiplied snapshot version {version}")]
    UnsupportedVersion {
        /// Rejected encoded version.
        version: u16,
    },
    /// Canonical shape cannot be reinterpreted even when the areas are equal.
    #[error("snapshot shape {actual:?} differs from declared {expected:?}")]
    SizeMismatch {
        /// Descriptor/step shape.
        expected: PhysicalSize,
        /// Container shape.
        actual: PhysicalSize,
    },
    /// Exact container size excludes both truncation and trailing bytes.
    #[error("snapshot container needs {expected} bytes, got {actual}")]
    InvalidContainerLength {
        /// Required header and payload length.
        expected: usize,
        /// Actual bytes.
        actual: usize,
    },
    /// Encoded size overflowed before allocation.
    #[error("premultiplied snapshot byte length overflow")]
    LengthOverflow,
    /// The caller's explicit resource ceiling was exceeded.
    #[error("snapshot requires {requested} bytes, above limit {limit}")]
    LimitExceeded {
        /// Requested encoded or decoded bytes for the operation.
        requested: usize,
        /// Caller-supplied corresponding limit.
        limit: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: u32, height: u32) -> PhysicalSize {
        PhysicalSize::new(width, height).unwrap()
    }

    #[test]
    fn exact_container_roundtrip_preserves_pm_values_and_separates_shapes_and_raw_images() {
        let pixels = vec![0, 0, 253, 253, 7, 9, 11, 255];
        let image = PremultipliedRgbaSurface::new(size(2, 1), pixels.clone()).unwrap();
        let bytes = image.encode(25).unwrap();
        assert_eq!(
            &bytes[..17],
            b"GFSPM8\0\x01\x00\x02\x00\x00\x00\x01\x00\x00\x00"
        );
        assert_eq!(&bytes[17..], &pixels);
        assert_eq!(bytes.len() % 4, 1);
        let rotated = PremultipliedRgbaSurface::new(size(1, 2), pixels).unwrap();
        assert_ne!(bytes, rotated.encode(25).unwrap());
        assert_eq!(
            PremultipliedRgbaSurface::decode(bytes.clone(), size(2, 1), 8).unwrap(),
            image
        );
        assert!(PremultipliedRgbaSurface::decode(bytes, size(1, 2), 8).is_err());
        assert!(image.encode(24).is_err());
    }

    #[test]
    fn malformed_headers_pixels_lengths_and_limits_are_rejected() {
        let image = PremultipliedRgbaSurface::new(size(1, 1), vec![2, 3, 4, 4]).unwrap();
        let bytes = image.encode(21).unwrap();
        for length in 0..21 {
            assert!(
                PremultipliedRgbaSurface::decode(bytes[..length].to_vec(), size(1, 1), 4).is_err()
            );
        }
        for index in [0, 7, 9, 13, 17] {
            let mut invalid = bytes.clone();
            invalid[index] = 255;
            assert!(PremultipliedRgbaSurface::decode(invalid, size(1, 1), 4).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(PremultipliedRgbaSurface::decode(extra, size(1, 1), 4).is_err());
        assert!(PremultipliedRgbaSurface::decode(bytes, size(1, 1), 3).is_err());
        assert!(PremultipliedRgbaSurface::new(size(1, 1), vec![1, 0, 0, 0]).is_err());
        assert!(PremultipliedRgbaSurface::new(size(1, 1), vec![0; 3]).is_err());
    }
}
