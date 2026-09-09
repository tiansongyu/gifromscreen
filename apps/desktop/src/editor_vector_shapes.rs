//! Atomic, bounded vector group authoring. Drafts do not own a mutable project.

use gif_from_screen_domain::{
    BlendMode, EditCommand, FrameId, FrameOverlayMark, MAX_FRAME_OVERLAY_CELLS,
    MAX_FRAME_OVERLAY_MARKS, OverlayContent, OverlayId, OverlayTrack, PhysicalSize, TrackId,
    VectorShape, selected_frame_cells,
};
use gif_from_screen_editor::{MAX_FRAME_BUNDLE_METADATA_BYTES, author_vector_shape_track};
use gif_from_screen_render::MAX_VECTOR_PREVIEW_SHAPES;
use uuid::Uuid;

use super::{EditorWorkspace, EditorWorkspaceError, OverlaySelectionAnchor};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VectorShapeRequest {
    pub(crate) anchor: OverlaySelectionAnchor,
    pub(crate) reference_frame: FrameId,
    pub(crate) canvas_size: PhysicalSize,
    pub(crate) shapes: Vec<VectorShape>,
}

impl EditorWorkspace {
    /// Prepare every mark and independent paint stage before a single journal edit.
    /// Shape order is the explicit draft paint order; selection gaps stay gaps.
    pub(crate) fn apply_vector_shapes(
        &mut self,
        request: &VectorShapeRequest,
    ) -> Result<TrackId, EditorWorkspaceError> {
        self.validate_vector_request(request)?;
        let contents: Vec<_> = request
            .shapes
            .iter()
            .map(|shape| OverlayContent::VectorShape { shape: *shape })
            .collect();
        validate_group_metadata(&contents, self.selection().len())?;
        // This helper checks the complete selection before invoking its factory,
        // and preserves noncontiguous ownership as distinct authoring runs.
        let mut cells = selected_frame_cells(
            &self.manifest().timeline,
            self.selection().selected(),
            |_| mark(contents[0].clone(), 0),
        )
        .map_err(preparation)?;
        for cell in &mut cells {
            cell.marks.extend(
                contents
                    .iter()
                    .enumerate()
                    .skip(1)
                    .map(|(index, content)| mark(content.clone(), index)),
            );
        }
        let id = TrackId::from_u128(Uuid::new_v4().as_u128());
        let track = OverlayTrack {
            id,
            name: "Shapes".to_owned(),
            visible: true,
            opacity: 255,
            blend_mode: BlendMode::Normal,
            annotation: None,
            annotation_scope: None,
            frame_cells: Some(cells),
            items: Vec::new(),
        };
        let commands = author_vector_shape_track(self.manifest(), track)?;
        self.execute(EditCommand::Compound { commands })?;
        Ok(id)
    }

    fn validate_vector_request(
        &self,
        request: &VectorShapeRequest,
    ) -> Result<(), EditorWorkspaceError> {
        if !request.anchor.matches(self) {
            return Err(preparation(
                "The shape draft's project, revision or target selection changed.",
            ));
        }
        if request.shapes.is_empty() || request.shapes.len() > MAX_VECTOR_PREVIEW_SHAPES {
            return Err(preparation(format!(
                "Apply between 1 and {MAX_VECTOR_PREVIEW_SHAPES} shapes; no shapes were truncated."
            )));
        }
        let selected = self.selected_frame_ids()?;
        if selected.len() > MAX_FRAME_OVERLAY_CELLS
            || selected
                .len()
                .checked_mul(request.shapes.len())
                .is_none_or(|marks| marks > MAX_FRAME_OVERLAY_MARKS)
        {
            return Err(preparation(
                "The complete shape group exceeds the frame or mark authoring limit.",
            ));
        }
        let reference = self
            .manifest()
            .timeline
            .frames
            .iter()
            .find(|frame| {
                frame.id == request.reference_frame && self.selection().contains(frame.id)
            })
            .ok_or_else(|| {
                preparation("The shape reference frame is no longer a selected target.")
            })?;
        let actual =
            crate::annotation_engine::authoring_stage_size(self.manifest(), reference, None)
                .map_err(preparation)?;
        if actual != request.canvas_size {
            return Err(preparation(
                "The shape reference paint-space dimensions changed.",
            ));
        }
        for shape in &request.shapes {
            shape.validate().map_err(preparation)?;
        }
        Ok(())
    }
}

fn mark(content: OverlayContent, index: usize) -> FrameOverlayMark {
    FrameOverlayMark {
        id: OverlayId::from_u128(Uuid::new_v4().as_u128()),
        z_index: i32::try_from(index).expect("validated bounded vector group"),
        content,
    }
}

fn validate_group_metadata(
    contents: &[OverlayContent],
    frames: usize,
) -> Result<(), EditorWorkspaceError> {
    let content_bytes = super::overlay_authoring::json_metadata_bytes(&contents)?;
    // Include every repeated value before cloning cells. Reserve explicit mark
    // IDs/order/content wrappers and whole-frame owner/scope metadata. The shared
    // author then measures the actual full Compound including previous frame data.
    let required = contents
        .len()
        .checked_mul(128)
        .and_then(|overhead| content_bytes.checked_add(overhead))
        .and_then(|per_frame| per_frame.checked_add(512))
        .and_then(|per_frame| per_frame.checked_mul(frames));
    if required.is_none_or(|bytes| bytes > MAX_FRAME_BUNDLE_METADATA_BYTES) {
        return Err(preparation(
            "The complete shape authoring command exceeds the 16 MiB metadata budget.",
        ));
    }
    Ok(())
}

fn preparation(reason: impl Into<String>) -> EditorWorkspaceError {
    EditorWorkspaceError::FrameOverlayPreparation(reason.into())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
    };

    use super::*;
    use gif_from_screen_domain::{
        AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureBinding, CaptureClockContext,
        CaptureClockId, CaptureMetadata, ClipTransform, ColorSpace, CompositePrecision, DurationUs,
        FrameClip, FrameOverlayCell, FrameRenderStep, MAX_FRAME_RENDER_STEPS, ProjectId,
        ProjectManifest, RasterEncoding, Rgba, TimeUs, UnixTimeMs, VECTOR_SHAPE_VERSION,
        VectorShapeBounds, WPF_VECTOR_SHAPE_VERSION,
    };
    use gif_from_screen_project::{ActiveProject, LockPolicy};

    fn frame(number: u128) -> FrameId {
        FrameId::from_u128(number)
    }

    fn workspace(directory: &tempfile::TempDir) -> EditorWorkspace {
        let size = PhysicalSize::new(8, 6).unwrap();
        let mut manifest = ProjectManifest::new(
            ProjectId::from_u128(17),
            "vector-author-test",
            UnixTimeMs::new(1),
            Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        manifest.schema_version = 8;
        let mut active = ActiveProject::create(directory.path(), manifest).unwrap();
        let bytes = [20, 40, 80, 255].repeat(48);
        let asset_id = active.assets().put(&bytes).unwrap();
        let frames = (1..=3_u128)
            .map(|number| {
                let sampled_at = TimeUs::new(u64::try_from(number).unwrap() * 1000);
                FrameClip {
                    id: frame(number),
                    asset_id,
                    duration: DurationUs::new(100_000).unwrap(),
                    transform: ClipTransform::default(),
                    effects: Vec::new(),
                    render_steps: Vec::new(),
                    capture_metadata: CaptureMetadata {
                        captured_at: Some(sampled_at),
                        ..CaptureMetadata::default()
                    },
                    capture_clock: Some(CaptureClockContext {
                        id: Some(CaptureClockId::from_u128(77)),
                        sampled_at,
                    }),
                    capture_binding: CaptureBinding::Original,
                }
            })
            .collect();
        active
            .commit(EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: AssetDescriptor {
                            id: asset_id,
                            byte_len: 192,
                            kind: AssetKind::Frame {
                                size,
                                encoding: RasterEncoding::Rgba8,
                            },
                        },
                    },
                    EditCommand::InsertFrames { index: 0, frames },
                ],
            })
            .unwrap();
        active.checkpoint_and_compact().unwrap();
        let mut workspace = EditorWorkspace::from_active(active, 32).unwrap();
        workspace.select_only(frame(1)).unwrap();
        workspace
    }

    fn request(workspace: &EditorWorkspace, version: u8) -> VectorShapeRequest {
        let reference_frame = *workspace.selection().selected().iter().next().unwrap();
        let reference = workspace
            .manifest()
            .timeline
            .frames
            .iter()
            .find(|f| f.id == reference_frame)
            .unwrap();
        VectorShapeRequest {
            anchor: workspace.overlay_selection_anchor().unwrap(),
            reference_frame,
            canvas_size: crate::annotation_engine::authoring_stage_size(
                workspace.manifest(),
                reference,
                None,
            )
            .unwrap(),
            shapes: vec![VectorShape {
                version,
                bounds: VectorShapeBounds {
                    x_hundredths: 100,
                    y_hundredths: 100,
                    width_hundredths: 300,
                    height_hundredths: 200,
                },
                stroke_width_hundredths: 0,
                fill: Some(Rgba {
                    red: 0,
                    green: 255,
                    blue: 0,
                    alpha: 255,
                }),
                ..VectorShape::default()
            }],
        }
    }

    fn image(workspace: &EditorWorkspace, id: FrameId) -> gif_from_screen_render::RgbaSurface {
        crate::editor_preview::render_frame_surface(workspace.active_project(), id, 1024 * 1024)
            .unwrap()
    }

    fn disk_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn read(root: &Path, directory: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    read(root, &entry.path(), result);
                } else {
                    assert!(kind.is_file());
                    result.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut result = BTreeMap::new();
        read(root, root, &mut result);
        result
    }

    fn assert_selected_v2(workspace: &EditorWorkspace, id: TrackId, stage: u32) {
        assert_eq!(workspace.manifest().schema_version, 9);
        let track = workspace
            .manifest()
            .timeline
            .overlay_tracks
            .iter()
            .find(|track| track.id == id)
            .unwrap();
        let cells = track.frame_cells.as_ref().unwrap();
        assert_eq!(
            cells.iter().map(|cell| cell.frame_id).collect::<Vec<_>>(),
            [frame(1), frame(3)]
        );
        assert_eq!(
            cells
                .iter()
                .map(|cell| cell.scopes[0].run_id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(cells.iter().all(|cell| cell.stage == Some(stage)));
        assert!(track.all_mark_contents().all(|(_,content)|matches!(content,OverlayContent::VectorShape{shape} if shape.version==WPF_VECTOR_SHAPE_VERSION)));
        for position in [0, 2] {
            assert_eq!(
                workspace.manifest().timeline.frames[position]
                    .render_steps
                    .last(),
                Some(&FrameRenderStep::Composite {
                    stage_id: stage,
                    precision: CompositePrecision::VectorCanvasPbgra8PngV2
                })
            );
        }
    }

    #[test]
    fn v2_apply_selected_1_and_3_upgrades_once_undo_redo_and_real_journal_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory);
        workspace.toggle_selection(frame(3)).unwrap();
        let before = workspace.manifest().clone();
        let untouched = image(&workspace, frame(2));
        let request = request(&workspace, WPF_VECTOR_SHAPE_VERSION);
        let id = workspace.apply_vector_shapes(&request).unwrap();
        assert_selected_v2(&workspace, id, 2);
        assert_eq!(
            workspace.manifest().revision,
            before.revision.next().unwrap()
        );
        assert_eq!(
            workspace.manifest().timeline.frames[1],
            before.timeline.frames[1]
        );
        for index in [0, 2] {
            let mut frame = workspace.manifest().timeline.frames[index].clone();
            assert_eq!(frame.render_steps.len(), 2);
            frame.render_steps.clear();
            assert_eq!(frame, before.timeline.frames[index]);
        }
        let track = workspace.manifest().timeline.overlay_tracks[0].clone();
        let painted = image(&workspace, frame(1));
        assert_ne!(painted, untouched);
        assert_eq!(image(&workspace, frame(2)), untouched);
        let snapshot: ProjectManifest =
            serde_json::from_slice(&std::fs::read(directory.path().join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(snapshot.schema_version, 9);
        assert_eq!(snapshot.revision, before.revision);
        assert!(snapshot.timeline.overlay_tracks.is_empty());
        assert!(workspace.undo().unwrap());
        assert!(!workspace.can_undo());
        assert_eq!(workspace.manifest().timeline, before.timeline);
        assert_eq!(workspace.manifest().schema_version, 9);
        assert!(workspace.redo().unwrap());
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0], track);
        let expected = workspace.manifest().clone();
        drop(workspace);
        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 32).unwrap();
        assert_eq!(reopened.manifest(), &expected);
        assert_eq!(reopened.journal_recovery().unwrap().replayed_records, 3);
        assert!(reopened.journal_recovery().unwrap().is_clean());
        assert!(reopened.asset_issues().is_empty());
        assert_eq!(image(&reopened, frame(1)), painted);
        assert_eq!(image(&reopened, frame(2)), untouched);
        assert_eq!(image(&reopened, frame(3)), painted);
    }

    #[test]
    fn v1_resize_then_v2_keeps_previous_stages_and_marks_in_order() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory);
        workspace.toggle_selection(frame(3)).unwrap();
        let old_id = workspace
            .apply_vector_shapes(&request(&workspace, VECTOR_SHAPE_VERSION))
            .unwrap();
        let old_track = workspace.manifest().timeline.overlay_tracks[0].clone();
        workspace
            .set_selection_output_size(PhysicalSize::new(16, 12).unwrap())
            .unwrap();
        let before = workspace.manifest().clone();
        let id = workspace
            .apply_vector_shapes(&request(&workspace, WPF_VECTOR_SHAPE_VERSION))
            .unwrap();
        assert_selected_v2(&workspace, id, 3);
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0], old_track);
        assert_eq!(workspace.manifest().timeline.overlay_tracks[0].id, old_id);
        for index in [0, 2] {
            let steps = &workspace.manifest().timeline.frames[index].render_steps;
            assert_eq!(
                &steps[..steps.len() - 1],
                before.timeline.frames[index].render_steps
            );
            assert!(matches!(
                steps[1],
                FrameRenderStep::Composite {
                    precision: CompositePrecision::VectorCanvasPbgra8PngV1,
                    ..
                }
            ));
            assert!(matches!(steps[2], FrameRenderStep::Resize { .. }));
        }
        assert_eq!(
            workspace.manifest().timeline.frames[1],
            before.timeline.frames[1]
        );
        assert!(workspace.undo().unwrap());
        assert_eq!(workspace.manifest().timeline, before.timeline);
        assert!(workspace.redo().unwrap());
        let expected = workspace.manifest().clone();
        drop(workspace);
        let reopened =
            EditorWorkspace::open(directory.path(), LockPolicy::FailIfPresent, 32).unwrap();
        assert_eq!(reopened.manifest(), &expected);
        assert_selected_v2(&reopened, id, 3);
        assert_eq!(
            image(&reopened, frame(1)).size(),
            PhysicalSize::new(16, 12).unwrap()
        );
    }

    #[test]
    fn invalid_mixed_and_stale_requests_do_not_write_any_project_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory);
        let valid = request(&workspace, WPF_VECTOR_SHAPE_VERSION);
        let mut mixed = valid.clone();
        mixed.shapes.push(VectorShape {
            version: VECTOR_SHAPE_VERSION,
            ..valid.shapes[0]
        });
        let mut wrong_frame = valid.clone();
        wrong_frame.reference_frame = frame(99);
        let mut wrong_size = valid.clone();
        wrong_size.canvas_size = PhysicalSize::new(9, 6).unwrap();
        let before = workspace.manifest().clone();
        let disk = disk_bytes(directory.path());
        for invalid in [mixed, wrong_frame, wrong_size] {
            assert!(workspace.apply_vector_shapes(&invalid).is_err());
            assert_eq!(workspace.manifest(), &before);
            assert_eq!(disk_bytes(directory.path()), disk);
        }
        workspace.select_only(frame(2)).unwrap();
        assert!(workspace.apply_vector_shapes(&valid).is_err());
        assert_eq!(disk_bytes(directory.path()), disk);
        assert_eq!(workspace.manifest(), &before);
    }

    fn invalid_direct_track(stage: Option<u32>) -> OverlayTrack {
        let mut cell = FrameOverlayCell::whole(
            frame(1),
            1,
            vec![FrameOverlayMark {
                id: OverlayId::from_u128(9001),
                z_index: 0,
                content: OverlayContent::VectorShape {
                    shape: VectorShape::wpf_v2(),
                },
            }],
        );
        cell.stage = stage;
        OverlayTrack {
            id: TrackId::from_u128(900),
            name: "invalid direct V2".into(),
            visible: false,
            opacity: 0,
            blend_mode: BlendMode::Normal,
            annotation: None,
            annotation_scope: None,
            frame_cells: Some(vec![cell]),
            items: Vec::new(),
        }
    }

    #[test]
    fn missing_explicit_stage_and_exhausted_stage_program_fail_before_schema_stamp() {
        let directory = tempfile::tempdir().unwrap();
        let mut workspace = workspace(&directory);
        let before = workspace.manifest().clone();
        let disk = disk_bytes(directory.path());
        for stage in [None, Some(99)] {
            assert!(
                workspace
                    .execute(EditCommand::UpsertOverlayTrack {
                        track: invalid_direct_track(stage)
                    })
                    .is_err()
            );
            assert_eq!(workspace.manifest(), &before);
            assert_eq!(disk_bytes(directory.path()), disk);
        }
        let mut replacement = workspace.manifest().timeline.frames[0].clone();
        replacement.render_steps = vec![FrameRenderStep::composite(1)];
        replacement.render_steps.extend(std::iter::repeat_n(
            FrameRenderStep::FlipHorizontal,
            MAX_FRAME_RENDER_STEPS - 1,
        ));
        workspace
            .execute(EditCommand::ReplaceFrame {
                frame_id: frame(1),
                replacement: Box::new(replacement),
            })
            .unwrap();
        let before = workspace.manifest().clone();
        let disk = disk_bytes(directory.path());
        let pending = request(&workspace, WPF_VECTOR_SHAPE_VERSION);
        assert!(workspace.apply_vector_shapes(&pending).is_err());
        assert_eq!(workspace.manifest(), &before);
        assert_eq!(disk_bytes(directory.path()), disk);
        assert_eq!(workspace.manifest().schema_version, 8);
    }
}
