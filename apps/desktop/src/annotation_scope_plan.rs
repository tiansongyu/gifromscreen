//! Resolve persisted authoring coverage into frame samples without changing source clocks.

use gif_from_screen_domain::{
    DurationUs, FrameId, MAX_ANNOTATION_SCOPE_SPANS, ProjectManifest, TimeUs, TimelineSpan,
    normalize_annotation_scope, validate_annotation_scope,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(super) struct ScopeSample {
    pub index: usize,
    pub frame_start: u64,
    pub frame_end: u64,
    pub span: TimelineSpan,
    pub run: usize,
}

pub(super) struct ScopePlan {
    pub scope: Vec<TimelineSpan>,
    pub samples: Vec<ScopeSample>,
    pub selected: BTreeSet<FrameId>,
}

impl ScopePlan {
    pub fn from_selection(
        manifest: &ProjectManifest,
        selected: &BTreeSet<FrameId>,
    ) -> Result<Self, String> {
        let mut start = 0_u64;
        let mut scope = Vec::with_capacity(selected.len());
        for frame in &manifest.timeline.frames {
            if selected.contains(&frame.id) {
                scope.push(TimelineSpan {
                    start: TimeUs::new(start),
                    duration: frame.duration,
                });
            }
            start = start
                .checked_add(frame.duration.get())
                .ok_or("Annotation timeline duration overflow.")?;
        }
        Self::new(manifest, &scope)
    }

    pub fn new(manifest: &ProjectManifest, scope: &[TimelineSpan]) -> Result<Self, String> {
        let scope = normalize_annotation_scope(scope)?;
        let total = manifest
            .timeline
            .total_duration()
            .ok_or("Annotation timeline duration overflow.")?;
        validate_annotation_scope(&scope, total)?;
        let mut intervals = Vec::with_capacity(manifest.timeline.frames.len());
        let mut start = 0_u64;
        for frame in &manifest.timeline.frames {
            let end = start
                .checked_add(frame.duration.get())
                .ok_or("Annotation timeline duration overflow.")?;
            intervals.push((start, end));
            start = end;
        }
        // Joining adjacent intervals is safe only for execution. Persisted scope
        // keeps its frame boundaries so later insertions do not expand membership.
        let mut runs: Vec<(u64, u64)> = Vec::new();
        for span in &scope {
            let end = span.end().expect("validated authoring scope").get();
            if let Some(previous) = runs.last_mut()
                && previous.1 == span.start.get()
            {
                previous.1 = end;
            } else {
                runs.push((span.start.get(), end));
            }
        }
        let mut samples = Vec::new();
        let mut selected = BTreeSet::new();
        for (run, (left, right)) in runs.into_iter().enumerate() {
            let first = intervals.partition_point(|(_, end)| *end <= left);
            for (index, &(frame_start, frame_end)) in intervals
                .iter()
                .enumerate()
                .skip(first)
                .take_while(|(_, (start, _))| *start < right)
            {
                if samples.len() >= MAX_ANNOTATION_SCOPE_SPANS {
                    return Err(
                        "Annotation authoring scope exceeds 40,000 frame fragments.".to_owned()
                    );
                }
                let start = left.max(frame_start);
                let end = right.min(frame_end);
                samples.push(ScopeSample {
                    index,
                    frame_start,
                    frame_end,
                    span: TimelineSpan {
                        start: TimeUs::new(start),
                        duration: DurationUs::new(end - start)
                            .expect("overlapping positive interval"),
                    },
                    run,
                });
                selected.insert(manifest.timeline.frames[index].id);
            }
        }
        Ok(Self {
            scope,
            samples,
            selected,
        })
    }
}
