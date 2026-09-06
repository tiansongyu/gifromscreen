use std::collections::BTreeSet;

use gif_from_screen_domain::{FrameId, TimeUs, Timeline};
use thiserror::Error;

/// Parses a one-based frame expression against `timeline`.
///
/// An expression is a comma-separated sequence of individual frame numbers and inclusive ranges.
/// Ranges retain their written direction, so `5-3` expands to frames 5, 4, and 3. Whitespace is
/// accepted before and after numbers and separators, but not within a decimal number. The returned
/// stable [`FrameId`] values retain expression order. When a frame occurs more than once, only its
/// first occurrence is retained.
///
/// # Examples
///
/// `1, 3-5, 9-7` selects frames 1, 3, 4, 5, 9, 8, and 7 in that order.
///
/// # Errors
///
/// Returns [`FrameExpressionError`] for malformed input, zero, a frame beyond the current
/// timeline, or a decimal number too large for a frame index. Error positions are zero-based byte
/// and Unicode-scalar offsets into the original, untrimmed expression.
pub fn parse_frame_expression(
    timeline: &Timeline,
    expression: &str,
) -> Result<Vec<FrameId>, FrameExpressionError> {
    let mut cursor = Cursor::new(expression);
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();

    cursor.skip_whitespace();
    if cursor.is_at_end() {
        return Err(cursor.error_here(FrameExpressionErrorReason::EmptyItem));
    }

    loop {
        cursor.skip_whitespace();
        match cursor.peek() {
            None | Some(',') => {
                return Err(cursor.error_here(FrameExpressionErrorReason::EmptyItem));
            }
            _ => {}
        }

        let first = parse_frame_number(&mut cursor, timeline.frames.len(), Endpoint::Start)?;
        cursor.skip_whitespace();
        if cursor.consume('-') {
            cursor.skip_whitespace();
            if matches!(cursor.peek(), None | Some(',' | '-')) {
                return Err(cursor.error_here(FrameExpressionErrorReason::MissingRangeEnd));
            }
            let last = parse_frame_number(&mut cursor, timeline.frames.len(), Endpoint::End)?;
            append_range(timeline, first, last, &mut seen, &mut selected);
        } else {
            append_frame(timeline, first, &mut seen, &mut selected);
        }

        cursor.skip_whitespace();
        match cursor.peek() {
            None => break,
            Some(',') => {
                cursor.bump();
                cursor.skip_whitespace();
                if matches!(cursor.peek(), None | Some(',')) {
                    return Err(cursor.error_here(FrameExpressionErrorReason::EmptyItem));
                }
            }
            Some(character) => {
                return Err(
                    cursor.error_here(FrameExpressionErrorReason::InvalidCharacter { character })
                );
            }
        }
    }

    Ok(selected)
}

/// Selects frames whose display intervals overlap the project-relative range `[start, end)`.
///
/// Frame durations may differ. Both the query and each frame use start-inclusive, end-exclusive
/// semantics: a range ending exactly when a frame starts does not select that frame. A range may
/// extend beyond the timeline and is effectively clipped to it. An empty range, an empty timeline,
/// or a range with no overlap returns an empty vector. Results follow timeline order.
///
/// # Errors
///
/// Returns [`FrameTimeRangeError::Reversed`] when `start` is later than `end`, or
/// [`FrameTimeRangeError::DurationOverflow`] when the sum of frame durations cannot be represented
/// in microseconds. Duration overflow is checked even when the requested range is empty or lies
/// outside the timeline.
pub fn select_frames_by_time_range(
    timeline: &Timeline,
    start: TimeUs,
    end: TimeUs,
) -> Result<Vec<FrameId>, FrameTimeRangeError> {
    if start > end {
        return Err(FrameTimeRangeError::Reversed {
            start_us: start.get(),
            end_us: end.get(),
        });
    }

    let total = timeline
        .total_duration()
        .ok_or(FrameTimeRangeError::DurationOverflow)?;
    if start == end || start >= total {
        return Ok(Vec::new());
    }

    let mut selected = Vec::new();
    let mut frame_start = 0_u64;
    for frame in &timeline.frames {
        if frame_start >= end.get() {
            break;
        }
        let frame_end = frame_start
            .checked_add(frame.duration.get())
            .ok_or(FrameTimeRangeError::DurationOverflow)?;
        if frame_start < end.get() && frame_end > start.get() {
            selected.push(frame.id);
        }
        frame_start = frame_end;
    }
    Ok(selected)
}

fn append_range(
    timeline: &Timeline,
    first: usize,
    last: usize,
    seen: &mut BTreeSet<usize>,
    selected: &mut Vec<FrameId>,
) {
    if first <= last {
        for frame_number in first..=last {
            append_frame(timeline, frame_number, seen, selected);
        }
    } else {
        for frame_number in (last..=first).rev() {
            append_frame(timeline, frame_number, seen, selected);
        }
    }
}

fn append_frame(
    timeline: &Timeline,
    frame_number: usize,
    seen: &mut BTreeSet<usize>,
    selected: &mut Vec<FrameId>,
) {
    if seen.insert(frame_number) {
        selected.push(timeline.frames[frame_number - 1].id);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Endpoint {
    Start,
    End,
}

fn parse_frame_number(
    cursor: &mut Cursor<'_>,
    frame_count: usize,
    endpoint: Endpoint,
) -> Result<usize, FrameExpressionError> {
    let start = cursor.byte_offset;
    match cursor.peek() {
        Some(character) if character.is_ascii_digit() => {}
        Some('-') if endpoint == Endpoint::Start => {
            return Err(cursor.error_here(FrameExpressionErrorReason::MissingRangeStart));
        }
        Some('-') | None if endpoint == Endpoint::End => {
            return Err(cursor.error_here(FrameExpressionErrorReason::MissingRangeEnd));
        }
        Some(character) => {
            return Err(
                cursor.error_here(FrameExpressionErrorReason::InvalidCharacter { character })
            );
        }
        None => return Err(cursor.error_here(FrameExpressionErrorReason::EmptyItem)),
    }

    let mut number = 0_usize;
    while let Some(character) = cursor.peek() {
        let Some(digit) = character
            .to_digit(10)
            .filter(|_| character.is_ascii_digit())
        else {
            break;
        };
        let digit_position = cursor.byte_offset;
        cursor.bump();
        number = number
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit as usize))
            .ok_or_else(|| {
                cursor.error_at(digit_position, FrameExpressionErrorReason::NumberTooLarge)
            })?;
    }

    if number == 0 {
        return Err(cursor.error_at(start, FrameExpressionErrorReason::ZeroFrameNumber));
    }
    if number > frame_count {
        return Err(cursor.error_at(
            start,
            FrameExpressionErrorReason::FrameOutOfRange {
                frame_number: number,
                frame_count,
            },
        ));
    }
    Ok(number)
}

struct Cursor<'a> {
    source: &'a str,
    byte_offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(source: &'a str) -> Self {
        Self {
            source,
            byte_offset: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.byte_offset..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.byte_offset += character.len_utf8();
        Some(character)
    }

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    const fn is_at_end(&self) -> bool {
        self.byte_offset == self.source.len()
    }

    fn error_here(&self, reason: FrameExpressionErrorReason) -> FrameExpressionError {
        self.error_at(self.byte_offset, reason)
    }

    fn error_at(
        &self,
        byte_offset: usize,
        reason: FrameExpressionErrorReason,
    ) -> FrameExpressionError {
        FrameExpressionError {
            byte_offset,
            character_offset: self.source[..byte_offset].chars().count(),
            reason,
        }
    }
}

/// A syntax or bounds error in a frame expression.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("invalid frame expression at byte {byte_offset}, character {character_offset}: {reason}")]
pub struct FrameExpressionError {
    /// Zero-based UTF-8 byte offset in the original expression.
    pub byte_offset: usize,
    /// Zero-based Unicode-scalar offset in the original expression.
    pub character_offset: usize,
    /// Why parsing failed at this position.
    pub reason: FrameExpressionErrorReason,
}

/// The reason a frame expression could not be parsed.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum FrameExpressionErrorReason {
    /// A comma-delimited item was empty, including an entirely empty expression.
    #[error("frame list item is empty")]
    EmptyItem,
    /// A range separator appeared without a preceding frame number.
    #[error("range is missing its start frame")]
    MissingRangeStart,
    /// A range separator appeared without a following frame number.
    #[error("range is missing its end frame")]
    MissingRangeEnd,
    /// The character is not valid at this point in the expression.
    #[error("character {character:?} is not valid here")]
    InvalidCharacter {
        /// The unexpected Unicode scalar.
        character: char,
    },
    /// Frame numbers are one-based and therefore cannot be zero.
    #[error("frame number zero is invalid; frame numbers are one-based")]
    ZeroFrameNumber,
    /// The frame number is greater than the number of timeline frames.
    #[error("frame number {frame_number} is outside a timeline with {frame_count} frames")]
    FrameOutOfRange {
        /// The one-based number found in the expression.
        frame_number: usize,
        /// Number of frames in the timeline used for resolution.
        frame_count: usize,
    },
    /// The decimal token cannot be represented as a platform frame index.
    #[error("frame number is too large")]
    NumberTooLarge,
}

/// Errors produced while selecting frames by project-relative time.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum FrameTimeRangeError {
    /// The half-open range has its end before its start.
    #[error("time range start {start_us}us is later than end {end_us}us")]
    Reversed {
        /// Requested inclusive start time.
        start_us: u64,
        /// Requested exclusive end time.
        end_us: u64,
    },
    /// Adding the individual frame durations overflowed.
    #[error("the timeline duration exceeds the supported microsecond range")]
    DurationOverflow,
}

#[cfg(test)]
mod tests {
    use gif_from_screen_domain::{
        AssetId, CaptureMetadata, ClipTransform, DurationUs, FrameClip, FrameId, TimeUs, Timeline,
    };

    use super::{
        FrameExpressionErrorReason, FrameTimeRangeError, parse_frame_expression,
        select_frames_by_time_range,
    };

    fn frame(number: u128, duration_us: u64) -> FrameClip {
        FrameClip {
            render_steps: Vec::new(),
            capture_clock: None,
            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
            id: FrameId::from_u128(number),
            asset_id: AssetId::from_digest([u8::try_from(number).unwrap_or(0); 32]),
            duration: DurationUs::new(duration_us).unwrap(),
            transform: ClipTransform::default(),
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
        }
    }

    fn timeline_with_durations(durations: &[u64]) -> Timeline {
        Timeline {
            frames: durations
                .iter()
                .copied()
                .enumerate()
                .map(|(index, duration)| frame(u128::try_from(index).unwrap() + 1, duration))
                .collect(),
            ..Timeline::default()
        }
    }

    fn numeric_ids(ids: &[FrameId]) -> Vec<u128> {
        ids.iter()
            .map(|id| u128::from_be_bytes(*id.as_bytes()))
            .collect()
    }

    #[test]
    fn expressions_expand_in_written_order_and_first_occurrence_wins() {
        let timeline = timeline_with_durations(&[1; 9]);
        let cases = [
            ("1,3-5,9-7", vec![1, 3, 4, 5, 9, 8, 7]),
            (" 1 , 3 - 5 , 9 - 7 ", vec![1, 3, 4, 5, 9, 8, 7]),
            ("3-1,2,1-3,4", vec![3, 2, 1, 4]),
            ("2-2,02,1", vec![2, 1]),
        ];

        for (expression, expected) in cases {
            let parsed = parse_frame_expression(&timeline, expression).unwrap();
            assert_eq!(numeric_ids(&parsed), expected, "expression: {expression:?}");
        }
    }

    #[test]
    fn every_bounded_range_expands_in_its_written_direction() {
        for frame_count in 1_usize..=16 {
            let timeline = timeline_with_durations(&vec![1; frame_count]);
            for first in 1..=frame_count {
                for last in 1..=frame_count {
                    let expression = format!("{first}-{last}");
                    let parsed =
                        numeric_ids(&parse_frame_expression(&timeline, &expression).unwrap());
                    let expected: Vec<u128> = if first <= last {
                        (first..=last)
                            .map(|number| u128::try_from(number).unwrap())
                            .collect()
                    } else {
                        (last..=first)
                            .rev()
                            .map(|number| u128::try_from(number).unwrap())
                            .collect()
                    };
                    assert_eq!(parsed, expected, "expression: {expression}");
                }
            }
        }
    }

    #[test]
    fn malformed_expression_table_reports_exact_original_positions() {
        let timeline = timeline_with_durations(&[1; 5]);
        let cases = [
            ("", 0, 0, FrameExpressionErrorReason::EmptyItem),
            ("   ", 3, 3, FrameExpressionErrorReason::EmptyItem),
            (",1", 0, 0, FrameExpressionErrorReason::EmptyItem),
            ("1,,2", 2, 2, FrameExpressionErrorReason::EmptyItem),
            ("1,", 2, 2, FrameExpressionErrorReason::EmptyItem),
            ("1, ,2", 3, 3, FrameExpressionErrorReason::EmptyItem),
            ("-2", 0, 0, FrameExpressionErrorReason::MissingRangeStart),
            ("1,-2", 2, 2, FrameExpressionErrorReason::MissingRangeStart),
            ("1-", 2, 2, FrameExpressionErrorReason::MissingRangeEnd),
            ("1- ,2", 3, 3, FrameExpressionErrorReason::MissingRangeEnd),
            ("1--2", 2, 2, FrameExpressionErrorReason::MissingRangeEnd),
            (
                "a",
                0,
                0,
                FrameExpressionErrorReason::InvalidCharacter { character: 'a' },
            ),
            (
                "1;2",
                1,
                1,
                FrameExpressionErrorReason::InvalidCharacter { character: ';' },
            ),
            (
                "1 2",
                2,
                2,
                FrameExpressionErrorReason::InvalidCharacter { character: '2' },
            ),
            (
                "1-2-3",
                3,
                3,
                FrameExpressionErrorReason::InvalidCharacter { character: '-' },
            ),
            ("0", 0, 0, FrameExpressionErrorReason::ZeroFrameNumber),
            ("1-0", 2, 2, FrameExpressionErrorReason::ZeroFrameNumber),
            (
                "6",
                0,
                0,
                FrameExpressionErrorReason::FrameOutOfRange {
                    frame_number: 6,
                    frame_count: 5,
                },
            ),
        ];

        for (expression, byte_offset, character_offset, reason) in cases {
            let error = parse_frame_expression(&timeline, expression).unwrap_err();
            assert_eq!(error.byte_offset, byte_offset, "expression: {expression:?}");
            assert_eq!(
                error.character_offset, character_offset,
                "expression: {expression:?}"
            );
            assert_eq!(error.reason, reason, "expression: {expression:?}");
        }
    }

    #[test]
    fn unicode_whitespace_keeps_byte_and_character_offsets_distinct() {
        let timeline = timeline_with_durations(&[1]);
        let expression = "1,\u{2003}@";

        let error = parse_frame_expression(&timeline, expression).unwrap_err();
        assert_eq!(error.byte_offset, 5);
        assert_eq!(error.character_offset, 3);
        assert_eq!(
            error.reason,
            FrameExpressionErrorReason::InvalidCharacter { character: '@' }
        );
    }

    #[test]
    fn arbitrarily_large_numbers_return_an_error_without_panicking() {
        let timeline = timeline_with_durations(&[1]);
        let expression = format!("{}0", usize::MAX);

        let error = parse_frame_expression(&timeline, &expression).unwrap_err();
        assert_eq!(error.byte_offset, usize::MAX.to_string().len());
        assert_eq!(error.character_offset, error.byte_offset);
        assert_eq!(error.reason, FrameExpressionErrorReason::NumberTooLarge);

        let much_larger = "9".repeat(10_000);
        assert!(matches!(
            parse_frame_expression(&timeline, &much_larger),
            Err(error) if error.reason == FrameExpressionErrorReason::NumberTooLarge
        ));
    }

    #[test]
    fn time_ranges_use_overlap_and_half_open_boundary_semantics() {
        let timeline = timeline_with_durations(&[10, 20, 30]);
        let cases = [
            (0, 1, vec![1]),
            (0, 10, vec![1]),
            (9, 10, vec![1]),
            (10, 11, vec![2]),
            (9, 11, vec![1, 2]),
            (10, 30, vec![2]),
            (29, 30, vec![2]),
            (30, 60, vec![3]),
            (0, 60, vec![1, 2, 3]),
            (0, 100, vec![1, 2, 3]),
            (60, 100, vec![]),
            (11, 11, vec![]),
        ];

        for (start, end, expected) in cases {
            let selected =
                select_frames_by_time_range(&timeline, TimeUs::new(start), TimeUs::new(end))
                    .unwrap();
            assert_eq!(
                numeric_ids(&selected),
                expected,
                "time range: [{start}, {end})"
            );
        }
    }

    #[test]
    fn every_small_time_range_matches_interval_intersection() {
        let durations = [1_u64, 3, 2, 5, 1];
        let timeline = timeline_with_durations(&durations);
        let total = durations.iter().sum::<u64>();

        for start in 0..=total + 2 {
            for end in start..=total + 2 {
                let selected =
                    select_frames_by_time_range(&timeline, TimeUs::new(start), TimeUs::new(end))
                        .unwrap();
                let mut frame_start = 0_u64;
                let expected = durations
                    .iter()
                    .copied()
                    .enumerate()
                    .filter_map(|(index, duration)| {
                        let frame_end = frame_start + duration;
                        let overlaps = start < end && frame_start < end && frame_end > start;
                        frame_start = frame_end;
                        overlaps.then_some(u128::try_from(index).unwrap() + 1)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    numeric_ids(&selected),
                    expected,
                    "time range: [{start}, {end})"
                );
            }
        }
    }

    #[test]
    fn empty_timeline_and_non_overlapping_ranges_are_empty() {
        assert_eq!(
            select_frames_by_time_range(&Timeline::default(), TimeUs::ZERO, TimeUs::new(10))
                .unwrap(),
            []
        );
        let timeline = timeline_with_durations(&[10]);
        assert_eq!(
            select_frames_by_time_range(&timeline, TimeUs::new(10), TimeUs::new(20)).unwrap(),
            []
        );
    }

    #[test]
    fn reversed_ranges_and_total_duration_overflow_are_typed() {
        let timeline = timeline_with_durations(&[10]);
        assert_eq!(
            select_frames_by_time_range(&timeline, TimeUs::new(2), TimeUs::new(1)),
            Err(FrameTimeRangeError::Reversed {
                start_us: 2,
                end_us: 1,
            })
        );

        let overflow = timeline_with_durations(&[u64::MAX, 1]);
        assert_eq!(
            select_frames_by_time_range(&overflow, TimeUs::ZERO, TimeUs::ZERO),
            Err(FrameTimeRangeError::DurationOverflow)
        );
    }
}
