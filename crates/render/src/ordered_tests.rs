//! Pixel contracts for schema 3 chronology, independent of project persistence.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gif_from_screen_domain::{
    AssetId, BlendMode, CaptureBinding, CaptureMetadata, ClipTransform, DurationUs, Effect,
    FrameClip, FrameId, FrameOverlayCell, FrameOverlayMark, FrameRenderStep, HorizontalAlignment,
    OverlayContent, OverlayId, OverlayItem, OverlayTrack, PhysicalPoint, PhysicalPx, PhysicalRect,
    PhysicalSize, QuarterTurn, Rgba, TextRaster, TimeUs, TimelineSpan, TrackId,
};

use crate::{
    AssetProviderError, CancellationToken, CpuRenderer, FrameAssetProvider, NeverCancel,
    OverlayRenderPlan, RenderError, RenderLimits, RgbaSurface,
    active_raster_overlay_assets_for_frame,
};

fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}

fn point(x: u32, y: u32) -> PhysicalPoint {
    PhysicalPoint {
        x: PhysicalPx::new(x),
        y: PhysicalPx::new(y),
    }
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> PhysicalRect {
    PhysicalRect::new(x, y, width, height).unwrap()
}

fn asset(number: u8) -> AssetId {
    AssetId::from_digest([number; 32])
}

fn solid(width: u32, height: u32, pixel: [u8; 4]) -> RgbaSurface {
    RgbaSurface::new(
        size(width, height),
        pixel.repeat(usize::try_from(width * height).unwrap()),
    )
    .unwrap()
}

fn labelled(width: u32, height: u32, values: &[u8]) -> RgbaSurface {
    let pixels = values
        .iter()
        .flat_map(|value| [*value, 0, 0, 255])
        .collect();
    RgbaSurface::new(size(width, height), pixels).unwrap()
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
        render_steps: Vec::new(),
    }
}

fn composite(stage_id: u32) -> FrameRenderStep {
    FrameRenderStep::composite(stage_id)
}

fn glyph(number: u8, x: u32, y: u32) -> OverlayContent {
    OverlayContent::Text {
        text: format!("glyph {number}"),
        position: point(x, y),
        max_width: None,
        font_family: "frozen fixture, no font access".into(),
        font_size_px: 12,
        foreground: Rgba {
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
        },
        background: None,
        alignment: HorizontalAlignment::Start,
        raster: Some(TextRaster {
            asset_id: asset(number),
            size: size(1, 1),
        }),
    }
}

fn mark(number: u128, z_index: i32, content: OverlayContent) -> FrameOverlayMark {
    FrameOverlayMark {
        id: OverlayId::from_u128(number),
        z_index,
        content,
    }
}

fn cell(stage: Option<u32>, marks: Vec<FrameOverlayMark>) -> FrameOverlayCell {
    FrameOverlayCell {
        frame_id: clip().id,
        stage,
        scopes: Vec::new(),
        marks,
        input_replay: None,
    }
}

fn timed(number: u128, z_index: i32, content: OverlayContent) -> OverlayItem {
    OverlayItem {
        id: OverlayId::from_u128(number),
        z_index,
        content,
        span: TimelineSpan {
            start: TimeUs::ZERO,
            duration: DurationUs::new(10).unwrap(),
        },
    }
}

fn track(number: u128, items: Vec<OverlayItem>, cells: Vec<FrameOverlayCell>) -> OverlayTrack {
    OverlayTrack {
        id: TrackId::from_u128(number),
        name: format!("track {number}"),
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        items,
        frame_cells: Some(cells),
        annotation: None,
        annotation_scope: None,
    }
}

fn provider(base: &RgbaSurface, id: AssetId) -> Result<RgbaSurface, AssetProviderError> {
    Ok(if id == asset(1) {
        base.clone()
    } else if id == asset(2) {
        solid(1, 1, [255, 0, 0, 255])
    } else if id == asset(3) {
        solid(1, 1, [0, 255, 0, 255])
    } else if id == asset(4) {
        solid(1, 1, [0, 0, 255, 255])
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("unexpected resource {id}"),
        )
        .into());
    })
}

fn render_both<P: FrameAssetProvider>(
    owner: &FrameClip,
    tracks: &[OverlayTrack],
    provider: &P,
) -> RgbaSurface {
    let renderer = CpuRenderer::new();
    let direct = renderer
        .render_clip_with_overlays(owner, tracks, TimeUs::ZERO, provider, &NeverCancel)
        .unwrap();
    let plan = OverlayRenderPlan::for_frame(tracks, owner.id, TimeUs::ZERO, &NeverCancel).unwrap();
    let detached = renderer
        .render_clip_with_overlay_plan(owner, &plan, provider, &NeverCancel)
        .unwrap();
    assert_eq!(
        direct, detached,
        "detached plans must retain stage and owner semantics"
    );
    direct
}

#[test]
fn first_composite_preserves_legacy_and_owned_stable_alpha_blend_order() {
    let mut owner = clip();
    let mut tracks = vec![
        track(
            1,
            vec![timed(10, 8, glyph(2, 0, 0)), timed(11, -1, glyph(3, 0, 0))],
            vec![cell(None, vec![mark(12, 8, glyph(4, 0, 0))])],
        ),
        track(
            2,
            vec![timed(13, 8, glyph(3, 0, 0))],
            vec![cell(None, vec![mark(14, 8, glyph(2, 0, 0))])],
        ),
        track(
            3,
            Vec::new(),
            vec![cell(None, vec![mark(15, 9, glyph(4, 0, 0))])],
        ),
    ];
    tracks[0].opacity = 127;
    tracks[1].opacity = 211;
    tracks[1].blend_mode = BlendMode::Multiply;
    tracks[2].opacity = 73;
    tracks[2].blend_mode = BlendMode::Screen;
    let base = solid(2, 1, [70, 100, 90, 170]);
    let load = |id| {
        let mut value = provider(&base, id)?;
        if id != asset(1) {
            value.pixels_mut()[3] = 128;
        }
        Ok(value)
    };
    let old = render_both(&owner, &tracks, &load);
    owner.render_steps = vec![composite(77)];
    for cell in tracks
        .iter_mut()
        .flat_map(|track| track.frame_cells.iter_mut().flatten())
    {
        cell.stage = Some(77);
    }
    assert_eq!(render_both(&owner, &tracks, &load), old);
    assert_eq!(&old.pixels()[4..], &[70, 100, 90, 170]);
}

#[test]
fn old_text_is_rotated_and_blurred_but_new_text_remains_crisp() {
    let mut owner = clip();
    owner.render_steps = vec![
        composite(9),
        FrameRenderStep::Rotate {
            rotation: QuarterTurn::Clockwise90,
        },
        FrameRenderStep::Effect {
            effect: Effect::Blur {
                region: rect(0, 0, 2, 3),
                radius: 1,
            },
        },
        composite(2),
    ];
    let tracks = vec![track(
        1,
        Vec::new(),
        vec![
            cell(Some(9), vec![mark(10, 0, glyph(2, 0, 0))]),
            cell(Some(2), vec![mark(11, 0, glyph(3, 0, 0))]),
            cell(None, vec![mark(12, 0, glyph(4, 1, 2))]),
        ],
    )];
    let base = solid(3, 2, [0, 0, 0, 255]);
    let actual = render_both(&owner, &tracks, &|id| provider(&base, id));
    let first = render_both(
        &clip(),
        &[track(
            1,
            Vec::new(),
            vec![cell(None, vec![mark(10, 0, glyph(2, 0, 0))])],
        )],
        &|id| provider(&base, id),
    );
    let mut old_operations = clip();
    old_operations.transform.rotation = QuarterTurn::Clockwise90;
    old_operations.effects.push(Effect::Blur {
        region: rect(0, 0, 2, 3),
        radius: 1,
    });
    let edited = CpuRenderer::new()
        .render_clip(&old_operations, &|id| provider(&first, id), &NeverCancel)
        .unwrap();
    let expected = render_both(
        &clip(),
        &[track(
            1,
            Vec::new(),
            vec![cell(
                None,
                vec![mark(11, 0, glyph(3, 0, 0)), mark(12, 0, glyph(4, 1, 2))],
            )],
        )],
        &|id| provider(&edited, id),
    );
    assert_eq!(actual, expected);
    assert_eq!(actual.size(), size(2, 3));
    assert_eq!(&actual.pixels()[..4], &[0, 255, 0, 255]);
    assert_eq!(&actual.pixels()[20..], &[0, 0, 255, 255]);
    assert!(
        actual
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[0] > 0 && pixel[0] < 255)
    );
    let overlay_free = CpuRenderer::new()
        .render_clip(
            &owner,
            &|id| {
                assert_eq!(id, asset(1), "overlay-free rendering must not load glyphs");
                Ok(base.clone())
            },
            &NeverCancel,
        )
        .unwrap();
    assert_eq!(overlay_free, solid(2, 3, [0, 0, 0, 255]));
}

#[test]
fn chronological_stage_order_overrides_numeric_ids_and_cross_stage_z() {
    let mut owner = clip();
    owner.render_steps = vec![composite(100), composite(1)];
    let mut tracks = vec![
        track(
            1,
            Vec::new(),
            vec![
                cell(Some(100), vec![mark(1, 1_000, glyph(2, 0, 0))]),
                cell(Some(1), vec![mark(2, -1_000, glyph(3, 0, 0))]),
            ],
        ),
        track(
            2,
            Vec::new(),
            vec![cell(None, vec![mark(3, -9_999, glyph(4, 0, 0))])],
        ),
    ];
    tracks[1].opacity = 128;
    let base = solid(1, 1, [0, 0, 0, 255]);
    assert_eq!(
        render_both(&owner, &tracks, &|id| provider(&base, id)).pixels(),
        &[0, 127, 128, 255]
    );
}

#[test]
fn resizing_affects_only_marks_already_composited() {
    let mut owner = clip();
    owner.render_steps = vec![
        composite(1),
        FrameRenderStep::Resize { size: size(4, 2) },
        composite(2),
    ];
    let tracks = vec![track(
        1,
        Vec::new(),
        vec![
            cell(Some(1), vec![mark(1, 0, glyph(2, 0, 0))]),
            cell(Some(2), vec![mark(2, 0, glyph(3, 3, 1))]),
        ],
    )];
    let base = solid(2, 1, [0, 0, 0, 255]);
    let actual = render_both(&owner, &tracks, &|id| provider(&base, id));
    assert_eq!(actual.size(), size(4, 2));
    let red = [255, 0, 0, 255];
    let black = [0, 0, 0, 255];
    let green = [0, 255, 0, 255];
    assert_eq!(
        actual.pixels(),
        [red, red, black, black, red, red, black, green].concat()
    );
}

#[test]
fn prefix_and_ordered_steps_are_applied_once_by_every_entrypoint() {
    let mut owner = clip();
    owner.transform = ClipTransform {
        crop: Some(rect(0, 1, 2, 2)),
        output_size: Some(size(2, 1)),
        rotation: QuarterTurn::Clockwise90,
        ..ClipTransform::default()
    };
    owner.effects.push(Effect::Lighten {
        region: rect(0, 0, 1, 2),
        amount_percent: 20,
    });
    owner.render_steps = vec![
        composite(11),
        FrameRenderStep::Rotate {
            rotation: QuarterTurn::Clockwise90,
        },
        composite(3),
    ];
    let base = labelled(2, 3, &[1, 2, 3, 4, 5, 6]);
    let expected = RgbaSurface::new(size(2, 1), vec![54, 51, 51, 255, 53, 51, 51, 255]).unwrap();
    let load = |id| provider(&base, id);
    let rendered = CpuRenderer::new()
        .render_clip(&owner, &load, &NeverCancel)
        .unwrap();
    assert_eq!(rendered, expected);
    assert_eq!(render_both(&owner, &[], &load), expected);
    let transformed = CpuRenderer::new()
        .transform_surface(&base, owner.transform, &NeverCancel)
        .unwrap();
    assert_eq!(transformed, labelled(1, 2, &[3, 4]));
    assert_eq!(base, labelled(2, 3, &[1, 2, 3, 4, 5, 6]));
}

#[test]
fn ordered_crop_flips_and_identity_steps_use_the_current_surface() {
    let mut owner = clip();
    owner.render_steps = vec![
        composite(8),
        FrameRenderStep::FlipHorizontal,
        FrameRenderStep::Crop {
            rect: rect(1, 0, 2, 2),
        },
        FrameRenderStep::Resize { size: size(2, 2) },
        FrameRenderStep::Rotate {
            rotation: QuarterTurn::Zero,
        },
        FrameRenderStep::FlipVertical,
    ];
    let base = labelled(3, 2, &[1, 2, 3, 4, 5, 6]);
    assert_eq!(
        render_both(&owner, &[], &|id| provider(&base, id)),
        labelled(2, 2, &[5, 4, 2, 1])
    );
}

#[test]
fn plans_detach_all_stage_assets_and_remain_bound_to_the_original_owner() {
    let mut owner = clip();
    owner.render_steps = vec![composite(90), composite(2)];
    let mut tracks = vec![track(
        1,
        vec![timed(1, 1, glyph(2, 0, 0))],
        vec![
            cell(Some(90), vec![mark(2, 0, glyph(3, 1, 0))]),
            cell(Some(2), vec![mark(3, 2, glyph(4, 2, 0))]),
            cell(None, vec![mark(4, 3, glyph(2, 3, 0))]),
        ],
    )];
    let plan = OverlayRenderPlan::for_frame(&tracks, owner.id, TimeUs::ZERO, &NeverCancel).unwrap();
    let assets = plan.raster_assets().collect::<Vec<_>>();
    assert_eq!(
        assets,
        active_raster_overlay_assets_for_frame(&tracks, owner.id, TimeUs::ZERO, &NeverCancel)
            .unwrap()
    );
    assert_eq!(
        assets
            .iter()
            .map(|entry| entry.asset_id)
            .collect::<Vec<_>>(),
        vec![asset(3), asset(2), asset(4), asset(2)]
    );
    let base = solid(4, 1, [0, 0, 0, 255]);
    let load = |id| provider(&base, id);
    let expected = render_both(&owner, &tracks, &load);
    let mut copied_owner = owner.clone();
    copied_owner.id = FrameId::from_u128(2);
    assert!(matches!(
        CpuRenderer::new().render_clip_with_overlay_plan(&copied_owner, &plan, &load, &NeverCancel),
        Err(RenderError::OverlayPlanFrameMismatch { .. })
    ));
    for cell in tracks
        .iter_mut()
        .flat_map(|track| track.frame_cells.iter_mut().flatten())
    {
        cell.frame_id = copied_owner.id;
    }
    assert_eq!(render_both(&copied_owner, &tracks, &load), expected);
    drop(tracks);
    assert_eq!(
        CpuRenderer::new()
            .render_clip_with_overlay_plan(&owner, &plan, &load, &NeverCancel)
            .unwrap(),
        expected
    );
}

#[test]
fn malformed_steps_and_unknown_active_stages_fail_before_loading_assets() {
    let mut owner = clip();
    let no_load = |_| -> Result<RgbaSurface, AssetProviderError> {
        panic!("invalid pipeline loaded an asset")
    };
    for steps in [
        vec![FrameRenderStep::FlipHorizontal],
        vec![composite(0)],
        vec![composite(1), composite(1)],
        vec![composite(1); 4_097],
    ] {
        owner.render_steps = steps;
        assert!(matches!(
            CpuRenderer::new().render_clip(&owner, &no_load, &NeverCancel),
            Err(RenderError::InvalidRenderSteps { .. })
        ));
        assert!(matches!(
            CpuRenderer::new().render_clip_with_overlays(
                &owner,
                &[],
                TimeUs::ZERO,
                &no_load,
                &NeverCancel
            ),
            Err(RenderError::InvalidRenderSteps { .. })
        ));
    }
    let tracks = [track(
        1,
        Vec::new(),
        vec![cell(Some(99), vec![mark(10, 0, glyph(2, 0, 0))])],
    )];
    let plan = OverlayRenderPlan::for_frame(&tracks, owner.id, TimeUs::ZERO, &NeverCancel).unwrap();
    for steps in [Vec::new(), vec![composite(1)]] {
        owner.render_steps = steps;
        assert!(matches!(
            CpuRenderer::new().render_clip_with_overlays(
                &owner,
                &tracks,
                TimeUs::ZERO,
                &no_load,
                &NeverCancel
            ),
            Err(RenderError::OverlayStageMissing { stage_id: 99, .. })
        ));
        assert!(matches!(
            CpuRenderer::new().render_clip_with_overlay_plan(&owner, &plan, &no_load, &NeverCancel),
            Err(RenderError::OverlayStageMissing { stage_id: 99, .. })
        ));
    }
}

#[test]
fn hidden_content_retains_legacy_skip_semantics_but_cannot_lose_visible_marks() {
    let mut owner = clip();
    owner.render_steps = vec![composite(1)];
    let mut tracks = vec![track(
        1,
        Vec::new(),
        vec![cell(Some(99), vec![mark(1, 0, glyph(2, 0, 0))])],
    )];
    tracks[0].visible = false;
    let base = solid(1, 1, [5, 6, 7, 255]);
    let load = |id| {
        assert_eq!(id, asset(1));
        Ok(base.clone())
    };
    assert_eq!(render_both(&owner, &tracks, &load), base);
    tracks[0].visible = true;
    assert!(matches!(
        CpuRenderer::new().render_clip_with_overlays(
            &owner,
            &tracks,
            TimeUs::ZERO,
            &load,
            &NeverCancel
        ),
        Err(RenderError::OverlayStageMissing { .. })
    ));
}

struct Flag<'a>(&'a AtomicBool);

impl CancellationToken for Flag<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[test]
fn cancellation_during_a_stage_never_loads_or_returns_later_stage_pixels() {
    let mut owner = clip();
    owner.render_steps = vec![composite(1), FrameRenderStep::FlipHorizontal, composite(2)];
    let tracks = vec![track(
        1,
        Vec::new(),
        vec![
            cell(Some(1), vec![mark(1, 0, glyph(2, 0, 0))]),
            cell(Some(2), vec![mark(2, 0, glyph(3, 0, 0))]),
        ],
    )];
    let plan = OverlayRenderPlan::for_frame(&tracks, owner.id, TimeUs::ZERO, &NeverCancel).unwrap();
    let base = solid(2, 1, [0, 0, 0, 255]);
    for detached in [false, true] {
        let cancel = AtomicBool::new(false);
        let loads = AtomicUsize::new(0);
        let load = |id| {
            loads.fetch_add(1, Ordering::Relaxed);
            assert_ne!(id, asset(3), "later stage must not be reached");
            if id == asset(2) {
                cancel.store(true, Ordering::Relaxed);
            }
            provider(&base, id)
        };
        let result = if detached {
            CpuRenderer::new().render_clip_with_overlay_plan(&owner, &plan, &load, &Flag(&cancel))
        } else {
            CpuRenderer::new().render_clip_with_overlays(
                &owner,
                &tracks,
                TimeUs::ZERO,
                &load,
                &Flag(&cancel),
            )
        };
        assert!(matches!(result, Err(RenderError::Cancelled)));
        assert_eq!(loads.load(Ordering::Relaxed), 2);
    }
}

#[test]
fn pure_geometry_and_ordered_resize_respect_limits_and_early_cancellation() {
    let mut owner = clip();
    owner.render_steps = vec![composite(1), FrameRenderStep::Resize { size: size(10, 10) }];
    let base = solid(1, 1, [0, 0, 0, 255]);
    let renderer = CpuRenderer::with_limits(RenderLimits {
        max_surface_bytes: 16,
    });
    let load = |id| provider(&base, id);
    assert!(matches!(
        renderer.render_clip(&owner, &load, &NeverCancel),
        Err(RenderError::SurfaceLimitExceeded {
            requested: 400,
            limit: 16
        })
    ));
    assert!(matches!(
        renderer.render_clip_with_overlays(&owner, &[], TimeUs::ZERO, &load, &NeverCancel),
        Err(RenderError::SurfaceLimitExceeded {
            requested: 400,
            limit: 16
        })
    ));
    let transform = ClipTransform {
        output_size: Some(size(10, 10)),
        ..ClipTransform::default()
    };
    assert!(matches!(
        renderer.transform_surface(&base, transform, &NeverCancel),
        Err(RenderError::SurfaceLimitExceeded { .. })
    ));
    let cancel = AtomicBool::new(true);
    let no_load = |_| -> Result<RgbaSurface, AssetProviderError> {
        panic!("pre-cancelled render loaded an asset")
    };
    assert!(matches!(
        renderer.render_clip(&owner, &no_load, &Flag(&cancel)),
        Err(RenderError::Cancelled)
    ));
    assert!(matches!(
        renderer.transform_surface(&base, transform, &Flag(&cancel)),
        Err(RenderError::Cancelled)
    ));
}
