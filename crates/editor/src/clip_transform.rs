use std::collections::BTreeSet;

use gif_from_screen_domain::{
    ClipTransform, EditCommand, FrameClip, FrameId, PhysicalRect, PhysicalSize, ProjectManifest,
    QuarterTurn,
};

use crate::{EditorError, ensure_known_selection};

/// One batch update applied independently to every selected clip transform.
///
/// Transform order follows the renderer's persisted semantics: crop in immutable source-asset
/// coordinates, resize to `output_size`, clockwise rotation, then horizontal and vertical flips.
/// A 90° or 270° rotation therefore swaps the final rendered width and height; rotation edits do
/// not implicitly rewrite `output_size` or the project canvas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipTransformEdit {
    /// Set a crop rectangle expressed in each source asset's original pixel coordinates.
    SetCrop(PhysicalRect),
    /// Remove the crop and use the complete source asset.
    ClearCrop,
    /// Set the resize dimensions applied before rotation.
    SetOutputSize(PhysicalSize),
    /// Remove explicit resizing.
    ClearOutputSize,
    /// Set an absolute clockwise orientation.
    SetRotation(QuarterTurn),
    /// Rotate 90° clockwise relative to each clip's current orientation.
    RotateClockwise,
    /// Rotate 90° counterclockwise relative to each clip's current orientation.
    RotateCounterclockwise,
    /// Set horizontal flipping to an absolute value.
    SetHorizontalFlip(bool),
    /// Toggle horizontal flipping independently for every selected clip.
    ToggleHorizontalFlip,
    /// Set vertical flipping to an absolute value.
    SetVerticalFlip(bool),
    /// Toggle vertical flipping independently for every selected clip.
    ToggleVerticalFlip,
}

/// Builds one atomic compound command that updates selected clip transforms.
///
/// Selection input order and duplicates are ignored. Selected frames are resolved in timeline
/// order, cloned, and emitted as [`EditCommand::ReplaceFrame`] children of one
/// [`EditCommand::Compound`]. Only [`FrameClip::transform`] changes; asset identity, duration,
/// capture metadata, effects, and all other clip state are preserved.
///
/// Crop coordinates always refer to the immutable source frame asset, not a prior crop, resize,
/// rotation, or the project canvas. `output_size` is the pre-rotation size: 90° and 270° rotations
/// swap the final rendered dimensions without changing the global canvas.
///
/// # Errors
///
/// Returns [`EditorError`] when selection is empty or unknown, a crop is empty/overflowing/outside
/// any selected source asset, a selected source asset is missing or is not a frame raster, or an
/// output size is invalid. Validation completes for the entire selection before a command is
/// returned, so failure cannot yield a partially applicable command.
pub fn edit_clip_transforms(
    project: &ProjectManifest,
    frame_ids: impl IntoIterator<Item = FrameId>,
    edit: ClipTransformEdit,
) -> Result<EditCommand, EditorError> {
    let selected: BTreeSet<_> = frame_ids.into_iter().collect();
    ensure_known_selection(project, &selected)?;
    validate_edit_geometry(edit)?;

    let selected_frames = project
        .timeline
        .frames
        .iter()
        .filter(|frame| selected.contains(&frame.id))
        .collect::<Vec<_>>();
    if let ClipTransformEdit::SetCrop(crop) = edit {
        for frame in &selected_frames {
            validate_crop_for_frame(project, frame, crop)?;
        }
    }

    let commands = selected_frames
        .into_iter()
        .map(|frame| {
            let mut replacement = frame.clone();
            apply_transform_edit(&mut replacement.transform, edit);
            EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(replacement),
            }
        })
        .collect();
    Ok(EditCommand::Compound { commands })
}

fn validate_edit_geometry(edit: ClipTransformEdit) -> Result<(), EditorError> {
    match edit {
        ClipTransformEdit::SetCrop(crop)
            if crop.size.validate().is_err()
                || crop.end_x().is_none()
                || crop.end_y().is_none() =>
        {
            Err(EditorError::InvalidCrop(crop))
        }
        ClipTransformEdit::SetOutputSize(size) if size.validate().is_err() => {
            Err(EditorError::InvalidOutputSize(size))
        }
        _ => Ok(()),
    }
}

fn validate_crop_for_frame(
    project: &ProjectManifest,
    frame: &FrameClip,
    crop: PhysicalRect,
) -> Result<(), EditorError> {
    let asset = project
        .assets
        .get(&frame.asset_id)
        .ok_or(EditorError::MissingFrameAsset {
            frame_id: frame.id,
            asset_id: frame.asset_id,
        })?;
    let Some((size, _)) = asset.kind.raster_descriptor() else {
        return Err(EditorError::UnsupportedFrameAsset {
            frame_id: frame.id,
            asset_id: frame.asset_id,
        });
    };
    if !crop.fits_within(size) {
        return Err(EditorError::CropOutsideFrameAsset {
            frame_id: frame.id,
            asset_id: frame.asset_id,
            crop,
            source_size: size,
        });
    }
    Ok(())
}

fn apply_transform_edit(transform: &mut ClipTransform, edit: ClipTransformEdit) {
    match edit {
        ClipTransformEdit::SetCrop(crop) => transform.crop = Some(crop),
        ClipTransformEdit::ClearCrop => transform.crop = None,
        ClipTransformEdit::SetOutputSize(size) => transform.output_size = Some(size),
        ClipTransformEdit::ClearOutputSize => transform.output_size = None,
        ClipTransformEdit::SetRotation(rotation) => transform.rotation = rotation,
        ClipTransformEdit::RotateClockwise => {
            transform.rotation = clockwise(transform.rotation);
        }
        ClipTransformEdit::RotateCounterclockwise => {
            transform.rotation = counterclockwise(transform.rotation);
        }
        ClipTransformEdit::SetHorizontalFlip(flipped) => transform.flip_horizontal = flipped,
        ClipTransformEdit::ToggleHorizontalFlip => {
            transform.flip_horizontal = !transform.flip_horizontal;
        }
        ClipTransformEdit::SetVerticalFlip(flipped) => transform.flip_vertical = flipped,
        ClipTransformEdit::ToggleVerticalFlip => {
            transform.flip_vertical = !transform.flip_vertical;
        }
    }
}

const fn clockwise(rotation: QuarterTurn) -> QuarterTurn {
    match rotation {
        QuarterTurn::Zero => QuarterTurn::Clockwise90,
        QuarterTurn::Clockwise90 => QuarterTurn::Clockwise180,
        QuarterTurn::Clockwise180 => QuarterTurn::Clockwise270,
        QuarterTurn::Clockwise270 => QuarterTurn::Zero,
    }
}

const fn counterclockwise(rotation: QuarterTurn) -> QuarterTurn {
    match rotation {
        QuarterTurn::Zero => QuarterTurn::Clockwise270,
        QuarterTurn::Clockwise90 => QuarterTurn::Zero,
        QuarterTurn::Clockwise180 => QuarterTurn::Clockwise90,
        QuarterTurn::Clockwise270 => QuarterTurn::Clockwise180,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gif_from_screen_domain::{
        AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ColorSpace,
        DurationUs, Effect, MouseButton, PhysicalPoint, PhysicalPx, ProjectId, ProjectRevision,
        RasterEncoding, Rgba, UnixTimeMs,
    };

    use super::*;
    use crate::EditorSession;

    fn asset_id(value: u8) -> AssetId {
        AssetId::from_digest([value; 32])
    }

    fn frame(id: u128, asset: AssetId, transform: ClipTransform) -> FrameClip {
        FrameClip {
            render_steps: Vec::new(),
            capture_clock: None,
            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
            id: FrameId::from_u128(id),
            asset_id: asset,
            duration: DurationUs::new(10_000 + u64::try_from(id).unwrap()).unwrap(),
            transform,
            capture_metadata: CaptureMetadata {
                cursor_position: Some(PhysicalPoint {
                    x: PhysicalPx::new(1),
                    y: PhysicalPx::new(2),
                }),
                pressed_mouse_buttons: vec![MouseButton::Left],
                dropped_frames_before: u32::try_from(id).unwrap(),
                ..CaptureMetadata::default()
            },
            effects: vec![Effect::Border {
                widths: gif_from_screen_domain::EdgeWidths {
                    top: 1,
                    right: 2,
                    bottom: 3,
                    left: 4,
                },
                color: Rgba {
                    red: 1,
                    green: 2,
                    blue: 3,
                    alpha: 4,
                },
            }],
        }
    }

    fn project() -> ProjectManifest {
        let first_asset = asset_id(1);
        let second_asset = asset_id(2);
        let mut assets = BTreeMap::new();
        for (id, size) in [
            (first_asset, PhysicalSize::new(4, 3).unwrap()),
            (second_asset, PhysicalSize::new(2, 2).unwrap()),
        ] {
            assets.insert(
                id,
                AssetDescriptor {
                    id,
                    byte_len: u64::from(size.width.get()) * u64::from(size.height.get()) * 4,
                    kind: AssetKind::Frame {
                        size,
                        encoding: RasterEncoding::Rgba8,
                    },
                },
            );
        }
        ProjectManifest {
            schema_version: gif_from_screen_domain::CURRENT_SCHEMA_VERSION,
            project_id: ProjectId::from_u128(1),
            revision: ProjectRevision::ZERO,
            app_version: "transform-test".to_owned(),
            created_at: UnixTimeMs::new(1),
            canvas: Canvas {
                size: PhysicalSize::new(8, 6).unwrap(),
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
            timeline: gif_from_screen_domain::Timeline {
                frames: vec![
                    frame(1, first_asset, ClipTransform::default()),
                    frame(
                        2,
                        second_asset,
                        ClipTransform {
                            output_size: Some(PhysicalSize::new(5, 3).unwrap()),
                            rotation: QuarterTurn::Clockwise90,
                            flip_horizontal: true,
                            ..ClipTransform::default()
                        },
                    ),
                    frame(3, first_asset, ClipTransform::default()),
                ],
                ..gif_from_screen_domain::Timeline::default()
            },
            assets,
            export_presets: BTreeMap::new(),
            task_runs: Vec::new(),
            source_provenance: Vec::new(),
        }
    }

    fn replacement_ids(command: &EditCommand) -> Vec<FrameId> {
        let EditCommand::Compound { commands } = command else {
            panic!("expected compound transform command");
        };
        commands
            .iter()
            .map(|command| match command {
                EditCommand::ReplaceFrame { frame_id, .. } => *frame_id,
                _ => panic!("expected only ReplaceFrame children"),
            })
            .collect()
    }

    #[test]
    fn multi_selection_is_deduplicated_and_resolved_in_timeline_order() {
        let project = project();
        let command = edit_clip_transforms(
            &project,
            [
                FrameId::from_u128(3),
                FrameId::from_u128(1),
                FrameId::from_u128(3),
            ],
            ClipTransformEdit::SetCrop(PhysicalRect::new(1, 1, 1, 1).unwrap()),
        )
        .unwrap();

        assert_eq!(
            replacement_ids(&command),
            [FrameId::from_u128(1), FrameId::from_u128(3)]
        );
        assert_eq!(
            project.timeline.frames[0].transform,
            ClipTransform::default()
        );
    }

    #[test]
    fn replacement_changes_only_transform_and_roundtrips_through_undo_redo() {
        let original = project();
        let original_frames = original.timeline.frames.clone();
        let original_canvas = original.canvas.clone();
        let command = edit_clip_transforms(
            &original,
            [FrameId::from_u128(2), FrameId::from_u128(1)],
            ClipTransformEdit::ToggleVerticalFlip,
        )
        .unwrap();
        let mut session = EditorSession::new(original, 4).unwrap();

        session.execute(&command).unwrap();
        for (changed, before) in session
            .project()
            .timeline
            .frames
            .iter()
            .take(2)
            .zip(&original_frames)
        {
            assert_eq!(changed.id, before.id);
            assert_eq!(changed.asset_id, before.asset_id);
            assert_eq!(changed.duration, before.duration);
            assert_eq!(changed.capture_metadata, before.capture_metadata);
            assert_eq!(changed.effects, before.effects);
            assert_eq!(
                changed.transform.flip_vertical,
                !before.transform.flip_vertical
            );
        }
        assert_eq!(session.project().canvas, original_canvas);
        assert!(session.undo().unwrap());
        assert_eq!(session.project().timeline.frames, original_frames);
        assert!(session.redo().unwrap());
        assert!(session.project().timeline.frames[0].transform.flip_vertical);
        assert!(session.project().timeline.frames[1].transform.flip_vertical);
    }

    #[test]
    fn crop_accepts_exact_source_edge_and_rejects_all_invalid_geometry_atomically() {
        let project = project();
        let edge = PhysicalRect::new(0, 0, 2, 2).unwrap();
        assert!(
            edit_clip_transforms(
                &project,
                [FrameId::from_u128(2)],
                ClipTransformEdit::SetCrop(edge),
            )
            .is_ok()
        );

        let before = project.clone();
        let outside = PhysicalRect::new(1, 1, 2, 2).unwrap();
        assert!(matches!(
            edit_clip_transforms(
                &project,
                [FrameId::from_u128(1), FrameId::from_u128(2)],
                ClipTransformEdit::SetCrop(outside),
            ),
            Err(EditorError::CropOutsideFrameAsset { frame_id, .. })
                if frame_id == FrameId::from_u128(2)
        ));
        let empty = PhysicalRect {
            origin: PhysicalPoint::default(),
            size: PhysicalSize {
                width: PhysicalPx::ZERO,
                height: PhysicalPx::new(1),
            },
        };
        assert!(matches!(
            edit_clip_transforms(
                &project,
                [FrameId::from_u128(1)],
                ClipTransformEdit::SetCrop(empty),
            ),
            Err(EditorError::InvalidCrop(found)) if found == empty
        ));
        let overflowing = PhysicalRect {
            origin: PhysicalPoint {
                x: PhysicalPx::new(u32::MAX),
                y: PhysicalPx::ZERO,
            },
            size: PhysicalSize::new(1, 1).unwrap(),
        };
        assert!(matches!(
            edit_clip_transforms(
                &project,
                [FrameId::from_u128(1)],
                ClipTransformEdit::SetCrop(overflowing),
            ),
            Err(EditorError::InvalidCrop(found)) if found == overflowing
        ));
        assert_eq!(project, before);
    }

    #[test]
    fn output_size_and_selection_errors_are_typed() {
        let project = project();
        let invalid_size = PhysicalSize {
            width: PhysicalPx::new(1),
            height: PhysicalPx::ZERO,
        };
        assert!(matches!(
            edit_clip_transforms(
                &project,
                [FrameId::from_u128(1)],
                ClipTransformEdit::SetOutputSize(invalid_size),
            ),
            Err(EditorError::InvalidOutputSize(found)) if found == invalid_size
        ));
        assert!(matches!(
            edit_clip_transforms(&project, [], ClipTransformEdit::ClearOutputSize),
            Err(EditorError::EmptySelection)
        ));
        assert!(matches!(
            edit_clip_transforms(
                &project,
                [FrameId::from_u128(99)],
                ClipTransformEdit::ClearCrop,
            ),
            Err(EditorError::UnknownSelectedFrame(frame_id))
                if frame_id == FrameId::from_u128(99)
        ));
    }

    #[test]
    fn crop_rejects_missing_and_non_raster_assets() {
        let crop = PhysicalRect::new(0, 0, 1, 1).unwrap();
        let mut missing = project();
        let missing_asset = missing.timeline.frames[0].asset_id;
        missing.assets.remove(&missing_asset);
        assert!(matches!(
            edit_clip_transforms(
                &missing,
                [FrameId::from_u128(1)],
                ClipTransformEdit::SetCrop(crop),
            ),
            Err(EditorError::MissingFrameAsset { asset_id, .. }) if asset_id == missing_asset
        ));

        let mut unsupported = project();
        let unsupported_asset = unsupported.timeline.frames[0].asset_id;
        unsupported.assets.get_mut(&unsupported_asset).unwrap().kind = AssetKind::ImportedSource {
            media_type: "image/png".to_owned(),
        };
        assert!(matches!(
            edit_clip_transforms(
                &unsupported,
                [FrameId::from_u128(1)],
                ClipTransformEdit::SetCrop(crop),
            ),
            Err(EditorError::UnsupportedFrameAsset { asset_id, .. })
                if asset_id == unsupported_asset
        ));
    }

    #[test]
    fn frame_crop_and_clipboard_reuse_overlay_and_mask_asset_roles() {
        let mut project = project();
        let first_id = project.timeline.frames[0].id;
        for (index, asset) in project.assets.values_mut().enumerate() {
            let (size, encoding) = asset.kind.raster_descriptor().unwrap();
            asset.kind = if index == 0 {
                AssetKind::OverlayImage { size, encoding }
            } else {
                AssetKind::Mask { size, encoding }
            };
        }
        let command = edit_clip_transforms(
            &project,
            project.timeline.frames.iter().map(|frame| frame.id),
            ClipTransformEdit::SetCrop(PhysicalRect::new(0, 0, 1, 1).unwrap()),
        )
        .unwrap();
        project.apply_command(&command).unwrap();
        let clipboard = crate::copy_selected_frames(&project, [first_id]).unwrap();
        let paste = crate::paste_frame_clipboard(&project, &clipboard, Some(first_id), || {
            FrameId::from_u128(99)
        })
        .unwrap();
        project.apply_command(&paste).unwrap();
        assert_eq!(
            project.timeline.frames[0].asset_id,
            project.timeline.frames[1].asset_id
        );
        assert_eq!(project.assets.len(), 2);
        assert!(project.assets.values().all(|asset| !asset.kind.is_frame()));
        project.validate().unwrap();
    }

    #[test]
    fn clear_set_rotate_and_flip_variants_have_explicit_independent_semantics() {
        let mut project = project();
        let frame_id = FrameId::from_u128(2);
        let edits = [
            ClipTransformEdit::ClearOutputSize,
            ClipTransformEdit::SetOutputSize(PhysicalSize::new(7, 5).unwrap()),
            ClipTransformEdit::SetCrop(PhysicalRect::new(0, 0, 2, 2).unwrap()),
            ClipTransformEdit::ClearCrop,
            ClipTransformEdit::RotateClockwise,
            ClipTransformEdit::RotateCounterclockwise,
            ClipTransformEdit::SetRotation(QuarterTurn::Clockwise180),
            ClipTransformEdit::ToggleHorizontalFlip,
            ClipTransformEdit::SetHorizontalFlip(false),
            ClipTransformEdit::ToggleVerticalFlip,
            ClipTransformEdit::SetVerticalFlip(false),
        ];
        for edit in edits {
            let command = edit_clip_transforms(&project, [frame_id], edit).unwrap();
            project.apply_command(&command).unwrap();
        }

        let transform = project.timeline.frames[1].transform;
        assert_eq!(transform.crop, None);
        assert_eq!(
            transform.output_size,
            Some(PhysicalSize::new(7, 5).unwrap())
        );
        assert_eq!(transform.rotation, QuarterTurn::Clockwise180);
        assert!(!transform.flip_horizontal);
        assert!(!transform.flip_vertical);
        assert_eq!(project.canvas.size, PhysicalSize::new(8, 6).unwrap());
    }

    #[test]
    fn domain_compound_inverse_restores_each_distinct_original_transform() {
        let mut project = project();
        let before = project.timeline.frames.clone();
        let command = edit_clip_transforms(
            &project,
            [FrameId::from_u128(1), FrameId::from_u128(2)],
            ClipTransformEdit::SetRotation(QuarterTurn::Clockwise270),
        )
        .unwrap();

        let applied = project.apply_command(&command).unwrap();
        assert!(matches!(applied.inverse, EditCommand::Compound { .. }));
        let redo = project.apply_command(&applied.inverse).unwrap().inverse;
        assert_eq!(project.timeline.frames, before);
        project.apply_command(&redo).unwrap();
        assert_eq!(
            project.timeline.frames[0].transform.rotation,
            QuarterTurn::Clockwise270
        );
        assert_eq!(
            project.timeline.frames[1].transform.rotation,
            QuarterTurn::Clockwise270
        );
    }
}
