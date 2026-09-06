//! Current-image effect edits, including effects that change the animation canvas.

use std::collections::BTreeSet;

use gif_from_screen_domain::{
    Effect, FrameClip, FrameGeometryPlan, FrameId, FrameRenderStep, ImageBorderStyle,
    ImageShadowStyle, PhysicalSize, ProjectManifest,
};

use super::{geometry, pipeline_error};
use crate::{EditorError, FrameEffectEdit};

/// Bounded image-effect output used by the interactive editor and task runner.
pub const MAX_COMPOSED_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// An effect on the complete pixels at one point in the edit history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComposedImageEffect {
    /// An existing fixed-canvas effect, with its original pixel semantics.
    Legacy(Effect),
    /// Signed inner/outer borders and an explicit background.
    Border(ImageBorderStyle),
    /// Software-reference Gaussian shadow with an expanded canvas.
    Shadow(ImageShadowStyle),
}

/// Effects are indexed in the legacy prefix followed by chronological effect steps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComposedEffectEdit {
    /// Add a new effect after the current complete image.
    Add(ComposedImageEffect),
    /// Replace an earlier effect without silently regenerating later artwork.
    Replace {
        /// Zero-based effect position on every affected frame.
        index: usize,
        /// Replacement effect parameters.
        effect: ComposedImageEffect,
    },
    /// Clear effects; canvas-changing effects require the complete animation.
    Clear,
}

impl From<&FrameEffectEdit> for ComposedEffectEdit {
    fn from(edit: &FrameEffectEdit) -> Self {
        match edit {
            FrameEffectEdit::Add(effect) => Self::Add(ComposedImageEffect::Legacy(effect.clone())),
            FrameEffectEdit::Replace { index, effect } => Self::Replace {
                index: *index,
                effect: ComposedImageEffect::Legacy(effect.clone()),
            },
            FrameEffectEdit::Clear => Self::Clear,
        }
    }
}

impl ComposedImageEffect {
    pub(super) fn step(&self) -> FrameRenderStep {
        match self {
            Self::Legacy(effect) => FrameRenderStep::Effect {
                effect: effect.clone(),
            },
            Self::Border(style) => FrameRenderStep::ImageBorder { style: *style },
            Self::Shadow(style) => FrameRenderStep::ImageShadow { style: *style },
        }
    }

    pub(super) fn requires_all_frames(&self) -> bool {
        step_requires_all_frames(&self.step())
    }

    pub(super) fn validate(
        &self,
        frame_id: FrameId,
        size: PhysicalSize,
    ) -> Result<(), EditorError> {
        let output = match self {
            Self::Legacy(effect) => return crate::frame_effect::validate_effect(effect, size),
            Self::Border(style) => style.placement(size),
            Self::Shadow(style) => style.placement(size),
        }
        .map_err(|reason| pipeline_error(frame_id, reason))?
        .output_size;
        validate_buffer(frame_id, output)
    }
}

fn limit_error(frame_id: FrameId) -> EditorError {
    pipeline_error(
        frame_id,
        "The image-effect program exceeds a 64 MiB intermediate buffer or the final 65,535-pixel GIF dimension limit. Resize the animation first.",
    )
}

fn validate_buffer(frame_id: FrameId, size: PhysicalSize) -> Result<(), EditorError> {
    if size
        .area()
        .and_then(|pixels| pixels.checked_mul(4))
        .is_none_or(|bytes| bytes > MAX_COMPOSED_IMAGE_BYTES)
    {
        return Err(limit_error(frame_id));
    }
    Ok(())
}

/// Replacing an early operation can enlarge later operations. Validate every
/// intermediate buffer, not only the requested effect or the cropped tail.
/// GIF dimensions constrain the final image; a narrow, wide intermediate that
/// is subsequently cropped is valid when it stays within the buffer budget.
pub(super) fn validate_program(
    frame: &FrameClip,
    source_size: PhysicalSize,
    plan: &FrameGeometryPlan,
) -> Result<(), EditorError> {
    if !frame.render_steps.iter().any(|step| {
        matches!(
            step,
            FrameRenderStep::ImageBorder { .. } | FrameRenderStep::ImageShadow { .. }
        )
    }) {
        return Ok(());
    }
    validate_buffer(frame.id, source_size)?;
    validate_buffer(frame.id, plan.base_size())?;
    for index in 0..frame.render_steps.len() {
        let input = plan
            .step_input_size(index)
            .map_err(|reason| pipeline_error(frame.id, reason))?;
        validate_buffer(frame.id, input)?;
    }
    let output = plan.output_size();
    validate_buffer(frame.id, output)?;
    if output.width.get() > u32::from(u16::MAX) || output.height.get() > u32::from(u16::MAX) {
        return Err(limit_error(frame.id));
    }
    Ok(())
}

pub(super) fn is_effect(step: &FrameRenderStep) -> bool {
    matches!(
        step,
        FrameRenderStep::Effect { .. }
            | FrameRenderStep::ImageBorder { .. }
            | FrameRenderStep::ImageShadow { .. }
    )
}

fn step_requires_all_frames(step: &FrameRenderStep) -> bool {
    match step {
        FrameRenderStep::ImageShadow { .. } => true,
        FrameRenderStep::ImageBorder { style } => {
            let widths = &style.widths;
            [
                widths.top_milli,
                widths.right_milli,
                widths.bottom_milli,
                widths.left_milli,
            ]
            .iter()
            .any(|edge| *edge < 0)
        }
        _ => false,
    }
}

impl ComposedEffectEdit {
    pub(super) fn appended(&self) -> Option<&ComposedImageEffect> {
        if let Self::Add(effect) = self {
            Some(effect)
        } else {
            None
        }
    }

    pub(super) fn requires_all_frames(
        &self,
        project: &ProjectManifest,
        selected: &BTreeSet<FrameId>,
    ) -> bool {
        match self {
            Self::Add(effect) => effect.requires_all_frames(),
            Self::Replace { effect, .. } if effect.requires_all_frames() => true,
            _ => project
                .timeline
                .frames
                .iter()
                .filter(|frame| selected.contains(&frame.id))
                .any(|frame| match self {
                    Self::Clear => frame.render_steps.iter().any(step_requires_all_frames),
                    Self::Replace { index, .. } => index
                        .checked_sub(frame.effects.len())
                        .and_then(|offset| {
                            frame
                                .render_steps
                                .iter()
                                .filter(|step| is_effect(step))
                                .nth(offset)
                        })
                        .is_some_and(step_requires_all_frames),
                    Self::Add(_) => false,
                }),
        }
    }

    pub(super) fn apply(
        &self,
        frame: &mut FrameClip,
        source_size: PhysicalSize,
    ) -> Result<(), EditorError> {
        match self {
            Self::Clear => {
                frame.effects.clear();
                frame.render_steps.retain(|step| !is_effect(step));
            }
            Self::Replace { index, effect } if *index < frame.effects.len() => {
                let ComposedImageEffect::Legacy(value) = effect else {
                    return Err(pipeline_error(
                        frame.id,
                        "A legacy prefix effect cannot be replaced by a current-image canvas effect. Keep its legacy type, or remove it and add the new image effect explicitly.",
                    ));
                };
                frame.effects[*index] = value.clone();
                effect.validate(frame.id, geometry(frame, source_size)?.base_size())?;
            }
            Self::Replace { index, effect } => {
                let count = super::frame_effect_count(frame);
                let (position, step) = frame
                    .render_steps
                    .iter_mut()
                    .enumerate()
                    .filter(|(_, step)| is_effect(step))
                    .nth(index - frame.effects.len())
                    .ok_or(EditorError::EffectIndexOutOfBounds {
                        frame_id: frame.id,
                        index: *index,
                        effect_count: count,
                    })?;
                *step = effect.step();
                let size = geometry(frame, source_size)?
                    .step_input_size(position)
                    .map_err(|reason| pipeline_error(frame.id, reason))?;
                effect.validate(frame.id, size)?;
            }
            Self::Add(_) => unreachable!("append operations are sealed by the caller"),
        }
        Ok(())
    }
}
