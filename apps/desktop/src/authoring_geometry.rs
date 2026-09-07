//! Authoring spaces follow forward geometry only; past image effects do not alter a new mark.

use gif_from_screen_domain::{
    ClipTransform, FrameClip, FrameGeometryPlan, FrameRenderStep, PhysicalPoint, PhysicalPx,
    PhysicalSize, ProjectManifest, QuarterTurn,
};
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum GeometryOperation {
    Transform {
        input_size: PhysicalSize,
        transform: ClipTransform,
    },
    PlaceCanvas {
        input_size: PhysicalSize,
        output_size: PhysicalSize,
        source_origin: PhysicalPoint,
    },
}

pub(crate) fn legacy_annotation_stage(frame: &FrameClip) -> Option<u32> {
    match frame.render_steps.first() {
        Some(FrameRenderStep::Composite { stage_id, .. }) => Some(*stage_id),
        _ => None,
    }
}

pub(crate) fn authoring_stage_size(
    manifest: &ProjectManifest,
    frame: &FrameClip,
    stage: Option<u32>,
) -> Result<PhysicalSize, String> {
    let source = manifest
        .assets
        .get(&frame.asset_id)
        .and_then(|asset| asset.kind.raster_size())
        .ok_or("The authoring frame has no source raster dimensions.")?;
    FrameGeometryPlan::new(frame, source)?.stage_size(stage)
}

pub(super) fn geometry_to_stage(
    frame: &FrameClip,
    source: PhysicalSize,
    stage: Option<u32>,
) -> Result<Vec<GeometryOperation>, String> {
    let plan = FrameGeometryPlan::new(frame, source)?;
    let target_size = plan.stage_size(stage)?;
    let mut geometry = vec![GeometryOperation::Transform {
        input_size: source,
        transform: frame.transform,
    }];
    let mut size = plan.base_size();
    for step in &frame.render_steps {
        if matches!(step, FrameRenderStep::Composite { stage_id, .. } if Some(*stage_id) == stage) {
            break;
        }
        let mut transform = ClipTransform::default();
        match step {
            FrameRenderStep::FreezeRegion { .. } | FrameRenderStep::CinemagraphOverlay { .. } => {
                return Err("Recorded input cannot be mapped through mixed frozen pixels. Re-edit its earlier paint stage instead.".to_owned());
            }
            FrameRenderStep::Composite { .. } | FrameRenderStep::Effect { .. } => continue,
            FrameRenderStep::ImageBorder { style } => {
                let placement = style.placement(size)?;
                geometry.push(GeometryOperation::PlaceCanvas {
                    input_size: size,
                    output_size: placement.output_size,
                    source_origin: placement.source_origin,
                });
                size = placement.output_size;
                continue;
            }
            FrameRenderStep::ImageShadow { style } => {
                let placement = style.placement(size)?;
                geometry.push(GeometryOperation::PlaceCanvas {
                    input_size: size,
                    output_size: placement.output_size,
                    source_origin: placement.source_origin,
                });
                size = placement.output_size;
                continue;
            }
            FrameRenderStep::Crop { rect } => transform.crop = Some(*rect),
            FrameRenderStep::Resize { size } => transform.output_size = Some(*size),
            FrameRenderStep::Rotate { rotation } => transform.rotation = *rotation,
            FrameRenderStep::FlipHorizontal => transform.flip_horizontal = true,
            FrameRenderStep::FlipVertical => transform.flip_vertical = true,
        }
        geometry.push(GeometryOperation::Transform {
            input_size: size,
            transform,
        });
        size = transform_size(size, transform);
    }
    if size != target_size {
        return Err("Authoring geometry disagrees with the frame stage canvas.".to_owned());
    }
    Ok(geometry)
}

fn transform_size(source: PhysicalSize, transform: ClipTransform) -> PhysicalSize {
    let size = transform
        .output_size
        .unwrap_or_else(|| transform.crop.map_or(source, |crop| crop.size));
    if matches!(
        transform.rotation,
        QuarterTurn::Clockwise90 | QuarterTurn::Clockwise270
    ) {
        PhysicalSize::new(size.height.get(), size.width.get()).expect("swapped valid dimensions")
    } else {
        size
    }
}

pub(super) fn transform_point_at_stage(
    manifest: &ProjectManifest,
    frame: &FrameClip,
    stage: Option<u32>,
    mut point: PhysicalPoint,
) -> Option<PhysicalPoint> {
    let source = manifest.assets.get(&frame.asset_id)?.kind.raster_size()?;
    for geometry in geometry_to_stage(frame, source, stage).ok()? {
        point = transform_point(point, geometry)?;
    }
    Some(point)
}

fn transform_point(point: PhysicalPoint, geometry: GeometryOperation) -> Option<PhysicalPoint> {
    let (input_size, transform) = match geometry {
        GeometryOperation::Transform {
            input_size,
            transform,
        } => (input_size, transform),
        GeometryOperation::PlaceCanvas {
            input_size,
            output_size,
            source_origin,
        } => {
            if point.x.get() >= input_size.width.get() || point.y.get() >= input_size.height.get() {
                return None;
            }
            let x = point.x.get().checked_add(source_origin.x.get())?;
            let y = point.y.get().checked_add(source_origin.y.get())?;
            return (x < output_size.width.get() && y < output_size.height.get()).then_some(
                PhysicalPoint {
                    x: PhysicalPx::new(x),
                    y: PhysicalPx::new(y),
                },
            );
        }
    };
    let crop = transform
        .crop
        .unwrap_or(gif_from_screen_domain::PhysicalRect {
            origin: PhysicalPoint::default(),
            size: input_size,
        });
    let mut x = point.x.get().checked_sub(crop.origin.x.get())?;
    let mut y = point.y.get().checked_sub(crop.origin.y.get())?;
    if x >= crop.size.width.get() || y >= crop.size.height.get() {
        return None;
    }
    let size = transform.output_size.unwrap_or(crop.size);
    x = u32::try_from(
        u64::from(x) * u64::from(size.width.get()) / u64::from(crop.size.width.get()),
    )
    .ok()?;
    y = u32::try_from(
        u64::from(y) * u64::from(size.height.get()) / u64::from(crop.size.height.get()),
    )
    .ok()?;
    let (mut x, mut y, w, h) = match transform.rotation {
        QuarterTurn::Zero => (x, y, size.width.get(), size.height.get()),
        QuarterTurn::Clockwise90 => (
            size.height.get() - 1 - y,
            x,
            size.height.get(),
            size.width.get(),
        ),
        QuarterTurn::Clockwise180 => (
            size.width.get() - 1 - x,
            size.height.get() - 1 - y,
            size.width.get(),
            size.height.get(),
        ),
        QuarterTurn::Clockwise270 => (
            y,
            size.width.get() - 1 - x,
            size.height.get(),
            size.width.get(),
        ),
    };
    if transform.flip_horizontal {
        x = w - 1 - x;
    }
    if transform.flip_vertical {
        y = h - 1 - y;
    }
    Some(PhysicalPoint {
        x: PhysicalPx::new(x),
        y: PhysicalPx::new(y),
    })
}
