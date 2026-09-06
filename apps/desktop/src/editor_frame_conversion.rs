//! Explicit, bounded timed-track snapshots without reading or baking pixel assets.

use std::{
    borrow::Cow,
    collections::BTreeMap,
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use gif_from_screen_domain::{
    AnnotationRequest, BlendMode, EditCommand, FrameAuthoringSpan, FrameClip, FrameId,
    FrameLocalSpan, FrameOverlayCell, FrameOverlayMark, MAX_ANNOTATION_SCOPE_SPANS,
    MAX_FRAME_OVERLAY_CELLS, MAX_FRAME_OVERLAY_MARKS, OverlayContent, OverlayId, OverlayItem,
    OverlayTrack, ProjectManifest, TimeUs, TrackId,
};
use gif_from_screen_editor::{FrameBundleIdentities, MAX_FRAME_BUNDLE_METADATA_BYTES};
use gif_from_screen_render::freeze_timed_overlay_content;
use serde::{
    Serialize, Serializer,
    ser::{Error as _, SerializeSeq},
};
use uuid::Uuid;

use super::{EditorWorkspace, OverlaySelectionAnchor};
use crate::annotation_engine::{AnnotationEditReport, AnnotationProgress, check_cancelled};

impl EditorWorkspace {
    pub(crate) fn convert_overlay_to_frames(
        &mut self,
        anchor: &OverlaySelectionAnchor,
        track_id: TrackId,
        cancellation: &AtomicBool,
        mut progress: impl FnMut(AnnotationProgress),
    ) -> Result<AnnotationEditReport, String> {
        check_cancelled(cancellation)?;
        if !anchor.matches_project(self) {
            return Err(
                "Project changed before timed-track conversion; nothing was modified.".to_owned(),
            );
        }
        let (command, frames) = {
            let manifest = self.manifest();
            let track = manifest
                .timeline
                .overlay_tracks
                .iter()
                .find(|track| track.id == track_id)
                .ok_or("The selected overlay track no longer exists.")?;
            if track.frame_cells.is_some() {
                return Err("This track already owns frozen frame marks.".to_owned());
            }
            if track.annotation.is_some() && track.annotation_scope.is_none() {
                return Err("This legacy group's original authoring scope is unknown. Conversion cannot infer it from visible marks; the group is unchanged.".to_owned());
            }
            // The inverse will retain this source track. Bound it before any
            // command preflight or history implementation can clone it.
            metadata_bytes(track, cancellation)?;
            manifest.validate().map_err(|error| error.to_string())?;
            let plan = ConversionPlan::new(manifest, track, cancellation)?;
            progress(AnnotationProgress {
                completed: 0,
                total: plan.cells.len(),
            });
            let planned_bytes = metadata_bytes(
                &PlannedCommand {
                    kind: "upsert_overlay_track",
                    track: TrackView(&plan),
                },
                cancellation,
            )?;
            let mut identities = FrameBundleIdentities::new(&manifest.timeline.overlay_tracks)
                .map_err(|error| error.to_string())?;
            let converted = plan.materialize(&mut identities, cancellation, &mut progress)?;
            let frames = converted.frame_cells.as_ref().map_or(0, Vec::len);
            let command = EditCommand::UpsertOverlayTrack { track: converted };
            let actual_bytes = metadata_bytes(&command, cancellation)?;
            if actual_bytes != planned_bytes {
                return Err("Conversion metadata preflight did not match its final command; nothing was modified.".to_owned());
            }
            (command, frames)
        };
        check_cancelled(cancellation)?;
        self.manifest()
            .clone()
            .apply_command(&command)
            .map_err(|error| error.to_string())?;
        check_cancelled(cancellation)?;
        self.execute(command).map_err(|error| error.to_string())?;
        Ok(AnnotationEditReport {
            frames,
            ..AnnotationEditReport::default()
        })
    }
}

#[derive(Clone, Copy)]
struct FrameInterval {
    start: u64,
    end: u64,
}

#[derive(Default)]
struct CellPlan {
    scopes: Vec<FrameAuthoringSpan>,
    items: Vec<usize>,
}

struct ConversionPlan<'a> {
    track: &'a OverlayTrack,
    frames: &'a [FrameClip],
    intervals: Vec<FrameInterval>,
    cells: BTreeMap<usize, CellPlan>,
}

impl<'a> ConversionPlan<'a> {
    fn new(
        manifest: &'a ProjectManifest,
        track: &'a OverlayTrack,
        cancellation: &AtomicBool,
    ) -> Result<Self, String> {
        let frames = &manifest.timeline.frames;
        let mut intervals = Vec::new();
        intervals
            .try_reserve_exact(frames.len())
            .map_err(|_| "Could not allocate frame-start index.")?;
        let mut start = 0_u64;
        for frame in frames {
            check_cancelled(cancellation)?;
            let end = start
                .checked_add(frame.duration.get())
                .ok_or("Timeline duration overflow.")?;
            intervals.push(FrameInterval { start, end });
            start = end;
        }
        let mut plan = Self {
            track,
            frames,
            intervals,
            cells: BTreeMap::new(),
        };
        plan.plan_scopes(cancellation)?;
        plan.plan_marks(cancellation)?;
        Ok(plan)
    }

    fn cell(&mut self, index: usize) -> Result<&mut CellPlan, String> {
        if !self.cells.contains_key(&index) && self.cells.len() >= MAX_FRAME_OVERLAY_CELLS {
            return Err("Conversion exceeds 40,000 owner frames.".to_owned());
        }
        Ok(self.cells.entry(index).or_default())
    }

    fn plan_scopes(&mut self, cancellation: &AtomicBool) -> Result<(), String> {
        let mut previous_end = None;
        let mut run_id = 0_u32;
        let mut fragments = 0_usize;
        for scope in self.track.annotation_scope.iter().flatten() {
            check_cancelled(cancellation)?;
            let left = scope.start.get();
            let right = scope
                .end()
                .ok_or("Authoring scope duration overflow.")?
                .get();
            if previous_end.is_none_or(|end| left > end) {
                run_id = run_id
                    .checked_add(1)
                    .ok_or("Authoring run identity overflow.")?;
            }
            previous_end = Some(right);
            let first = self.intervals.partition_point(|frame| frame.end <= left);
            let end = self.intervals.partition_point(|frame| frame.start < right);
            for index in first..end {
                check_cancelled(cancellation)?;
                fragments = fragments
                    .checked_add(1)
                    .filter(|count| *count <= MAX_ANNOTATION_SCOPE_SPANS)
                    .ok_or("Conversion exceeds 40,000 authoring scope fragments.")?;
                let frame = self.intervals[index];
                let local = FrameLocalSpan::new(
                    left.max(frame.start) - frame.start,
                    right.min(frame.end) - frame.start,
                    self.frames[index].duration,
                )
                .ok_or("Invalid frame-local authoring intersection.")?;
                self.cell(index)?.scopes.push(FrameAuthoringSpan {
                    run_id,
                    span: local,
                });
            }
        }
        Ok(())
    }

    fn plan_marks(&mut self, cancellation: &AtomicBool) -> Result<(), String> {
        let mut marks = 0_usize;
        // One binary search per endpoint, then only visit the output owners.
        // Appending in source-item order preserves all equal-z tie breaking.
        for (item_index, item) in self.track.items.iter().enumerate() {
            check_cancelled(cancellation)?;
            let end_time = item.span.end().ok_or("Overlay duration overflow.")?.get();
            let first = self
                .intervals
                .partition_point(|frame| frame.start < item.span.start.get());
            let end = self
                .intervals
                .partition_point(|frame| frame.start < end_time);
            marks = marks
                .checked_add(end - first)
                .filter(|count| *count <= MAX_FRAME_OVERLAY_MARKS)
                .ok_or("Conversion exceeds 100,000 frozen marks.")?;
            for index in first..end {
                check_cancelled(cancellation)?;
                if self.track.annotation.is_some()
                    && self
                        .cells
                        .get(&index)
                        .is_none_or(|cell| cell.scopes.is_empty())
                {
                    return Err("An annotation mark has no original authoring coverage on its owner frame; conversion cannot invent a scope.".to_owned());
                }
                self.cell(index)?.items.push(item_index);
            }
        }
        Ok(())
    }

    fn materialize(
        &self,
        identities: &mut FrameBundleIdentities,
        cancellation: &AtomicBool,
        progress: &mut impl FnMut(AnnotationProgress),
    ) -> Result<OverlayTrack, String> {
        let mut cells = Vec::with_capacity(self.cells.len());
        let mut generate = || FrameId::from_u128(Uuid::new_v4().as_u128());
        for (&index, planned) in &self.cells {
            check_cancelled(cancellation)?;
            let sample = TimeUs::new(self.intervals[index].start);
            let mut marks = Vec::with_capacity(planned.items.len());
            for &item_index in &planned.items {
                check_cancelled(cancellation)?;
                let item = &self.track.items[item_index];
                let content = freeze_timed_overlay_content(item, sample)
                    .map_err(|error| error.to_string())?
                    .ok_or("An indexed overlay unexpectedly became inactive.")?;
                marks.push(FrameOverlayMark {
                    id: identities
                        .mark(&mut generate)
                        .map_err(|error| error.to_string())?,
                    z_index: item.z_index,
                    content,
                });
            }
            cells.push(FrameOverlayCell {
                frame_id: self.frames[index].id,
                scopes: planned.scopes.clone(),
                marks,
                // Current raw events are not proof of the historical replay pool.
                input_replay: None,
            });
            progress(AnnotationProgress {
                completed: cells.len(),
                total: self.cells.len(),
            });
        }
        Ok(OverlayTrack {
            id: self.track.id,
            frame_cells: Some(cells),
            annotation: self.track.annotation.clone(),
            annotation_scope: None,
            name: self.track.name.clone(),
            visible: self.track.visible,
            opacity: self.track.opacity,
            blend_mode: self.track.blend_mode,
            items: Vec::new(),
        })
    }
}

// These borrowing views serialize the exact destination representation before
// any variable-size artwork is cloned. IDs are fixed-width 32-digit hex strings.
#[derive(Serialize)]
struct PlannedCommand<'a, 'p> {
    #[serde(rename = "type")]
    kind: &'static str,
    track: TrackView<'a, 'p>,
}

struct TrackView<'a, 'p>(&'a ConversionPlan<'p>);

impl Serialize for TrackView<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct View<'a, 'p> {
            id: TrackId,
            frame_cells: CellsView<'a, 'p>,
            #[serde(skip_serializing_if = "Option::is_none")]
            annotation: Option<&'p AnnotationRequest>,
            name: &'p str,
            visible: bool,
            opacity: u8,
            blend_mode: BlendMode,
            items: [OverlayItem; 0],
        }
        let track = self.0.track;
        View {
            id: track.id,
            frame_cells: CellsView(self.0),
            annotation: track.annotation.as_ref(),
            name: &track.name,
            visible: track.visible,
            opacity: track.opacity,
            blend_mode: track.blend_mode,
            items: [],
        }
        .serialize(serializer)
    }
}

struct CellsView<'a, 'p>(&'a ConversionPlan<'p>);

impl Serialize for CellsView<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct View<'a, 'p> {
            frame_id: FrameId,
            scopes: &'a [FrameAuthoringSpan],
            marks: MarksView<'a, 'p>,
        }
        let mut sequence = serializer.serialize_seq(Some(self.0.cells.len()))?;
        for (&index, cell) in &self.0.cells {
            sequence.serialize_element(&View {
                frame_id: self.0.frames[index].id,
                scopes: &cell.scopes,
                marks: MarksView {
                    plan: self.0,
                    index,
                    cell,
                },
            })?;
        }
        sequence.end()
    }
}

struct MarksView<'a, 'p> {
    plan: &'a ConversionPlan<'p>,
    index: usize,
    cell: &'a CellPlan,
}

impl Serialize for MarksView<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct View<'a> {
            id: OverlayId,
            z_index: i32,
            content: &'a OverlayContent,
        }
        let mut sequence = serializer.serialize_seq(Some(self.cell.items.len()))?;
        for &index in &self.cell.items {
            let item = &self.plan.track.items[index];
            let content = if matches!(item.content, OverlayContent::Progress { style: None, .. }) {
                // This legacy variant contains no variable-size artwork; only a
                // small numeric style is created while determining exact bytes.
                Cow::Owned(
                    freeze_timed_overlay_content(
                        item,
                        TimeUs::new(self.plan.intervals[self.index].start),
                    )
                    .map_err(S::Error::custom)?
                    .ok_or_else(|| S::Error::custom("inactive indexed overlay"))?,
                )
            } else {
                Cow::Borrowed(&item.content)
            };
            sequence.serialize_element(&View {
                id: OverlayId::NIL,
                z_index: item.z_index,
                content: &content,
            })?;
        }
        sequence.end()
    }
}

fn metadata_bytes(value: &impl Serialize, cancellation: &AtomicBool) -> Result<usize, String> {
    let mut counter = MetadataCounter {
        bytes: 0,
        cancellation,
    };
    let result = serde_json::to_writer(&mut counter, value);
    check_cancelled(cancellation)?;
    result.map_err(|error| format!("Could not prepare bounded conversion metadata: {error}"))?;
    Ok(counter.bytes)
}

struct MetadataCounter<'a> {
    bytes: usize,
    cancellation: &'a AtomicBool,
}

impl Write for MetadataCounter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancellation.load(Ordering::Relaxed) {
            return Err(io::Error::other("conversion cancelled"));
        }
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|bytes| *bytes <= MAX_FRAME_BUNDLE_METADATA_BYTES)
            .ok_or_else(|| io::Error::other("conversion exceeds 16 MiB"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "editor_frame_conversion_tests.rs"]
mod tests;
