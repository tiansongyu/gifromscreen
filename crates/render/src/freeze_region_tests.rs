use std::{
    collections::BTreeMap,
    io,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use gif_from_screen_domain::{
    BlendMode, CaptureBinding, CaptureMetadata, ClipTransform, DurationUs, FrameClip, FrameId,
    FrameOverlayCell, FrameOverlayMark, FrameRenderStep, OverlayContent, OverlayId, OverlayTrack,
    QuarterTurn, Rgba, ShapeKind, TimeUs, TrackId,
};

use super::*;
use crate::{AssetProviderError, CpuRenderer, NeverCancel, OverlayRenderPlan};

fn asset(number: u8) -> AssetId {
    AssetId::from_digest([number; 32])
}
fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}
fn surface(width: u32, height: u32, pixels: &[u8]) -> RgbaSurface {
    RgbaSurface::new(size(width, height), pixels.to_vec()).unwrap()
}
fn clip(steps: Vec<FrameRenderStep>) -> FrameClip {
    FrameClip {
        id: FrameId::from_u128(1),
        asset_id: asset(1),
        duration: DurationUs::new(10).unwrap(),
        transform: ClipTransform::default(),
        effects: Vec::new(),
        render_steps: steps,
        capture_metadata: CaptureMetadata::default(),
        capture_clock: None,
        capture_binding: CaptureBinding::Original,
    }
}
fn freeze(view: PhysicalSize, region: PhysicalRect, invert: bool) -> FrameRenderStep {
    FrameRenderStep::FreezeRegion {
        baseline_asset: asset(2),
        baseline_size: view,
        region,
        invert,
    }
}
fn both_routes<P: FrameAssetProvider>(
    frame: &FrameClip,
    tracks: &[OverlayTrack],
    provider: &P,
) -> RgbaSurface {
    let renderer = CpuRenderer::default();
    let actual = renderer
        .render_clip_with_overlays(frame, tracks, TimeUs::ZERO, provider, &NeverCancel)
        .unwrap();
    let plan = OverlayRenderPlan::for_frame(tracks, frame.id, TimeUs::ZERO, &NeverCancel).unwrap();
    assert_eq!(
        renderer
            .render_clip_with_overlay_plan(frame, &plan, provider, &NeverCancel)
            .unwrap(),
        actual
    );
    if tracks.is_empty() {
        assert_eq!(
            renderer.render_clip(frame, provider, &NeverCancel).unwrap(),
            actual
        );
    }
    actual
}

#[test]
fn inside_and_outside_freeze_copy_all_rgba_bytes_including_transparent_rgb() {
    let source = [
        190, 30, 40, 255, 10, 20, 30, 120, 50, 60, 70, 255, 20, 30, 40, 255, 60, 70, 80, 255, 90,
        100, 110, 255,
    ];
    let baseline = [
        1, 2, 3, 0, 4, 5, 6, 64, 7, 8, 9, 0, 11, 12, 13, 0, 14, 15, 16, 100, 17, 18, 19, 0,
    ];
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        Ok(surface(
            3,
            2,
            if id == asset(1) { &source } else { &baseline },
        ))
    };
    for invert in [false, true] {
        let frame = clip(vec![
            FrameRenderStep::composite(1),
            freeze(size(3, 2), PhysicalRect::new(1, 0, 1, 2).unwrap(), invert),
        ]);
        let rendered = both_routes(&frame, &[], &provider);
        let mut expected = source;
        for index in 0..6 {
            if (index % 3 == 1) == invert {
                expected[index * 4..index * 4 + 4]
                    .copy_from_slice(&baseline[index * 4..index * 4 + 4]);
            }
        }
        assert_eq!(rendered.pixels(), expected);
    }
}

#[test]
fn same_asset_can_supply_a_different_baseline_view_without_resizing_or_cache_mutation() {
    let canonical = surface(2, 1, &[250, 30, 20, 255, 10, 40, 230, 255]);
    let original = canonical.clone();
    let frame = clip(vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::Rotate {
            rotation: QuarterTurn::Clockwise90,
        },
        FrameRenderStep::FlipVertical,
        FrameRenderStep::FreezeRegion {
            baseline_asset: asset(1),
            baseline_size: size(1, 2),
            region: PhysicalRect::new(0, 0, 1, 2).unwrap(),
            invert: true,
        },
    ]);
    let reads = AtomicUsize::new(0);
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        assert_eq!(id, asset(1));
        reads.fetch_add(1, Ordering::Relaxed);
        Ok(canonical.clone())
    };
    let before = frame.clone();
    let rendered = both_routes(&frame, &[], &provider);
    assert_eq!(rendered.size(), size(1, 2));
    assert_eq!(rendered.pixels(), canonical.pixels());
    assert_eq!(reads.load(Ordering::Relaxed), 6);
    assert_eq!(canonical, original);
    assert_eq!(frame, before);
}

fn hidden_track() -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(1),
        name: "Hidden before rotate".to_owned(),
        visible: false,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        annotation: None,
        annotation_scope: None,
        items: Vec::new(),
        frame_cells: Some(vec![FrameOverlayCell {
            frame_id: FrameId::from_u128(1),
            stage: Some(1),
            input_replay: None,
            scopes: Vec::new(),
            marks: vec![FrameOverlayMark {
                id: OverlayId::from_u128(1),
                z_index: 0,
                content: OverlayContent::Shape {
                    kind: ShapeKind::Rectangle,
                    bounds: PhysicalRect::new(0, 0, 2, 1).unwrap(),
                    stroke_width: 0,
                    stroke: Rgba::TRANSPARENT,
                    fill: Some(Rgba {
                        red: 0,
                        green: 255,
                        blue: 0,
                        alpha: 128,
                    }),
                },
            }],
        }]),
    }
}

#[test]
fn revealing_a_hidden_earlier_stage_changes_live_pixels_but_not_the_frozen_reference() {
    let frame = clip(vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::Rotate {
            rotation: QuarterTurn::Clockwise90,
        },
        freeze(size(1, 2), PhysicalRect::new(0, 0, 1, 1).unwrap(), false),
    ]);
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        Ok(if id == asset(1) {
            surface(2, 1, &[255, 0, 0, 255].repeat(2))
        } else {
            surface(2, 1, &[0, 0, 255, 255, 255, 255, 0, 255])
        })
    };
    let mut track = hidden_track();
    let before = track.clone();
    assert_eq!(
        both_routes(&frame, std::slice::from_ref(&track), &provider).pixels(),
        &[255, 0, 0, 255, 255, 255, 0, 255]
    );
    assert_eq!(track, before);
    track.visible = true;
    assert_eq!(
        both_routes(&frame, std::slice::from_ref(&track), &provider).pixels(),
        &[127, 128, 0, 255, 255, 255, 0, 255]
    );
    track.opacity = 0;
    assert_eq!(
        both_routes(&frame, &[track], &provider).pixels(),
        &[255, 0, 0, 255, 255, 255, 0, 255]
    );
}

#[test]
fn freeze_obeys_geometry_before_and_after_its_saved_view() {
    let source = surface(
        3,
        2,
        &[
            1, 2, 3, 255, 10, 20, 30, 255, 40, 50, 60, 255, 4, 5, 6, 255, 70, 80, 90, 255, 100,
            110, 120, 255,
        ],
    );
    let baseline = surface(
        4,
        1,
        &[
            5, 6, 7, 255, 200, 20, 30, 255, 8, 9, 10, 255, 20, 30, 200, 255,
        ],
    );
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        Ok(if id == asset(1) {
            source.clone()
        } else {
            baseline.clone()
        })
    };
    let frame = clip(vec![
        FrameRenderStep::composite(1),
        FrameRenderStep::Crop {
            rect: PhysicalRect::new(1, 0, 2, 2).unwrap(),
        },
        freeze(size(2, 2), PhysicalRect::new(1, 0, 1, 2).unwrap(), true),
        FrameRenderStep::Rotate {
            rotation: QuarterTurn::Clockwise90,
        },
        FrameRenderStep::Resize { size: size(4, 2) },
    ]);
    let expected = [
        70, 80, 90, 255, 70, 80, 90, 255, 10, 20, 30, 255, 10, 20, 30, 255, 20, 30, 200, 255, 20,
        30, 200, 255, 200, 20, 30, 255, 200, 20, 30, 255,
    ];
    assert_eq!(both_routes(&frame, &[], &provider).pixels(), expected);
    assert_eq!(both_routes(&clip(Vec::new()), &[], &provider), source);
}

#[test]
fn freeze_rejects_bad_geometry_and_working_set_before_loading_the_reference() {
    let original = surface(2, 1, &[1, 2, 3, 255].repeat(2));
    let never_load = |_| -> Result<RgbaSurface, AssetProviderError> {
        panic!("invalid preflight cannot load a baseline")
    };
    let region = PhysicalRect::new(0, 0, 1, 1).unwrap();
    let mut working = original.clone();
    assert!(matches!(
        apply(
            &mut working,
            asset(2),
            size(1, 1),
            region,
            true,
            &never_load,
            RenderLimits::default(),
            &NeverCancel
        ),
        Err(RenderError::FreezeRegionSizeMismatch { .. })
    ));
    assert!(matches!(
        apply(
            &mut working,
            asset(2),
            size(2, 1),
            PhysicalRect::new(1, 0, 2, 1).unwrap(),
            true,
            &never_load,
            RenderLimits::default(),
            &NeverCancel
        ),
        Err(RenderError::InvalidFreezeRegion { .. })
    ));
    assert!(matches!(
        apply(
            &mut working,
            asset(2),
            size(2, 1),
            region,
            true,
            &never_load,
            RenderLimits {
                max_surface_bytes: 15
            },
            &NeverCancel
        ),
        Err(RenderError::EffectWorkingMemoryLimitExceeded {
            requested: 16,
            limit: 15,
            ..
        })
    ));
    assert_eq!(working, original);
    let provider = |_| -> Result<RgbaSurface, AssetProviderError> { Ok(original.clone()) };
    apply(
        &mut working,
        asset(2),
        size(2, 1),
        region,
        true,
        &provider,
        RenderLimits {
            max_surface_bytes: 16,
        },
        &NeverCancel,
    )
    .unwrap();
}

#[test]
fn missing_or_wrong_length_baselines_produce_typed_errors_without_changing_pixels() {
    let original = surface(2, 1, &[1, 2, 3, 255].repeat(2));
    let region = PhysicalRect::new(0, 0, 1, 1).unwrap();
    let mut working = original.clone();
    let missing = |_| -> Result<RgbaSurface, AssetProviderError> {
        Err(Box::new(io::Error::new(
            io::ErrorKind::NotFound,
            "baseline",
        )))
    };
    assert!(
        matches!(apply(&mut working, asset(2), size(2, 1), region, true, &missing,
        RenderLimits::default(), &NeverCancel), Err(RenderError::FreezeBaselineLoad { asset_id, .. }) if asset_id == asset(2))
    );
    let wrong = |_| -> Result<RgbaSurface, AssetProviderError> { Ok(surface(1, 1, &[0; 4])) };
    assert!(matches!(
        apply(
            &mut working,
            asset(2),
            size(2, 1),
            region,
            true,
            &wrong,
            RenderLimits::default(),
            &NeverCancel
        ),
        Err(RenderError::FreezeBaselineLengthMismatch {
            expected: 8,
            actual: 4,
            ..
        })
    ));
    assert_eq!(working, original);
}

#[test]
fn cancellation_during_overwrite_never_returns_partial_output_or_mutates_provider_data() {
    struct CancelCopy<'a> {
        loaded: &'a AtomicBool,
        checks: AtomicUsize,
    }
    impl CancellationToken for CancelCopy<'_> {
        fn is_cancelled(&self) -> bool {
            self.loaded.load(Ordering::Relaxed) && self.checks.fetch_add(1, Ordering::Relaxed) >= 4
        }
    }
    let original = surface(4096, 2, &[10, 20, 30, 255].repeat(8192));
    let frozen = surface(8192, 1, &[80, 90, 100, 0].repeat(8192));
    let assets = BTreeMap::from([(asset(1), original.clone()), (asset(2), frozen.clone())]);
    let loaded = AtomicBool::new(false);
    let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
        if id == asset(2) {
            loaded.store(true, Ordering::Relaxed);
        }
        Ok(assets[&id].clone())
    };
    let frame = clip(vec![
        FrameRenderStep::composite(1),
        freeze(
            size(4096, 2),
            PhysicalRect::new(0, 0, 4096, 2).unwrap(),
            true,
        ),
    ]);
    let plan = OverlayRenderPlan::for_frame(&[], frame.id, TimeUs::ZERO, &NeverCancel).unwrap();
    assert!(matches!(
        CpuRenderer::default().render_clip_with_overlay_plan(
            &frame,
            &plan,
            &provider,
            &CancelCopy {
                loaded: &loaded,
                checks: AtomicUsize::new(0)
            }
        ),
        Err(RenderError::Cancelled)
    ));
    assert_eq!(assets[&asset(1)], original);
    assert_eq!(assets[&asset(2)], frozen);
}
