//! Explicit paint boundaries retain legacy bytes and match the WPF/WIC fixture.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gif_from_screen_domain::{
    CaptureBinding, CaptureMetadata, ClipTransform, DurationUs, FrameOverlayCell, FrameOverlayMark,
    MouseButton, PhysicalPx, ProgressFraction, StrokePoint, TextRaster, TrackId,
};

use super::*;
use crate::{AssetProviderError, NeverCancel};

fn asset(number: u8) -> AssetId {
    AssetId::from_digest([number; 32])
}
fn point(x: u32, y: u32) -> PhysicalPoint {
    PhysicalPoint {
        x: PhysicalPx::new(x),
        y: PhysicalPx::new(y),
    }
}
fn surface(width: u32, height: u32, pixels: &[u8]) -> RgbaSurface {
    RgbaSurface::new(PhysicalSize::new(width, height).unwrap(), pixels.to_vec()).unwrap()
}
fn wpf(stage_id: u32) -> FrameRenderStep {
    FrameRenderStep::Composite {
        stage_id,
        precision: CompositePrecision::WpfPbgra8PngV1,
    }
}
fn clip() -> FrameClip {
    FrameClip {
        id: FrameId::from_u128(1),
        asset_id: asset(1),
        duration: DurationUs::new(10).unwrap(),
        transform: ClipTransform::default(),
        effects: Vec::new(),
        capture_metadata: CaptureMetadata::default(),
        capture_binding: CaptureBinding::Original,
        capture_clock: None,
        render_steps: vec![FrameRenderStep::composite(1), wpf(2)],
    }
}
fn raster(id: u8, position: PhysicalPoint, size: PhysicalSize, opacity: u8) -> OverlayContent {
    OverlayContent::Raster {
        asset_id: asset(id),
        position,
        size,
        opacity,
    }
}
fn track(number: u128, stage: u32, contents: Vec<OverlayContent>) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(number),
        annotation: None,
        annotation_scope: None,
        name: format!("paint {number}"),
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
        frame_cells: Some(vec![FrameOverlayCell {
            frame_id: FrameId::from_u128(1),
            stage: Some(stage),
            scopes: Vec::new(),
            input_replay: None,
            marks: contents
                .into_iter()
                .enumerate()
                .map(|(index, content)| FrameOverlayMark {
                    id: OverlayId::from_u128(number * 100 + index as u128),
                    z_index: 0,
                    content,
                })
                .collect(),
        }]),
    }
}
fn both_routes<P: FrameAssetProvider>(
    clip: &FrameClip,
    tracks: &[OverlayTrack],
    provider: &P,
) -> RgbaSurface {
    let renderer = CpuRenderer::default();
    let direct = renderer
        .render_clip_with_overlays(clip, tracks, TimeUs::ZERO, provider, &NeverCancel)
        .unwrap();
    let plan = OverlayRenderPlan::for_frame(tracks, clip.id, TimeUs::ZERO, &NeverCancel).unwrap();
    assert_eq!(
        renderer
            .render_clip_with_overlay_plan(clip, &plan, provider, &NeverCancel)
            .unwrap(),
        direct
    );
    direct
}

#[test]
fn real_overlay_a_fixture_matches_all_pixels_including_three_wic_color_changes() {
    // Hosted WPF run 34071253697-1, ordered-fractional-chain/stage-01.rgba.
    // Input and overlay pixels are pinned in scripts/qa/wpf_reference/fixtures.json.
    // These expected bytes were read from the real WIC PNG round trip, not generated here.
    let source = [
        30, 70, 180, 255, 30, 70, 180, 128, 0, 0, 0, 0, 90, 20, 130, 64, 250, 220, 60, 255, 10,
        110, 230, 160,
    ];
    let tracks = [track(
        1,
        2,
        vec![raster(
            2,
            point(1, 1),
            PhysicalSize::new(3, 2).unwrap(),
            255,
        )],
    )];
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        Ok(if id == asset(1) {
            surface(8, 5, &[0; 160])
        } else {
            surface(3, 2, &source)
        })
    };
    let actual = both_routes(&clip(), &tracks, &provider);
    let mut expected = vec![0; 160];
    expected[36..48].copy_from_slice(&[30, 70, 180, 255, 29, 69, 179, 128, 0, 0, 0, 0]);
    expected[68..80].copy_from_slice(&[91, 19, 131, 64, 250, 220, 60, 255, 9, 109, 229, 160]);
    assert_eq!(actual.pixels(), expected);
}

#[test]
fn one_stage_keeps_multiple_marks_premultiplied_but_two_png_boundaries_are_distinct() {
    let one = PhysicalSize::new(1, 1).unwrap();
    let blue = raster(2, point(0, 0), one, 255);
    let transparent = raster(3, point(0, 0), one, 255);
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        Ok(surface(
            1,
            1,
            if id == asset(2) {
                &[0, 0, 255, 253]
            } else {
                &[0, 0, 0, 0]
            },
        ))
    };
    let combined = both_routes(
        &clip(),
        &[track(1, 2, vec![blue.clone(), transparent.clone()])],
        &provider,
    );
    let mut separate = clip();
    separate.render_steps.push(wpf(3));
    let split = both_routes(
        &separate,
        &[track(1, 2, vec![blue]), track(2, 3, vec![transparent])],
        &provider,
    );
    assert_eq!(combined.pixels(), &[0, 0, 254, 253]);
    assert_eq!(split.pixels(), &[0, 0, 253, 253]);
}

#[test]
fn opacity_is_applied_sequentially_after_premultiplication() {
    let mut layer = track(
        1,
        2,
        vec![raster(
            2,
            point(0, 0),
            PhysicalSize::new(1, 1).unwrap(),
            128,
        )],
    );
    layer.opacity = 128;
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        Ok(surface(
            1,
            1,
            if id == asset(1) {
                &[0; 4]
            } else {
                &[50, 100, 200, 128]
            },
        ))
    };
    assert_eq!(
        both_routes(&clip(), &[layer], &provider).pixels(),
        &[55, 103, 199, 32]
    );
}

#[test]
fn active_stage_quantizes_untouched_base_pixels_but_inactive_stages_do_not() {
    let base = [0, 0, 0, 0, 0, 0, 255, 253, 120, 50, 90, 0];
    let source = |id| -> Result<RgbaSurface, AssetProviderError> {
        Ok(if id == asset(1) {
            surface(3, 1, &base)
        } else {
            surface(1, 1, &[255, 0, 0, 255])
        })
    };
    let active = track(
        1,
        2,
        vec![raster(
            2,
            point(0, 0),
            PhysicalSize::new(1, 1).unwrap(),
            255,
        )],
    );
    assert_eq!(
        both_routes(&clip(), std::slice::from_ref(&active), &source).pixels(),
        &[255, 0, 0, 255, 0, 0, 254, 253, 0, 0, 0, 0]
    );
    for variant in 0..4 {
        let mut hidden = active.clone();
        match variant {
            0 => hidden.visible = false,
            1 => hidden.opacity = 0,
            2 => hidden.frame_cells.as_mut().unwrap()[0].marks.clear(),
            _ => {
                let OverlayContent::Raster { opacity, .. } =
                    &mut hidden.frame_cells.as_mut().unwrap()[0].marks[0].content
                else {
                    unreachable!()
                };
                *opacity = 0;
            }
        }
        assert_eq!(both_routes(&clip(), &[hidden], &source).pixels(), base);
    }
    assert_eq!(both_routes(&clip(), &[], &source).pixels(), base);
}

#[test]
fn every_supported_paint_family_uses_the_same_wpf_pixel_blender() {
    let position = point(0, 0);
    let size = PhysicalSize::new(1, 1).unwrap();
    let color = Rgba {
        red: 30,
        green: 70,
        blue: 180,
        alpha: 128,
    };
    let raster = TextRaster {
        asset_id: asset(2),
        size,
    };
    let contents = vec![
        OverlayContent::Raster {
            asset_id: asset(2),
            position,
            size,
            opacity: 255,
        },
        OverlayContent::Text {
            text: "A".to_owned(),
            font_family: "not resolved".to_owned(),
            font_size_px: 12,
            position,
            max_width: None,
            foreground: color,
            background: None,
            alignment: gif_from_screen_domain::HorizontalAlignment::Start,
            raster: Some(raster.clone()),
        },
        OverlayContent::KeyStroke {
            text: "A".to_owned(),
            position,
            raster: Some(raster),
        },
        OverlayContent::Shape {
            kind: ShapeKind::Rectangle,
            bounds: PhysicalRect {
                origin: position,
                size,
            },
            stroke_width: 0,
            stroke: Rgba::TRANSPARENT,
            fill: Some(color),
        },
        OverlayContent::Drawing {
            points: vec![StrokePoint {
                point: position,
                pressure_milli: 1000,
            }],
            width: 2,
            color,
        },
        OverlayContent::MouseClick {
            position,
            color,
            radius: 1,
            button: MouseButton::Left,
        },
        OverlayContent::Cursor {
            cursor_asset: Some(asset(2)),
            position,
            hotspot: position,
        },
        OverlayContent::Progress {
            bounds: PhysicalRect {
                origin: position,
                size,
            },
            foreground: color,
            background: Rgba::TRANSPARENT,
            show_frame_number: false,
            style: Some(ProgressStyle {
                amount_millionths: 1_000_000,
                fraction: ProgressFraction::new(1, 1),
                direction: ProgressDirection::LeftToRight,
                label: None,
                label_position: position,
                label_text: String::new(),
            }),
        },
    ];
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        Ok(surface(
            1,
            1,
            if id == asset(1) {
                &[0; 4]
            } else {
                &[30, 70, 180, 128]
            },
        ))
    };
    for content in contents {
        assert_eq!(
            both_routes(&clip(), &[track(1, 2, vec![content])], &provider).pixels(),
            &[29, 69, 179, 128]
        );
    }
}

#[test]
fn non_normal_wpf_layers_fail_before_asset_loading_but_legacy_blends_keep_their_bytes() {
    let one = PhysicalSize::new(1, 1).unwrap();
    for (mode, expected) in [
        (BlendMode::Normal, [200, 100, 50, 255]),
        (BlendMode::Multiply, [78, 59, 39, 255]),
        (BlendMode::Screen, [222, 191, 211, 255]),
    ] {
        let mut layer = track(1, 2, vec![raster(2, point(0, 0), one, 255)]);
        layer.blend_mode = mode;
        if mode != BlendMode::Normal {
            let forbidden = |_| -> Result<RgbaSurface, AssetProviderError> {
                panic!("invalid precision must not load assets")
            };
            let plan = OverlayRenderPlan::for_frame(
                std::slice::from_ref(&layer),
                clip().id,
                TimeUs::ZERO,
                &NeverCancel,
            )
            .unwrap();
            assert!(matches!(
                CpuRenderer::default().render_clip_with_overlays(
                    &clip(),
                    std::slice::from_ref(&layer),
                    TimeUs::ZERO,
                    &forbidden,
                    &NeverCancel
                ),
                Err(RenderError::InvalidRenderSteps { .. })
            ));
            assert!(matches!(
                CpuRenderer::default().render_clip_with_overlay_plan(
                    &clip(),
                    &plan,
                    &forbidden,
                    &NeverCancel
                ),
                Err(RenderError::InvalidRenderSteps { .. })
            ));
        }
        let mut legacy = clip();
        legacy.render_steps[1] = FrameRenderStep::composite(2);
        let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
            Ok(surface(
                1,
                1,
                if id == asset(1) {
                    &[100, 150, 200, 255]
                } else {
                    &[200, 100, 50, 255]
                },
            ))
        };
        assert_eq!(both_routes(&legacy, &[layer], &provider).pixels(), expected);
    }
}

#[test]
fn full_surface_conversion_checks_cancellation_without_publishing_partial_pixels() {
    struct CancelAt(AtomicUsize);
    impl CancellationToken for CancelAt {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed) >= 2
        }
    }
    struct Flag<'a>(&'a AtomicBool);
    impl CancellationToken for Flag<'_> {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }
    for convert in [
        crate::wpf_pixels::premultiply,
        crate::wpf_pixels::unpremultiply,
    ] {
        let mut pixels = surface(4096, 1, &[19, 69, 189, 128].repeat(4096));
        assert!(matches!(
            convert_surface_precision(&mut pixels, convert, &CancelAt(AtomicUsize::new(0))),
            Err(RenderError::Cancelled)
        ));
        assert_eq!(&pixels.pixels()[4096 * 4 - 4..], &[19, 69, 189, 128]);
    }
    let cancelled = AtomicBool::new(false);
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        if id != asset(1) {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(surface(1, 1, &[19, 69, 189, 128]))
    };
    let tracks = [track(
        1,
        2,
        vec![raster(
            2,
            point(0, 0),
            PhysicalSize::new(1, 1).unwrap(),
            255,
        )],
    )];
    assert!(matches!(
        CpuRenderer::default().render_clip_with_overlays(
            &clip(),
            &tracks,
            TimeUs::ZERO,
            &provider,
            &Flag(&cancelled)
        ),
        Err(RenderError::Cancelled)
    ));
}
