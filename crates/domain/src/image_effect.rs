//! Expanded-image effects retain their authored parameters instead of changing
//! the meaning of the legacy inset Border and clipped Shadow effects.

use serde::{Deserialize, Serialize};

use crate::{PhysicalPoint, PhysicalPx, PhysicalSize, Rgba};

/// Signed thousandths of a physical pixel: negative expands outward, positive
/// draws inward. Mixed signs on opposite edges are intentional.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignedEdgeWidths {
    pub top_milli: i32,
    pub right_milli: i32,
    pub bottom_milli: i32,
    pub left_milli: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageBorderStyle {
    pub widths: SignedEdgeWidths,
    pub color: Rgba,
    pub background: Rgba,
}

/// Original hundredth-pixel/degree parameters are persisted, not rounded
/// Cartesian offsets. Shadow color alpha is ignored by the reference effect;
/// its independent opacity controls the shadow, while background keeps alpha.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageShadowStyle {
    pub blur_radius_hundredths: u16,
    pub depth_hundredths: u16,
    pub direction_hundredths: u16,
    pub opacity_basis_points: u16,
    pub color: Rgba,
    pub background: Rgba,
}

/// Canvas and integer source placement shared by geometry, authoring and pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CanvasPlacement {
    pub output_size: PhysicalSize,
    pub source_origin: PhysicalPoint,
}

impl Default for ImageBorderStyle {
    fn default() -> Self {
        Self {
            widths: SignedEdgeWidths {
                top_milli: 1_000,
                right_milli: 1_000,
                bottom_milli: 1_000,
                left_milli: 1_000,
            },
            color: Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            background: Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
        }
    }
}

impl Default for ImageShadowStyle {
    fn default() -> Self {
        Self {
            blur_radius_hundredths: 1_000,
            depth_hundredths: 1_000,
            direction_hundredths: 0,
            opacity_basis_points: 6_000,
            color: Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            background: Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
        }
    }
}

impl ImageBorderStyle {
    /// Validates the style independently of a particular image. All signed
    /// edge values and RGBA colors are supported; the actual canvas size is
    /// additionally checked by [`Self::placement`].
    ///
    /// # Errors
    /// Returns a geometry error if the style cannot place even a one-pixel image.
    pub fn validate(self) -> Result<(), String> {
        self.placement(PhysicalSize {
            width: PhysicalPx::new(1),
            height: PhysicalPx::new(1),
        })
        .map(|_| ())
    }

    /// Uses the reference bitmap's ties-to-even rounding of the summed
    /// exterior edges. The source origin is independently floored. This does
    /// not describe the different background rectangle/line raster geometry.
    ///
    /// # Errors
    /// Rejects invalid input dimensions or output dimensions beyond u32.
    pub fn placement(self, input: PhysicalSize) -> Result<CanvasPlacement, String> {
        input
            .validate()
            .map_err(|error| format!("Image border input: {error}"))?;
        let left = exterior_milli(self.widths.left_milli);
        let right = exterior_milli(self.widths.right_milli);
        let top = exterior_milli(self.widths.top_milli);
        let bottom = exterior_milli(self.widths.bottom_milli);
        let horizontal = left
            .checked_add(right)
            .ok_or("Image border width overflows.")?;
        let vertical = top
            .checked_add(bottom)
            .ok_or("Image border height overflows.")?;
        let width = expanded_dimension(input.width.get(), round_milli_even(horizontal)?, "width")?;
        let height = expanded_dimension(input.height.get(), round_milli_even(vertical)?, "height")?;
        Ok(CanvasPlacement {
            output_size: PhysicalSize::new(width, height).map_err(|error| error.to_string())?,
            source_origin: point_from_u64(left / 1_000, top / 1_000)?,
        })
    }
}

impl ImageShadowStyle {
    /// Validates the reference UI's original two-decimal parameter ranges.
    ///
    /// # Errors
    /// Radius/depth must be 0..=100 pixels, direction 0..=360 degrees and
    /// opacity 0..=1, retaining their hundredth/basis-point precision.
    pub fn validate(self) -> Result<(), String> {
        if self.blur_radius_hundredths > 10_000 {
            return Err("Image shadow blur radius must be between 0 and 100 pixels.".to_owned());
        }
        if self.depth_hundredths > 10_000 {
            return Err("Image shadow depth must be between 0 and 100 pixels.".to_owned());
        }
        if self.direction_hundredths > 36_000 {
            return Err("Image shadow direction must be between 0 and 360 degrees.".to_owned());
        }
        if self.opacity_basis_points > 10_000 {
            return Err("Image shadow opacity must be between 0 and 100 percent.".to_owned());
        }
        Ok(())
    }

    /// Exact shared floating-point polar conversion. Positive raster Y is down.
    /// Deliberately does not snap axes or round offsets to milli-pixels.
    ///
    /// # Errors
    /// Rejects invalid authored parameters.
    pub fn offset(self) -> Result<(f64, f64), String> {
        self.validate()?;
        let radians = std::f64::consts::PI / 180.0 * (f64::from(self.direction_hundredths) / 100.0);
        let depth = f64::from(self.depth_hundredths) / 100.0;
        Ok((radians.cos() * depth, -radians.sin() * depth))
    }

    /// Software shadow sampling narrows offsets to f32, then truncates toward
    /// zero, separately from the f64 margins used to allocate the image.
    ///
    /// # Errors
    /// Rejects invalid authored parameters.
    pub fn pixel_offset(self) -> Result<(i32, i32), String> {
        let (x, y) = self.offset()?;
        // WPF CalculateOffset writes float before ApplyEffectSw casts to int.
        // Validated depth <=100 bounds both finite offsets far inside i32.
        Ok(((x as f32).trunc() as i32, (y as f32).trunc() as i32))
    }

    /// Floors the reference's fractional margins plus input size. The margin
    /// radius retains hundredths even when the software blur kernel uses its
    /// integer-pixel part.
    ///
    /// # Errors
    /// Rejects invalid parameters/input or dimensions outside u32.
    pub fn placement(self, input: PhysicalSize) -> Result<CanvasPlacement, String> {
        input
            .validate()
            .map_err(|error| format!("Image shadow input: {error}"))?;
        let (x, y) = self.offset()?;
        let half_blur = f64::from(self.blur_radius_hundredths) / 100.0 / 2.0;
        let left = half_blur + (-x).max(0.0);
        let right = half_blur + x.max(0.0);
        let top = half_blur + (-y).max(0.0);
        let bottom = half_blur + y.max(0.0);
        let width = floored_u32(left + f64::from(input.width.get()) + right, "width")?;
        let height = floored_u32(top + f64::from(input.height.get()) + bottom, "height")?;
        Ok(CanvasPlacement {
            output_size: PhysicalSize::new(width, height).map_err(|error| error.to_string())?,
            source_origin: PhysicalPoint {
                x: PhysicalPx::new(floored_u32(left, "source X")?),
                y: PhysicalPx::new(floored_u32(top, "source Y")?),
            },
        })
    }
}

fn exterior_milli(value: i32) -> u64 {
    // Widen before absolute magnitude, including the valid i32::MIN edge.
    i64::from(value).min(0).unsigned_abs()
}

fn round_milli_even(value: u64) -> Result<u64, String> {
    let whole = value / 1_000;
    let remainder = value % 1_000;
    if remainder > 500 || remainder == 500 && !whole.is_multiple_of(2) {
        whole
            .checked_add(1)
            .ok_or_else(|| "Rounded border extent overflows.".to_owned())
    } else {
        Ok(whole)
    }
}

fn expanded_dimension(input: u32, expansion: u64, axis: &str) -> Result<u32, String> {
    u64::from(input)
        .checked_add(expansion)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("Expanded image {axis} exceeds u32 dimensions."))
}

fn point_from_u64(x: u64, y: u64) -> Result<PhysicalPoint, String> {
    Ok(PhysicalPoint {
        x: PhysicalPx::new(u32::try_from(x).map_err(|_| "Image source X exceeds u32.")?),
        y: PhysicalPx::new(u32::try_from(y).map_err(|_| "Image source Y exceeds u32.")?),
    })
}

fn floored_u32(value: f64, name: &str) -> Result<u32, String> {
    let value = value.floor();
    if !value.is_finite() || !(0.0..=f64::from(u32::MAX)).contains(&value) {
        return Err(format!("Expanded image {name} exceeds u32 dimensions."));
    }
    Ok(value as u32)
}

#[cfg(test)]
#[path = "image_effect_tests.rs"]
mod tests;
