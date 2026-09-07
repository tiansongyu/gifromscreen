//! Ordered, pixel-independent frame geometry after the unchanged legacy prefix.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{AssetId, Effect, FrameClip, PhysicalRect, PhysicalSize, QuarterTurn};

/// Hard bound on persisted per-frame execution and planning work.
pub const MAX_FRAME_RENDER_STEPS: usize = 4_096;
/// Radius accepted by the deterministic blur/shadow implementations.
pub const MAX_FRAME_RENDER_EFFECT_RADIUS: u16 = 256;

/// Persisted precision at a paint boundary. Existing projects retain their
/// original straight-alpha arithmetic unless a new authoring edit opts in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositePrecision {
    #[default]
    LegacyStraightRgba8,
    WpfPbgra8PngV1,
}

impl CompositePrecision {
    pub const fn is_legacy(&self) -> bool {
        matches!(self, Self::LegacyStraightRgba8)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FrameRenderStep {
    Composite {
        stage_id: u32,
        #[serde(default, skip_serializing_if = "CompositePrecision::is_legacy")]
        precision: CompositePrecision,
    },
    Crop {
        rect: PhysicalRect,
    },
    Resize {
        size: PhysicalSize,
    },
    Rotate {
        rotation: QuarterTurn,
    },
    FlipHorizontal,
    FlipVertical,
    Effect {
        effect: Effect,
    },
    ImageBorder {
        style: crate::ImageBorderStyle,
    },
    ImageShadow {
        style: crate::ImageShadowStyle,
    },
    FreezeRegion {
        baseline_asset: AssetId,
        baseline_size: PhysicalSize,
        region: PhysicalRect,
        invert: bool,
    },
    CinemagraphOverlay {
        snapshot_asset: AssetId,
        snapshot_size: PhysicalSize,
    },
}

impl FrameRenderStep {
    /// Constructs the legacy boundary without changing its serialized shape.
    pub const fn composite(stage_id: u32) -> Self {
        Self::Composite {
            stage_id,
            precision: CompositePrecision::LegacyStraightRgba8,
        }
    }

    pub const fn required_schema_version(&self) -> u32 {
        match self {
            Self::CinemagraphOverlay { .. } => 7,
            Self::FreezeRegion { .. } => 6,
            Self::Composite {
                precision: CompositePrecision::WpfPbgra8PngV1,
                ..
            } => 5,
            Self::ImageBorder { .. } | Self::ImageShadow { .. } => 4,
            _ => 3,
        }
    }

    pub const fn effect(&self) -> Option<&Effect> {
        match self {
            Self::Effect { effect } => Some(effect),
            _ => None,
        }
    }

    pub const fn referenced_asset(&self) -> Option<AssetId> {
        match self {
            Self::CinemagraphOverlay { snapshot_asset, .. } => Some(*snapshot_asset),
            Self::FreezeRegion { baseline_asset, .. } => Some(*baseline_asset),
            _ => match self.effect() {
                Some(effect) => effect.referenced_asset(),
                None => None,
            },
        }
    }
}

/// Checks bounded structure without loading the source asset or any pixels.
///
/// # Errors
/// Rejects more than 4,096 steps, a non-Composite first step, and zero or
/// repeated Composite identities. Errors identify the one-based step.
pub fn validate_frame_render_steps(steps: &[FrameRenderStep]) -> Result<(), String> {
    if steps.len() > MAX_FRAME_RENDER_STEPS {
        return Err("Frame render steps exceed the 4,096-step limit.".to_owned());
    }
    if !steps.is_empty() && !matches!(steps.first(), Some(FrameRenderStep::Composite { .. })) {
        return Err("Render step 1 must be a Composite stage.".to_owned());
    }
    if matches!(
        steps.first(),
        Some(FrameRenderStep::Composite {
            precision: CompositePrecision::WpfPbgra8PngV1,
            ..
        })
    ) {
        return Err(
            "Render step 1 must retain legacy precision for time-anchored overlays.".to_owned(),
        );
    }
    let mut identities = BTreeSet::new();
    for (index, step) in steps.iter().enumerate() {
        if let FrameRenderStep::Composite { stage_id, .. } = step
            && (*stage_id == 0 || !identities.insert(*stage_id))
        {
            return Err(format!(
                "Render step {}: Composite stage identities must be unique and nonzero.",
                index + 1
            ));
        }
    }
    Ok(())
}

impl FrameClip {
    /// Legacy and ordered-stage effects, including assets not currently visible.
    pub fn all_effects(&self) -> impl Iterator<Item = &Effect> {
        self.effects
            .iter()
            .chain(self.render_steps.iter().filter_map(FrameRenderStep::effect))
    }

    pub fn referenced_effect_assets(&self) -> impl Iterator<Item = AssetId> + '_ {
        self.effects
            .iter()
            .filter_map(Effect::referenced_asset)
            .chain(
                self.render_steps
                    .iter()
                    .filter_map(FrameRenderStep::referenced_asset),
            )
    }

    pub fn required_schema_version(&self) -> u32 {
        self.render_steps
            .iter()
            .map(FrameRenderStep::required_schema_version)
            .max()
            .unwrap_or(1)
    }
}

/// Dimensions at every ordered operation and explicitly identified paint stage.
///
/// No pixels are loaded or allocated. Legacy effects keep the surface size;
/// image border/shadow steps expand it. Parameters are checked at their input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameGeometryPlan {
    base_size: PhysicalSize,
    output_size: PhysicalSize,
    step_inputs: Vec<PhysicalSize>,
    stages: BTreeMap<u32, PhysicalSize>,
}

impl FrameGeometryPlan {
    /// Plans source crop, pre-rotation resize, rotation/flips, legacy effects,
    /// then the persisted ordered steps. Empty steps retain that exact prefix.
    ///
    /// # Errors
    /// Rejects invalid dimensions, crops, effect inputs, stage identities or
    /// more than 4,096 steps. Ordered errors include a one-based step position.
    pub fn new(frame: &FrameClip, source_size: PhysicalSize) -> Result<Self, String> {
        validate_frame_render_steps(&frame.render_steps)?;
        source_size
            .validate()
            .map_err(|error| format!("Source size: {error}"))?;
        let mut current = source_size;
        if let Some(crop) = frame.transform.crop {
            validate_crop(crop, current).map_err(|reason| format!("Legacy crop: {reason}"))?;
            current = crop.size;
        }
        if let Some(size) = frame.transform.output_size {
            size.validate()
                .map_err(|error| format!("Legacy resize: {error}"))?;
            current = size;
        }
        current = rotated_size(current, frame.transform.rotation);
        for (index, effect) in frame.effects.iter().enumerate() {
            validate_render_effect(effect, current)
                .map_err(|reason| format!("Legacy effect {}: {reason}", index + 1))?;
        }
        let base_size = current;
        let mut step_inputs = Vec::new();
        step_inputs
            .try_reserve_exact(frame.render_steps.len())
            .map_err(|_| "Could not allocate the bounded frame geometry plan.".to_owned())?;
        let mut stages = BTreeMap::new();
        for (index, step) in frame.render_steps.iter().enumerate() {
            step_inputs.push(current);
            current = apply_geometry_step(step, current, &mut stages)
                .map_err(|reason| format!("Render step {}: {reason}", index + 1))?;
        }
        Ok(Self {
            base_size,
            output_size: current,
            step_inputs,
            stages,
        })
    }

    /// Size after the complete legacy prefix, before the first Composite.
    pub const fn base_size(&self) -> PhysicalSize {
        self.base_size
    }

    /// Size after the final ordered operation, also the unanchored paint tail.
    pub const fn output_size(&self) -> PhysicalSize {
        self.output_size
    }

    /// None selects the current tail; Some selects an owner's Composite stage.
    ///
    /// # Errors
    /// Rejects an unknown or zero stage identity.
    pub fn stage_size(&self, stage: Option<u32>) -> Result<PhysicalSize, String> {
        stage.map_or(Ok(self.output_size), |id| {
            self.stages
                .get(&id)
                .copied()
                .ok_or_else(|| format!("Composite stage {id} does not exist on this frame."))
        })
    }

    /// Input dimensions for a zero-based index in the persisted step vector.
    ///
    /// # Errors
    /// Rejects an index outside this plan.
    pub fn step_input_size(&self, index: usize) -> Result<PhysicalSize, String> {
        self.step_inputs
            .get(index)
            .copied()
            .ok_or_else(|| format!("Render step index {index} is outside this frame."))
    }
}

fn apply_geometry_step(
    step: &FrameRenderStep,
    size: PhysicalSize,
    stages: &mut BTreeMap<u32, PhysicalSize>,
) -> Result<PhysicalSize, String> {
    match step {
        FrameRenderStep::Composite { stage_id, .. } => {
            if *stage_id == 0 || stages.insert(*stage_id, size).is_some() {
                return Err("Composite stage identities must be unique and nonzero.".to_owned());
            }
        }
        FrameRenderStep::Crop { rect } => {
            validate_crop(*rect, size)?;
            return Ok(rect.size);
        }
        FrameRenderStep::Resize { size } => {
            size.validate()
                .map_err(|error| format!("Resize: {error}"))?;
            return Ok(*size);
        }
        FrameRenderStep::Rotate { rotation } => return Ok(rotated_size(size, *rotation)),
        FrameRenderStep::FlipHorizontal | FrameRenderStep::FlipVertical => {}
        FrameRenderStep::Effect { effect } => validate_render_effect(effect, size)?,
        FrameRenderStep::ImageBorder { style } => return Ok(style.placement(size)?.output_size),
        FrameRenderStep::ImageShadow { style } => return Ok(style.placement(size)?.output_size),
        FrameRenderStep::FreezeRegion {
            baseline_size,
            region,
            ..
        } => {
            if *baseline_size != size {
                return Err("Freeze baseline dimensions must match the current step input without resizing.".to_owned());
            }
            validate_crop(*region, size).map_err(|reason| format!("Freeze region: {reason}"))?;
        }
        FrameRenderStep::CinemagraphOverlay { snapshot_size, .. } => {
            if *snapshot_size != size {
                return Err("Cinemagraph snapshot dimensions must match the current step input without resizing.".to_owned());
            }
        }
    }
    Ok(size)
}

/// Validates a tightly packed raw RGBA8 view without loading pixels. Content
/// identities hash bytes, not dimensions: equal-area views may intentionally
/// have a different shape from the immutable descriptor. No resize is implied.
///
/// # Errors
/// Rejects non-raster or compressed assets, empty/overflowing dimensions, and
/// any descriptor/view whose exact RGBA byte count differs from the asset length.
pub fn validate_raw_rgba_view(
    asset: &crate::AssetDescriptor,
    view: PhysicalSize,
) -> Result<(), String> {
    let Some((canonical, crate::RasterEncoding::Rgba8)) = asset.kind.raster_descriptor() else {
        return Err("A frozen baseline requires a raw RGBA8 raster asset.".to_owned());
    };
    for size in [canonical, view] {
        size.validate()
            .map_err(|error| format!("Invalid raw RGBA view: {error}"))?;
        let expected = size.area().and_then(|area| area.checked_mul(4));
        if expected != Some(asset.byte_len) {
            return Err("Frozen baseline descriptor and view must both match the exact raw RGBA byte length.".to_owned());
        }
    }
    Ok(())
}

fn rotated_size(size: PhysicalSize, rotation: QuarterTurn) -> PhysicalSize {
    match rotation {
        QuarterTurn::Clockwise90 | QuarterTurn::Clockwise270 => PhysicalSize {
            width: size.height,
            height: size.width,
        },
        QuarterTurn::Zero | QuarterTurn::Clockwise180 => size,
    }
}

fn validate_crop(rect: PhysicalRect, size: PhysicalSize) -> Result<(), String> {
    if rect.size.validate().is_err() || !rect.fits_within(size) {
        return Err(format!(
            "Crop {rect:?} falls outside the current {}x{} surface.",
            size.width.get(),
            size.height.get()
        ));
    }
    Ok(())
}

/// Renderer-level validity, not UI policy: transparent colors and oversized
/// inset border widths remain renderable no-ops/clamped borders.
///
/// # Errors
/// Rejects out-of-input effect regions, unsupported parameters, or the old
/// Cinemagraph effect variant. Interactive Cinemagraph uses its separate bake
/// workflow and is not represented by that unsupported persisted variant.
pub fn validate_render_effect(effect: &Effect, size: PhysicalSize) -> Result<(), String> {
    size.validate()
        .map_err(|error| format!("Effect input size: {error}"))?;
    if let Some(region) = effect.region()
        && (region.size.validate().is_err() || !region.fits_within(size))
    {
        return Err(format!(
            "Effect region {region:?} falls outside the current {}x{} surface.",
            size.width.get(),
            size.height.get()
        ));
    }
    match effect {
        Effect::Blur { radius, .. } if !(1..=MAX_FRAME_RENDER_EFFECT_RADIUS).contains(radius) => {
            Err("Blur radius must be between 1 and 256.".to_owned())
        }
        Effect::Pixelate { block_size: 0, .. } => {
            Err("Pixelate block size must be nonzero.".to_owned())
        }
        Effect::Darken { amount_percent, .. } | Effect::Lighten { amount_percent, .. }
            if *amount_percent > 100 =>
        {
            Err("Tone amount must be between 0 and 100 percent.".to_owned())
        }
        Effect::Shadow { blur_radius, .. } if *blur_radius > MAX_FRAME_RENDER_EFFECT_RADIUS => {
            Err("Shadow blur radius must not exceed 256.".to_owned())
        }
        Effect::Cinemagraph { .. } => Err(
            "The persisted Cinemagraph effect is unsupported; use the motion bake workflow."
                .to_owned(),
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[path = "frame_geometry_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "freeze_region_tests.rs"]
mod freeze_region_tests;
