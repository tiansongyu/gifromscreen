//! Authoring coverage is independent of the marks currently visible in a group.

use crate::{DurationUs, TimeUs, TimelineSpan};

pub const MAX_ANNOTATION_SCOPE_SPANS: usize = 40_000;

/// Sort and union overlapping intervals, retaining adjacent frame boundaries.
pub fn normalize_annotation_scope(scope: &[TimelineSpan]) -> Result<Vec<TimelineSpan>, String> {
    if scope.len() > MAX_ANNOTATION_SCOPE_SPANS {
        return Err("Annotation authoring scope exceeds 40,000 fragments.".to_owned());
    }
    let mut intervals = scope
        .iter()
        .map(|span| {
            let end = span
                .end()
                .ok_or("Annotation authoring scope overflows time.")?;
            Ok((span.start.get(), end.get()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    intervals.sort_unstable();
    let mut result: Vec<TimelineSpan> = Vec::with_capacity(intervals.len());
    for (start, end) in intervals {
        if let Some(last) = result.last_mut()
            && start < last.end().expect("checked scope endpoint").get()
        {
            let end = end.max(last.end().expect("checked scope endpoint").get());
            last.duration = DurationUs::new(end - last.start.get()).expect("nonempty union");
            continue;
        }
        result.push(scope_span(start, end)?);
    }
    Ok(result)
}

/// Validate canonical order and bounds without inferring scope from visible items.
pub fn validate_annotation_scope(
    scope: &[TimelineSpan],
    timeline_end: TimeUs,
) -> Result<(), String> {
    if scope.len() > MAX_ANNOTATION_SCOPE_SPANS {
        return Err("Annotation authoring scope exceeds 40,000 fragments.".to_owned());
    }
    let mut previous_end = 0;
    for span in scope {
        let end = span
            .end()
            .ok_or("Annotation authoring scope overflows time.")?
            .get();
        if span.start.get() < previous_end {
            return Err(
                "Annotation authoring scope must be sorted and non-overlapping.".to_owned(),
            );
        }
        if end > timeline_end.get() {
            return Err("Annotation authoring scope extends beyond the timeline.".to_owned());
        }
        previous_end = end;
    }
    Ok(())
}

/// Remove baked or explicitly trimmed ranges, preserving all remaining gaps.
pub fn subtract_annotation_scope(
    scope: &[TimelineSpan],
    removed: &[TimelineSpan],
) -> Result<Vec<TimelineSpan>, String> {
    let scope = normalize_annotation_scope(scope)?;
    let removed = normalize_annotation_scope(removed)?;
    let mut output = Vec::new();
    for span in scope {
        let mut left = span.start.get();
        let end = span.end().expect("normalized scope").get();
        let index =
            removed.partition_point(|span| span.end().expect("normalized removal").get() <= left);
        for cut in removed[index..]
            .iter()
            .take_while(|cut| cut.start.get() < end)
        {
            if cut.start.get() > left {
                push_scope_span(&mut output, left, cut.start.get().min(end))?;
            }
            left = left
                .max(cut.end().expect("normalized removal").get())
                .min(end);
        }
        if left < end {
            push_scope_span(&mut output, left, end)?;
        }
    }
    Ok(output)
}

/// Shift existing coverage while excluding the entire newly inserted interval.
pub fn shift_annotation_scope_for_insert(
    scope: &[TimelineSpan],
    start: u64,
    duration: DurationUs,
) -> Result<Vec<TimelineSpan>, String> {
    let mut output = Vec::new();
    for span in normalize_annotation_scope(scope)? {
        let left = span.start.get();
        let end = span.end().expect("normalized scope").get();
        if end <= start {
            push_scope_span(&mut output, left, end)?;
            continue;
        }
        if left < start {
            push_scope_span(&mut output, left, start)?;
        }
        let shifted_left = left
            .max(start)
            .checked_add(duration.get())
            .ok_or("Annotation insertion overflows time.")?;
        let shifted_end = end
            .checked_add(duration.get())
            .ok_or("Annotation insertion overflows time.")?;
        push_scope_span(&mut output, shifted_left, shifted_end)?;
    }
    Ok(output)
}

pub(crate) fn push_scope_span(
    output: &mut Vec<TimelineSpan>,
    start: u64,
    end: u64,
) -> Result<(), String> {
    if output.len() >= MAX_ANNOTATION_SCOPE_SPANS {
        return Err("Annotation authoring scope exceeds 40,000 fragments.".to_owned());
    }
    output.push(scope_span(start, end)?);
    Ok(())
}

fn scope_span(start: u64, end: u64) -> Result<TimelineSpan, String> {
    let duration = end
        .checked_sub(start)
        .and_then(DurationUs::new)
        .ok_or("Annotation scope must contain positive intervals.")?;
    Ok(TimelineSpan {
        start: TimeUs::new(start),
        duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn span(start: u64, end: u64) -> TimelineSpan {
        scope_span(start, end).unwrap()
    }

    #[test]
    fn normalization_unions_overlaps_but_preserves_adjacent_frame_boundaries() {
        assert_eq!(
            normalize_annotation_scope(&[span(10, 20), span(0, 5), span(4, 10), span(30, 40)])
                .unwrap(),
            [span(0, 10), span(10, 20), span(30, 40)]
        );
    }
    #[test]
    fn insertion_and_subtraction_preserve_partial_frame_gaps() {
        let scope = [span(2, 8), span(12, 18)];
        assert_eq!(
            shift_annotation_scope_for_insert(&scope, 5, DurationUs::new(10).unwrap()).unwrap(),
            [span(2, 5), span(15, 18), span(22, 28)]
        );
        assert_eq!(
            subtract_annotation_scope(&scope, &[span(4, 6), span(7, 14)]).unwrap(),
            [span(2, 4), span(6, 7), span(14, 18)]
        );
    }
    #[test]
    fn malformed_or_unbounded_scopes_are_rejected_without_truncation() {
        for scope in [
            vec![span(5, 10), span(2, 3)],
            vec![span(0, 8), span(7, 9)],
            vec![span(0, 11)],
        ] {
            assert!(validate_annotation_scope(&scope, TimeUs::new(10)).is_err());
        }
        assert!(
            normalize_annotation_scope(&vec![span(0, 1); MAX_ANNOTATION_SCOPE_SPANS + 1]).is_err()
        );
        assert!(
            shift_annotation_scope_for_insert(&[span(0, 2)], 0, DurationUs::new(u64::MAX).unwrap())
                .is_err()
        );
        validate_annotation_scope(&[], TimeUs::ZERO).unwrap();
    }
}
