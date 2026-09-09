use gif_from_screen_domain::{
    AssetId, BlendMode, CaptureBinding, CaptureMetadata, ClipTransform, CompositePrecision,
    DurationUs, FrameClip, FrameId, FrameOverlayCell, FrameOverlayMark, FrameRenderStep,
    OverlayContent, OverlayId, OverlayTrack, PhysicalSize, Rgba, TimeUs, TrackId, VectorShape,
    VectorShapeBounds, VectorShapeKind,
};
use sha2::{Digest, Sha256};

use crate::{
    AssetProviderError, CpuRenderer, NeverCancel, OverlayRenderPlan, RenderError, RenderLimits,
    RgbaSurface, vector_shape_geometry, wpf_vector_shape_geometry,
};

fn frame() -> FrameClip {
    FrameClip {
        id: FrameId::from_u128(1),
        asset_id: AssetId::from_digest([1; 32]),
        duration: DurationUs::new(100_000).unwrap(),
        transform: ClipTransform::default(),
        capture_metadata: CaptureMetadata::default(),
        capture_binding: CaptureBinding::NotRecorded,
        capture_clock: None,
        effects: vec![],
        render_steps: vec![
            FrameRenderStep::composite(1),
            FrameRenderStep::Composite {
                stage_id: 2,
                precision: CompositePrecision::VectorCanvasPbgra8PngV2,
            },
        ],
    }
}

fn color(red: u8, green: u8, blue: u8, alpha: u8) -> Rgba {
    Rgba {
        red,
        green,
        blue,
        alpha,
    }
}

fn shapes() -> Vec<VectorShape> {
    let rectangle = VectorShape {
        bounds: VectorShapeBounds {
            x_hundredths: 125,
            y_hundredths: 175,
            width_hundredths: 1250,
            height_hundredths: 1125,
        },
        stroke_width_hundredths: 50,
        corner_radius_hundredths: 25,
        stroke: color(200, 35, 61, 117),
        fill: Some(color(40, 130, 210, 173)),
        ..VectorShape::wpf_v2()
    };
    vec![
        rectangle,
        VectorShape {
            kind: VectorShapeKind::Triangle,
            bounds: VectorShapeBounds {
                x_hundredths: 200,
                y_hundredths: 100,
                width_hundredths: 1000,
                height_hundredths: 1400,
            },
            stroke_width_hundredths: 125,
            corner_radius_hundredths: 0,
            ..rectangle
        },
    ]
}

fn track(shapes: &[VectorShape]) -> OverlayTrack {
    let mut cell = FrameOverlayCell::whole(
        frame().id,
        1,
        shapes
            .iter()
            .enumerate()
            .map(|(index, shape)| FrameOverlayMark {
                id: OverlayId::from_u128(index as u128 + 100),
                z_index: i32::try_from(index).unwrap(),
                content: OverlayContent::VectorShape { shape: *shape },
            })
            .collect(),
    );
    cell.stage = Some(2);
    OverlayTrack {
        id: TrackId::from_u128(10),
        name: "V2 group".into(),
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        frame_cells: Some(vec![cell]),
        items: vec![],
        annotation: None,
        annotation_scope: None,
    }
}

fn source(_: AssetId) -> Result<RgbaSurface, AssetProviderError> {
    Ok(RgbaSurface::new(
        PhysicalSize::new(16, 16).unwrap(),
        [31, 63, 129, 111].repeat(256),
    )?)
}
fn transparent(_: AssetId) -> Result<RgbaSurface, AssetProviderError> {
    Ok(RgbaSurface::new(
        PhysicalSize::new(16, 16).unwrap(),
        vec![0; 1024],
    )?)
}
fn digest(surface: &RgbaSurface) -> String {
    format!("{:x}", Sha256::digest(surface.pixels()))
}

#[test]
fn saved_stage_and_detached_plan_match_the_actual_wpf_group_not_the_old_vector_one_pixels() {
    let clip = frame();
    let shapes = shapes();
    let tracks = [track(&shapes)];
    let renderer = CpuRenderer::default();
    let direct = renderer
        .render_clip_with_overlays(&clip, &tracks, TimeUs::ZERO, &source, &NeverCancel)
        .unwrap();
    // Untouched Windows WPF 4e28a35 vector-rounded-fraction stage-01.rgba.
    assert_eq!(
        digest(&direct),
        "b09efdcf001b566a1cda5302e466389e1a4577c2466f1d6fee305c18633d72a3"
    );
    let plan = OverlayRenderPlan::for_frame(&tracks, clip.id, TimeUs::ZERO, &NeverCancel).unwrap();
    assert_eq!(
        renderer
            .render_clip_with_overlay_plan(&clip, &plan, &source, &NeverCancel)
            .unwrap(),
        direct
    );
    let mut legacy = clip.clone();
    legacy.render_steps[1] = FrameRenderStep::Composite {
        stage_id: 2,
        precision: CompositePrecision::VectorCanvasPbgra8PngV1,
    };
    let legacy_shapes = shapes
        .iter()
        .map(|shape| VectorShape {
            version: 1,
            ..*shape
        })
        .collect::<Vec<_>>();
    let old = renderer
        .render_clip_with_overlays(
            &legacy,
            &[track(&legacy_shapes)],
            TimeUs::ZERO,
            &source,
            &NeverCancel,
        )
        .unwrap();
    // Archived V1 actual bytes, not WPF expected pixels. Keep this contract too.
    assert_eq!(
        digest(&old),
        "dcf22199e765aab36b433ce1554c8d83e34c6bafbc6097a27ebfea65fa355d26"
    );
    assert_ne!(direct, old);
}

#[test]
fn all_four_public_geometry_dispatches_reach_v2_without_custom_contour_recursion() {
    for kind in [
        VectorShapeKind::Rectangle,
        VectorShapeKind::Ellipse,
        VectorShapeKind::Triangle,
        VectorShapeKind::BlockArrow,
    ] {
        let shape = VectorShape {
            kind,
            rotation_hundredths: 3300,
            ..shapes()[0]
        };
        assert_eq!(
            vector_shape_geometry(&shape).unwrap(),
            wpf_vector_shape_geometry(&shape).unwrap()
        );
    }
}

#[test]
fn post_mark_opacity_is_applied_once_after_fill_and_stroke() {
    let shape = VectorShape {
        bounds: VectorShapeBounds {
            x_hundredths: 400,
            y_hundredths: 400,
            width_hundredths: 800,
            height_hundredths: 800,
        },
        stroke_width_hundredths: 400,
        stroke: color(0, 0, 255, 128),
        fill: Some(color(255, 0, 0, 128)),
        ..VectorShape::wpf_v2()
    };
    let mut layer = track(&[shape]);
    layer.opacity = 128;
    let output = CpuRenderer::default()
        .render_clip_with_overlays(&frame(), &[layer], TimeUs::ZERO, &transparent, &NeverCancel)
        .unwrap();
    let center = (8 * 16 + 8) * 4;
    assert_eq!(&output.pixels()[center..center + 4], &[85, 0, 170, 96]);
}

#[test]
fn hidden_or_zero_track_is_a_noop_but_active_transparent_paint_keeps_the_stage_boundary() {
    let mut layer = track(&shapes());
    let original = source(frame().asset_id).unwrap();
    let renderer = CpuRenderer::default();
    for (visible, opacity) in [(false, 255), (true, 0)] {
        layer.visible = visible;
        layer.opacity = opacity;
        assert_eq!(
            renderer
                .render_clip_with_overlays(
                    &frame(),
                    std::slice::from_ref(&layer),
                    TimeUs::ZERO,
                    &source,
                    &NeverCancel
                )
                .unwrap(),
            original
        );
    }
    let invisible = VectorShape {
        stroke: Rgba::TRANSPARENT,
        fill: Some(Rgba::TRANSPARENT),
        ..VectorShape::wpf_v2()
    };
    let output = renderer
        .render_clip_with_overlays(
            &frame(),
            &[track(&[invisible])],
            TimeUs::ZERO,
            &source,
            &NeverCancel,
        )
        .unwrap();
    assert_eq!(output.pixels(), [29, 62, 128, 111].repeat(256));
}

#[test]
fn wrong_stage_content_owner_or_blend_never_falls_back_to_vector_one() {
    let renderer = CpuRenderer::default();
    for case in 0..5 {
        let mut clip = frame();
        let mut layer = track(&shapes());
        match case {
            0 => {
                if let OverlayContent::VectorShape { shape } =
                    &mut layer.frame_cells.as_mut().unwrap()[0].marks[0].content
                {
                    shape.version = 1;
                }
            }
            1 => layer.frame_cells.as_mut().unwrap()[0].stage = None,
            2 => {
                clip.render_steps[1] = FrameRenderStep::Composite {
                    stage_id: 2,
                    precision: CompositePrecision::WpfPbgra8PngV1,
                }
            }
            3 => layer.blend_mode = BlendMode::Multiply,
            _ => layer.frame_cells.as_mut().unwrap()[0].stage = Some(77),
        }
        assert!(
            renderer
                .render_clip_with_overlays(&clip, &[layer], TimeUs::ZERO, &source, &NeverCancel)
                .is_err(),
            "case {case}"
        );
    }
}

#[test]
fn destination_and_metadata_working_bytes_are_charged_before_v2_shape_buffers() {
    let renderer = CpuRenderer::with_limits(RenderLimits {
        max_surface_bytes: 1024,
    });
    assert!(matches!(
        renderer.render_clip_with_overlays(
            &frame(),
            &[track(&shapes())],
            TimeUs::ZERO,
            &source,
            &NeverCancel
        ),
        Err(RenderError::EffectWorkingMemoryLimitExceeded { .. })
    ));
}
