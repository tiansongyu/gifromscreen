use std::sync::atomic::{AtomicUsize, Ordering};

use gif_from_screen_domain::{
    AssetId, BlendMode, CaptureBinding, CaptureMetadata, ClipTransform, DurationUs, Effect,
    FrameClip, FrameId, FrameOverlayCell, FrameOverlayMark, FrameRenderStep, ImageBorderStyle,
    ImageShadowStyle, OverlayContent, OverlayId, OverlayTrack, PhysicalRect, PhysicalSize, Rgba,
    ShapeKind, SignedEdgeWidths, TimeUs, TrackId,
};

use super::{GaussianKernel, alpha_from_float, border, shadow, shadow_pixel};
use crate::{
    AssetProviderError, CancellationToken, CpuRenderer, NeverCancel, OverlayRenderPlan,
    RenderError, RenderLimits, RgbaSurface,
};

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0; 4];

fn rgba(pixel: [u8; 4]) -> Rgba {
    Rgba {
        red: pixel[0],
        green: pixel[1],
        blue: pixel[2],
        alpha: pixel[3],
    }
}

fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}

fn surface(width: u32, height: u32, pixels: &[[u8; 4]]) -> RgbaSurface {
    RgbaSurface::new(size(width, height), pixels.concat()).unwrap()
}

fn shadow_style() -> ImageShadowStyle {
    ImageShadowStyle {
        blur_radius_hundredths: 0,
        depth_hundredths: 100,
        direction_hundredths: 0,
        opacity_basis_points: 10_000,
        color: rgba(BLUE),
        background: rgba(CLEAR),
    }
}

fn border_style(left: i32, top: i32, right: i32, bottom: i32) -> ImageBorderStyle {
    ImageBorderStyle {
        widths: SignedEdgeWidths {
            left_milli: left,
            top_milli: top,
            right_milli: right,
            bottom_milli: bottom,
        },
        color: rgba(GREEN),
        background: rgba([255; 4]),
    }
}

fn render_shadow(source: &RgbaSurface, style: &ImageShadowStyle) -> RgbaSurface {
    shadow(source, style, RenderLimits::default(), &NeverCancel).unwrap()
}

#[test]
fn cardinal_hard_shadows_follow_wpf_software_offset_and_alpha() {
    let source = surface(1, 1, &[RED]);
    let shadow_pixel = [0, 0, 255, 253];
    for (direction, width, height, expected) in [
        (0, 2, 1, [RED, shadow_pixel]),
        (9_000, 1, 2, [shadow_pixel, RED]),
        (18_000, 2, 1, [shadow_pixel, RED]),
        (27_000, 1, 2, [RED, shadow_pixel]),
    ] {
        let style = ImageShadowStyle {
            direction_hundredths: direction,
            ..shadow_style()
        };
        assert_eq!(
            render_shadow(&source, &style),
            surface(width, height, &expected)
        );
    }
}

#[test]
fn fractional_geometry_is_not_replaced_by_integer_kernel_or_offset() {
    let source = surface(1, 1, &[RED]);
    let style = ImageShadowStyle {
        blur_radius_hundredths: 50,
        depth_hundredths: 199,
        ..shadow_style()
    };
    assert_eq!(style.pixel_offset().unwrap(), (1, 0));
    assert_eq!(
        render_shadow(&source, &style),
        surface(3, 1, &[RED, [0, 0, 255, 253], CLEAR])
    );
    let diagonal = ImageShadowStyle {
        direction_hundredths: 13_500,
        ..style
    };
    assert_eq!(diagonal.pixel_offset().unwrap(), (-1, -1));
    assert_eq!(
        render_shadow(&source, &diagonal),
        surface(2, 2, &[[0, 0, 255, 253], CLEAR, CLEAR, RED])
    );
}

#[test]
fn independent_opacity_ignores_shadow_color_alpha_and_composites_background_last() {
    let source = surface(1, 1, &[RED]);
    let style = ImageShadowStyle {
        opacity_basis_points: 6_000,
        color: rgba([0, 0, 255, 0]),
        ..shadow_style()
    };
    assert_eq!(
        render_shadow(&source, &style),
        surface(2, 1, &[RED, [0, 0, 255, 151]])
    );
    let no_shadow = ImageShadowStyle {
        opacity_basis_points: 0,
        background: rgba(GREEN),
        ..style
    };
    assert_eq!(
        render_shadow(&source, &no_shadow),
        surface(2, 1, &[RED, GREEN])
    );
    let translucent = surface(1, 1, &[[255, 0, 0, 128]]);
    let centered = ImageShadowStyle {
        depth_hundredths: 0,
        background: rgba([255; 4]),
        ..shadow_style()
    };
    assert_eq!(
        render_shadow(&translucent, &centered),
        surface(1, 1, &[[192, 64, 127, 255]])
    );
    assert_eq!(
        shadow_pixel([255, 0, 0, 128], 128, 255, rgba(BLUE), rgba(CLEAR)),
        [171, 0, 84, 191]
    );
}

#[test]
fn gaussian_is_not_box_and_each_pass_uses_nearest_even() {
    let kernel = GaussianKernel::new(1);
    // Equal-error compensation is not division by the sampled sum. At radius
    // one it deliberately produces a >1 center and negative edge weights.
    assert!((kernel.weights()[1] - 1.122_354_1).abs() < 0.000_001);
    assert!((kernel.weights()[0] - -0.061_177_09).abs() < 0.000_001);
    assert_eq!(kernel.weights()[0].to_bits(), kernel.weights()[2].to_bits());
    assert_eq!(alpha_from_float(2.5), 2);
    assert_eq!(alpha_from_float(3.5), 4);
    assert_eq!(GaussianKernel::new(0).weights(), &[1.0]);
    let source = surface(1, 1, &[RED]);
    let style = ImageShadowStyle {
        blur_radius_hundredths: 100,
        depth_hundredths: 100,
        ..shadow_style()
    };
    // Both passes saturate the radius-one center to 255 and negative edges to 0.
    assert_eq!(
        render_shadow(&source, &style),
        surface(3, 2, &[RED, [0, 0, 255, 253], CLEAR, CLEAR, CLEAR, CLEAR])
    );
}

#[test]
fn radius_two_impulse_matches_separate_8bit_vertical_and_horizontal_reference() {
    let source = surface(1, 1, &[RED]);
    let style = ImageShadowStyle {
        blur_radius_hundredths: 200,
        ..shadow_style()
    };
    // Reference kernel center/side/edge: .59836107/.19422404/.006595425.
    // Vertical alpha is 50,153,50; horizontal then rounds each sample again.
    // Software opacity maps those alphas with /65536, not ideal /65025.
    let nine = [0, 0, 255, 9];
    let twenty_nine = [0, 0, 255, 29];
    assert_eq!(
        render_shadow(&source, &style),
        surface(
            4,
            3,
            &[
                CLEAR,
                nine,
                twenty_nine,
                nine,
                CLEAR,
                RED,
                [0, 0, 255, 91],
                twenty_nine,
                CLEAR,
                nine,
                twenty_nine,
                nine,
            ]
        )
    );
}

#[test]
fn mixed_inner_outer_border_preserves_every_pixel_and_source_translation() {
    let source = surface(4, 3, &[RED; 12]);
    let style = border_style(-2_000, 1_000, 0, -1_000);
    let actual = border(&source, &style, RenderLimits::default(), &NeverCancel).unwrap();
    let expected = [
        GREEN, GREEN, GREEN, GREEN, GREEN, GREEN, GREEN, GREEN, RED, RED, RED, RED, GREEN, GREEN,
        RED, RED, RED, RED, GREEN, GREEN, GREEN, GREEN, GREEN, GREEN,
    ];
    assert_eq!(actual, surface(6, 4, &expected));
    assert_eq!(source, surface(4, 3, &[RED; 12]));
}

#[test]
fn transparent_inner_border_still_flattens_source_onto_its_background() {
    let source = surface(3, 3, &[CLEAR; 9]);
    let mut style = border_style(1_000, 1_000, 1_000, 1_000);
    style.color = rgba([0, 255, 0, 128]);
    let edge = [127, 255, 127, 255];
    assert_eq!(
        border(&source, &style, RenderLimits::default(), &NeverCancel).unwrap(),
        surface(
            3,
            3,
            &[edge, edge, edge, edge, [255; 4], edge, edge, edge, edge]
        )
    );
    style.color.alpha = 0;
    assert_eq!(
        border(&source, &style, RenderLimits::default(), &NeverCancel).unwrap(),
        surface(3, 3, &[[255; 4]; 9])
    );
}

#[test]
fn fractional_border_uses_cast_background_extent_not_whole_rounded_canvas() {
    let source = surface(1, 1, &[RED]);
    let mut style = border_style(0, -750, 0, 0);
    style.color = rgba(BLUE);
    // Output height rounds .75 to 1, but source origin and background top
    // contribution truncate to zero. The extra row must not become white.
    assert_eq!(
        border(&source, &style, RenderLimits::default(), &NeverCancel).unwrap(),
        surface(1, 2, &[[64, 0, 191, 255], CLEAR])
    );
    let asymmetric = ImageBorderStyle {
        color: rgba(CLEAR),
        ..border_style(-750, 0, -750, 0)
    };
    // Sum of the two outward widths rounds 1.5 to 2; only left is truncated
    // for the background, leaving .75 coverage and one completely clear pixel.
    assert_eq!(
        border(&source, &asymmetric, RenderLimits::default(), &NeverCancel).unwrap(),
        surface(3, 1, &[RED, [255, 255, 255, 191], CLEAR])
    );
}

#[test]
fn reversed_line_endpoints_and_overlapping_strokes_are_not_silently_dropped() {
    let source = surface(2, 2, &[RED; 4]);
    let mut style = border_style(4_000, 1_000, 0, 0);
    style.color = rgba([0, 255, 0, 128]);
    // Oversized left stroke covers the canvas; reversed top segment 4 -> 2
    // lies outside it. The single pass must not be blended twice at corners.
    assert_eq!(
        border(&source, &style, RenderLimits::default(), &NeverCancel).unwrap(),
        surface(2, 2, &[[127, 128, 0, 255]; 4])
    );
    style.widths.right_milli = 4_000;
    // Left and right really overlap, so each of those two strokes is painted.
    assert_eq!(
        border(&source, &style, RenderLimits::default(), &NeverCancel)
            .unwrap()
            .pixels()[8..],
        [63, 192, 0, 255].repeat(2)
    );
}

fn clip(steps: Vec<FrameRenderStep>) -> FrameClip {
    FrameClip {
        id: FrameId::from_u128(1),
        asset_id: AssetId::from_digest([1; 32]),
        duration: DurationUs::new(1).unwrap(),
        transform: ClipTransform::default(),
        effects: Vec::new(),
        capture_metadata: CaptureMetadata::default(),
        capture_binding: CaptureBinding::Original,
        capture_clock: None,
        render_steps: steps,
    }
}

fn shape_cell(stage: Option<u32>, id: u128, color: [u8; 4]) -> FrameOverlayCell {
    FrameOverlayCell {
        frame_id: FrameId::from_u128(1),
        stage,
        scopes: Vec::new(),
        input_replay: None,
        marks: vec![FrameOverlayMark {
            id: OverlayId::from_u128(id),
            z_index: 0,
            content: OverlayContent::Shape {
                kind: ShapeKind::Rectangle,
                bounds: PhysicalRect::new(0, 0, 1, 1).unwrap(),
                stroke_width: 0,
                stroke: rgba(CLEAR),
                fill: Some(rgba(color)),
            },
        }],
    }
}

#[test]
fn expanding_steps_transform_already_composited_layers_in_both_entrypoints() {
    let owner = clip(vec![
        FrameRenderStep::Composite { stage_id: 1 },
        FrameRenderStep::ImageShadow {
            style: ImageShadowStyle {
                direction_hundredths: 18_000,
                ..shadow_style()
            },
        },
        FrameRenderStep::ImageBorder {
            style: ImageBorderStyle {
                color: rgba(CLEAR),
                background: rgba(CLEAR),
                ..border_style(-1_000, 0, 0, 0)
            },
        },
        FrameRenderStep::Composite { stage_id: 2 },
    ]);
    let tracks = [OverlayTrack {
        id: TrackId::from_u128(1),
        name: "before and after".into(),
        visible: true,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        items: Vec::new(),
        annotation: None,
        annotation_scope: None,
        frame_cells: Some(vec![
            shape_cell(Some(1), 1, RED),
            shape_cell(Some(2), 2, GREEN),
        ]),
    }];
    let provider = |_| -> Result<RgbaSurface, AssetProviderError> { Ok(surface(1, 1, &[CLEAR])) };
    let renderer = CpuRenderer::new();
    let direct = renderer
        .render_clip_with_overlays(&owner, &tracks, TimeUs::ZERO, &provider, &NeverCancel)
        .unwrap();
    assert_eq!(direct, surface(3, 1, &[GREEN, [0, 0, 255, 253], RED]));
    let plan = OverlayRenderPlan::for_frame(&tracks, owner.id, TimeUs::ZERO, &NeverCancel).unwrap();
    assert_eq!(
        renderer
            .render_clip_with_overlay_plan(&owner, &plan, &provider, &NeverCancel)
            .unwrap(),
        direct
    );
    assert_eq!(
        renderer
            .render_clip(&owner, &provider, &NeverCancel)
            .unwrap(),
        surface(3, 1, &[CLEAR; 3])
    );
}

struct CountCancel {
    polls: AtomicUsize,
    allowed: usize,
}
impl CancellationToken for CountCancel {
    fn is_cancelled(&self) -> bool {
        self.polls.fetch_add(1, Ordering::Relaxed) >= self.allowed
    }
}

#[test]
fn output_limits_bad_parameters_and_cancellation_never_return_partial_effects() {
    let source = surface(1, 1, &[RED]);
    let limits = RenderLimits {
        max_surface_bytes: 4,
    };
    assert!(matches!(
        shadow(&source, &shadow_style(), limits, &NeverCancel),
        Err(RenderError::SurfaceLimitExceeded { requested: 8, .. })
    ));
    assert!(matches!(
        border(
            &source,
            &border_style(-1_000, 0, 0, 0),
            limits,
            &NeverCancel
        ),
        Err(RenderError::SurfaceLimitExceeded { requested: 8, .. })
    ));
    let invalid = ImageShadowStyle {
        blur_radius_hundredths: 10_001,
        ..shadow_style()
    };
    assert!(matches!(
        shadow(&source, &invalid, RenderLimits::default(), &NeverCancel),
        Err(RenderError::InvalidImageEffect { .. })
    ));
    for allowed in [0, 2, 5] {
        let cancel = CountCancel {
            polls: AtomicUsize::new(0),
            allowed,
        };
        let style = ImageShadowStyle {
            blur_radius_hundredths: 10_000,
            ..shadow_style()
        };
        assert!(matches!(
            shadow(&source, &style, RenderLimits::default(), &cancel),
            Err(RenderError::Cancelled)
        ));
        let cancel = CountCancel {
            polls: AtomicUsize::new(0),
            allowed,
        };
        assert!(matches!(
            border(
                &source,
                &border_style(-2_000, -2_000, -2_000, -2_000),
                RenderLimits::default(),
                &cancel
            ),
            Err(RenderError::Cancelled)
        ));
    }
    assert_eq!(source, surface(1, 1, &[RED]));
}

#[test]
fn legacy_box_shadow_still_keeps_canvas_and_prior_pixel_rounding() {
    let mut owner = clip(Vec::new());
    owner.effects.push(Effect::Shadow {
        offset_x: 1,
        offset_y: 0,
        blur_radius: 0,
        color: rgba(BLUE),
    });
    let provider =
        |_| -> Result<RgbaSurface, AssetProviderError> { Ok(surface(2, 1, &[RED, CLEAR])) };
    assert_eq!(
        CpuRenderer::new()
            .render_clip(&owner, &provider, &NeverCancel)
            .unwrap(),
        surface(2, 1, &[RED, BLUE])
    );
}

// Deliberately slow per-pixel model: recompute every vertical column for each
// horizontal tap instead of sharing the production intermediate or its indexing.
fn scalar_blur(source: &RgbaSurface, x: i64, y: i64, kernel: &GaussianKernel) -> u8 {
    let radius = i64::try_from(kernel.radius).unwrap();
    let mut result = 0.0;
    for (ix, horizontal_weight) in kernel.weights().iter().enumerate() {
        let source_x = x + i64::try_from(ix).unwrap() - radius;
        let mut column = 0.0;
        for (iy, vertical_weight) in kernel.weights().iter().enumerate() {
            let source_y = y + i64::try_from(iy).unwrap() - radius;
            if source_x >= 0
                && source_y >= 0
                && source_x < i64::from(source.width())
                && source_y < i64::from(source.height())
            {
                let offset =
                    usize::try_from((source_y * i64::from(source.width()) + source_x) * 4 + 3)
                        .unwrap();
                column += vertical_weight * f32::from(source.pixels()[offset]);
            }
        }
        result += horizontal_weight * f32::from(alpha_from_float(column));
    }
    alpha_from_float(result)
}

#[test]
fn buffered_gaussian_matches_scalar_model_for_partial_alpha_and_fractional_directions() {
    let source = surface(
        3,
        2,
        &[
            RED,
            [19, 201, 47, 73],
            CLEAR,
            [67, 88, 210, 161],
            [9, 44, 189, 128],
            GREEN,
        ],
    );
    for blur in [0, 99, 100, 199, 200, 350, 750] {
        for direction in [0, 4_500, 13_500, 35_999] {
            let style = ImageShadowStyle {
                blur_radius_hundredths: blur,
                depth_hundredths: 299,
                direction_hundredths: direction,
                opacity_basis_points: 6_001,
                color: rgba([71, 213, 29, 42]),
                background: rgba([31, 63, 99, 137]),
            };
            let output = render_shadow(&source, &style);
            let placement = style.placement(source.size()).unwrap();
            assert_eq!(output.size(), placement.output_size);
            let offset = style.pixel_offset().unwrap();
            let kernel = GaussianKernel::new(blur / 100);
            for y in 0..output.height() {
                for x in 0..output.width() {
                    let source_x = i64::from(x) - i64::from(placement.source_origin.x.get());
                    let source_y = i64::from(y) - i64::from(placement.source_origin.y.get());
                    let original = if source_x >= 0
                        && source_y >= 0
                        && source_x < i64::from(source.width())
                        && source_y < i64::from(source.height())
                    {
                        let byte =
                            usize::try_from((source_y * i64::from(source.width()) + source_x) * 4)
                                .unwrap();
                        source.pixels()[byte..byte + 4].try_into().unwrap()
                    } else {
                        CLEAR
                    };
                    let blurred = scalar_blur(
                        &source,
                        source_x - i64::from(offset.0),
                        source_y - i64::from(offset.1),
                        &kernel,
                    );
                    let expected = if original[3] == 255 {
                        original
                    } else {
                        shadow_pixel(
                            original,
                            blurred,
                            u32::from(style.opacity_basis_points) * 255 / 10_000,
                            style.color,
                            style.background,
                        )
                    };
                    let byte = usize::try_from((y * output.width() + x) * 4).unwrap();
                    assert_eq!(
                        &output.pixels()[byte..byte + 4],
                        expected,
                        "blur={blur}, direction={direction}, ({x},{y})"
                    );
                }
            }
        }
    }
}

#[test]
fn cancellation_at_every_effect_checkpoint_discards_partial_output() {
    let source = surface(2, 2, &[RED, CLEAR, GREEN, BLUE]);
    let shadow_style = ImageShadowStyle {
        blur_radius_hundredths: 200,
        ..shadow_style()
    };
    let border_style = border_style(-1_000, 1_000, -1_000, 1_000);
    for is_shadow in [true, false] {
        let run = |cancel: &CountCancel| {
            if is_shadow {
                shadow(&source, &shadow_style, RenderLimits::default(), cancel)
            } else {
                border(&source, &border_style, RenderLimits::default(), cancel)
            }
        };
        let counter = CountCancel {
            polls: AtomicUsize::new(0),
            allowed: usize::MAX,
        };
        let expected = run(&counter).unwrap();
        let checkpoints = counter.polls.load(Ordering::Relaxed);
        assert!(checkpoints > 10);
        for allowed in 0..checkpoints {
            let cancel = CountCancel {
                polls: AtomicUsize::new(0),
                allowed,
            };
            assert!(
                matches!(run(&cancel), Err(RenderError::Cancelled)),
                "is_shadow={is_shadow} checkpoint={allowed}"
            );
        }
        assert_eq!(run(&counter).unwrap(), expected);
    }
    assert_eq!(source, surface(2, 2, &[RED, CLEAR, GREEN, BLUE]));
}

#[test]
fn full_parameter_bounds_remain_safe_before_allocation_or_pixel_loops() {
    let source = surface(1, 1, &[RED]);
    let limit = RenderLimits {
        max_surface_bytes: 4,
    };
    for side in [i32::MIN, -1_000_000] {
        assert!(matches!(
            border(&source, &border_style(side, 0, 0, 0), limit, &NeverCancel),
            Err(RenderError::SurfaceLimitExceeded { .. })
        ));
    }
    assert_eq!(
        border(
            &source,
            &border_style(i32::MAX, 0, 0, 0),
            limit,
            &NeverCancel
        )
        .unwrap(),
        surface(1, 1, &[GREEN])
    );
    assert!(matches!(
        super::allocate_alpha(5, limit),
        Err(RenderError::EffectWorkingMemoryLimitExceeded {
            requested: 5,
            limit: 4,
            ..
        })
    ));
    let maximum = ImageShadowStyle {
        blur_radius_hundredths: 10_000,
        depth_hundredths: 10_000,
        direction_hundredths: 36_000,
        opacity_basis_points: 10_000,
        ..shadow_style()
    };
    let output = render_shadow(&source, &maximum);
    assert_eq!(
        output.size(),
        maximum.placement(source.size()).unwrap().output_size
    );
}
