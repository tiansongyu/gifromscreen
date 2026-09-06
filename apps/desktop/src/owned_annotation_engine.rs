//! Whole-frame annotation authoring, with immutable source-input pools shared by owner cells.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    sync::atomic::AtomicBool,
};

use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, AssetDescriptor, AssetId, AssetKind, CaptureBinding,
    EditCommand, FrameAuthoringSpan, FrameClip, FrameId, FrameInputReplay, FrameInputReplayPool,
    FrameInputReplayRef, FrameLocalSpan, FrameOverlayCell, FrameOverlayMark,
    INPUT_REPLAY_MEDIA_TYPE, InputReplayStep, MAX_INPUT_REPLAY_EVENTS, MAX_INPUT_REPLAY_POOL_BYTES,
    OverlayContent, OverlayId, OverlayTrack, ProjectManifest, TimeUs,
};
use gif_from_screen_project::AssetStore;
use gif_from_screen_render::RgbaSurface;

use super::{
    AnnotationFrame, AnnotationProgress, AnnotationReplaySkips, CommandBudget, Labels,
    PreparedAnnotations, ScopePlan, check_cancelled, click, keys::KeyLabelHistory,
    prepare_annotation_plan, prepare_frame, rebase_position, replay_filter, transform_point,
    update_click_history,
};
use uuid::Uuid;

const MAX_REPLAY_BYTES: usize = 64 * 1024 * 1024;

#[cfg(test)]
#[path = "owned_annotation_engine_tests.rs"]
mod tests;

struct PoolBuilder {
    pool: FrameInputReplayPool,
    cells: Vec<(FrameId, FrameInputReplayRef)>,
    events: usize,
    serialized_bytes: usize,
}

pub(super) fn prepare_new(
    manifest: &ProjectManifest,
    plan: &ScopePlan,
    request: &AnnotationRequest,
    cancellation: &AtomicBool,
    progress: impl FnMut(AnnotationProgress),
    provider: &dyn Fn(AssetId) -> Result<RgbaSurface, String>,
) -> Result<PreparedAnnotations, String> {
    let mut prepared =
        prepare_annotation_plan(manifest, plan, request, cancellation, progress, provider)?;
    let Some(EditCommand::UpsertOverlayTrack { track }) = prepared.commands.last_mut() else {
        return Ok(prepared);
    };
    let (blocked, _) = replay_filter(manifest, &plan.selected, &request.mode);
    let mut cells: BTreeMap<FrameId, FrameOverlayCell> = BTreeMap::new();
    let mut by_start = BTreeMap::new();
    for sample in &plan.samples {
        check_cancelled(cancellation)?;
        let frame = &manifest.timeline.frames[sample.index];
        if blocked.contains(&frame.id) {
            continue;
        }
        let run_id = u32::try_from(sample.run + 1).map_err(|_| "Too many annotation runs.")?;
        let span = FrameLocalSpan::new(
            sample.span.start.get() - sample.frame_start,
            sample.span.end().ok_or("Annotation span overflow.")?.get() - sample.frame_start,
            frame.duration,
        )
        .ok_or("Invalid frame authoring coverage.")?;
        by_start.insert(sample.span.start, frame.id);
        cells
            .entry(frame.id)
            .or_insert_with(|| FrameOverlayCell {
                frame_id: frame.id,
                scopes: Vec::new(),
                marks: Vec::new(),
                input_replay: None,
            })
            .scopes
            .push(FrameAuthoringSpan { run_id, span });
    }
    for item in std::mem::take(&mut track.items) {
        let owner = by_start
            .get(&item.span.start)
            .ok_or("An annotation lost its owner frame.")?;
        cells
            .get_mut(owner)
            .expect("planned cell")
            .marks
            .push(FrameOverlayMark {
                id: item.id,
                z_index: item.z_index,
                content: item.content,
            });
    }
    let pool_assets = if matches!(
        request.mode,
        AnnotationMode::RecordedKeys | AnnotationMode::RecordedClicks
    ) {
        attach_replay(manifest, plan, &blocked, &mut cells, cancellation)?
    } else {
        Vec::new()
    };
    let clocks = missing_sample_contexts(manifest, &cells);
    track.annotation_scope = None;
    track.frame_cells = Some(cells.into_values().collect());
    let track_command = prepared.commands.pop().expect("prepared track");
    for (asset, bytes) in pool_assets {
        if let Some(existing) = manifest.assets.get(&asset.id) {
            if *existing != asset {
                return Err(
                    "Input replay asset descriptor conflicts with existing data.".to_owned(),
                );
            }
        } else {
            prepared.commands.push(EditCommand::RegisterAsset {
                asset: asset.clone(),
            });
        }
        prepared.assets.push((asset, bytes));
    }
    if !clocks.is_empty() {
        prepared
            .commands
            .push(EditCommand::SetCaptureClocks { changes: clocks });
    }
    prepared.commands.push(track_command);
    serde_json::to_writer(&mut CommandBudget(MAX_REPLAY_BYTES), &prepared.commands)
        .map_err(|_| "Annotation commands exceed 64 MiB.".to_owned())?;
    Ok(prepared)
}

fn missing_sample_contexts(
    manifest: &ProjectManifest,
    cells: &BTreeMap<FrameId, FrameOverlayCell>,
) -> Vec<gif_from_screen_domain::FrameCaptureClockChange> {
    manifest
        .timeline
        .frames
        .iter()
        .filter_map(|frame| {
            if frame.capture_clock.is_some() || frame.capture_metadata.captured_at.is_some() {
                return None;
            }
            let replay = cells.get(&frame.id)?.input_replay.as_ref()?.runs.first()?;
            Some(gif_from_screen_domain::FrameCaptureClockChange {
                frame_id: frame.id,
                clock: Some(gif_from_screen_domain::CaptureClockContext {
                    id: None,
                    sampled_at: replay.sample_at,
                }),
            })
        })
        .collect()
}

fn attach_replay(
    manifest: &ProjectManifest,
    plan: &ScopePlan,
    blocked: &BTreeSet<FrameId>,
    cells: &mut BTreeMap<FrameId, FrameOverlayCell>,
    cancellation: &AtomicBool,
) -> Result<Vec<(AssetDescriptor, Vec<u8>)>, String> {
    let mut pools: Vec<PoolBuilder> = Vec::new();
    let mut previous = None;
    let mut retained_bytes = 0_usize;
    for sample in &plan.samples {
        check_cancelled(cancellation)?;
        let frame = &manifest.timeline.frames[sample.index];
        if blocked.contains(&frame.id) || frame.capture_binding != CaptureBinding::Original {
            previous = None;
            continue;
        }
        let at = frame
            .capture_sample_time()
            .unwrap_or(TimeUs::new(sample.frame_start));
        let clock = frame.capture_clock.and_then(|clock| clock.id);
        let key = (sample.run, clock, at);
        if previous.is_none_or(|(run, identity, before)| {
            run != sample.run || identity != clock || clock.is_none() || at <= before
        }) {
            retained_bytes = retained_bytes
                .checked_add(512)
                .filter(|n| *n <= MAX_REPLAY_BYTES)
                .ok_or("Input replay pools exceed 64 MiB.")?;
            pools.push(PoolBuilder {
                pool: FrameInputReplayPool {
                    version: 1,
                    clock_id: clock,
                    started_at: at,
                    steps: Vec::new(),
                },
                cells: Vec::new(),
                events: 0,
                serialized_bytes: 512,
            });
        }
        previous = Some(key);
        let builder = pools.last_mut().expect("source segment");
        let raw = &frame.capture_metadata;
        if !raw.key_strokes.is_empty() || !raw.mouse_events.is_empty() {
            check_step_budget(builder, raw, at, &mut retained_bytes)?;
            builder.events = builder
                .events
                .checked_add(raw.key_strokes.len())
                .and_then(|n| n.checked_add(raw.mouse_events.len()))
                .filter(|n| *n <= MAX_INPUT_REPLAY_EVENTS)
                .ok_or(
                    "An input replay segment exceeds 65,536 events; select a shorter interval.",
                )?;
            builder.pool.steps.push(InputReplayStep {
                sample_at: at,
                capture_origin: raw.capture_origin,
                keys: raw.key_strokes.clone(),
                mouse_events: raw.mouse_events.clone(),
            });
        }
        builder.cells.push((
            frame.id,
            FrameInputReplayRef {
                run_id: u32::try_from(sample.run + 1).map_err(|_| "Too many annotation runs.")?,
                asset_id: AssetId::from_digest([0; 32]),
                sample_at: at,
                step_end: u32::try_from(builder.pool.steps.len())
                    .map_err(|_| "Too many input steps.")?,
            },
        ));
    }
    finish_pools(pools, cells, cancellation)
}

fn finish_pools(
    pools: Vec<PoolBuilder>,
    cells: &mut BTreeMap<FrameId, FrameOverlayCell>,
    cancellation: &AtomicBool,
) -> Result<Vec<(AssetDescriptor, Vec<u8>)>, String> {
    let mut assets = BTreeMap::new();
    let mut total_bytes = 0_usize;
    for builder in pools {
        check_cancelled(cancellation)?;
        builder.pool.validate()?;
        serde_json::to_writer(
            &mut CommandBudget(
                usize::try_from(MAX_INPUT_REPLAY_POOL_BYTES).expect("8 MiB fits usize"),
            ),
            &builder.pool,
        )
        .map_err(|_| "An input replay pool exceeds 8 MiB; select a shorter interval.".to_owned())?;
        let bytes = serde_json::to_vec(&builder.pool).map_err(|error| error.to_string())?;
        let id = AssetStore::id_for_bytes(&bytes);
        if let std::collections::btree_map::Entry::Vacant(entry) = assets.entry(id) {
            total_bytes = total_bytes
                .checked_add(bytes.len())
                .filter(|n| *n <= MAX_REPLAY_BYTES)
                .ok_or("Input replay pools exceed the 64 MiB budget.")?;
            entry.insert((
                AssetDescriptor {
                    id,
                    byte_len: bytes.len() as u64,
                    kind: AssetKind::ImportedSource {
                        media_type: INPUT_REPLAY_MEDIA_TYPE.to_owned(),
                    },
                },
                bytes,
            ));
        }
        for (owner, mut reference) in builder.cells {
            reference.asset_id = id;
            let cell = cells
                .get_mut(&owner)
                .ok_or("Input replay owner is missing.")?;
            cell.input_replay
                .get_or_insert_with(|| FrameInputReplay { runs: Vec::new() })
                .runs
                .push(reference);
        }
    }
    Ok(assets.into_values().collect())
}

fn check_step_budget(
    builder: &mut PoolBuilder,
    raw: &gif_from_screen_domain::CaptureMetadata,
    at: TimeUs,
    retained: &mut usize,
) -> Result<(), String> {
    #[derive(serde::Serialize)]
    struct BorrowedStep<'a> {
        sample_at: TimeUs,
        capture_origin: Option<gif_from_screen_domain::CaptureOrigin>,
        keys: &'a [gif_from_screen_domain::KeyStroke],
        mouse_events: &'a [gif_from_screen_domain::MouseInputEvent],
    }
    if raw.key_strokes.len() > 512 || raw.mouse_events.len() > 512 {
        return Err("A replay sample exceeds 512 events of one input kind.".to_owned());
    }
    let maximum = usize::try_from(MAX_INPUT_REPLAY_POOL_BYTES).expect("8 MiB fits usize");
    let mut budget = CommandBudget(maximum);
    serde_json::to_writer(
        &mut budget,
        &BorrowedStep {
            sample_at: at,
            capture_origin: raw.capture_origin,
            keys: &raw.key_strokes,
            mouse_events: &raw.mouse_events,
        },
    )
    .map_err(|_| "An input replay sample exceeds 8 MiB.".to_owned())?;
    let bytes = maximum - budget.0 + 1;
    builder.serialized_bytes = builder
        .serialized_bytes
        .checked_add(bytes)
        .filter(|n| *n <= maximum)
        .ok_or("An input replay segment exceeds 8 MiB.")?;
    *retained = retained
        .checked_add(bytes)
        .filter(|n| *n <= MAX_REPLAY_BYTES)
        .ok_or("Input replay pools exceed 64 MiB.")?;
    Ok(())
}

// The replacement implementation below uses source-pool order, never playback order.

pub(crate) fn prepare_replacement(
    manifest: &ProjectManifest,
    original: &OverlayTrack,
    request: &AnnotationRequest,
    store: &AssetStore,
    cancellation: &AtomicBool,
    mut progress: impl FnMut(AnnotationProgress),
    provider: &dyn Fn(AssetId) -> Result<RgbaSurface, String>,
) -> Result<PreparedAnnotations, String> {
    check_cancelled(cancellation)?;
    manifest.validate().map_err(|error| error.to_string())?;
    request.validate(manifest.canvas.size)?;
    let cells = original
        .frame_cells
        .as_ref()
        .ok_or("This group has no frame-owned cells.")?;
    if cells.len() > super::MAX_FRAMES {
        return Err("Select at most 10,000 owner frames for re-editing.".to_owned());
    }
    serde_json::to_writer(&mut CommandBudget(MAX_REPLAY_BYTES), original)
        .map_err(|_| "Annotation source metadata exceeds 64 MiB.".to_owned())?;
    let mut labels = Labels {
        manifest,
        request,
        rasterizer: None,
        cache: BTreeMap::new(),
        assets: Vec::new(),
        bytes: 0,
        provider,
        cancellation,
        cursor_cache: BTreeMap::new(),
    };
    let intervals = frame_intervals(manifest)?;
    let total_us = manifest
        .timeline
        .total_duration()
        .ok_or("Timeline duration overflow.")?
        .get();
    let mut content = BTreeMap::new();
    if matches!(
        request.mode,
        AnnotationMode::RecordedKeys | AnnotationMode::RecordedClicks
    ) {
        replay_cells(&mut labels, cells, &intervals, store, &mut content)?;
    } else {
        for cell in cells {
            check_cancelled(cancellation)?;
            let (index, end) = intervals[&cell.frame_id];
            let frame = &manifest.timeline.frames[index];
            ensure_original_binding(frame, request)?;
            let sample = AnnotationFrame {
                frame,
                index,
                end,
                total_us,
                clock: frame.capture_sample_time().map_or(0, TimeUs::get),
            };
            content.insert(
                frame.id,
                prepare_frame(
                    &mut labels,
                    &sample,
                    &mut KeyLabelHistory::default(),
                    &mut Vec::new(),
                )?,
            );
        }
    }
    finish_replacement(labels, original, content, &mut progress)
}

fn finish_replacement(
    labels: Labels<'_>,
    original: &OverlayTrack,
    mut content: BTreeMap<FrameId, Vec<OverlayContent>>,
    progress: &mut impl FnMut(AnnotationProgress),
) -> Result<PreparedAnnotations, String> {
    let request = labels.request;
    let manifest = labels.manifest;
    let total = original.frame_cells.as_ref().expect("owned group").len();
    let mut replacement = original.clone();
    replacement.annotation = Some(request.clone());
    replacement.opacity = request.opacity;
    let mut affected = 0_usize;
    let mut count = 0_usize;
    for (index, cell) in replacement
        .frame_cells
        .as_mut()
        .expect("owned track")
        .iter_mut()
        .enumerate()
    {
        check_cancelled(labels.cancellation)?;
        let contents = content.remove(&cell.frame_id).unwrap_or_default();
        count = count
            .checked_add(contents.len())
            .filter(|n| *n <= super::MAX_ITEMS)
            .ok_or("Annotations exceed 40,000 marks.")?;
        affected += usize::from(!contents.is_empty());
        cell.marks = contents
            .into_iter()
            .map(|content| FrameOverlayMark {
                id: OverlayId::from_u128(Uuid::new_v4().as_u128()),
                z_index: request.z_index,
                content,
            })
            .collect();
        progress(AnnotationProgress {
            completed: index + 1,
            total,
        });
    }
    let mut commands: Vec<_> = labels
        .assets
        .iter()
        .filter(|(asset, _)| !manifest.assets.contains_key(&asset.id))
        .map(|(asset, _)| EditCommand::RegisterAsset {
            asset: asset.clone(),
        })
        .collect();
    commands.push(EditCommand::UpsertOverlayTrack { track: replacement });
    serde_json::to_writer(&mut CommandBudget(MAX_REPLAY_BYTES), &commands)
        .map_err(|_| "Annotation commands exceed 64 MiB.".to_owned())?;
    Ok(PreparedAnnotations {
        commands,
        assets: labels.assets,
        frames: affected,
        replay_skips: AnnotationReplaySkips::default(),
    })
}

fn frame_intervals(manifest: &ProjectManifest) -> Result<BTreeMap<FrameId, (usize, u64)>, String> {
    let mut result = BTreeMap::new();
    let mut end = 0_u64;
    for (index, frame) in manifest.timeline.frames.iter().enumerate() {
        end = end
            .checked_add(frame.duration.get())
            .ok_or("Timeline duration overflow.")?;
        result.insert(frame.id, (index, end));
    }
    Ok(result)
}

fn ensure_original_binding(frame: &FrameClip, request: &AnnotationRequest) -> Result<(), String> {
    if gif_from_screen_domain::recorded_annotation_barrier(frame, &request.mode) {
        return Err("This group's owner frames include unverified, non-recorded or composited pixels. Recorded-input re-edit is unavailable; the whole group remains unchanged.".to_owned());
    }
    Ok(())
}

fn load_pool(
    manifest: &ProjectManifest,
    store: &AssetStore,
    id: AssetId,
) -> Result<FrameInputReplayPool, String> {
    let descriptor = manifest
        .assets
        .get(&id)
        .ok_or("Input replay asset descriptor is missing.")?;
    if !matches!(&descriptor.kind, AssetKind::ImportedSource { media_type } if media_type == INPUT_REPLAY_MEDIA_TYPE)
        || descriptor.byte_len == 0
        || descriptor.byte_len > MAX_INPUT_REPLAY_POOL_BYTES
    {
        return Err("Input replay asset has an invalid type or exceeds 8 MiB.".to_owned());
    }
    let path = store.asset_path(id);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() != descriptor.byte_len {
        return Err("Input replay must be a regular file of its declared length.".to_owned());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(descriptor.byte_len + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != descriptor.byte_len || AssetStore::id_for_bytes(&bytes) != id {
        return Err(
            "Input replay asset length or digest changed; the whole group is unchanged.".to_owned(),
        );
    }
    let pool: FrameInputReplayPool =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    pool.validate()?;
    Ok(pool)
}

#[derive(Default)]
struct ReplayOutput {
    contents: BTreeMap<FrameId, Vec<OverlayContent>>,
    texts: BTreeMap<FrameId, BTreeMap<u32, String>>,
    marks: usize,
}

impl ReplayOutput {
    fn push(&mut self, owner: FrameId, content: OverlayContent) -> Result<(), String> {
        self.marks = self
            .marks
            .checked_add(1)
            .filter(|n| *n <= super::MAX_ITEMS)
            .ok_or("Annotations exceed 40,000 marks.")?;
        self.contents.entry(owner).or_default().push(content);
        Ok(())
    }

    fn text(&mut self, owner: FrameId, run_id: u32, text: String) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        let runs = self.texts.entry(owner).or_default();
        let bytes = runs.values().map(String::len).sum::<usize>() + 2 * runs.len() + text.len();
        if bytes > 4096 {
            return Err("Combined authoring-run labels exceed 4096 bytes.".to_owned());
        }
        runs.insert(run_id, text);
        Ok(())
    }
}

fn replay_cells(
    labels: &mut Labels<'_>,
    cells: &[FrameOverlayCell],
    intervals: &BTreeMap<FrameId, (usize, u64)>,
    store: &AssetStore,
    output: &mut BTreeMap<FrameId, Vec<OverlayContent>>,
) -> Result<(), String> {
    let mut groups: BTreeMap<(AssetId, u32), Vec<(FrameId, FrameInputReplayRef)>> = BTreeMap::new();
    for cell in cells {
        check_cancelled(labels.cancellation)?;
        let frame = &labels.manifest.timeline.frames[intervals[&cell.frame_id].0];
        ensure_original_binding(frame, labels.request)?;
        let replay = cell.input_replay.as_ref().ok_or("This frame-owned group has no complete recorded-input replay context. Existing marks are preserved; create a new group from the original recording instead.")?;
        replay.validate(&cell.scopes)?;
        let mut contributions = BTreeSet::new();
        let mut references: Vec<_> = replay.runs.iter().collect();
        references.sort_unstable_by_key(|reference| reference.run_id);
        for reference in references {
            // Multiple authoring fragments may name the same source sample.
            // Consume that prefix once, without collapsing distinct real events
            // inside it (two actual clicks at one position remain two clicks).
            if !contributions.insert((reference.asset_id, reference.sample_at, reference.step_end))
            {
                continue;
            }
            groups
                .entry((reference.asset_id, reference.run_id))
                .or_default()
                .push((cell.frame_id, *reference));
        }
    }
    let mut result = ReplayOutput::default();
    let mut total_bytes = 0_u64;
    let mut loaded = None;
    for ((id, run_id), mut owners) in groups {
        check_cancelled(labels.cancellation)?;
        if loaded.as_ref().is_none_or(|(asset, _)| *asset != id) {
            total_bytes = total_bytes
                .checked_add(
                    labels
                        .manifest
                        .assets
                        .get(&id)
                        .ok_or("Input replay asset is missing.")?
                        .byte_len,
                )
                .filter(|n| *n <= MAX_REPLAY_BYTES as u64)
                .ok_or("Input replay reading exceeds 64 MiB.")?;
            loaded = Some((id, load_pool(labels.manifest, store, id)?));
        }
        let pool = &loaded.as_ref().expect("current pool").1;
        owners.sort_unstable_by_key(|(frame, reference)| (reference.sample_at, *frame));
        replay_pool(labels, pool, run_id, &owners, intervals, &mut result)?;
    }
    for (frame_id, runs) in std::mem::take(&mut result.texts) {
        let text = runs
            .into_values()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("  ");
        if text.len() > 4096 {
            return Err("Combined authoring-run labels exceed 4096 bytes.".to_owned());
        }
        if !text.is_empty() {
            result.push(
                frame_id,
                OverlayContent::KeyStroke {
                    text: text.clone(),
                    position: labels.request.position,
                    raster: Some(labels.raster(&text)?),
                },
            )?;
        }
    }
    *output = result.contents;
    Ok(())
}

fn replay_pool(
    labels: &mut Labels<'_>,
    pool: &FrameInputReplayPool,
    run_id: u32,
    owners: &[(FrameId, FrameInputReplayRef)],
    intervals: &BTreeMap<FrameId, (usize, u64)>,
    output: &mut ReplayOutput,
) -> Result<(), String> {
    let mut keys = KeyLabelHistory::default();
    let mut clicks = Vec::new();
    let mut processed = 0_usize;
    for (id, reference) in owners {
        check_cancelled(labels.cancellation)?;
        pool.validate_reference(reference)?;
        let frame = &labels.manifest.timeline.frames[intervals[id].0];
        if pool.clock_id != frame.capture_clock.and_then(|clock| clock.id)
            || frame
                .capture_sample_time()
                .is_some_and(|at| at != reference.sample_at)
        {
            return Err("An owner's source clock no longer matches its saved replay context; the group is unchanged.".to_owned());
        }
        while processed < reference.step_end as usize {
            check_cancelled(labels.cancellation)?;
            let step = &pool.steps[processed];
            if matches!(labels.request.mode, AnnotationMode::RecordedKeys) {
                keys.update(&step.keys, step.sample_at.get(), labels.request.hold_ms)?;
            } else {
                update_click_history(
                    &step.mouse_events,
                    step.capture_origin,
                    step.sample_at.get(),
                    labels.request.hold_ms,
                    &mut clicks,
                )?;
            }
            processed += 1;
        }
        let clock = reference.sample_at.get();
        if matches!(labels.request.mode, AnnotationMode::RecordedKeys) {
            output.text(
                *id,
                run_id,
                keys.update(&[], clock, labels.request.hold_ms)?,
            )?;
        } else {
            update_click_history(&[], None, clock, labels.request.hold_ms, &mut clicks)?;
            for (_, button, position, origin) in &clicks {
                if let Some(position) =
                    rebase_position(*position, *origin, frame.capture_metadata.capture_origin)
                        .and_then(|point| transform_point(labels.manifest, frame, point))
                {
                    output.push(*id, click(position, *button, labels.request))?;
                }
            }
        }
    }
    Ok(())
}
