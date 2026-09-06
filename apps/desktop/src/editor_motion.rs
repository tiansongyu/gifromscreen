//! Bounded baked motion edits. The caller lends the workspace to a background worker.

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};

use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, CaptureMetadata, ClipTransform, DurationUs, EditCommand,
    FrameClip, FrameId, OverlayId, OverlayItem, PhysicalRect, PhysicalSize, RasterEncoding, TimeUs,
    TimelineSpan, TransitionKind,
};
use gif_from_screen_project::ActiveProject;
use gif_from_screen_render::{
    CancellationToken, RgbaSurface, TransitionProgress, render_transition,
};
use uuid::Uuid;

use super::{EditorWorkspace, OverlaySelectionAnchor};
use crate::editor_preview::PreviewRenderPlan;

const MAX_BAKED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SURFACE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CINEMAGRAPH_FRAMES: usize = 1_000;
const MAX_LOOP_FRAMES: u16 = 120;
const MAX_RESULTING_FRAMES: usize = 100_000;
const MAX_OVERLAY_FRAGMENTS: usize = 10_000;

#[derive(Clone, Copy, Debug)]
pub(crate) enum MotionOperation {
    /// Preserve current-frame pixels outside the rectangle; invert freezes inside instead.
    Cinemagraph { region: PhysicalRect, invert: bool },
    /// Append fully rendered cross-fade frames from the last original to the first original.
    SmoothLoop { frames: u16, duration_us: u64 },
}

impl MotionOperation {
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Cinemagraph { .. } => "Rectangular cinemagraph",
            Self::SmoothLoop { .. } => "Smooth loop",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MotionProgress {
    pub(crate) completed: usize,
    pub(crate) total: usize,
}

struct Cancellation<'a>(&'a AtomicBool);
impl CancellationToken for Cancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl EditorWorkspace {
    /// Must run on a background worker: rendering, immutable blobs, and the
    /// one atomic journal commit all happen before the workspace is returned.
    pub(crate) fn apply_motion_edit(
        &mut self,
        anchor: &OverlaySelectionAnchor,
        operation: MotionOperation,
        cancellation: &AtomicBool,
        mut progress: impl FnMut(MotionProgress),
    ) -> Result<usize, String> {
        check_cancelled(cancellation)?;
        if !anchor.matches(self) {
            return Err(
                "The project revision or selected frames changed. Choose the motion edit again."
                    .to_owned(),
            );
        }
        let (command, count) = match operation {
            MotionOperation::Cinemagraph { region, invert } => {
                self.cinemagraph_command(region, invert, cancellation, &mut progress)?
            }
            MotionOperation::SmoothLoop {
                frames,
                duration_us,
            } => self.smooth_loop_command(frames, duration_us, cancellation, &mut progress)?,
        };
        check_cancelled(cancellation)?;
        // This includes fsync and undo history updates, exclusively on the worker.
        self.execute(command).map_err(|error| error.to_string())?;
        Ok(count)
    }

    fn cinemagraph_command(
        &self,
        region: PhysicalRect,
        invert: bool,
        cancellation: &AtomicBool,
        progress: &mut impl FnMut(MotionProgress),
    ) -> Result<(EditCommand, usize), String> {
        let current = self
            .selection()
            .current()
            .ok_or_else(|| "Select the frame to use as the frozen image first.".to_owned())?;
        let selected: Vec<_> = self
            .manifest()
            .timeline
            .frames
            .iter()
            .filter(|frame| self.selection().contains(frame.id))
            .cloned()
            .collect();
        if selected.is_empty() || selected.len() > MAX_CINEMAGRAPH_FRAMES {
            return Err(
                "Select between 1 and 1,000 frames for a rectangular cinemagraph.".to_owned(),
            );
        }
        let canvas = self.manifest().canvas.size;
        validate_budget(canvas, selected.len())?;
        if region.size.validate().is_err() || !region.fits_within(canvas) {
            return Err(
                "The motion rectangle must have nonzero dimensions and fit inside the canvas."
                    .to_owned(),
            );
        }
        let baseline = render(self.active_project(), current, canvas, cancellation)?;
        let spans = self
            .selected_timeline_spans()
            .map_err(|error| error.to_string())?;
        let mut commands =
            remove_baked_overlay_spans(&self.manifest().timeline.overlay_tracks, &spans)?;
        let mut registered = BTreeSet::new();
        for (index, original) in selected.iter().enumerate() {
            check_cancelled(cancellation)?;
            let mut pixels = render(self.active_project(), original.id, canvas, cancellation)?;
            freeze_rectangle(&mut pixels, &baseline, region, invert, cancellation)?;
            let asset_id = store_surface(
                self.active_project(),
                &pixels,
                &mut registered,
                &mut commands,
            )?;
            let replacement = FrameClip {
                asset_id,
                transform: ClipTransform::default(),
                effects: Vec::new(),
                ..original.clone()
            };
            commands.push(EditCommand::ReplaceFrame {
                frame_id: original.id,
                replacement,
            });
            progress(MotionProgress {
                completed: index + 1,
                total: selected.len(),
            });
        }
        Ok((EditCommand::Compound { commands }, selected.len()))
    }

    fn smooth_loop_command(
        &self,
        count: u16,
        duration_us: u64,
        cancellation: &AtomicBool,
        progress: &mut impl FnMut(MotionProgress),
    ) -> Result<(EditCommand, usize), String> {
        if count == 0
            || count > MAX_LOOP_FRAMES
            || duration_us < u64::from(count)
            || duration_us > 60_000_000
        {
            return Err("A smooth loop needs 1–120 frames and a total duration up to 60 seconds, at least one microsecond per frame.".to_owned());
        }
        let originals = &self.manifest().timeline.frames;
        if originals.len() < 2
            || originals.len().saturating_add(usize::from(count)) > MAX_RESULTING_FRAMES
        {
            return Err("A smooth loop needs at least two original frames and cannot exceed 100,000 total frames.".to_owned());
        }
        let canvas = self.manifest().canvas.size;
        validate_budget(canvas, usize::from(count))?;
        self.manifest()
            .timeline
            .total_duration()
            .and_then(|duration| {
                self.manifest()
                    .timeline
                    .transitions
                    .iter()
                    .try_fold(duration.get(), |total, transition| {
                        total.checked_add(transition.duration.get())
                    })
            })
            .and_then(|duration| duration.checked_add(duration_us))
            .ok_or_else(|| {
                "The smooth loop would overflow the presentation duration.".to_owned()
            })?;
        let first = render(self.active_project(), originals[0].id, canvas, cancellation)?;
        let last = render(
            self.active_project(),
            originals[originals.len() - 1].id,
            canvas,
            cancellation,
        )?;
        let mut commands = Vec::new();
        let mut registered = BTreeSet::new();
        let mut frames = Vec::with_capacity(usize::from(count));
        for index in 0..count {
            check_cancelled(cancellation)?;
            let pixels = render_transition(
                &last,
                &first,
                &TransitionKind::FadeToNext,
                TransitionProgress::new(u32::from(index) + 1, u32::from(count))
                    .map_err(|error| error.to_string())?,
                &Cancellation(cancellation),
            )
            .map_err(|error| error.to_string())?;
            let asset_id = store_surface(
                self.active_project(),
                &pixels,
                &mut registered,
                &mut commands,
            )?;
            let start = u64::from(index) * duration_us / u64::from(count);
            let end = (u64::from(index) + 1) * duration_us / u64::from(count);
            frames.push(FrameClip {
                id: FrameId::from_u128(Uuid::new_v4().as_u128()),
                asset_id,
                duration: DurationUs::new(end - start)
                    .ok_or_else(|| "Loop frame duration became zero.".to_owned())?,
                transform: ClipTransform::default(),
                effects: Vec::new(),
                capture_metadata: CaptureMetadata::default(),
            });
            progress(MotionProgress {
                completed: usize::from(index) + 1,
                total: usize::from(count),
            });
        }
        commands.push(EditCommand::InsertFrames {
            index: originals.len(),
            frames,
        });
        // These endpoint pixels already include their original timed overlays.
        // Preserve all original overlay spans exactly; none may stretch into
        // the appended baked frames when insertion remaps timeline anchors.
        commands.extend(
            self.manifest()
                .timeline
                .overlay_tracks
                .iter()
                .cloned()
                .map(|track| EditCommand::UpsertOverlayTrack { track }),
        );
        Ok((EditCommand::Compound { commands }, usize::from(count)))
    }
}

fn validate_budget(canvas: PhysicalSize, frames: usize) -> Result<(), String> {
    let bytes = u64::from(canvas.width.get())
        .checked_mul(u64::from(canvas.height.get()))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "Motion canvas byte count overflow.".to_owned())?;
    if canvas.width.get() == 0
        || canvas.height.get() == 0
        || bytes > MAX_SURFACE_BYTES as u64
        || bytes
            .checked_mul(frames as u64)
            .is_none_or(|total| total > MAX_BAKED_BYTES)
    {
        return Err("Motion edit exceeds the 64 MiB per-frame or 256 MiB total baked-pixel budget. Use fewer frames or a smaller canvas.".to_owned());
    }
    Ok(())
}

fn render(
    project: &ActiveProject,
    id: FrameId,
    canvas: PhysicalSize,
    cancellation: &AtomicBool,
) -> Result<RgbaSurface, String> {
    check_cancelled(cancellation)?;
    let timeline = &project.manifest().timeline;
    let clip = timeline
        .frames
        .iter()
        .find(|frame| frame.id == id)
        .ok_or_else(|| "The motion endpoint frame no longer exists.".to_owned())?;
    let start = timeline
        .frame_start(id)
        .ok_or_else(|| "The motion frame time cannot be represented.".to_owned())?;
    let pixels = PreviewRenderPlan::new(project, clip, start)
        .map_err(|error| error.to_string())?
        .render(MAX_SURFACE_BYTES, &Cancellation(cancellation))
        .map_err(|error| error.to_string())?;
    if pixels.size() != canvas {
        return Err("Rendered frame dimensions differ from the canvas. Apply consistent resize/crop settings before this motion edit.".to_owned());
    }
    Ok(pixels)
}

fn freeze_rectangle(
    animated: &mut RgbaSurface,
    baseline: &RgbaSurface,
    region: PhysicalRect,
    invert: bool,
    cancellation: &AtomicBool,
) -> Result<(), String> {
    let row_bytes =
        usize::try_from(u64::from(animated.width()) * 4).map_err(|error| error.to_string())?;
    let x =
        usize::try_from(u64::from(region.origin.x.get()) * 4).map_err(|error| error.to_string())?;
    let right = x + usize::try_from(u64::from(region.size.width.get()) * 4)
        .map_err(|error| error.to_string())?;
    let top = region.origin.y.get();
    let bottom = top + region.size.height.get();
    for (index, (output, frozen)) in animated
        .pixels_mut()
        .chunks_exact_mut(row_bytes)
        .zip(baseline.pixels().chunks_exact(row_bytes))
        .enumerate()
    {
        check_cancelled(cancellation)?;
        let row = u32::try_from(index).map_err(|error| error.to_string())?;
        let within = row >= top && row < bottom;
        if invert {
            if within {
                output[x..right].copy_from_slice(&frozen[x..right]);
            }
        } else if within {
            output[..x].copy_from_slice(&frozen[..x]);
            output[right..].copy_from_slice(&frozen[right..]);
        } else {
            output.copy_from_slice(frozen);
        }
    }
    Ok(())
}

fn store_surface(
    project: &ActiveProject,
    surface: &RgbaSurface,
    registered: &mut BTreeSet<AssetId>,
    commands: &mut Vec<EditCommand>,
) -> Result<AssetId, String> {
    let id = gif_from_screen_project::AssetStore::id_for_bytes(surface.pixels());
    if let Some(descriptor) = project.manifest().assets.get(&id)
        && (descriptor.kind.raster_descriptor() != Some((surface.size(), RasterEncoding::Rgba8))
            || descriptor.byte_len != surface.pixels().len() as u64)
    {
        return Err("An existing raster asset has incompatible dimensions or encoding.".to_owned());
    }
    project
        .assets()
        .put(surface.pixels())
        .map_err(|error| error.to_string())?;
    if !project.manifest().assets.contains_key(&id) && registered.insert(id) {
        commands.push(EditCommand::RegisterAsset {
            asset: AssetDescriptor {
                id,
                byte_len: surface.pixels().len() as u64,
                kind: AssetKind::Frame {
                    size: surface.size(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        });
    }
    Ok(id)
}

fn remove_baked_overlay_spans(
    tracks: &[gif_from_screen_domain::OverlayTrack],
    selected: &[TimelineSpan],
) -> Result<Vec<EditCommand>, String> {
    let mut commands = Vec::new();
    let mut total_fragments = 0_usize;
    for track in tracks {
        let mut replacement = track.clone();
        replacement.items.clear();
        for item in &track.items {
            for (index, span) in subtract_spans(item.span, selected)?.into_iter().enumerate() {
                total_fragments += 1;
                if total_fragments > MAX_OVERLAY_FRAGMENTS {
                    return Err(
                        "Motion edit would create more than 10,000 overlay fragments.".to_owned(),
                    );
                }
                replacement.items.push(OverlayItem {
                    id: if index == 0 {
                        item.id
                    } else {
                        OverlayId::from_u128(Uuid::new_v4().as_u128())
                    },
                    span,
                    ..item.clone()
                });
            }
        }
        if replacement != *track {
            commands.push(if replacement.items.is_empty() {
                EditCommand::RemoveOverlayTrack { track_id: track.id }
            } else {
                EditCommand::UpsertOverlayTrack { track: replacement }
            });
        }
    }
    Ok(commands)
}

fn subtract_spans(
    span: TimelineSpan,
    selected: &[TimelineSpan],
) -> Result<Vec<TimelineSpan>, String> {
    let end = span
        .end()
        .ok_or_else(|| "Overlay span overflows the timeline.".to_owned())?
        .get();
    let mut cursor = span.start.get();
    let mut output = Vec::new();
    for cut in selected {
        let cut_end = cut
            .end()
            .ok_or_else(|| "Selected span overflows the timeline.".to_owned())?
            .get();
        if cut_end <= cursor || cut.start.get() >= end {
            continue;
        }
        if cut.start.get() > cursor {
            output.push(TimelineSpan {
                start: TimeUs::new(cursor),
                duration: DurationUs::new(cut.start.get() - cursor).unwrap(),
            });
        }
        cursor = cursor.max(cut_end).min(end);
    }
    if cursor < end {
        output.push(TimelineSpan {
            start: TimeUs::new(cursor),
            duration: DurationUs::new(end - cursor).unwrap(),
        });
    }
    Ok(output)
}

fn check_cancelled(cancellation: &AtomicBool) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("Motion edit cancelled before commit; the timeline is unchanged.".to_owned())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "editor_motion/tests.rs"]
mod tests;
