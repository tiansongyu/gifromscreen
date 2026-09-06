//! Authoring spaces follow forward geometry only; past image effects do not alter a new mark.

use gif_from_screen_domain::{
    ClipTransform, FrameClip, FrameGeometryPlan, FrameRenderStep, PhysicalPoint, PhysicalPx,
    PhysicalSize, ProjectManifest, QuarterTurn,
};
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
pub(super) struct GeometryTransform {
    pub input_size: PhysicalSize,
    pub transform: ClipTransform,
}

pub(crate) fn legacy_annotation_stage(frame: &FrameClip) -> Option<u32> {
    match frame.render_steps.first() {
        Some(FrameRenderStep::Composite { stage_id }) => Some(*stage_id),
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
) -> Result<Vec<GeometryTransform>, String> {
    let plan = FrameGeometryPlan::new(frame, source)?;
    let target_size = plan.stage_size(stage)?;
    let mut geometry = vec![GeometryTransform {
        input_size: source,
        transform: frame.transform,
    }];
    let mut size = plan.base_size();
    for step in &frame.render_steps {
        if matches!(step, FrameRenderStep::Composite { stage_id } if Some(*stage_id) == stage) {
            break;
        }
        let mut transform = ClipTransform::default();
        match step {
            FrameRenderStep::Composite { .. } | FrameRenderStep::Effect { .. } => continue,
            FrameRenderStep::Crop { rect } => transform.crop = Some(*rect),
            FrameRenderStep::Resize { size } => transform.output_size = Some(*size),
            FrameRenderStep::Rotate { rotation } => transform.rotation = *rotation,
            FrameRenderStep::FlipHorizontal => transform.flip_horizontal = true,
            FrameRenderStep::FlipVertical => transform.flip_vertical = true,
        }
        geometry.push(GeometryTransform {
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

fn transform_point(point: PhysicalPoint, geometry: GeometryTransform) -> Option<PhysicalPoint> {
    let transform = geometry.transform;
    let crop = transform
        .crop
        .unwrap_or(gif_from_screen_domain::PhysicalRect {
            origin: PhysicalPoint::default(),
            size: geometry.input_size,
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
