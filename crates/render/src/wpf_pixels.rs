//! Explicit WPF/WIC 8-bit pixel boundaries, never implicit legacy conversions.
//!
//! Reference: dotnet/wpf a04736ac `common/shared/pixelformatutils.cpp/.h`
//! (`UnpremultiplyTable`, `Unpremultiply`, `MyPremultiply`) and `soblend.cpp`.
//! The reciprocal rule also matches all 1,000 real stage PBGRA/RGBA pixels from
//! hosted WPF reference run 34071253697. Pixel arrays here use RGBA byte order,
//! even while their RGB channels are premultiplied.

/// Rounded channel multiplication from WPF's integer premultiplication path.
pub(crate) fn mul_byte(channel: u8, alpha: u8) -> u8 {
    let product = u32::from(channel) * u32::from(alpha) + 128;
    u8::try_from((product + (product >> 8)) >> 8).expect("unit-alpha multiplication remains u8")
}

/// Converts straight sRGB RGBA8 into WPF's premultiplied channel values.
pub(crate) fn premultiply(pixel: [u8; 4]) -> [u8; 4] {
    [
        mul_byte(pixel[0], pixel[3]),
        mul_byte(pixel[1], pixel[3]),
        mul_byte(pixel[2], pixel[3]),
        pixel[3],
    ]
}

/// Converts premultiplied RGBA8 with the published 16-bit reciprocal table rule.
/// This is lossy: e.g. an alpha-253 fully blue shadow decodes to blue 254, not 255.
pub(crate) fn unpremultiply(pixel: [u8; 4]) -> [u8; 4] {
    let alpha = u32::from(pixel[3]);
    if alpha == 0 {
        return [0; 4];
    }
    if alpha == 255 {
        return pixel;
    }
    let reciprocal = (255 << 16) / alpha;
    let channel = |value: u8| {
        u8::try_from(((u32::from(value) * reciprocal) >> 16).min(255))
            .expect("fixed-reciprocal color is clamped to u8")
    };
    [
        channel(pixel[0]),
        channel(pixel[1]),
        channel(pixel[2]),
        pixel[3],
    ]
}

/// Source-over of already premultiplied pixels, with WPF's separate rounding.
pub(crate) fn over(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
    let inverse = 255 - source[3];
    std::array::from_fn(|channel| {
        source[channel].saturating_add(mul_byte(destination[channel], inverse))
    })
}

#[cfg(test)]
mod tests {
    use super::{mul_byte, over, premultiply, unpremultiply};

    #[test]
    fn channel_multiplication_is_bounded_and_matches_exact_nearest_integer() {
        for alpha in 0..=255_u8 {
            for channel in 0..=255_u8 {
                let result = mul_byte(channel, alpha);
                assert_eq!(
                    u32::from(result),
                    (u32::from(channel) * u32::from(alpha) + 127) / 255
                );
                assert!(result <= alpha);
            }
        }
    }

    #[test]
    fn fixed_reciprocal_matches_real_wpf_codec_observations() {
        for (premultiplied, rgba) in [
            ([0, 0, 253, 253], [0, 0, 254, 253]),
            ([0, 0, 127, 127], [0, 0, 254, 127]),
            ([0, 0, 63, 63], [0, 0, 254, 63]),
            ([10, 35, 95, 128], [19, 69, 189, 128]),
            ([15, 40, 110, 135], [28, 75, 207, 135]),
            ([0, 1, 4, 34], [0, 7, 30, 34]),
        ] {
            assert_eq!(unpremultiply(premultiplied), rgba);
        }
    }

    #[test]
    fn transparent_opaque_and_superluminous_inputs_never_overflow() {
        assert_eq!(premultiply([255, 17, 39, 0]), [0; 4]);
        assert_eq!(unpremultiply([255, 17, 39, 0]), [0; 4]);
        assert_eq!(unpremultiply([255, 17, 39, 255]), [255, 17, 39, 255]);
        assert_eq!(unpremultiply([255, 255, 255, 1]), [255, 255, 255, 1]);
        assert_eq!(over([255; 4], [255; 4]), [255; 4]);
        assert_eq!(over([0; 4], [31, 63, 99, 137]), [31, 63, 99, 137]);
    }

    #[test]
    fn border_channels_are_rounded_before_source_over_not_after_combining_products() {
        let image_on_white = over(premultiply([190, 25, 71, 128]), [255; 4]);
        assert_eq!(image_on_white, [222, 140, 163, 255]);
        assert_eq!(
            over(premultiply([40, 80, 180, 128]), image_on_white),
            [131, 110, 171, 255]
        );
    }
}
