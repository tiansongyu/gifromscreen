//! Versioned object geometry, separate from the unchanged legacy `Shape` pixels.
//!
//! Units are hundredths of a physical image pixel at the owning paint stage.
//! They never depend on display DPI, viewport scale or editor zoom.
//! The new shape, bounds and color payloads reject unknown JSON fields. The
//! surrounding `OverlayContent` envelope retains its legacy serde behavior;
//! geometry extensions belong inside this versioned payload. After decoding,
//! [`VectorShape::validate`] checks supported version and numerical invariants.

use serde::{Deserialize, Deserializer, Serialize};

use crate::Rgba;

/// First vector-shape geometry and rendering contract.
pub const VECTOR_SHAPE_VERSION: u8 = 1;
/// Minimum project schema which understands the new overlay content.
pub const VECTOR_SHAPE_SCHEMA_VERSION: u32 = 8;
/// Twice the maximum GIF axis (65,535 pixels), allowing a full canvas of overscan.
pub const MAX_VECTOR_SHAPE_EXTENT_HUNDREDTHS: u64 = 13_107_000;
/// Both origin and unrotated checked end must fit this signed stage-coordinate range.
pub const MAX_VECTOR_SHAPE_COORDINATE_HUNDREDTHS: i64 = 13_107_000;
/// The reference UI permits stroke width and rectangle radius from 0 through 100 pixels.
pub const MAX_VECTOR_SHAPE_STYLE_HUNDREDTHS: u32 = 10_000;
/// Canonical angles are clockwise, zero inclusive and one turn exclusive.
pub const VECTOR_SHAPE_TURN_HUNDREDTHS: u16 = 36_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorShapeKind {
    #[default]
    Rectangle,
    Ellipse,
    Triangle,
    /// Closed, right-pointing arrow; never the legacy line-segment `ShapeKind::Arrow`.
    BlockArrow,
}

/// Axis-aligned object layout before rotation around its center.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorShapeBounds {
    pub x_hundredths: i64,
    pub y_hundredths: i64,
    pub width_hundredths: u64,
    pub height_hundredths: u64,
}

impl Default for VectorShapeBounds {
    fn default() -> Self {
        Self {
            x_hundredths: 0,
            y_hundredths: 0,
            width_hundredths: 10_000,
            height_hundredths: 10_000,
        }
    }
}

impl VectorShapeBounds {
    pub fn end_x_hundredths(self) -> Option<i64> {
        self.x_hundredths
            .checked_add(i64::try_from(self.width_hundredths).ok()?)
    }

    pub fn end_y_hundredths(self) -> Option<i64> {
        self.y_hundredths
            .checked_add(i64::try_from(self.height_hundredths).ok()?)
    }

    /// Accepts negative and fully clipped positions, but never empty or unbounded geometry.
    pub fn validate(self) -> Result<(), String> {
        if self.width_hundredths == 0 || self.height_hundredths == 0 {
            return Err(
                "Vector shape width and height must be positive hundredths of a pixel.".into(),
            );
        }
        let end_x = self
            .end_x_hundredths()
            .ok_or("Vector shape X end overflows i64.")?;
        let end_y = self
            .end_y_hundredths()
            .ok_or("Vector shape Y end overflows i64.")?;
        if self.width_hundredths > MAX_VECTOR_SHAPE_EXTENT_HUNDREDTHS
            || self.height_hundredths > MAX_VECTOR_SHAPE_EXTENT_HUNDREDTHS
        {
            return Err(format!(
                "Vector shape dimensions must not exceed {MAX_VECTOR_SHAPE_EXTENT_HUNDREDTHS} hundredths (twice the maximum GIF axis)."
            ));
        }
        let allowed =
            -MAX_VECTOR_SHAPE_COORDINATE_HUNDREDTHS..=MAX_VECTOR_SHAPE_COORDINATE_HUNDREDTHS;
        if ![self.x_hundredths, self.y_hundredths, end_x, end_y]
            .into_iter()
            .all(|value| allowed.contains(&value))
        {
            return Err(format!(
                "Vector shape origins and ends must lie within ±{MAX_VECTOR_SHAPE_COORDINATE_HUNDREDTHS} hundredths of a pixel."
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorShape {
    pub version: u8,
    pub kind: VectorShapeKind,
    pub bounds: VectorShapeBounds,
    pub stroke_width_hundredths: u32,
    #[serde(deserialize_with = "deserialize_color")]
    pub stroke: Rgba,
    #[serde(default, deserialize_with = "deserialize_optional_color")]
    pub fill: Option<Rgba>,
    /// Used only by Rectangle; other kinds retain this shared style value without applying it.
    pub corner_radius_hundredths: u32,
    /// Clockwise about the bounds center, in the canonical range 0..36,000.
    pub rotation_hundredths: u16,
}

impl Default for VectorShape {
    fn default() -> Self {
        Self {
            version: VECTOR_SHAPE_VERSION,
            kind: VectorShapeKind::Rectangle,
            bounds: VectorShapeBounds::default(),
            stroke_width_hundredths: 400,
            stroke: Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            fill: Some(Rgba::TRANSPARENT),
            corner_radius_hundredths: 0,
            rotation_hundredths: 0,
        }
    }
}

impl VectorShape {
    /// Validates metadata only; invisible styles and zero-width strokes are intentional.
    pub fn validate(self) -> Result<(), String> {
        if self.version != VECTOR_SHAPE_VERSION {
            return Err(format!(
                "Unsupported vector shape version {}; expected {VECTOR_SHAPE_VERSION}.",
                self.version
            ));
        }
        self.bounds.validate()?;
        if self.stroke_width_hundredths > MAX_VECTOR_SHAPE_STYLE_HUNDREDTHS {
            return Err("Vector shape stroke width must be between 0 and 100 pixels.".into());
        }
        if self.corner_radius_hundredths > MAX_VECTOR_SHAPE_STYLE_HUNDREDTHS {
            return Err("Vector shape corner radius must be between 0 and 100 pixels.".into());
        }
        if self.rotation_hundredths >= VECTOR_SHAPE_TURN_HUNDREDTHS {
            return Err(
                "Vector shape rotation must be clockwise and in 0..360 degrees (exclusive).".into(),
            );
        }
        Ok(())
    }
}

// Preserve legacy Rgba deserialization. Strict color fields apply only inside
// this new versioned payload, where silently dropping style fields is unsafe.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ColorWire {
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

impl From<ColorWire> for Rgba {
    fn from(color: ColorWire) -> Self {
        Self {
            red: color.red,
            green: color.green,
            blue: color.blue,
            alpha: color.alpha,
        }
    }
}

fn deserialize_color<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Rgba, D::Error> {
    ColorWire::deserialize(deserializer).map(Into::into)
}

fn deserialize_optional_color<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Rgba>, D::Error> {
    Option::<ColorWire>::deserialize(deserializer).map(|color| color.map(Into::into))
}

#[cfg(test)]
#[path = "vector_shape_tests.rs"]
mod tests;
