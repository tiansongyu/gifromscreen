//! Frame-owned, already authored marks. Scope fractions never change playback pixels.

use std::{cmp::Ordering, num::NonZeroU64};

use serde::{Deserialize, Serialize};

use crate::{DurationUs, FrameId, OverlayContent, OverlayId, TimeUs, TimelineSpan};

pub const MAX_FRAME_OVERLAY_CELLS: usize = 40_000;
pub const MAX_FRAME_OVERLAY_MARKS: usize = 100_000;

/// Canonical, exact position in a frame, in the inclusive range zero through one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FractionWire")]
pub struct FrameFraction {
    numerator: u64,
    denominator: NonZeroU64,
}

#[derive(Deserialize)]
struct FractionWire {
    numerator: u64,
    denominator: u64,
}

impl TryFrom<FractionWire> for FrameFraction {
    type Error = &'static str;

    fn try_from(wire: FractionWire) -> Result<Self, Self::Error> {
        let fraction = Self::new(wire.numerator, wire.denominator)
            .ok_or("Frame fractions require a nonzero denominator and a value in 0..=1.")?;
        if fraction.numerator != wire.numerator || fraction.denominator.get() != wire.denominator {
            return Err("Stored frame fractions must be reduced to canonical form.");
        }
        Ok(fraction)
    }
}

impl FrameFraction {
    pub const ZERO: Self = Self {
        numerator: 0,
        denominator: NonZeroU64::MIN,
    };
    pub const ONE: Self = Self {
        numerator: 1,
        denominator: NonZeroU64::MIN,
    };

    /// Reduces a valid ratio without floating-point conversion.
    pub fn new(numerator: u64, denominator: u64) -> Option<Self> {
        if denominator == 0 || numerator > denominator {
            return None;
        }
        let divisor = gcd(numerator, denominator);
        Some(Self {
            numerator: numerator / divisor,
            denominator: NonZeroU64::new(denominator / divisor)?,
        })
    }

    pub fn scaled_floor(self, duration: DurationUs) -> u64 {
        let product = u128::from(self.numerator) * u128::from(duration.get());
        u64::try_from(product / u128::from(self.denominator.get()))
            .expect("a unit fraction cannot exceed its scale")
    }

    pub fn scaled_ceil(self, duration: DurationUs) -> u64 {
        let product = u128::from(self.numerator) * u128::from(duration.get());
        u64::try_from(product.div_ceil(u128::from(self.denominator.get())))
            .expect("a unit fraction cannot exceed its scale")
    }
}

impl Ord for FrameFraction {
    fn cmp(&self, other: &Self) -> Ordering {
        (u128::from(self.numerator) * u128::from(other.denominator.get()))
            .cmp(&(u128::from(other.numerator) * u128::from(self.denominator.get())))
    }
}

impl PartialOrd for FrameFraction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

/// Positive half-open authoring coverage; this is not a sub-frame playback interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "LocalSpanWire")]
pub struct FrameLocalSpan {
    start: FrameFraction,
    end: FrameFraction,
}

#[derive(Deserialize)]
struct LocalSpanWire {
    start: FrameFraction,
    end: FrameFraction,
}

impl TryFrom<LocalSpanWire> for FrameLocalSpan {
    type Error = &'static str;
    fn try_from(wire: LocalSpanWire) -> Result<Self, Self::Error> {
        Self::from_fractions(wire.start, wire.end)
            .ok_or("Frame-local authoring scope requires start < end.")
    }
}

impl FrameLocalSpan {
    pub const WHOLE: Self = Self {
        start: FrameFraction::ZERO,
        end: FrameFraction::ONE,
    };

    pub fn new(start: u64, end: u64, duration: DurationUs) -> Option<Self> {
        Self::from_fractions(
            FrameFraction::new(start, duration.get())?,
            FrameFraction::new(end, duration.get())?,
        )
    }

    pub fn from_fractions(start: FrameFraction, end: FrameFraction) -> Option<Self> {
        (start < end).then_some(Self { start, end })
    }

    pub const fn start(self) -> FrameFraction {
        self.start
    }
    pub const fn end(self) -> FrameFraction {
        self.end
    }

    /// Resolves coverage outward only when needed; persisted fractions stay unchanged.
    pub fn resolve(self, duration: DurationUs) -> TimelineSpan {
        let start = self.start.scaled_floor(duration);
        let end = self.end.scaled_ceil(duration);
        TimelineSpan {
            start: TimeUs::new(start),
            duration: DurationUs::new(end - start).expect("positive coverage rounds outward"),
        }
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        Self::from_fractions(self.start.max(other.start), self.end.min(other.end))
    }

    pub fn subtract(self, removed: Self) -> Vec<Self> {
        let Some(cut) = self.intersection(removed) else {
            return vec![self];
        };
        [
            Self::from_fractions(self.start, cut.start),
            Self::from_fractions(cut.end, self.end),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// A run identity is local to its track and must be nonzero. Deleting a gap never renumbers it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameAuthoringSpan {
    pub run_id: u32,
    pub span: FrameLocalSpan,
}

/// Frozen marks paint the whole owner frame, as if written into its pixels.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameOverlayMark {
    pub id: OverlayId,
    pub z_index: i32,
    pub content: OverlayContent,
}

/// One unique owner frame per track. Multiple scope fragments never duplicate its marks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameOverlayCell {
    pub frame_id: FrameId,
    /// None paints at the current tail; Some targets the owner's Composite stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<u32>,
    pub scopes: Vec<FrameAuthoringSpan>,
    pub marks: Vec<FrameOverlayMark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_replay: Option<crate::FrameInputReplay>,
}

pub(crate) fn validate_cells(
    track: &crate::OverlayTrack,
    frame_ids: &std::collections::BTreeSet<FrameId>,
) -> Result<(), String> {
    let Some(cells) = &track.frame_cells else {
        return Ok(());
    };
    if !track.items.is_empty() || track.annotation_scope.is_some() {
        return Err("A frame-owned track cannot also store timed items or timed scope.".to_owned());
    }
    if cells.len() > MAX_FRAME_OVERLAY_CELLS {
        return Err("Frame-owned track exceeds 40,000 owner frames.".to_owned());
    }
    let mut owners = std::collections::BTreeSet::new();
    let mut scopes = 0_usize;
    let mut marks = 0_usize;
    for cell in cells {
        if let Some(replay) = &cell.input_replay {
            replay.validate(&cell.scopes)?;
        }
        if !frame_ids.contains(&cell.frame_id) || !owners.insert(cell.frame_id) {
            return Err("Frame-owned cells require distinct existing owner frame IDs.".to_owned());
        }
        scopes = scopes
            .checked_add(cell.scopes.len())
            .filter(|count| *count <= crate::MAX_ANNOTATION_SCOPE_SPANS)
            .ok_or("Frame-owned scope exceeds 40,000 fragments.")?;
        marks = marks
            .checked_add(cell.marks.len())
            .filter(|count| *count <= MAX_FRAME_OVERLAY_MARKS)
            .ok_or("Frame-owned track exceeds 100,000 marks.")?;
        if track.annotation.is_some() && cell.scopes.is_empty() {
            return Err(
                "Frame-owned annotation cells require original authoring coverage.".to_owned(),
            );
        }
        if cell
            .marks
            .iter()
            .any(|mark| matches!(mark.content, OverlayContent::Progress { style: None, .. }))
        {
            return Err("Frame-owned progress must contain frozen per-frame values, not a legacy time-dependent style.".to_owned());
        }
        let mut previous_end = FrameFraction::ZERO;
        for scope in &cell.scopes {
            if scope.run_id == 0 || scope.span.start < previous_end {
                return Err("Frame authoring scope must have nonzero run IDs and sorted, non-overlapping spans.".to_owned());
            }
            previous_end = scope.span.end;
        }
    }
    Ok(())
}

/// Checks frame-owned cell structure against known owners without copying a
/// project or requiring newly prepared raster assets to be registered yet.
/// Final command validation still verifies asset and global mark identities.
///
/// # Errors
/// Rejects mixed representations, unknown/repeated owners, invalid scopes or
/// replay data, and excessive per-track metadata counts.
pub fn validate_frame_overlay_cells(
    track: &crate::OverlayTrack,
    frame_ids: &std::collections::BTreeSet<FrameId>,
) -> Result<(), String> {
    validate_cells(track, frame_ids)
}

#[cfg(test)]
#[path = "frame_overlay_tests.rs"]
mod model_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn duration(value: u64) -> DurationUs {
        DurationUs::new(value).unwrap()
    }

    #[test]
    fn fractions_are_exact_canonical_and_outward_scaled_at_extreme_durations() {
        let fraction = FrameFraction::new(2, 6).unwrap();
        assert_eq!(fraction, FrameFraction::new(1, 3).unwrap());
        assert_eq!(fraction.scaled_floor(duration(2)), 0);
        assert_eq!(fraction.scaled_ceil(duration(2)), 1);
        let near_one = FrameFraction::new(u64::MAX - 1, u64::MAX).unwrap();
        assert_eq!(near_one.scaled_floor(duration(u64::MAX)), u64::MAX - 1);
        assert_eq!(near_one.scaled_ceil(duration(u64::MAX)), u64::MAX - 1);
        assert!(near_one < FrameFraction::ONE);
        assert!(fraction < near_one);
        assert_eq!(FrameFraction::new(0, u64::MAX), Some(FrameFraction::ZERO));
    }

    #[test]
    fn malformed_or_noncanonical_stored_fractions_and_empty_spans_are_rejected() {
        for json in [
            r#"{"numerator":0,"denominator":0}"#,
            r#"{"numerator":2,"denominator":1}"#,
            r#"{"numerator":2,"denominator":4}"#,
        ] {
            assert!(serde_json::from_str::<FrameFraction>(json).is_err());
        }
        let json = serde_json::to_value(FrameLocalSpan::WHOLE).unwrap();
        let mut reversed = json.clone();
        reversed["start"] = json["end"].clone();
        assert!(serde_json::from_value::<FrameLocalSpan>(reversed).is_err());
        assert_eq!(
            serde_json::from_value::<FrameLocalSpan>(json).unwrap(),
            FrameLocalSpan::WHOLE
        );
    }

    #[test]
    fn retiming_does_not_drift_coverage_and_subtraction_preserves_real_gaps() {
        let span = FrameLocalSpan::new(2, 8, duration(10)).unwrap();
        let before = serde_json::to_string(&span).unwrap();
        for value in [1, 3, 17, u64::MAX, 10] {
            let resolved = span.resolve(duration(value));
            assert!(resolved.end().unwrap().get() <= value);
            assert_eq!(serde_json::to_string(&span).unwrap(), before);
        }
        assert_eq!(span.resolve(duration(10)).start, TimeUs::new(2));
        let remaining = span.subtract(FrameLocalSpan::new(4, 6, duration(10)).unwrap());
        assert_eq!(
            remaining,
            [
                FrameLocalSpan::new(2, 4, duration(10)).unwrap(),
                FrameLocalSpan::new(6, 8, duration(10)).unwrap()
            ]
        );
        assert!(span.subtract(FrameLocalSpan::WHOLE).is_empty());
    }
}
