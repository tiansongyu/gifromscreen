use super::*;

fn size(width: u32, height: u32) -> PhysicalSize {
    PhysicalSize::new(width, height).unwrap()
}

#[test]
fn border_rounds_the_sum_ties_to_even_and_floors_only_source_origin() {
    for (left, right, expansion, origin) in [
        (-250, -250, 0, 0),
        (-1_250, -250, 2, 1),
        (-2_250, -250, 2, 2),
        (-3_250, -250, 4, 3),
        (-1_999, 25_000, 2, 1),
        (50_000, 20_000, 0, 0),
    ] {
        let style = ImageBorderStyle {
            widths: SignedEdgeWidths {
                left_milli: left,
                right_milli: right,
                top_milli: -2_900,
                bottom_milli: 3_000,
            },
            ..ImageBorderStyle::default()
        };
        style.validate().unwrap();
        let placement = style.placement(size(20, 10)).unwrap();
        assert_eq!(placement.output_size, size(20 + expansion, 13));
        assert_eq!(placement.source_origin.x.get(), origin);
        assert_eq!(placement.source_origin.y.get(), 2);
    }
}

#[test]
fn signed_minimum_is_supported_without_abs_overflow_and_canvas_overflow_is_rejected() {
    let style = ImageBorderStyle {
        widths: SignedEdgeWidths {
            left_milli: i32::MIN,
            right_milli: i32::MIN,
            top_milli: i32::MAX,
            bottom_milli: 0,
        },
        color: Rgba::TRANSPARENT,
        background: Rgba::TRANSPARENT,
    };
    style.validate().unwrap();
    let placement = style.placement(size(20, 10)).unwrap();
    assert_eq!(placement.output_size, size(4_294_987, 10));
    assert_eq!(placement.source_origin.x.get(), 2_147_483);
    assert!(style.placement(size(u32::MAX, 1)).is_err());
    assert!(
        style
            .placement(PhysicalSize {
                width: PhysicalPx::new(0),
                height: PhysicalPx::new(1)
            })
            .is_err()
    );
}

#[test]
fn shadow_preserves_polar_precision_separate_from_f32_pixel_sampling() {
    let style = ImageShadowStyle {
        blur_radius_hundredths: 0,
        depth_hundredths: 100,
        direction_hundredths: 1,
        ..ImageShadowStyle::default()
    };
    let (x, y) = style.offset().unwrap();
    assert!(x < 1.0 && x > 0.999_999_9);
    assert!(y < 0.0);
    // The reference software path narrows this near-axis offset to exactly
    // 1f32, but its allocation still sees the original f64 value below one.
    assert_eq!(style.pixel_offset().unwrap(), (1, 0));
    assert_eq!(
        style.placement(size(20, 10)).unwrap().output_size,
        size(20, 10)
    );

    let diagonal = ImageShadowStyle {
        depth_hundredths: 200,
        direction_hundredths: 4_500,
        ..style
    };
    assert_eq!(diagonal.pixel_offset().unwrap(), (1, -1));
    let placed = diagonal.placement(size(20, 10)).unwrap();
    assert_eq!(placed.output_size, size(21, 11));
    assert_eq!(
        placed.source_origin,
        PhysicalPoint {
            x: PhysicalPx::new(0),
            y: PhysicalPx::new(1)
        }
    );
}

#[test]
fn fractional_shadow_margins_do_not_use_integer_kernel_or_opacity() {
    let style = ImageShadowStyle {
        blur_radius_hundredths: 199,
        depth_hundredths: 0,
        direction_hundredths: 0,
        opacity_basis_points: 0,
        color: Rgba::TRANSPARENT,
        background: Rgba::TRANSPARENT,
    };
    let placement = style.placement(size(20, 10)).unwrap();
    assert_eq!(placement.output_size, size(21, 11));
    assert_eq!(
        placement.source_origin,
        PhysicalPoint {
            x: PhysicalPx::new(0),
            y: PhysicalPx::new(0)
        }
    );
    assert!(style.placement(size(u32::MAX, 1)).is_err());
}

#[test]
fn shadow_ranges_and_original_integer_wire_parameters_round_trip() {
    let valid = ImageShadowStyle {
        blur_radius_hundredths: 10_000,
        depth_hundredths: 10_000,
        direction_hundredths: 36_000,
        opacity_basis_points: 10_000,
        ..ImageShadowStyle::default()
    };
    valid.validate().unwrap();
    let bytes = serde_json::to_vec(&valid).unwrap();
    assert_eq!(
        serde_json::from_slice::<ImageShadowStyle>(&bytes).unwrap(),
        valid
    );
    for invalid in [
        ImageShadowStyle {
            blur_radius_hundredths: 10_001,
            ..valid
        },
        ImageShadowStyle {
            depth_hundredths: 10_001,
            ..valid
        },
        ImageShadowStyle {
            direction_hundredths: 36_001,
            ..valid
        },
        ImageShadowStyle {
            opacity_basis_points: 10_001,
            ..valid
        },
    ] {
        assert!(invalid.validate().is_err());
        assert!(invalid.offset().is_err());
        assert!(invalid.pixel_offset().is_err());
        assert!(invalid.placement(size(20, 10)).is_err());
    }
}

#[test]
fn shared_defaults_match_authoring_and_automatic_task_initial_values() {
    let border = ImageBorderStyle::default();
    assert_eq!(SignedEdgeWidths::default().left_milli, 0);
    assert_eq!(
        border.widths,
        SignedEdgeWidths {
            top_milli: 1_000,
            right_milli: 1_000,
            bottom_milli: 1_000,
            left_milli: 1_000
        }
    );
    assert_eq!(border.color.alpha, 255);
    assert_eq!(border.background.red, 255);
    let shadow = ImageShadowStyle::default();
    assert_eq!(
        (
            shadow.blur_radius_hundredths,
            shadow.depth_hundredths,
            shadow.direction_hundredths,
            shadow.opacity_basis_points
        ),
        (1_000, 1_000, 0, 6_000)
    );
    assert_eq!(
        shadow.placement(size(20, 10)).unwrap().output_size,
        size(40, 20)
    );
}
