//! Explicit version-two stage. No legacy precision is routed through this code.

use std::mem::size_of;

use gif_from_screen_domain::{
    CompositePrecision, FrameId, OverlayContent, WPF_VECTOR_SHAPE_VERSION,
};

use super::{OverlayLayer, PaintBlend, check_cancelled};
use crate::{
    CancellationToken, InkError, MAX_VECTOR_PREVIEW_SHAPES, RenderError, RenderLimits, RgbaSurface,
    surface::checked_byte_len,
    vector_shape::{WpfVectorVisual, render_wpf_vector_visuals_with_tail_work},
};

pub(super) fn validate_layer(
    frame_id: FrameId,
    layer: &OverlayLayer<'_>,
    precision: Option<CompositePrecision>,
) -> Result<(), RenderError> {
    let v2 = matches!(layer.content, OverlayContent::VectorShape { shape }
        if shape.version == WPF_VECTOR_SHAPE_VERSION);
    let stage = precision == Some(CompositePrecision::VectorCanvasPbgra8PngV2);
    if v2 != stage || (v2 && (layer.span.is_some() || layer.stage.is_none())) {
        return Err(RenderError::InvalidRenderSteps {
            frame_id,
            reason: format!(
                "Overlay {} requires matching frame-owned vector V2 content and stage",
                layer.id
            ),
        });
    }
    Ok(())
}

pub(super) fn composite<C: CancellationToken + ?Sized>(
    destination: &mut RgbaSurface,
    layers: &Vec<OverlayLayer<'_>>,
    limits: RenderLimits,
    cancel: &C,
) -> Result<(), RenderError> {
    check_cancelled(cancel)?;
    if layers.len() > MAX_VECTOR_PREVIEW_SHAPES {
        return Err(invalid(
            "Vector V2 stage exceeds the 256-object rendering bound",
        ));
    }
    let bytes = checked_byte_len(destination.size())?;
    let required = retained_bytes(bytes, layers.capacity(), layers.len())?;
    remaining_bytes(limits, required)?;
    let mut visuals = Vec::new();
    visuals.try_reserve_exact(layers.len()).map_err(|_| {
        RenderError::OverlayPlanAllocationFailed {
            requested: layers.len(),
        }
    })?;
    for layer in layers {
        check_cancelled(cancel)?;
        let OverlayContent::VectorShape { shape } = layer.content else {
            return Err(invalid("Vector V2 stage contains non-vector content"));
        };
        if shape.version != WPF_VECTOR_SHAPE_VERSION
            || layer.span.is_some()
            || layer.stage.is_none()
            || layer.blend_mode != PaintBlend::WpfSourceOver
        {
            return Err(invalid(
                "Vector V2 stage requires frame-owned version-two Normal marks",
            ));
        }
        visuals.push(WpfVectorVisual {
            shape,
            opacity: layer.track_opacity,
        });
    }
    let retained = retained_bytes(bytes, layers.capacity(), visuals.capacity())?;
    let remaining = remaining_bytes(limits, retained)?;
    // Reserve the final whole-frame conversion as part of the same operation;
    // unpainted semi-transparent base pixels still cross this explicit boundary.
    let canvas = render_wpf_vector_visuals_with_tail_work(
        &visuals,
        destination.size(),
        RenderLimits {
            max_surface_bytes: remaining,
        },
        u64::try_from(bytes / 4).map_err(|_| invalid("Vector V2 pixel count overflows"))?,
        cancel,
    )
    .map_err(|error| match error {
        InkError::Cancelled => RenderError::Cancelled,
        InkError::Surface(error) => RenderError::Surface(error),
        error => invalid(error.to_string()),
    })?;
    for (index, (target, source)) in destination
        .pixels_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(canvas.pixels().as_chunks::<4>().0)
        .enumerate()
    {
        if index.is_multiple_of(1024) {
            check_cancelled(cancel)?;
        }
        *target = crate::wpf_pixels::unpremultiply(crate::wpf_pixels::over(
            *source,
            crate::wpf_pixels::premultiply(*target),
        ));
    }
    check_cancelled(cancel)
}

fn retained_bytes(bytes: usize, layers: usize, entries: usize) -> Result<usize, RenderError> {
    layers
        .checked_mul(size_of::<OverlayLayer<'_>>())
        .and_then(|n| {
            entries
                .checked_mul(size_of::<WpfVectorVisual<'_>>())
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_add(bytes))
        .ok_or_else(|| invalid("Vector V2 retained working bytes overflow"))
}

fn remaining_bytes(limits: RenderLimits, retained: usize) -> Result<usize, RenderError> {
    limits.max_surface_bytes.checked_sub(retained).ok_or(
        RenderError::EffectWorkingMemoryLimitExceeded {
            effect: "vector V2 canvas",
            requested: retained,
            limit: limits.max_surface_bytes,
        },
    )
}

fn invalid(reason: impl Into<String>) -> RenderError {
    RenderError::InvalidVectorShape {
        reason: reason.into(),
    }
}

#[cfg(test)]
#[path = "vector_canvas_v2_tests.rs"]
mod tests;
