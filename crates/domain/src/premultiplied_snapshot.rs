//! Metadata contract for typed clipped-image snapshots. Pixel/container decoding
//! is owned by the renderer; ordinary RGBA assets are never reinterpreted here.

use crate::{AssetDescriptor, AssetKind, PhysicalSize};

/// Seven magic bytes, a little-endian u16 version and two little-endian u32 dimensions.
/// Its non-multiple-of-four length separates valid snapshots from raw RGBA byte identities.
pub const PREMULTIPLIED_SNAPSHOT_HEADER_LEN: usize = 17;
/// The only supported container version for tightly packed premultiplied RGBA8 payloads.
pub const PREMULTIPLIED_SNAPSHOT_FORMAT_VERSION: u16 = 1;

/// Validates the independent storage type, dimensions, version and exact length.
///
/// # Errors
/// Rejects ordinary rasters/binary data, unsupported versions, empty/overflowing
/// dimensions, or any length other than 17 + width * height * 4.
pub fn validate_premultiplied_snapshot_descriptor(asset: &AssetDescriptor) -> Result<(), String> {
    let AssetKind::PremultipliedSnapshot {
        size,
        format_version,
    } = asset.kind
    else {
        return Err(
            "Cinemagraph requires a typed premultiplied snapshot, not a straight-alpha raster."
                .to_owned(),
        );
    };
    if format_version != PREMULTIPLIED_SNAPSHOT_FORMAT_VERSION {
        return Err(
            "Unsupported premultiplied snapshot format version; only version 1 is supported."
                .to_owned(),
        );
    }
    size.validate()
        .map_err(|error| format!("Invalid premultiplied snapshot dimensions: {error}"))?;
    let expected = size
        .area()
        .and_then(|area| area.checked_mul(4))
        .and_then(|payload| payload.checked_add(PREMULTIPLIED_SNAPSHOT_HEADER_LEN as u64));
    if expected != Some(asset.byte_len) {
        return Err("Premultiplied snapshot length must exactly match its 17-byte header and RGBA8 payload.".to_owned());
    }
    Ok(())
}

/// Validates a snapshot at its exact authored dimensions. Unlike raw RGBA views,
/// the typed container includes its dimensions and cannot be reshaped by equal area.
///
/// # Errors
/// Returns descriptor validation failures or an incompatible requested shape.
pub fn validate_premultiplied_snapshot_view(
    asset: &AssetDescriptor,
    expected_size: PhysicalSize,
) -> Result<(), String> {
    validate_premultiplied_snapshot_descriptor(asset)?;
    if !matches!(asset.kind, AssetKind::PremultipliedSnapshot { size, .. } if size == expected_size)
    {
        return Err("Cinemagraph snapshot dimensions must match exactly; premultiplied shape aliases are not supported.".to_owned());
    }
    Ok(())
}

#[cfg(test)]
#[path = "premultiplied_snapshot_tests.rs"]
mod tests;
