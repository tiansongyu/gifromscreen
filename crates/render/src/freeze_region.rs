//! A frozen reference overwrites pixels, without flattening or discarding its upstream stages.

use gif_from_screen_domain::{AssetId, PhysicalRect, PhysicalSize};

use super::check_cancelled;
use crate::{
    CancellationToken, FrameAssetProvider, RenderError, RenderLimits, RgbaSurface,
    surface::checked_byte_len,
};

const COPY_CHUNK_BYTES: usize = 1024 * 4;

#[allow(
    clippy::too_many_arguments,
    reason = "reference identity/view, region policy, provider, budget and cancellation remain explicit"
)]
pub(super) fn apply<P: FrameAssetProvider + ?Sized, C: CancellationToken + ?Sized>(
    destination: &mut RgbaSurface,
    baseline_asset: AssetId,
    baseline_size: PhysicalSize,
    region: PhysicalRect,
    invert: bool,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    check_cancelled(cancellation)?;
    if destination.size() != baseline_size {
        return Err(RenderError::FreezeRegionSizeMismatch {
            expected: baseline_size,
            actual: destination.size(),
        });
    }
    if region.size.validate().is_err() || !region.fits_within(destination.size()) {
        return Err(RenderError::InvalidFreezeRegion {
            region,
            canvas: destination.size(),
        });
    }
    let view_bytes = checked_byte_len(baseline_size)?;
    let requested = destination.pixels().len().checked_add(view_bytes).ok_or(
        RenderError::EffectWorkingMemorySizeOverflow {
            effect: "freeze region",
        },
    )?;
    if requested > limits.max_surface_bytes {
        return Err(RenderError::EffectWorkingMemoryLimitExceeded {
            effect: "freeze region",
            requested,
            limit: limits.max_surface_bytes,
        });
    }
    check_cancelled(cancellation)?;
    let baseline =
        provider
            .load_rgba8(baseline_asset)
            .map_err(|source| RenderError::FreezeBaselineLoad {
                asset_id: baseline_asset,
                source,
            })?;
    check_cancelled(cancellation)?;
    if baseline.pixels().len() != view_bytes {
        return Err(RenderError::FreezeBaselineLengthMismatch {
            asset_id: baseline_asset,
            expected: view_bytes,
            actual: baseline.pixels().len(),
        });
    }
    // Only byte length is shared with the asset's canonical surface. Its row
    // shape can differ (e.g. a rotated uniform bitmap has the same raw hash).
    // The saved view determines addressing; no resize or provider-cache mutation occurs.
    let row_bytes = usize::try_from(u64::from(baseline_size.width.get()) * 4)
        .expect("validated view length bounds its row size");
    let left = usize::try_from(u64::from(region.origin.x.get()) * 4).expect("validated X");
    let right =
        left + usize::try_from(u64::from(region.size.width.get()) * 4).expect("validated width");
    let top = region.origin.y.get();
    let bottom = top + region.size.height.get();
    for (index, (output, frozen)) in destination
        .pixels_mut()
        .chunks_exact_mut(row_bytes)
        .zip(baseline.pixels().chunks_exact(row_bytes))
        .enumerate()
    {
        check_cancelled(cancellation)?;
        let y = u32::try_from(index).expect("row index bounded by height");
        let inside_row = y >= top && y < bottom;
        if invert {
            if inside_row {
                overwrite(&mut output[left..right], &frozen[left..right], cancellation)?;
            }
        } else if inside_row {
            overwrite(&mut output[..left], &frozen[..left], cancellation)?;
            overwrite(&mut output[right..], &frozen[right..], cancellation)?;
        } else {
            overwrite(output, frozen, cancellation)?;
        }
    }
    check_cancelled(cancellation)
}

fn overwrite<C: CancellationToken + ?Sized>(
    output: &mut [u8],
    frozen: &[u8],
    cancellation: &C,
) -> Result<(), RenderError> {
    for (output, frozen) in output
        .chunks_mut(COPY_CHUNK_BYTES)
        .zip(frozen.chunks(COPY_CHUNK_BYTES))
    {
        check_cancelled(cancellation)?;
        output.copy_from_slice(frozen);
    }
    Ok(())
}

#[cfg(test)]
#[path = "freeze_region_tests.rs"]
mod tests;
