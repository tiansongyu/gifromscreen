//! Exact persisted progress ratios; legacy overlays can retain their original millionths.

use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

/// An immutable ratio in the inclusive range zero through one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FractionWire")]
pub struct ProgressFraction {
    numerator: u64,
    denominator: NonZeroU64,
}

#[derive(Deserialize)]
struct FractionWire {
    numerator: u64,
    denominator: u64,
}

impl TryFrom<FractionWire> for ProgressFraction {
    type Error = &'static str;
    fn try_from(value: FractionWire) -> Result<Self, Self::Error> {
        Self::new(value.numerator, value.denominator)
            .ok_or("Progress fraction requires denominator > 0 and numerator <= denominator.")
    }
}

impl ProgressFraction {
    /// Returns `None` for a zero denominator or a ratio greater than one.
    pub fn new(numerator: u64, denominator: u64) -> Option<Self> {
        if numerator > denominator {
            return None;
        }
        Some(Self {
            numerator,
            denominator: NonZeroU64::new(denominator)?,
        })
    }

    /// Scale and round to nearest, with exact midpoint ties going to the even integer.
    pub fn scaled_rounded(self, scale: u32) -> u32 {
        let denominator = u128::from(self.denominator.get());
        let scaled = u128::from(self.numerator) * u128::from(scale);
        let quotient = scaled / denominator;
        let remainder = scaled % denominator;
        let up = remainder * 2 > denominator || remainder * 2 == denominator && quotient & 1 == 1;
        u32::try_from(quotient + u128::from(up)).expect("validated ratio rounds within scale")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_midpoints_and_large_products_do_not_lose_precision() {
        let sixth = ProgressFraction::new(1, 6).unwrap();
        assert_eq!(sixth.scaled_rounded(3), 0);
        assert_eq!(sixth.scaled_rounded(9), 2);
        assert_eq!(
            ProgressFraction::new(2, 3).unwrap().scaled_rounded(1000),
            667
        );
        assert_eq!(
            ProgressFraction::new(u64::MAX, u64::MAX)
                .unwrap()
                .scaled_rounded(u32::MAX),
            u32::MAX
        );
        assert_eq!(
            ProgressFraction::new(0, u64::MAX)
                .unwrap()
                .scaled_rounded(u32::MAX),
            0
        );
    }

    #[test]
    fn invalid_ratios_cannot_be_constructed_or_loaded() {
        assert!(ProgressFraction::new(0, 0).is_none());
        assert!(ProgressFraction::new(2, 1).is_none());
        for json in [
            r#"{"numerator":0,"denominator":0}"#,
            r#"{"numerator":2,"denominator":1}"#,
        ] {
            assert!(serde_json::from_str::<ProgressFraction>(json).is_err());
        }
        let fraction = ProgressFraction::new(1, 6).unwrap();
        let json = serde_json::to_string(&fraction).unwrap();
        assert_eq!(
            serde_json::from_str::<ProgressFraction>(&json).unwrap(),
            fraction
        );
    }

    #[test]
    fn legacy_styles_omit_fraction_and_invalid_new_styles_are_rejected() {
        let legacy = r#"{"amount_millionths":333333,"direction":"left_to_right","label":null,"label_position":{"x":0,"y":0},"label_text":""}"#;
        let style: crate::ProgressStyle = serde_json::from_str(legacy).unwrap();
        assert_eq!(style.fraction, None);
        assert!(
            serde_json::to_value(&style)
                .unwrap()
                .get("fraction")
                .is_none()
        );
        let mut value = serde_json::to_value(style).unwrap();
        value["fraction"] = serde_json::json!({"numerator":1,"denominator":0});
        assert!(serde_json::from_value::<crate::ProgressStyle>(value).is_err());
    }
}
