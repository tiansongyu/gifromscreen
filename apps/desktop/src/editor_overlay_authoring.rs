//! Shared bounded authoring of ordinary frame-owned artwork.

use std::io::{self, Write};

use gif_from_screen_domain::{FrameOverlayCell, FrameOverlayMark, OverlayContent, OverlayId};
use gif_from_screen_editor::MAX_FRAME_BUNDLE_METADATA_BYTES;
use uuid::Uuid;

use super::{EditorWorkspace, EditorWorkspaceError};

impl EditorWorkspace {
    pub(super) fn generic_overlay_cells(
        &self,
        content: OverlayContent,
        z_index: i32,
    ) -> Result<Vec<FrameOverlayCell>, EditorWorkspaceError> {
        let selected = self.selected_frame_ids()?;
        validate_repeated_content(&content, selected.len())?;
        gif_from_screen_domain::selected_frame_cells(
            &self.manifest().timeline,
            self.selection().selected(),
            move |_| FrameOverlayMark {
                id: OverlayId::from_u128(Uuid::new_v4().as_u128()),
                z_index,
                content: content.clone(),
            },
        )
        .map_err(EditorWorkspaceError::FrameOverlayPreparation)
    }
}

pub(super) fn validate_repeated_content(
    content: &OverlayContent,
    count: usize,
) -> Result<(), EditorWorkspaceError> {
    let mut counter = MetadataCounter::default();
    serde_json::to_writer(&mut counter, content).map_err(|_| budget_error())?;
    // Reserve ownership/mark IDs and canonical whole-frame scope overhead,
    // in addition to the dynamically sized text or drawing payload.
    let required = counter
        .0
        .checked_add(512)
        .and_then(|bytes| bytes.checked_mul(count));
    if required.is_none_or(|bytes| bytes > MAX_FRAME_BUNDLE_METADATA_BYTES) {
        return Err(budget_error());
    }
    Ok(())
}

fn budget_error() -> EditorWorkspaceError {
    EditorWorkspaceError::FrameOverlayPreparation(
        "Overlay metadata exceeds 16 MiB. Select fewer frames or simplify the artwork.".to_owned(),
    )
}

#[derive(Default)]
struct MetadataCounter(usize);

impl Write for MetadataCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|bytes| *bytes <= MAX_FRAME_BUNDLE_METADATA_BYTES)
            .ok_or_else(|| io::Error::other("overlay metadata budget exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{create_rendered_duplicate_workspace, frame_id};
    use super::*;
    use gif_from_screen_domain::{
        BlendMode, EditCommand, PhysicalPoint, PhysicalRect, PhysicalSize, Rgba, ShapeKind,
        StrokePoint,
    };

    fn paint(workspace: &EditorWorkspace, frame: gif_from_screen_domain::FrameId) -> Vec<u8> {
        crate::editor_preview::render_frame_surface(workspace.active_project(), frame, 1024 * 1024)
            .unwrap()
            .pixels()
            .to_vec()
    }

    fn shape(kind: ShapeKind) -> OverlayContent {
        OverlayContent::Shape {
            kind,
            bounds: PhysicalRect::new(0, 0, 2, 1).unwrap(),
            stroke_width: 1,
            stroke: Rgba {
                red: 0,
                green: 255,
                blue: 0,
                alpha: 255,
            },
            fill: None,
        }
    }

    fn add_artwork(workspace: &mut EditorWorkspace, kind: usize) {
        let green = Rgba {
            red: 0,
            green: 255,
            blue: 0,
            alpha: 255,
        };
        match kind {
            0 | 1 => {
                workspace
                    .add_overlay_for_selection(
                        "Shape".to_owned(),
                        shape(if kind == 0 {
                            ShapeKind::Rectangle
                        } else {
                            ShapeKind::Arrow
                        }),
                        3,
                        255,
                        BlendMode::Normal,
                    )
                    .unwrap();
            }
            2 => {
                workspace
                    .add_overlay_for_selection(
                        "Drawing".to_owned(),
                        OverlayContent::Drawing {
                            points: vec![
                                StrokePoint {
                                    point: PhysicalPoint::default(),
                                    pressure_milli: 1000,
                                },
                                StrokePoint {
                                    point: PhysicalPoint {
                                        x: gif_from_screen_domain::PhysicalPx::new(1),
                                        y: gif_from_screen_domain::PhysicalPx::ZERO,
                                    },
                                    pressure_milli: 1000,
                                },
                            ],
                            width: 1,
                            color: green,
                        },
                        3,
                        255,
                        BlendMode::Normal,
                    )
                    .unwrap();
            }
            3 => {
                workspace
                    .add_raster_overlay_for_selection(
                        super::super::RasterOverlayEdit {
                            name: "Watermark".to_owned(),
                            source_size: PhysicalSize::new(1, 1).unwrap(),
                            position: PhysicalPoint::default(),
                            display_size: PhysicalSize::new(1, 1).unwrap(),
                            item_opacity: 255,
                            track_opacity: 255,
                            blend_mode: BlendMode::Normal,
                            z_index: 3,
                        },
                        &[0, 255, 0, 255],
                    )
                    .unwrap();
            }
            _ => {
                let request = gif_from_screen_text::TextRequest {
                    text: "Saved caption".to_owned(),
                    font_family: "fixture".to_owned(),
                    font_size_px: 12,
                    size: PhysicalSize::new(1, 1).unwrap(),
                    foreground: green,
                    background: None,
                    alignment: gif_from_screen_domain::HorizontalAlignment::Start,
                };
                workspace
                    .add_text_overlay_for_selection(
                        &request,
                        &gif_from_screen_text::TextImage {
                            size: request.size,
                            rgba: vec![0, 255, 0, 255],
                        },
                        PhysicalPoint::default(),
                    )
                    .unwrap();
            }
        }
    }

    #[test]
    fn metadata_budget_checks_actual_content_times_owner_count_before_cloning() {
        let content = OverlayContent::KeyStroke {
            text: "x".repeat(1024),
            position: PhysicalPoint::default(),
            raster: None,
        };
        assert!(validate_repeated_content(&content, 1).is_ok());
        assert!(validate_repeated_content(&content, 40_000).is_err());
        assert!(validate_repeated_content(&content, usize::MAX).is_err());
    }

    #[test]
    fn new_shape_arrow_drawing_watermark_and_text_follow_owner_frames_and_copies() {
        for kind in 0..5 {
            let directory = tempfile::tempdir().unwrap();
            let mut workspace = create_rendered_duplicate_workspace(&directory);
            workspace.select_only(frame_id(1)).unwrap();
            workspace.toggle_selection(frame_id(3)).unwrap();
            add_artwork(&mut workspace, kind);
            let track = workspace.manifest().timeline.overlay_tracks[0].clone();
            let cells = track.frame_cells.as_ref().unwrap();
            assert_eq!(
                cells.iter().map(|cell| cell.frame_id).collect::<Vec<_>>(),
                [frame_id(1), frame_id(3)]
            );
            assert_eq!(
                [cells[0].scopes[0].run_id, cells[1].scopes[0].run_id],
                [1, 2]
            );
            let before: std::collections::BTreeMap<_, _> = workspace
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| (frame.id, paint(&workspace, frame.id)))
                .collect();
            workspace
                .execute(EditCommand::ReorderFrames {
                    order: vec![frame_id(3), frame_id(2), frame_id(1), frame_id(4)],
                })
                .unwrap();
            assert_eq!(workspace.manifest().timeline.overlay_tracks[0], track);
            for (&id, pixels) in &before {
                assert_eq!(&paint(&workspace, id), pixels, "artwork kind {kind}");
            }
            workspace.select_only(frame_id(1)).unwrap();
            workspace.copy_selection().unwrap();
            workspace.select_only(frame_id(2)).unwrap();
            workspace.paste_after_current().unwrap();
            let copied_id = workspace.manifest().timeline.frames[2].id;
            assert_ne!(copied_id, frame_id(1));
            assert_eq!(paint(&workspace, copied_id), before[&frame_id(1)]);
            let copied = &workspace.manifest().timeline.overlay_tracks[1];
            assert_ne!(copied.id, track.id);
            assert_eq!(copied.frame_cells.as_ref().unwrap()[0].frame_id, copied_id);
            assert_eq!(
                copied.frame_cells.as_ref().unwrap()[0].marks[0].content,
                cells[0].marks[0].content
            );
            workspace.undo().unwrap();
            workspace.redo().unwrap();
            let saved = workspace.manifest().clone();
            drop(workspace);
            let reopened = EditorWorkspace::open(
                directory.path(),
                gif_from_screen_project::LockPolicy::FailIfPresent,
                16,
            )
            .unwrap();
            assert_eq!(reopened.manifest(), &saved);
            assert_eq!(paint(&reopened, copied_id), before[&frame_id(1)]);
        }
    }
}
