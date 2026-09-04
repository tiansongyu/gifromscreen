use std::{fmt, num::NonZeroU64};

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::UnitError;

/// Monotonic project-relative time in microseconds.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TimeUs(u64);

impl TimeUs {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_add_duration(self, duration: DurationUs) -> Option<Self> {
        self.0.checked_add(duration.get()).map(Self)
    }
}

/// A strictly positive duration in microseconds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurationUs(NonZeroU64);

impl DurationUs {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for DurationUs {
    type Error = UnitError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(UnitError::ZeroDuration)
    }
}

/// Physical pixels. Zero is meaningful for coordinates, while sizes validate
/// that both of their `PhysicalPx` components are non-zero.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PhysicalPx(u32);

impl PhysicalPx {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct PhysicalSize {
    pub width: PhysicalPx,
    pub height: PhysicalPx,
}

impl PhysicalSize {
    pub fn new(width: u32, height: u32) -> Result<Self, UnitError> {
        let size = Self {
            width: PhysicalPx::new(width),
            height: PhysicalPx::new(height),
        };
        size.validate()?;
        Ok(size)
    }

    pub fn validate(self) -> Result<(), UnitError> {
        if self.width.get() == 0 || self.height.get() == 0 {
            return Err(UnitError::EmptyPhysicalSize);
        }
        self.area()
            .ok_or(UnitError::PhysicalAreaOverflow)
            .map(|_| ())
    }

    pub fn area(self) -> Option<u64> {
        u64::from(self.width.get()).checked_mul(u64::from(self.height.get()))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct PhysicalPoint {
    pub x: PhysicalPx,
    pub y: PhysicalPx,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct PhysicalRect {
    pub origin: PhysicalPoint,
    pub size: PhysicalSize,
}

impl PhysicalRect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Result<Self, UnitError> {
        let rect = Self {
            origin: PhysicalPoint {
                x: PhysicalPx::new(x),
                y: PhysicalPx::new(y),
            },
            size: PhysicalSize::new(width, height)?,
        };
        rect.end_x().ok_or(UnitError::PhysicalCoordinateOverflow)?;
        rect.end_y().ok_or(UnitError::PhysicalCoordinateOverflow)?;
        Ok(rect)
    }

    pub fn end_x(self) -> Option<u32> {
        self.origin.x.get().checked_add(self.size.width.get())
    }

    pub fn end_y(self) -> Option<u32> {
        self.origin.y.get().checked_add(self.size.height.get())
    }

    pub fn fits_within(self, size: PhysicalSize) -> bool {
        matches!((self.end_x(), self.end_y()), (Some(x), Some(y)) if x <= size.width.get() && y <= size.height.get())
    }
}

/// Logical UI points. This type must not be used in persisted frame geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicalPt(f64);

impl LogicalPt {
    pub fn new(value: f64) -> Result<Self, UnitError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(UnitError::NonFiniteLogicalPoint)
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

/// Strictly positive physical-pixels-per-logical-point conversion factor.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ScaleFactor(f64);

impl ScaleFactor {
    pub fn new(value: f64) -> Result<Self, UnitError> {
        if value.is_finite() && value > 0.0 {
            Ok(Self(value))
        } else {
            Err(UnitError::InvalidScaleFactor)
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ScaleFactor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// Wall-clock timestamp used only for user-facing project metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixTimeMs(i64);

impl UnixTimeMs {
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Monotonic project revision, incremented once per committed edit command.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ProjectRevision(u64);

impl ProjectRevision {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl fmt::Display for ProjectRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_boundary_is_checked_without_overflow() {
        let rect = PhysicalRect::new(u32::MAX, 0, 1, 1);
        assert_eq!(rect, Err(UnitError::PhysicalCoordinateOverflow));
    }

    #[test]
    fn duration_rejects_zero_even_when_deserializing() {
        assert!(serde_json::from_str::<DurationUs>("0").is_err());
    }

    #[test]
    fn scale_factor_rejects_invalid_json_values() {
        assert!(serde_json::from_str::<ScaleFactor>("0.0").is_err());
        assert_eq!(
            serde_json::from_str::<ScaleFactor>("2.0").unwrap().get(),
            2.0
        );
    }
}
