use std::collections::BTreeSet;

use gif_from_screen_domain::{
    EdgeWidths, EditCommand, Effect, FrameId, PhysicalRect, PhysicalSize, ProjectManifest, Rgba,
};

use crate::{EditorError, ensure_known_selection};

/// Maximum blur radius supported by frame-effect command construction and the CPU renderer.
pub const MAX_FRAME_EFFECT_BLUR_RADIUS: u16 = 256;

/// Atomic update to each selected frame's ordered effect list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameEffectEdit {
    /// Append one validated effect.
    Add(Effect),
    /// Replace the effect at the same zero-based index on every selected frame.
    Replace {
        /// Zero-based position in each selected frame's effect list.
        index: usize,
        /// Validated replacement effect.
        effect: Effect,
    },
    /// Remove all effects from every selected frame.
    Clear,
}

/// Builds one reversible compound command that edits selected frame effects.
///
/// Input order and duplicate frame identities are ignored; replacements are emitted in timeline
/// order. Every selected clip is cloned into one [`EditCommand::ReplaceFrame`], changing only its
/// ordered `effects` vector. Asset identity, duration, transform, and capture metadata are retained.
///
/// Region effects use project-canvas coordinates. This matches domain validation and gives a stable
/// editing space even when individual clip transforms produce different intermediate dimensions.
/// Border widths may meet but not overlap across the canvas. Fully transparent Border and Shadow
/// colors are rejected because they are invisible no-ops.
///
/// # Errors
///
/// Returns [`EditorError`] for empty/unknown selection, invalid region, radius, block size, tone
/// percentage, border widths, invisible color, unsupported effect type, or a replace index missing
/// from any selected frame. All validation finishes before the command is returned.
pub fn edit_frame_effects(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    edit: &FrameEffectEdit,
) -> Result<EditCommand, EditorError> {
    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;
    if let Some(effect) = edited_effect(edit) {
        validate_effect(effect, project.canvas.size)?;
    }

    let selected_frames = project
        .timeline
        .frames
        .iter()
        .filter(|frame| selected.contains(&frame.id))
        .collect::<Vec<_>>();
    if let FrameEffectEdit::Replace { index, .. } = edit {
        for frame in &selected_frames {
            if *index >= frame.effects.len() {
                return Err(EditorError::EffectIndexOutOfBounds {
                    frame_id: frame.id,
                    index: *index,
                    effect_count: frame.effects.len(),
                });
            }
        }
    }

    let commands = selected_frames
        .into_iter()
        .map(|frame| {
            let mut replacement = frame.clone();
            match edit {
                FrameEffectEdit::Add(effect) => replacement.effects.push(effect.clone()),
                FrameEffectEdit::Replace { index, effect } => {
                    replacement.effects[*index] = effect.clone();
                }
                FrameEffectEdit::Clear => replacement.effects.clear(),
            }
            EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(replacement),
            }
        })
        .collect();
    Ok(EditCommand::Compound { commands })
}

fn edited_effect(edit: &FrameEffectEdit) -> Option<&Effect> {
    match edit {
        FrameEffectEdit::Add(effect) | FrameEffectEdit::Replace { effect, .. } => Some(effect),
        FrameEffectEdit::Clear => None,
    }
}

fn validate_effect(effect: &Effect, canvas: PhysicalSize) -> Result<(), EditorError> {
    match effect {
        Effect::Blur { region, radius } => {
            validate_region("blur", *region, canvas)?;
            if !(1..=MAX_FRAME_EFFECT_BLUR_RADIUS).contains(radius) {
                return Err(EditorError::InvalidFrameEffectParameter {
                    effect: "blur",
                    parameter: "radius",
                    value: u64::from(*radius),
                    maximum: u64::from(MAX_FRAME_EFFECT_BLUR_RADIUS),
                });
            }
        }
        Effect::Pixelate { region, block_size } => {
            validate_region("pixelate", *region, canvas)?;
            if *block_size == 0 {
                return Err(EditorError::InvalidFrameEffectParameter {
                    effect: "pixelate",
                    parameter: "block_size",
                    value: 0,
                    maximum: u64::from(u16::MAX),
                });
            }
        }
        Effect::Darken {
            region,
            amount_percent,
        } => {
            validate_region("darken", *region, canvas)?;
            validate_percent("darken", *amount_percent)?;
        }
        Effect::Lighten {
            region,
            amount_percent,
        } => {
            validate_region("lighten", *region, canvas)?;
            validate_percent("lighten", *amount_percent)?;
        }
        Effect::Border { widths, color } => {
            validate_border(*widths, canvas)?;
            validate_visible_color("border", *color)?;
        }
        Effect::Shadow {
            blur_radius, color, ..
        } => {
            if *blur_radius > MAX_FRAME_EFFECT_BLUR_RADIUS {
                return Err(EditorError::InvalidFrameEffectParameter {
                    effect: "shadow",
                    parameter: "blur_radius",
                    value: u64::from(*blur_radius),
                    maximum: u64::from(MAX_FRAME_EFFECT_BLUR_RADIUS),
                });
            }
            validate_visible_color("shadow", *color)?;
        }
        Effect::Cinemagraph { .. } => {
            return Err(EditorError::UnsupportedFrameEffect("cinemagraph"));
        }
    }
    Ok(())
}

fn validate_region(
    effect: &'static str,
    region: PhysicalRect,
    canvas: PhysicalSize,
) -> Result<(), EditorError> {
    if region.size.validate().is_err()
        || region.end_x().is_none()
        || region.end_y().is_none()
        || !region.fits_within(canvas)
    {
        return Err(EditorError::InvalidFrameEffectRegion {
            effect,
            region,
            canvas,
        });
    }
    Ok(())
}

fn validate_percent(effect: &'static str, amount: u8) -> Result<(), EditorError> {
    if amount > 100 {
        return Err(EditorError::InvalidFrameEffectParameter {
            effect,
            parameter: "amount_percent",
            value: u64::from(amount),
            maximum: 100,
        });
    }
    Ok(())
}

fn validate_border(widths: EdgeWidths, canvas: PhysicalSize) -> Result<(), EditorError> {
    let horizontal = u32::from(widths.left) + u32::from(widths.right);
    let vertical = u32::from(widths.top) + u32::from(widths.bottom);
    if horizontal == 0 && vertical == 0
        || horizontal > canvas.width.get()
        || vertical > canvas.height.get()
    {
        return Err(EditorError::InvalidBorderWidths { widths, canvas });
    }
    Ok(())
}

fn validate_visible_color(effect: &'static str, color: Rgba) -> Result<(), EditorError> {
    if color.alpha == 0 {
        return Err(EditorError::InvisibleFrameEffectColor { effect, color });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata,
        ClipTransform, ColorSpace, DurationUs, FrameClip, PhysicalPoint, PhysicalPx, ProjectId,
        ProjectRevision, RasterEncoding, Timeline, UnixTimeMs,
    };

    use super::*;
    use crate::EditorSession;

    const OPAQUE: Rgba = Rgba {
        red: 10,
        green: 20,
        blue: 30,
        alpha: 255,
    };

    fn project() -> ProjectManifest {
        let size = PhysicalSize::new(8, 6).unwrap();
        let asset_id = AssetId::from_digest([7; 32]);
        let mut assets = BTreeMap::new();
        assets.insert(
            asset_id,
            AssetDescriptor {
                id: asset_id,
                byte_len: 192,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        let frames = (1..=2)
            .map(|number| FrameClip {
                id: FrameId::from_u128(number),
                asset_id,
                duration: DurationUs::new(10_000 * u64::try_from(number).unwrap()).unwrap(),
                transform: ClipTransform {
                    flip_horizontal: number == 2,
                    ..ClipTransform::default()
                },
                capture_metadata: CaptureMetadata {
                    cursor_position: Some(PhysicalPoint {
                        x: PhysicalPx::new(u32::try_from(number).unwrap()),
                        y: PhysicalPx::new(1),
                    }),
                    ..CaptureMetadata::default()
                },
                effects: if number == 2 {
                    vec![Effect::Border {
                        widths: EdgeWidths {
                            top: 1,
                            right: 0,
                            bottom: 0,
                            left: 0,
                        },
                        color: OPAQUE,
                    }]
                } else {
                    Vec::new()
                },
            })
            .collect();
        ProjectManifest {
            schema_version: gif_from_screen_domain::CURRENT_SCHEMA_VERSION,
            project_id: ProjectId::from_u128(1),
            revision: ProjectRevision::ZERO,
            app_version: "effect-test".to_owned(),
            created_at: UnixTimeMs::new(1),
            canvas: Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
            timeline: Timeline {
                frames,
                ..Timeline::default()
            },
            assets,
            export_presets: BTreeMap::new(),
            task_runs: Vec::new(),
            source_provenance: Vec::new(),
        }
    }

    fn region() -> PhysicalRect {
        PhysicalRect::new(1, 1, 4, 3).unwrap()
    }

    #[test]
    fn add_is_timeline_ordered_and_preserves_every_non_effect_field() {
        let project = project();
        let before = project.timeline.frames.clone();
        let effect = Effect::Blur {
            region: region(),
            radius: 2,
        };
        let command = edit_frame_effects(
            &project,
            [
                FrameId::from_u128(2),
                FrameId::from_u128(1),
                FrameId::from_u128(2),
            ],
            &FrameEffectEdit::Add(effect.clone()),
        )
        .unwrap();
        let EditCommand::Compound { commands } = command else {
            panic!("expected compound effect command");
        };
        assert_eq!(commands.len(), 2);
        for (command, original) in commands.iter().zip(&before) {
            let EditCommand::ReplaceFrame {
                frame_id,
                replacement,
            } = command
            else {
                panic!("expected ReplaceFrame child");
            };
            assert_eq!(*frame_id, original.id);
            assert_eq!(replacement.asset_id, original.asset_id);
            assert_eq!(replacement.duration, original.duration);
            assert_eq!(replacement.transform, original.transform);
            assert_eq!(replacement.capture_metadata, original.capture_metadata);
            assert_eq!(replacement.effects.last(), Some(&effect));
        }
        assert_eq!(project.timeline.frames, before);
    }

    #[test]
    fn replace_and_clear_roundtrip_through_session_undo_redo() {
        let original = project();
        let before = original.timeline.frames.clone();
        let replacement = Effect::Pixelate {
            region: region(),
            block_size: 2,
        };
        let add = edit_frame_effects(
            &original,
            [FrameId::from_u128(1)],
            &FrameEffectEdit::Add(Effect::Border {
                widths: EdgeWidths {
                    top: 1,
                    right: 1,
                    bottom: 1,
                    left: 1,
                },
                color: OPAQUE,
            }),
        )
        .unwrap();
        let mut session = EditorSession::new(original, 8).unwrap();
        session.execute(&add).unwrap();
        let replace = edit_frame_effects(
            session.project(),
            [FrameId::from_u128(1), FrameId::from_u128(2)],
            &FrameEffectEdit::Replace {
                index: 0,
                effect: replacement.clone(),
            },
        )
        .unwrap();
        session.execute(&replace).unwrap();
        assert_eq!(
            session.project().timeline.frames[0].effects.as_slice(),
            std::slice::from_ref(&replacement)
        );
        assert_eq!(
            session.project().timeline.frames[1].effects.as_slice(),
            std::slice::from_ref(&replacement)
        );

        let clear = edit_frame_effects(
            session.project(),
            [FrameId::from_u128(1), FrameId::from_u128(2)],
            &FrameEffectEdit::Clear,
        )
        .unwrap();
        session.execute(&clear).unwrap();
        assert!(
            session
                .project()
                .timeline
                .frames
                .iter()
                .all(|frame| frame.effects.is_empty())
        );
        assert!(session.undo().unwrap());
        assert_eq!(session.project().timeline.frames[0].effects.len(), 1);
        assert!(session.undo().unwrap());
        assert_eq!(session.project().timeline.frames[0].effects.len(), 1);
        assert_eq!(session.project().timeline.frames[1].effects.len(), 1);
        assert!(session.undo().unwrap());
        assert_eq!(session.project().timeline.frames, before);
        assert!(session.redo().unwrap());
    }

    #[test]
    fn strict_parameter_validation_rejects_every_invalid_family() {
        let project = project();
        let frame = [FrameId::from_u128(1)];
        let outside = PhysicalRect::new(7, 5, 2, 2).unwrap();
        let transparent = Rgba { alpha: 0, ..OPAQUE };
        let invalid = [
            Effect::Blur {
                region: outside,
                radius: 1,
            },
            Effect::Blur {
                region: region(),
                radius: 0,
            },
            Effect::Blur {
                region: region(),
                radius: MAX_FRAME_EFFECT_BLUR_RADIUS + 1,
            },
            Effect::Pixelate {
                region: region(),
                block_size: 0,
            },
            Effect::Darken {
                region: region(),
                amount_percent: 101,
            },
            Effect::Lighten {
                region: region(),
                amount_percent: 101,
            },
            Effect::Border {
                widths: EdgeWidths {
                    top: 0,
                    right: 0,
                    bottom: 0,
                    left: 0,
                },
                color: OPAQUE,
            },
            Effect::Border {
                widths: EdgeWidths {
                    top: 4,
                    right: 0,
                    bottom: 3,
                    left: 0,
                },
                color: OPAQUE,
            },
            Effect::Border {
                widths: EdgeWidths {
                    top: 1,
                    right: 0,
                    bottom: 0,
                    left: 0,
                },
                color: transparent,
            },
            Effect::Shadow {
                offset_x: 0,
                offset_y: 0,
                blur_radius: MAX_FRAME_EFFECT_BLUR_RADIUS + 1,
                color: OPAQUE,
            },
            Effect::Shadow {
                offset_x: i32::MIN,
                offset_y: i32::MAX,
                blur_radius: 0,
                color: transparent,
            },
        ];
        for effect in invalid {
            assert!(edit_frame_effects(&project, frame, &FrameEffectEdit::Add(effect)).is_err());
        }
    }

    #[test]
    fn valid_effect_families_and_boundary_values_build_commands() {
        let project = project();
        let effects = [
            Effect::Blur {
                region: PhysicalRect::new(0, 0, 8, 6).unwrap(),
                radius: MAX_FRAME_EFFECT_BLUR_RADIUS,
            },
            Effect::Pixelate {
                region: region(),
                block_size: u16::MAX,
            },
            Effect::Darken {
                region: region(),
                amount_percent: 0,
            },
            Effect::Lighten {
                region: region(),
                amount_percent: 100,
            },
            Effect::Border {
                widths: EdgeWidths {
                    top: 3,
                    right: 4,
                    bottom: 3,
                    left: 4,
                },
                color: OPAQUE,
            },
            Effect::Shadow {
                offset_x: i32::MIN,
                offset_y: i32::MAX,
                blur_radius: MAX_FRAME_EFFECT_BLUR_RADIUS,
                color: OPAQUE,
            },
        ];
        for effect in effects {
            assert!(
                edit_frame_effects(
                    &project,
                    [FrameId::from_u128(1)],
                    &FrameEffectEdit::Add(effect),
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn replace_index_and_unsupported_effect_fail_before_command_creation() {
        let project = project();
        assert!(matches!(
            edit_frame_effects(
                &project,
                [FrameId::from_u128(1), FrameId::from_u128(2)],
                &FrameEffectEdit::Replace {
                    index: 0,
                    effect: Effect::Pixelate {
                        region: region(),
                        block_size: 2,
                    },
                },
            ),
            Err(EditorError::EffectIndexOutOfBounds { frame_id, .. })
                if frame_id == FrameId::from_u128(1)
        ));
        assert!(matches!(
            edit_frame_effects(
                &project,
                [FrameId::from_u128(1)],
                &FrameEffectEdit::Add(Effect::Cinemagraph {
                    mask_asset: AssetId::from_digest([9; 32]),
                    invert_mask: false,
                }),
            ),
            Err(EditorError::UnsupportedFrameEffect("cinemagraph"))
        ));
    }
}
