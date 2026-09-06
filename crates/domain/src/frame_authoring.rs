//! Shared owner/scope construction for ordinary frame-oriented authoring tools.

use std::collections::BTreeSet;

use crate::{
    FrameAuthoringSpan, FrameId, FrameLocalSpan, FrameOverlayCell, FrameOverlayMark,
    MAX_FRAME_OVERLAY_CELLS, Timeline,
};

impl FrameOverlayCell {
    /// Makes whole-frame coverage. Frame/run identities are checked by manifest validation.
    pub fn whole(frame_id: FrameId, run_id: u32, marks: Vec<FrameOverlayMark>) -> Self {
        Self {
            frame_id,
            scopes: vec![FrameAuthoringSpan {
                run_id,
                span: FrameLocalSpan::WHOLE,
            }],
            marks,
            input_replay: None,
        }
    }
}

/// Builds one mark per selected frame, preserving actual selection gaps as distinct runs.
/// Validation finishes before invoking the caller's identity/content factory.
pub fn selected_frame_cells(
    timeline: &Timeline,
    selected: &BTreeSet<FrameId>,
    mut make_mark: impl FnMut(FrameId) -> FrameOverlayMark,
) -> Result<Vec<FrameOverlayCell>, String> {
    if selected.is_empty() || selected.len() > MAX_FRAME_OVERLAY_CELLS {
        return Err("Select between 1 and 40,000 frames for frame-owned marks.".to_owned());
    }
    let mut matched = BTreeSet::new();
    for frame in &timeline.frames {
        if selected.contains(&frame.id) && (frame.id.is_nil() || !matched.insert(frame.id)) {
            return Err(
                "Frame-owned selection contains invalid or duplicate frame identities.".to_owned(),
            );
        }
    }
    if &matched != selected {
        return Err("Frame-owned selection contains a missing frame.".to_owned());
    }
    let mut output = Vec::with_capacity(selected.len());
    let mut run_id = 0_u32;
    let mut previous_selected = false;
    for frame in &timeline.frames {
        let included = selected.contains(&frame.id);
        if included {
            if !previous_selected {
                run_id += 1;
            }
            output.push(FrameOverlayCell::whole(
                frame.id,
                run_id,
                vec![make_mark(frame.id)],
            ));
        }
        previous_selected = included;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        OverlayContent, OverlayId, PhysicalPoint,
        model::test_fixtures::{asset, frame},
    };

    fn timeline() -> Timeline {
        Timeline {
            frames: (1..=5).map(|id| frame(id, asset(1).id)).collect(),
            ..Timeline::default()
        }
    }

    fn mark(frame_id: FrameId) -> FrameOverlayMark {
        FrameOverlayMark {
            id: OverlayId::from_bytes(*frame_id.as_bytes()),
            z_index: 0,
            content: OverlayContent::KeyStroke {
                text: "Static".to_owned(),
                position: PhysicalPoint::default(),
                raster: None,
            },
        }
    }

    #[test]
    fn selected_cells_follow_timeline_order_without_joining_unselected_gaps() {
        let timeline = timeline();
        let selected = [5, 2, 1, 4].map(FrameId::from_u128).into_iter().collect();
        let cells = selected_frame_cells(&timeline, &selected, mark).unwrap();
        assert_eq!(
            cells.iter().map(|cell| cell.frame_id).collect::<Vec<_>>(),
            [1, 2, 4, 5].map(FrameId::from_u128)
        );
        assert_eq!(
            cells
                .iter()
                .map(|cell| cell.scopes[0].run_id)
                .collect::<Vec<_>>(),
            [1, 1, 2, 2]
        );
        assert!(
            cells
                .iter()
                .all(|cell| cell.scopes[0].span == FrameLocalSpan::WHOLE
                    && cell.marks.len() == 1
                    && cell.input_replay.is_none())
        );
        assert_eq!(timeline.frames.len(), 5);
    }

    #[test]
    fn invalid_selection_is_rejected_before_the_factory_allocates_any_marks() {
        for selected in [
            BTreeSet::new(),
            [FrameId::from_u128(99)].into_iter().collect(),
        ] {
            assert!(
                selected_frame_cells(&timeline(), &selected, |_| panic!(
                    "preflight must finish first"
                ))
                .is_err()
            );
        }
        let mut duplicate = timeline();
        duplicate.frames.push(duplicate.frames[0].clone());
        let selected = [FrameId::from_u128(1)].into_iter().collect();
        assert!(
            selected_frame_cells(&duplicate, &selected, |_| panic!("duplicate owner")).is_err()
        );
    }
}
