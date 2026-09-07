use crate::{InkError, InkPoint};
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct V {
    pub x: f64,
    pub y: f64,
}
impl V {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y
    }
    pub fn cross(self, other: Self) -> f64 {
        self.x * other.y - self.y * other.x
    }
    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }
    pub fn normalized(self) -> Self {
        let scaled = self / self.x.abs().max(self.y.abs());
        scaled / scaled.length()
    }
    pub fn point(self) -> Result<InkPoint, InkError> {
        if self.x.is_finite() && self.y.is_finite() {
            Ok(InkPoint {
                x: self.x,
                y: self.y,
            })
        } else {
            Err(super::invalid("non-finite generated geometry"))
        }
    }
}
impl From<InkPoint> for V {
    fn from(point: InkPoint) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}
impl Add for V {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}
impl AddAssign for V {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}
impl Sub for V {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}
impl Mul<f64> for V {
    type Output = Self;
    fn mul(self, value: f64) -> Self {
        Self::new(self.x * value, self.y * value)
    }
}
impl Div<f64> for V {
    type Output = Self;
    fn div(self, value: f64) -> Self {
        self * (1.0 / value)
    }
}
impl Neg for V {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}
pub(super) fn close(first: f64, last: f64) -> bool {
    if first.to_bits() == last.to_bits() {
        true
    } else {
        (first - last).abs() < (first.abs() + last.abs() + 10.0) * f64::EPSILON
    }
}
pub(super) fn zero(value: f64) -> bool {
    value.abs() < 10.0 * f64::EPSILON
}
