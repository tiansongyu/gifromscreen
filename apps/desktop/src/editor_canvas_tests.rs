use super::*;
use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
    ColorSpace, DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize, ProjectId,
    ProjectManifest, RasterEncoding, UnixTimeMs,
};
use gif_from_screen_project::ActiveProject;

pub(super) fn workspace() -> (tempfile::TempDir, EditorWorkspace) {
    let directory = tempfile::tempdir().unwrap();
    let size = PhysicalSize::new(8, 6).unwrap();
    let manifest = ProjectManifest::new(
        ProjectId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
        "canvas-test",
        UnixTimeMs::new(1),
        Canvas {
            size,
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    )
    .unwrap();
    let mut project = ActiveProject::create(directory.path(), manifest).unwrap();
    let pixels: Vec<u8> = (0_u8..6)
        .flat_map(|y| (0_u8..8).flat_map(move |x| [x * 20, y * 30, 100, 255]))
        .collect();
    let asset_id = project.assets().put(&pixels).unwrap();
    let frames = (1..=2)
        .map(|id| FrameClip {
            id: FrameId::from_u128(id),
            asset_id,
            duration: DurationUs::new(100_000).unwrap(),
            transform: ClipTransform::default(),
            effects: Vec::new(),
            capture_metadata: CaptureMetadata::default(),
            render_steps: Vec::new(),
            capture_clock: None,
            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
        })
        .collect();
    project
        .commit(EditCommand::Compound {
            commands: vec![
                EditCommand::RegisterAsset {
                    asset: AssetDescriptor {
                        id: asset_id,
                        byte_len: pixels.len() as u64,
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
    let mut workspace = EditorWorkspace::from_active(project, 32).unwrap();
    workspace.select_only(FrameId::from_u128(1)).unwrap();
    (directory, workspace)
}

fn frame(
    context: &egui::Context,
    canvas: &mut EditorCanvasState,
    preview: &EditorPreview,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, egui::Rect) {
    let mut painted = egui::Rect::NOTHING;
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            focused: true,
            events,
            ..Default::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                painted = canvas
                    .show_image(ui, preview, egui::Sense::click_and_drag(), true, |_, _| {})
                    .unwrap()
                    .rect;
            });
        },
    );
    (output, painted)
}

fn preview(context: &egui::Context) -> EditorPreview {
    EditorPreview {
        texture: context.load_texture(
            "canvas-geometry-test",
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::NEAREST,
        ),
        rendered_size: [800, 600],
        preview_size: [2, 2],
    }
}

#[test]
fn native_zoom_is_in_physical_pixels_and_fit_can_magnify_tiny_images() {
    for ppp in [1.0, 1.25, 2.0] {
        let available = egui::vec2(500.0, 360.0);
        assert_eq!(
            PreviewZoom::Native
                .extent([640, 420], available, ppp)
                .unwrap()
                * ppp,
            egui::vec2(640.0, 420.0)
        );
        assert_eq!(
            PreviewZoom::Double
                .extent([640, 420], available, ppp)
                .unwrap()
                * ppp,
            egui::vec2(1280.0, 840.0)
        );
        assert_eq!(
            PreviewZoom::Fit.extent([1, 1], available, ppp).unwrap(),
            egui::vec2(360.0, 360.0)
        );
    }
}

#[test]
fn exact_zoom_keeps_actual_image_extent_and_clips_scrolling_without_new_texture() {
    let context = egui::Context::default();
    let mut canvas = EditorCanvasState {
        zoom: PreviewZoom::Double,
        ..Default::default()
    };
    let preview = preview(&context);
    let texture = preview.texture.id();
    let viewport = egui::vec2(420.0, 500.0);
    frame(&context, &mut canvas, &preview, viewport, Vec::new());
    let (output, image) = frame(&context, &mut canvas, &preview, viewport, Vec::new());
    assert_eq!(image.size(), egui::vec2(1600.0, 1200.0));
    assert!(canvas.viewport.unwrap().height() <= CANVAS_HEIGHT);
    for shape in output.shapes.iter().filter(
        |shape| matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.texture_id == texture),
    ) {
        assert!(shape.clip_rect.height() <= CANVAS_HEIGHT);
    }
    canvas.scroll_offset = egui::vec2(120.0, 90.0);
    let (_, moved) = frame(&context, &mut canvas, &preview, viewport, Vec::new());
    assert!(moved.left() < image.left() && moved.top() < image.top());
    assert_eq!(moved.size(), image.size());
    assert_eq!(preview.texture.id(), texture);
}

#[test]
fn middle_drag_starts_only_inside_the_current_viewport_and_stops_on_release() {
    let context = egui::Context::default();
    let mut canvas = EditorCanvasState {
        zoom: PreviewZoom::Double,
        ..Default::default()
    };
    let preview = preview(&context);
    let size = egui::vec2(420.0, 500.0);
    frame(&context, &mut canvas, &preview, size, Vec::new());
    let button = |position, pressed| {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Middle,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    };
    let outside = egui::pos2(100.0, 450.0);
    frame(&context, &mut canvas, &preview, size, button(outside, true));
    assert!(!canvas.panning);
    frame(
        &context,
        &mut canvas,
        &preview,
        size,
        button(outside, false),
    );
    let inside = canvas.viewport.unwrap().center();
    frame(&context, &mut canvas, &preview, size, button(inside, true));
    assert!(canvas.panning);
    let moved = inside - egui::vec2(30.0, 20.0);
    frame(
        &context,
        &mut canvas,
        &preview,
        size,
        vec![egui::Event::PointerMoved(moved)],
    );
    assert_eq!(canvas.scroll_offset, egui::vec2(30.0, 20.0));
    frame(&context, &mut canvas, &preview, size, button(moved, false));
    assert!(!canvas.panning);
}

#[test]
fn panning_does_not_author_or_finish_a_freehand_stroke() {
    use crate::editor_ui::{DrawingDraftPhase, DrawingOverlayDraft};
    let context = egui::Context::default();
    let preview = preview(&context);
    let mut canvas = EditorCanvasState {
        zoom: PreviewZoom::Double,
        ..Default::default()
    };
    let mut draft = DrawingOverlayDraft::default();
    draft.begin();
    let mut draw = |events, draft: &mut DrawingOverlayDraft| {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 500.0),
                )),
                focused: true,
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    canvas
                        .show_image(ui, &preview, egui::Sense::drag(), true, |_, response| {
                            crate::update_drawing_draft_from_preview(
                                response,
                                preview.rendered_size,
                                draft,
                            );
                        })
                        .unwrap();
                });
            },
        );
    };
    draw(Vec::new(), &mut draft);
    for button in [
        egui::PointerButton::Middle,
        egui::PointerButton::Secondary,
        egui::PointerButton::Primary,
    ] {
        let start = egui::pos2(150.0, 160.0);
        let end = egui::pos2(100.0, 120.0);
        draw(
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            &mut draft,
        );
        draw(vec![egui::Event::PointerMoved(end)], &mut draft);
        draw(
            vec![egui::Event::PointerButton {
                pos: end,
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut draft,
        );
        if button == egui::PointerButton::Primary {
            assert!(!draft.points.is_empty());
            assert_eq!(draft.phase, DrawingDraftPhase::Ready);
        } else {
            assert!(draft.points.is_empty());
            assert_eq!(draft.phase, DrawingDraftPhase::Capturing);
        }
    }
}

#[test]
fn invalid_extents_are_rejected_instead_of_creating_unbounded_geometry() {
    for zoom in [PreviewZoom::Fit, PreviewZoom::Native, PreviewZoom::Double] {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(
                zoom.extent([8, 6], egui::vec2(100.0, 100.0), scale)
                    .is_err()
            );
        }
        assert!(zoom.extent([0, 6], egui::vec2(100.0, 100.0), 1.0).is_err());
    }
}
