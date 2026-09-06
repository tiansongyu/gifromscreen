//! Prepare time-anchored annotations without mutating the project or touching its files.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

use gif_from_screen_domain::{
    AnnotationMode, AnnotationRequest, AssetDescriptor, AssetKind, BlendMode, CaptureOrigin,
    EditCommand, FrameClip, FrameId, HorizontalAlignment, MouseButton, OverlayContent, OverlayId,
    OverlayItem, OverlayTrack, PhysicalPoint, PhysicalPx, PhysicalRect, ProgressFraction,
    ProgressMeasure, ProgressOptions, ProgressStyle, ProjectManifest, QuarterTurn, RasterEncoding,
    Rgba, TextRaster, TimeUs, TimelineSpan, TrackId,
};
use gif_from_screen_project::AssetStore;
use gif_from_screen_render::RgbaSurface;
use gif_from_screen_text::{TextRasterizer, TextRequest};
use uuid::Uuid;

const MAX_FRAMES: usize = 10_000;
const MAX_ITEMS: usize = 40_000;
const MAX_PIXEL_BYTES: usize = 256 * 1024 * 1024;
const MAX_EVENTS_PER_FRAME: usize = 512;
const MAX_ACTIVE_EVENTS: usize = 256;

#[path = "annotation_cursor.rs"]
mod cursor;

#[path = "annotation_keys.rs"]
mod keys;
use keys::KeyLabelHistory;

#[path = "annotation_scope_plan.rs"]
mod scope;
use scope::ScopePlan;

#[cfg(test)]
#[path = "annotation_scope_tests.rs"]
mod scope_tests;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AnnotationReplaySkips {
    pub(crate) legacy_unknown: usize,
    pub(crate) archived_after_composite: usize,
    pub(crate) not_recorded: usize,
}

impl AnnotationReplaySkips {
    pub(crate) fn is_empty(self) -> bool {
        self.legacy_unknown == 0 && self.archived_after_composite == 0 && self.not_recorded == 0
    }
    pub(crate) fn message(self) -> String {
        format!(
            "Recorded input skipped on {} frame(s) with unverified legacy coordinates, {} frame(s) with archived composite input, and {} non-recorded frame(s). Raw events are preserved; use original recordings or manual annotations.",
            self.legacy_unknown, self.archived_after_composite, self.not_recorded
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AnnotationEditReport {
    pub(crate) frames: usize,
    pub(crate) replay_skips: AnnotationReplaySkips,
}

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct AnnotationProgress {
    pub(crate) completed: usize,
    pub(crate) total: usize,
}

#[derive(Debug)]
pub(crate) struct PreparedAnnotations {
    /// Includes all registrations before the track upsert; suitable for one compound edit.
    pub(crate) commands: Vec<EditCommand>,
    pub(crate) assets: Vec<(AssetDescriptor, Vec<u8>)>,
    pub(crate) frames: usize,
    pub(crate) replay_skips: AnnotationReplaySkips,
}

struct Labels<'a> {
    manifest: &'a ProjectManifest,
    request: &'a AnnotationRequest,
    rasterizer: Option<TextRasterizer>,
    cache: BTreeMap<String, TextRaster>,
    assets: Vec<(AssetDescriptor, Vec<u8>)>,
    bytes: usize,
    provider: &'a dyn Fn(gif_from_screen_domain::AssetId) -> Result<RgbaSurface, String>,
    cancellation: &'a AtomicBool,
    cursor_cache: BTreeMap<String, Option<OverlayContent>>,
}

impl Labels<'_> {
    fn cursor_overlay(&mut self, frame: &FrameClip) -> Result<Option<OverlayContent>, String> {
        let meta = &frame.capture_metadata;
        if !meta.cursor_visible || meta.cursor_embedded {
            return Ok(None);
        }
        let asset_id = meta.cursor_asset.ok_or_else(||
            "This frame has no recorded cursor image. Use the manual built-in cursor mode when the backend cannot save the pointer.".to_owned())?;
        let descriptor = self
            .manifest
            .assets
            .get(&asset_id)
            .ok_or_else(|| "A recorded cursor asset is missing.".to_owned())?;
        if !matches!(
            descriptor.kind.raster_descriptor(),
            Some((_, RasterEncoding::Rgba8))
        ) || descriptor.byte_len > 64 * 1024 * 1024
        {
            return Err("Recorded cursor must be RGBA8, up to 64 MiB.".to_owned());
        }
        let frame_size = self
            .manifest
            .assets
            .get(&frame.asset_id)
            .and_then(|asset| asset.kind.raster_size())
            .ok_or_else(|| "Recorded cursor frame dimensions are unavailable.".to_owned())?;
        let key = serde_json::to_string(&(
            asset_id,
            frame.transform,
            frame_size,
            meta.cursor_position,
            meta.cursor_hotspot,
        ))
        .map_err(|error| error.to_string())?;
        if let Some(content) = self.cursor_cache.get(&key) {
            return Ok(content.clone());
        }
        let source = (self.provider)(asset_id)?;
        if source.size() != descriptor.kind.raster_size().expect("validated raster")
            || source.pixels().len() as u64 != descriptor.byte_len
            || AssetStore::id_for_bytes(source.pixels()) != asset_id
        {
            return Err(
                "Recorded cursor bytes do not match their immutable descriptor.".to_owned(),
            );
        }
        if source
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[3] == 0)
        {
            self.cursor_cache.insert(key, None);
            return Ok(None);
        }
        let content = if let Some((surface, position)) =
            cursor::transform_cursor(&source, frame, frame_size, self.cancellation)?
        {
            Some(OverlayContent::Cursor {
                cursor_asset: Some(self.store_cursor(surface)?),
                position,
                hotspot: PhysicalPoint::default(),
            })
        } else {
            None
        };
        self.cursor_cache.insert(key, content.clone());
        Ok(content)
    }

    fn store_cursor(
        &mut self,
        mut surface: RgbaSurface,
    ) -> Result<gif_from_screen_domain::AssetId, String> {
        for _ in 0..32 {
            let id = AssetStore::id_for_bytes(surface.pixels());
            let existing = self
                .assets
                .iter()
                .map(|(asset, _)| asset)
                .find(|asset| asset.id == id)
                .or_else(|| self.manifest.assets.get(&id));
            let descriptor = if let Some(existing) = existing {
                if existing.kind.raster_descriptor()
                    != Some((surface.size(), RasterEncoding::Rgba8))
                    || existing.byte_len != surface.pixels().len() as u64
                {
                    // Raw pixels are content-addressed without dimensions. A rotated solid
                    // icon can have the same bytes but a different shape. Transparent right
                    // padding preserves its exact visible pixels and existing descriptors.
                    surface = cursor::pad_cursor_surface(&surface)?;
                    continue;
                }
                existing.clone()
            } else {
                AssetDescriptor {
                    id,
                    byte_len: surface.pixels().len() as u64,
                    kind: AssetKind::OverlayImage {
                        size: surface.size(),
                        encoding: RasterEncoding::Rgba8,
                    },
                }
            };
            if !self.assets.iter().any(|(asset, _)| asset.id == id) {
                self.bytes = self
                    .bytes
                    .checked_add(surface.pixels().len())
                    .filter(|bytes| *bytes <= MAX_PIXEL_BYTES)
                    .ok_or_else(|| "Cursor and label assets exceed 256 MiB.".to_owned())?;
                self.assets.push((descriptor, surface.into_pixels()));
            }
            return Ok(id);
        }
        Err("Too many cursor raster shape aliases; the project was left unchanged.".to_owned())
    }

    fn raster(&mut self, text: &str) -> Result<TextRaster, String> {
        if let Some(raster) = self.cache.get(text) {
            return Ok(raster.clone());
        }
        let needed = self
            .request
            .size
            .area()
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| "Annotation image size overflow.".to_owned())?;
        self.bytes = self
            .bytes
            .checked_add(needed)
            .filter(|n| *n <= MAX_PIXEL_BYTES)
            .ok_or_else(|| {
                "Annotation labels exceed 256 MiB. Use a smaller box or select fewer frames."
                    .to_owned()
            })?;
        let image = self
            .rasterizer
            .get_or_insert_with(TextRasterizer::with_system_fonts)
            .rasterize(&TextRequest {
                text: text.to_owned(),
                font_family: self.request.font_family.clone(),
                font_size_px: self.request.font_size_px,
                size: self.request.size,
                foreground: self.request.foreground,
                background: if matches!(&self.request.mode, AnnotationMode::Progress(options) if options.show_bar) {
                    None
                } else {
                    Some(self.request.background)
                },
                alignment: HorizontalAlignment::Center,
            })
            .map_err(|error| error.to_string())?;
        let id = AssetStore::id_for_bytes(&image.rgba);
        let descriptor = if let Some(existing) = self.manifest.assets.get(&id) {
            if existing.kind.raster_descriptor() != Some((image.size, RasterEncoding::Rgba8))
                || existing.byte_len != needed as u64
            {
                return Err(
                    "Annotation pixels conflict with an existing asset descriptor.".to_owned(),
                );
            }
            existing.clone()
        } else {
            AssetDescriptor {
                id,
                byte_len: needed as u64,
                kind: AssetKind::OverlayImage {
                    size: image.size,
                    encoding: RasterEncoding::Rgba8,
                },
            }
        };
        let raster = TextRaster {
            asset_id: id,
            size: image.size,
        };
        self.cache.insert(text.to_owned(), raster.clone());
        if !self.assets.iter().any(|(asset, _)| asset.id == id) {
            self.assets.push((descriptor, image.rgba));
        }
        Ok(raster)
    }
}

#[cfg(test)]
pub(crate) fn prepare_annotations(
    manifest: &ProjectManifest,
    selected: &BTreeSet<FrameId>,
    request: &AnnotationRequest,
    cancellation: &AtomicBool,
    progress: impl FnMut(AnnotationProgress),
) -> Result<PreparedAnnotations, String> {
    prepare_annotations_with_assets(manifest, selected, request, cancellation, progress, &|_| {
        Err("A recorded cursor asset needs a project asset provider.".to_owned())
    })
}

pub(crate) fn prepare_annotations_with_assets(
    manifest: &ProjectManifest,
    selected: &BTreeSet<FrameId>,
    request: &AnnotationRequest,
    cancellation: &AtomicBool,
    progress: impl FnMut(AnnotationProgress),
    provider: &dyn Fn(gif_from_screen_domain::AssetId) -> Result<RgbaSurface, String>,
) -> Result<PreparedAnnotations, String> {
    check_cancelled(cancellation)?;
    manifest.validate().map_err(|error| error.to_string())?;
    validate_selection(manifest, selected)?;
    let plan = ScopePlan::from_selection(manifest, selected)?;
    prepare_annotation_plan(manifest, &plan, request, cancellation, progress, provider)
}

pub(crate) fn prepare_annotations_in_scope(
    manifest: &ProjectManifest,
    scope: &[TimelineSpan],
    request: &AnnotationRequest,
    cancellation: &AtomicBool,
    progress: impl FnMut(AnnotationProgress),
    provider: &dyn Fn(gif_from_screen_domain::AssetId) -> Result<RgbaSurface, String>,
) -> Result<PreparedAnnotations, String> {
    check_cancelled(cancellation)?;
    manifest.validate().map_err(|error| error.to_string())?;
    let plan = ScopePlan::new(manifest, scope)?;
    if plan.selected.len() > MAX_FRAMES {
        return Err("Annotation scope intersects more than 10,000 frames.".to_owned());
    }
    if manifest.timeline.frames.iter().any(|frame| {
        plan.selected.contains(&frame.id)
            && gif_from_screen_domain::recorded_annotation_barrier(frame, &request.mode)
    }) {
        return Err("This group's authoring scope includes frames without verified original input coordinates. The whole group is unchanged: confirm eligible legacy coordinates, undo the composite, or use manual annotations.".to_owned());
    }
    prepare_annotation_plan(manifest, &plan, request, cancellation, progress, provider)
}

fn prepare_annotation_plan(
    manifest: &ProjectManifest,
    plan: &ScopePlan,
    request: &AnnotationRequest,
    cancellation: &AtomicBool,
    mut progress: impl FnMut(AnnotationProgress),
    provider: &dyn Fn(gif_from_screen_domain::AssetId) -> Result<RgbaSurface, String>,
) -> Result<PreparedAnnotations, String> {
    request.validate_settings()?;
    let (blocked, replay_skips) = replay_filter(manifest, &plan.selected, &request.mode);
    let replayable = plan.selected.difference(&blocked).copied().collect();
    if plan.selected.is_empty()
        || no_recorded_candidates(manifest, &replayable, &request.mode, cancellation)?
    {
        progress(AnnotationProgress {
            completed: plan.selected.len(),
            total: plan.selected.len(),
        });
        return Ok(PreparedAnnotations {
            commands: Vec::new(),
            assets: Vec::new(),
            frames: 0,
            replay_skips,
        });
    }
    request.validate(manifest.canvas.size)?;
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
    let (items, affected) = render_scope_samples(&mut labels, plan, &blocked, &mut progress)?;
    check_cancelled(cancellation)?;
    let excluded: Vec<_> = plan
        .samples
        .iter()
        .filter(|sample| blocked.contains(&manifest.timeline.frames[sample.index].id))
        .map(|sample| sample.span)
        .collect();
    let scope = gif_from_screen_domain::subtract_annotation_scope(&plan.scope, &excluded)?;
    finish_annotations(labels, items, affected, scope, replay_skips)
}

fn replay_filter(
    manifest: &ProjectManifest,
    selected: &BTreeSet<FrameId>,
    mode: &AnnotationMode,
) -> (BTreeSet<FrameId>, AnnotationReplaySkips) {
    use gif_from_screen_domain::{
        CaptureReplayBlock, recorded_annotation_barrier, recorded_annotation_block,
    };
    let mut blocked = BTreeSet::new();
    let mut skips = AnnotationReplaySkips::default();
    if !matches!(
        mode,
        AnnotationMode::RecordedKeys
            | AnnotationMode::RecordedClicks
            | AnnotationMode::RecordedCursor
    ) {
        return (blocked, skips);
    }
    for frame in manifest
        .timeline
        .frames
        .iter()
        .filter(|frame| selected.contains(&frame.id))
    {
        if recorded_annotation_barrier(frame, mode) {
            blocked.insert(frame.id);
        }
        match recorded_annotation_block(frame, mode) {
            Some(CaptureReplayBlock::LegacyUnknown) => skips.legacy_unknown += 1,
            Some(CaptureReplayBlock::ArchivedAfterComposite) => skips.archived_after_composite += 1,
            Some(CaptureReplayBlock::NotRecorded) => skips.not_recorded += 1,
            None => {}
        }
    }
    (blocked, skips)
}

fn render_scope_samples(
    labels: &mut Labels<'_>,
    plan: &ScopePlan,
    blocked: &BTreeSet<FrameId>,
    progress: &mut impl FnMut(AnnotationProgress),
) -> Result<(Vec<OverlayItem>, usize), String> {
    let manifest = labels.manifest;
    let request = labels.request;
    let total_us = manifest
        .timeline
        .total_duration()
        .ok_or("Timeline duration overflow.")?
        .get();
    let mut items = Vec::new();
    let mut completed = BTreeSet::new();
    let mut affected = BTreeSet::new();
    let mut recent_keys = KeyLabelHistory::default();
    let mut recent_clicks: Vec<(u64, MouseButton, PhysicalPoint, Option<CaptureOrigin>)> =
        Vec::new();
    let mut previous_clock = None;
    let mut previous_run = None;
    for planned in &plan.samples {
        check_cancelled(labels.cancellation)?;
        let frame = &manifest.timeline.frames[planned.index];
        let clock = frame
            .capture_metadata
            .captured_at
            .map_or(planned.frame_start, TimeUs::get);
        // Never carry an event across a selection gap or a backwards/repeated capture clock.
        if blocked.contains(&frame.id)
            || previous_run != Some(planned.run)
            || previous_clock.is_some_and(|previous| clock <= previous)
        {
            recent_keys.clear();
            recent_clicks.clear();
        }
        previous_clock = Some(clock);
        previous_run = Some(planned.run);
        completed.insert(frame.id);
        progress(AnnotationProgress {
            completed: completed.len(),
            total: plan.selected.len(),
        });
        if blocked.contains(&frame.id) {
            previous_run = None;
            continue;
        }
        let sample = AnnotationFrame {
            frame,
            index: planned.index,
            end: planned.frame_end,
            total_us,
            clock,
        };
        let contents = prepare_frame(labels, &sample, &mut recent_keys, &mut recent_clicks)?;
        if items.len().saturating_add(contents.len()) > MAX_ITEMS {
            return Err("Annotations exceed 40,000 items; select fewer frames or shorten the event hold time.".to_owned());
        }
        if !contents.is_empty() {
            affected.insert(frame.id);
        }
        for content in contents {
            items.push(OverlayItem {
                id: OverlayId::from_u128(Uuid::new_v4().as_u128()),
                span: planned.span,
                z_index: request.z_index,
                content,
            });
        }
    }
    Ok((items, affected.len()))
}

fn validate_selection(
    manifest: &ProjectManifest,
    selected: &BTreeSet<FrameId>,
) -> Result<(), String> {
    if selected.is_empty() || selected.len() > MAX_FRAMES {
        return Err("Select between 1 and 10,000 frames for annotations.".to_owned());
    }
    let existing: BTreeSet<_> = manifest
        .timeline
        .frames
        .iter()
        .map(|frame| frame.id)
        .collect();
    if !selected.is_subset(&existing) {
        return Err("The annotation selection contains a missing frame.".to_owned());
    }
    Ok(())
}

/// Empty input tasks are no-ops even when their unused text box does not fit this project.
fn no_recorded_candidates(
    manifest: &ProjectManifest,
    selected: &BTreeSet<FrameId>,
    mode: &AnnotationMode,
    cancellation: &AtomicBool,
) -> Result<bool, String> {
    if !matches!(
        mode,
        AnnotationMode::RecordedKeys
            | AnnotationMode::RecordedClicks
            | AnnotationMode::RecordedCursor
    ) {
        return Ok(false);
    }
    for frame in manifest
        .timeline
        .frames
        .iter()
        .filter(|frame| selected.contains(&frame.id))
    {
        check_cancelled(cancellation)?;
        let metadata = &frame.capture_metadata;
        let exists = match mode {
            AnnotationMode::RecordedKeys => {
                if metadata.key_strokes.len() > MAX_EVENTS_PER_FRAME {
                    return Err("A frame has more than 512 key events.".to_owned());
                }
                metadata
                    .key_strokes
                    .iter()
                    .any(|key| key.pressed && !key.repeat)
            }
            AnnotationMode::RecordedClicks => {
                if metadata.mouse_events.len() > MAX_EVENTS_PER_FRAME {
                    return Err("A frame has more than 512 mouse events.".to_owned());
                }
                metadata
                    .mouse_events
                    .iter()
                    .any(|event| event.pressed && event.position.is_some())
            }
            AnnotationMode::RecordedCursor => {
                metadata.cursor_visible
                    && !metadata.cursor_embedded
                    && metadata.cursor_position.is_some()
            }
            _ => false,
        };
        if exists {
            return Ok(false);
        }
    }
    Ok(true)
}

fn finish_annotations(
    labels: Labels<'_>,
    items: Vec<OverlayItem>,
    affected: usize,
    scope: Vec<TimelineSpan>,
    replay_skips: AnnotationReplaySkips,
) -> Result<PreparedAnnotations, String> {
    let manifest = labels.manifest;
    let request = labels.request;
    if items.is_empty() {
        return Ok(PreparedAnnotations {
            commands: Vec::new(),
            assets: Vec::new(),
            frames: 0,
            replay_skips,
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
    commands.push(EditCommand::UpsertOverlayTrack {
        track: OverlayTrack {
            annotation: Some(request.clone()),
            annotation_scope: Some(scope),
            id: TrackId::from_u128(Uuid::new_v4().as_u128()),
            name: annotation_name(&request.mode).to_owned(),
            visible: true,
            opacity: request.opacity,
            blend_mode: BlendMode::Normal,
            items,
        },
    });
    // Count JSON bytes without allocating an unbounded journal buffer.
    serde_json::to_writer(&mut CommandBudget(64 * 1024 * 1024), &commands).map_err(|_| {
        "Annotation commands exceed 64 MiB; select fewer frames or use shorter labels.".to_owned()
    })?;
    Ok(PreparedAnnotations {
        commands,
        assets: labels.assets,
        frames: affected,
        replay_skips,
    })
}

struct CommandBudget(usize);
impl std::io::Write for CommandBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("annotation command budget exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct AnnotationFrame<'a> {
    frame: &'a FrameClip,
    index: usize,
    end: u64,
    total_us: u64,
    clock: u64,
}

fn prepare_frame(
    labels: &mut Labels<'_>,
    sample: &AnnotationFrame<'_>,
    recent_keys: &mut KeyLabelHistory,
    recent_clicks: &mut Vec<(u64, MouseButton, PhysicalPoint, Option<CaptureOrigin>)>,
) -> Result<Vec<OverlayContent>, String> {
    let request = labels.request;
    let frame = sample.frame;
    let mut contents = Vec::new();
    match &request.mode {
        AnnotationMode::Progress(options) => {
            prepare_progress(labels, sample, options, &mut contents)?;
        }
        AnnotationMode::ManualKeys { text } => contents.push(OverlayContent::KeyStroke {
            text: text.clone(),
            position: request.position,
            raster: Some(labels.raster(text)?),
        }),
        AnnotationMode::RecordedKeys => {
            prepare_keys(labels, sample, recent_keys, &mut contents)?;
        }
        AnnotationMode::ManualClick { button } => {
            contents.push(click(request.position, *button, request));
        }
        AnnotationMode::RecordedClicks => {
            prepare_clicks(labels, sample, recent_clicks, &mut contents)?;
        }
        AnnotationMode::RecordedCursor => {
            if let Some(content) = labels.cursor_overlay(frame)? {
                contents.push(content);
            }
        }
        AnnotationMode::BuiltinCursor => contents.push(OverlayContent::Cursor {
            cursor_asset: None,
            position: request.position,
            hotspot: PhysicalPoint::default(),
        }),
    }
    Ok(contents)
}

fn prepare_progress(
    labels: &mut Labels<'_>,
    sample: &AnnotationFrame<'_>,
    options: &ProgressOptions,
    contents: &mut Vec<OverlayContent>,
) -> Result<(), String> {
    let manifest = labels.manifest;
    let request = labels.request;
    let index = sample.index;
    let end = sample.end;
    let total_us = sample.total_us;
    let current = match options.measure {
        ProgressMeasure::Frames => (index + 1) as u64,
        ProgressMeasure::ElapsedTime => end,
    };
    let total = match options.measure {
        ProgressMeasure::Frames => manifest.timeline.frames.len() as u64,
        ProgressMeasure::ElapsedTime => total_us,
    };
    let value = if options.remaining {
        total - current
    } else {
        current
    };
    let amount = u32::try_from(u128::from(value) * 1_000_000 / u128::from(total))
        .expect("bounded progress fraction");
    let fraction =
        ProgressFraction::new(value, total).expect("bounded progress fraction with nonzero total");
    let text = progress_label(
        &options.format,
        index + 1,
        manifest.timeline.frames.len(),
        end,
        total_us,
        fraction.scaled_rounded(1000),
    );
    let label = if text.trim().is_empty() {
        None
    } else {
        Some(labels.raster(&text)?)
    };
    contents.push(OverlayContent::Progress {
        bounds: PhysicalRect {
            origin: request.position,
            size: request.size,
        },
        foreground: if options.show_bar {
            options.bar_color
        } else {
            Rgba::TRANSPARENT
        },
        background: if options.show_bar {
            request.background
        } else {
            Rgba::TRANSPARENT
        },
        show_frame_number: options.format.contains("{frame}")
            || options.format.contains("{frames}"),
        style: Some(ProgressStyle {
            amount_millionths: amount,
            fraction: Some(fraction),
            direction: options.direction,
            label,
            label_position: request.position,
            label_text: text,
        }),
    });
    Ok(())
}

fn prepare_keys(
    labels: &mut Labels<'_>,
    sample: &AnnotationFrame<'_>,
    recent_keys: &mut KeyLabelHistory,
    contents: &mut Vec<OverlayContent>,
) -> Result<(), String> {
    let frame = sample.frame;
    let clock = sample.clock;
    let request = labels.request;
    let text = recent_keys.update(&frame.capture_metadata.key_strokes, clock, request.hold_ms)?;
    if !text.is_empty() {
        contents.push(OverlayContent::KeyStroke {
            text: text.clone(),
            position: request.position,
            raster: Some(labels.raster(&text)?),
        });
    }
    Ok(())
}

fn prepare_clicks(
    labels: &Labels<'_>,
    sample: &AnnotationFrame<'_>,
    recent_clicks: &mut Vec<(u64, MouseButton, PhysicalPoint, Option<CaptureOrigin>)>,
    contents: &mut Vec<OverlayContent>,
) -> Result<(), String> {
    let frame = sample.frame;
    let clock = sample.clock;
    let request = labels.request;
    let manifest = labels.manifest;
    if frame.capture_metadata.mouse_events.len() > MAX_EVENTS_PER_FRAME {
        return Err("A frame has more than 512 mouse events.".to_owned());
    }
    let oldest = clock.saturating_sub(u64::from(request.hold_ms) * 1000);
    recent_clicks.retain(|(at, _, _, _)| *at >= oldest && *at <= clock);
    for event in &frame.capture_metadata.mouse_events {
        if event.pressed
            && event.at.get() <= clock
            && let Some(position) = event.position
        {
            recent_clicks.push((
                clock,
                event.button,
                position,
                frame.capture_metadata.capture_origin,
            ));
        }
    }
    if recent_clicks.len() > MAX_ACTIVE_EVENTS {
        return Err("Too many simultaneous click markers; shorten the hold time.".to_owned());
    }
    for (_, button, position, original_origin) in recent_clicks.iter() {
        if let Some(position) = rebase_position(
            *position,
            *original_origin,
            frame.capture_metadata.capture_origin,
        )
        .and_then(|point| transform_point(manifest, frame, point))
        {
            contents.push(click(position, *button, request));
        }
    }
    Ok(())
}

pub(crate) fn annotation_name(mode: &AnnotationMode) -> &'static str {
    match mode {
        AnnotationMode::Progress(_) => "Progress and time",
        AnnotationMode::ManualKeys { .. } => "Manual key label",
        AnnotationMode::RecordedKeys => "Recorded keys",
        AnnotationMode::ManualClick { .. } => "Manual click marker",
        AnnotationMode::RecordedClicks => "Recorded click markers",
        AnnotationMode::RecordedCursor => "Recorded cursor",
        AnnotationMode::BuiltinCursor => "Built-in cursor",
    }
}

fn click(
    position: PhysicalPoint,
    button: MouseButton,
    request: &AnnotationRequest,
) -> OverlayContent {
    OverlayContent::MouseClick {
        position,
        button,
        color: request.foreground,
        radius: request.click_radius,
    }
}

fn rebase_position(
    position: PhysicalPoint,
    original: Option<CaptureOrigin>,
    current: Option<CaptureOrigin>,
) -> Option<PhysicalPoint> {
    match (original, current) {
        (Some(original), Some(current)) => Some(PhysicalPoint {
            x: PhysicalPx::new(
                u32::try_from(
                    i64::from(position.x.get()) + i64::from(original.x) - i64::from(current.x),
                )
                .ok()?,
            ),
            y: PhysicalPx::new(
                u32::try_from(
                    i64::from(position.y.get()) + i64::from(original.y) - i64::from(current.y),
                )
                .ok()?,
            ),
        }),
        (None, None) => Some(position),
        _ => None,
    }
}

fn progress_label(
    format: &str,
    frame: usize,
    frames: usize,
    elapsed: u64,
    total: u64,
    percent_tenths: u32,
) -> String {
    format
        .replace("{frame}", &frame.to_string())
        .replace("{frames}", &frames.to_string())
        .replace("{elapsed}", &format_time(elapsed))
        .replace("{total}", &format_time(total))
        .replace("{remaining}", &format_time(total.saturating_sub(elapsed)))
        .replace(
            "{percent}",
            &if percent_tenths.is_multiple_of(10) {
                format!("{}%", percent_tenths / 10)
            } else {
                format!("{}.{:01}%", percent_tenths / 10, percent_tenths % 10)
            },
        )
}

fn format_time(us: u64) -> String {
    let ms = us / 1000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

/// Match crop → nearest resize → rotation → flips; pointer icon size stays native.
fn transform_point(
    manifest: &ProjectManifest,
    frame: &FrameClip,
    point: PhysicalPoint,
) -> Option<PhysicalPoint> {
    let source = manifest.assets.get(&frame.asset_id)?.kind.raster_size()?;
    let crop = frame.transform.crop.unwrap_or(PhysicalRect {
        origin: PhysicalPoint::default(),
        size: source,
    });
    let mut x = point.x.get().checked_sub(crop.origin.x.get())?;
    let mut y = point.y.get().checked_sub(crop.origin.y.get())?;
    if x >= crop.size.width.get() || y >= crop.size.height.get() {
        return None;
    }
    let size = frame.transform.output_size.unwrap_or(crop.size);
    x = u32::try_from(
        u64::from(x) * u64::from(size.width.get()) / u64::from(crop.size.width.get()),
    )
    .ok()?;
    y = u32::try_from(
        u64::from(y) * u64::from(size.height.get()) / u64::from(crop.size.height.get()),
    )
    .ok()?;
    let (mut x, mut y, w, h) = match frame.transform.rotation {
        QuarterTurn::Zero => (x, y, size.width.get(), size.height.get()),
        QuarterTurn::Clockwise90 => (
            size.height.get() - 1 - y,
            x,
            size.height.get(),
            size.width.get(),
        ),
        QuarterTurn::Clockwise180 => (
            size.width.get() - 1 - x,
            size.height.get() - 1 - y,
            size.width.get(),
            size.height.get(),
        ),
        QuarterTurn::Clockwise270 => (
            y,
            size.width.get() - 1 - x,
            size.height.get(),
            size.width.get(),
        ),
    };
    if frame.transform.flip_horizontal {
        x = w - 1 - x;
    }
    if frame.transform.flip_vertical {
        y = h - 1 - y;
    }
    Some(PhysicalPoint {
        x: PhysicalPx::new(x),
        y: PhysicalPx::new(y),
    })
}

pub(crate) fn check_cancelled(cancellation: &AtomicBool) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("Annotation preparation cancelled.".to_owned())
    } else {
        Ok(())
    }
}

/// Bounded, digest-verified source for an annotation preparation worker.
pub(crate) fn load_annotation_asset(
    manifest: &ProjectManifest,
    store: &AssetStore,
    id: gif_from_screen_domain::AssetId,
) -> Result<RgbaSurface, String> {
    use std::io::Read;
    let descriptor = manifest
        .assets
        .get(&id)
        .ok_or_else(|| "Annotation source asset is missing.".to_owned())?;
    let Some((size, RasterEncoding::Rgba8)) = descriptor.kind.raster_descriptor() else {
        return Err("Annotation source must be RGBA8.".to_owned());
    };
    let expected = size
        .area()
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= 64 * 1024 * 1024)
        .ok_or_else(|| "Annotation source exceeds 64 MiB.".to_owned())?;
    if expected != descriptor.byte_len {
        return Err("Annotation source descriptor length is inconsistent.".to_owned());
    }
    let path = store.asset_path(id);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() != expected {
        return Err("Annotation source must be a regular file of the declared length.".to_owned());
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(expected + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != expected || AssetStore::id_for_bytes(&bytes) != id {
        return Err("Annotation source asset length or digest does not match.".to_owned());
    }
    RgbaSurface::new(size, bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::{
        AssetId, Canvas, CanvasBackground, CaptureMetadata, ClipTransform, ColorSpace, DurationUs,
        KeyStroke, MouseInputEvent, PhysicalSize, ProjectId, UnixTimeMs,
    };

    pub(super) fn manifest() -> ProjectManifest {
        let size = PhysicalSize::new(240, 40).unwrap();
        let mut manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "annotations",
            UnixTimeMs::new(0),
            Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        let id = AssetId::from_digest([1; 32]);
        manifest.assets.insert(
            id,
            AssetDescriptor {
                id,
                byte_len: 240 * 40 * 4,
                kind: AssetKind::Frame {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        for index in 0_u64..3 {
            manifest.timeline.frames.push(FrameClip {
                capture_binding: gif_from_screen_domain::CaptureBinding::Original,
                id: FrameId::from_u128(u128::from(index) + 1),
                asset_id: id,
                duration: DurationUs::new((index + 1) * 100_000).unwrap(),
                transform: ClipTransform::default(),
                effects: Vec::new(),
                capture_metadata: CaptureMetadata {
                    captured_at: Some(TimeUs::new(index * 100_000)),
                    ..CaptureMetadata::default()
                },
            });
        }
        manifest
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "test cases own their independent request fixtures"
    )]
    fn prepare(
        manifest: &ProjectManifest,
        selected: &[u128],
        request: AnnotationRequest,
    ) -> PreparedAnnotations {
        prepare_annotations(
            manifest,
            &selected.iter().copied().map(FrameId::from_u128).collect(),
            &request,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap()
    }

    pub(super) fn track(prepared: &PreparedAnnotations) -> &OverlayTrack {
        let EditCommand::UpsertOverlayTrack { track } = prepared.commands.last().unwrap() else {
            panic!("last command must be a track")
        };
        track
    }

    #[test]
    fn progress_preserves_selection_gaps_and_uses_original_indices() {
        let manifest = manifest();
        let prepared = prepare(
            &manifest,
            &[1, 3],
            AnnotationRequest {
                mode: AnnotationMode::Progress(ProgressOptions {
                    format: String::new(),
                    ..ProgressOptions::default()
                }),
                ..AnnotationRequest::default()
            },
        );
        assert_eq!(track(&prepared).items.len(), 2);
        assert_eq!(track(&prepared).items[1].span.start.get(), 300_000);
        let OverlayContent::Progress {
            style: Some(first), ..
        } = &track(&prepared).items[0].content
        else {
            panic!()
        };
        let OverlayContent::Progress {
            style: Some(last), ..
        } = &track(&prepared).items[1].content
        else {
            panic!()
        };
        assert_eq!(first.amount_millionths, 333_333);
        assert_eq!(last.amount_millionths, 1_000_000);
        assert!(prepared.assets.is_empty());
    }

    #[test]
    fn elapsed_progress_weights_variable_durations_and_countdown() {
        let prepared = prepare(
            &manifest(),
            &[1, 2, 3],
            AnnotationRequest {
                mode: AnnotationMode::Progress(ProgressOptions {
                    format: String::new(),
                    measure: ProgressMeasure::ElapsedTime,
                    remaining: true,
                    ..ProgressOptions::default()
                }),
                ..AnnotationRequest::default()
            },
        );
        let amounts: Vec<_> = track(&prepared)
            .items
            .iter()
            .map(|item| {
                if let OverlayContent::Progress {
                    style: Some(style), ..
                } = &item.content
                {
                    style.amount_millionths
                } else {
                    panic!()
                }
            })
            .collect();
        assert_eq!(amounts, [833_333, 500_000, 0]);
    }

    #[test]
    fn progress_label_tokens_are_unambiguous() {
        assert_eq!(
            progress_label(
                "{frame}/{frames} {elapsed} {total} {remaining} {percent}",
                2,
                4,
                1_234_000,
                3_000_000,
                500
            ),
            "2/4 00:00:01.234 00:00:03.000 00:00:01.766 50%"
        );
        let invalid = AnnotationRequest {
            mode: AnnotationMode::Progress(ProgressOptions {
                format: "{fame}".to_owned(),
                ..ProgressOptions::default()
            }),
            ..AnnotationRequest::default()
        };
        assert!(invalid.validate_settings().is_err());
    }

    #[test]
    fn text_only_progress_composites_its_semitransparent_background_once() {
        use gif_from_screen_render::{CpuRenderer, NeverCancel};
        let manifest = manifest();
        let background = Rgba {
            red: 10,
            green: 30,
            blue: 50,
            alpha: 128,
        };
        let prepared = prepare(
            &manifest,
            &[1],
            AnnotationRequest {
                background,
                mode: AnnotationMode::Progress(ProgressOptions {
                    show_bar: false,
                    format: "{percent}".to_owned(),
                    ..ProgressOptions::default()
                }),
                ..AnnotationRequest::default()
            },
        );
        let track = track(&prepared);
        assert!(
            matches!(&track.items[0].content,OverlayContent::Progress{foreground,background,style:Some(style),..} if *foreground==Rgba::TRANSPARENT&&*background==Rgba::TRANSPARENT&&style.label_text=="33.3%")
        );
        let source = RgbaSurface::new(manifest.canvas.size, vec![0; 240 * 40 * 4]).unwrap();
        let label = RgbaSurface::new(
            prepared.assets[0].0.kind.raster_size().unwrap(),
            prepared.assets[0].1.clone(),
        )
        .unwrap();
        assert_eq!(&label.pixels()[..4], &[10, 30, 50, 128]);
        let frame = &manifest.timeline.frames[0];
        let output = CpuRenderer::new()
            .render_clip_with_overlays(
                frame,
                std::slice::from_ref(track),
                TimeUs::ZERO,
                &|id| {
                    Ok(if id == frame.asset_id {
                        source.clone()
                    } else {
                        label.clone()
                    })
                },
                &NeverCancel,
            )
            .unwrap();
        assert_eq!(&output.pixels()[..4], &[10, 30, 50, 128]);
    }

    #[test]
    fn new_percent_labels_round_the_exact_ratio_without_editing_the_format() {
        let manifest = manifest();
        let request = AnnotationRequest {
            mode: AnnotationMode::Progress(ProgressOptions {
                format: "{frame}/{frames} {percent}".to_owned(),
                ..ProgressOptions::default()
            }),
            ..AnnotationRequest::default()
        };
        let prepared = prepare(&manifest, &[1, 2, 3], request.clone());
        let labels: Vec<_> = track(&prepared)
            .items
            .iter()
            .map(|item| match &item.content {
                OverlayContent::Progress {
                    style: Some(style), ..
                } => style.label_text.as_str(),
                _ => panic!("progress expected"),
            })
            .collect();
        assert_eq!(labels, ["1/3 33.3%", "2/3 66.7%", "3/3 100%"]);
        assert_eq!(track(&prepared).annotation.as_ref().unwrap(), &request);
        let with_bar = prepare(&manifest, &[1], request);
        assert_eq!(
            with_bar.assets[0].1[3], 0,
            "bar background is not baked into the text twice"
        );
    }

    #[test]
    fn repeated_manual_labels_share_one_persistent_asset() {
        let manifest = manifest();
        let prepared = prepare(
            &manifest,
            &[1, 2, 3],
            AnnotationRequest {
                mode: AnnotationMode::ManualKeys {
                    text: "Ctrl+C".to_owned(),
                },
                ..AnnotationRequest::default()
            },
        );
        assert_eq!(prepared.assets.len(), 1);
        assert_eq!(track(&prepared).items.len(), 3);
        let mut clone = manifest.clone();
        clone
            .apply_command(&EditCommand::Compound {
                commands: prepared.commands,
            })
            .unwrap();
        clone.validate().unwrap();
    }

    #[test]
    fn recorded_keys_filter_releases_repeats_and_expire_at_capture_clock() {
        let mut manifest = manifest();
        manifest.timeline.frames[0].capture_metadata.key_strokes = vec![
            KeyStroke {
                physical_key: "C".to_owned(),
                display_text: Some("C".to_owned()),
                pressed: true,
                at: TimeUs::ZERO,
                repeat: false,
                modifiers: 2,
            },
            KeyStroke {
                physical_key: "D".to_owned(),
                display_text: None,
                pressed: false,
                at: TimeUs::ZERO,
                repeat: false,
                modifiers: 0,
            },
        ];
        let prepared = prepare(
            &manifest,
            &[1, 2, 3],
            AnnotationRequest {
                mode: AnnotationMode::RecordedKeys,
                hold_ms: 150,
                ..AnnotationRequest::default()
            },
        );
        assert_eq!(track(&prepared).items.len(), 2);
        assert!(
            matches!(&track(&prepared).items[0].content,OverlayContent::KeyStroke {text,..} if text=="Ctrl+C")
        );
        assert_eq!(prepared.assets.len(), 1);
        let gap = prepare(
            &manifest,
            &[1, 3],
            AnnotationRequest {
                mode: AnnotationMode::RecordedKeys,
                hold_ms: 1000,
                ..AnnotationRequest::default()
            },
        );
        assert_eq!(track(&gap).items.len(), 1);
    }

    #[test]
    fn cursor_metadata_skips_embedded_and_hidden_cursors() {
        let mut manifest = manifest();
        let source =
            RgbaSurface::new(PhysicalSize::new(1, 1).unwrap(), vec![255, 0, 0, 255]).unwrap();
        let id = AssetStore::id_for_bytes(source.pixels());
        manifest.assets.insert(
            id,
            AssetDescriptor {
                id,
                byte_len: 4,
                kind: AssetKind::OverlayImage {
                    size: source.size(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        for frame in &mut manifest.timeline.frames {
            frame.capture_metadata.cursor_position = Some(PhysicalPoint::default());
            frame.capture_metadata.cursor_visible = true;
            frame.capture_metadata.cursor_asset = Some(id);
        }
        manifest.timeline.frames[1].capture_metadata.cursor_embedded = true;
        manifest.timeline.frames[2].capture_metadata.cursor_visible = false;
        let prepared = prepare_annotations_with_assets(
            &manifest,
            &manifest
                .timeline
                .frames
                .iter()
                .map(|frame| frame.id)
                .collect(),
            &AnnotationRequest {
                mode: AnnotationMode::RecordedCursor,
                ..AnnotationRequest::default()
            },
            &AtomicBool::new(false),
            |_| {},
            &|_| Ok(source.clone()),
        )
        .unwrap();
        assert_eq!(track(&prepared).items.len(), 1);
    }

    #[test]
    fn padded_cursor_matches_edge_clipped_embedding_with_opacity_and_every_blend() {
        use gif_from_screen_render::{CpuRenderer, NeverCancel};
        let mut manifest = manifest();
        let cursor = RgbaSurface::new(PhysicalSize::new(2, 1).unwrap(), vec![255; 8]).unwrap();
        let cursor_id = AssetStore::id_for_bytes(cursor.pixels());
        manifest.assets.insert(
            cursor_id,
            AssetDescriptor {
                id: cursor_id,
                byte_len: 8,
                kind: AssetKind::OverlayImage {
                    size: cursor.size(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        let base_id = manifest.timeline.frames[0].asset_id;
        let base =
            RgbaSurface::new(manifest.canvas.size, [20, 40, 60, 255].repeat(240 * 40)).unwrap();
        manifest.canvas.size = PhysicalSize::new(40, 240).unwrap();
        for frame in &mut manifest.timeline.frames {
            frame.transform.rotation = QuarterTurn::Clockwise90;
            frame.capture_metadata.cursor_asset = Some(cursor_id);
            frame.capture_metadata.cursor_visible = true;
            frame.capture_metadata.cursor_position = Some(PhysicalPoint {
                x: PhysicalPx::new(238),
                y: PhysicalPx::ZERO,
            });
        }
        let prepared = prepare_annotations_with_assets(
            &manifest,
            &[FrameId::from_u128(1)].into(),
            &AnnotationRequest {
                mode: AnnotationMode::RecordedCursor,
                ..AnnotationRequest::default()
            },
            &AtomicBool::new(false),
            |_| {},
            &|_| Ok(cursor.clone()),
        )
        .unwrap();
        let patch = RgbaSurface::new(
            prepared.assets[0].0.kind.raster_size().unwrap(),
            prepared.assets[0].1.clone(),
        )
        .unwrap();
        let patch_id = prepared.assets[0].0.id;
        let frame = &manifest.timeline.frames[0];
        for blend in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
            let mut actual_track = track(&prepared).clone();
            actual_track.opacity = 137;
            actual_track.blend_mode = blend;
            let mut before_track = actual_track.clone();
            before_track.items[0].content = OverlayContent::Cursor {
                cursor_asset: Some(cursor_id),
                position: frame.capture_metadata.cursor_position.unwrap(),
                hotspot: PhysicalPoint::default(),
            };
            let before_clip = FrameClip {
                transform: ClipTransform::default(),
                ..frame.clone()
            };
            let embedded = CpuRenderer::new()
                .render_clip_with_overlays(
                    &before_clip,
                    &[before_track],
                    TimeUs::ZERO,
                    &|id| {
                        Ok(if id == base_id {
                            base.clone()
                        } else {
                            cursor.clone()
                        })
                    },
                    &NeverCancel,
                )
                .unwrap();
            let expected = CpuRenderer::new()
                .render_clip(frame, &|_| Ok(embedded.clone()), &NeverCancel)
                .unwrap();
            let actual = CpuRenderer::new()
                .render_clip_with_overlays(
                    frame,
                    &[actual_track],
                    TimeUs::ZERO,
                    &|id| {
                        Ok(if id == patch_id {
                            patch.clone()
                        } else {
                            base.clone()
                        })
                    },
                    &NeverCancel,
                )
                .unwrap();
            assert_eq!(actual.pixels(), expected.pixels(), "{blend:?}");
        }
    }

    #[test]
    fn rotated_solid_cursor_uses_transparent_padding_for_raw_byte_shape_alias() {
        let mut manifest = manifest();
        let source = RgbaSurface::new(PhysicalSize::new(2, 1).unwrap(), vec![255; 8]).unwrap();
        let id = AssetStore::id_for_bytes(source.pixels());
        manifest.assets.insert(
            id,
            AssetDescriptor {
                id,
                byte_len: 8,
                kind: AssetKind::OverlayImage {
                    size: source.size(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        let frame = &mut manifest.timeline.frames[0];
        frame.capture_metadata.cursor_asset = Some(id);
        frame.capture_metadata.cursor_visible = true;
        frame.capture_metadata.cursor_position = Some(PhysicalPoint::default());
        frame.transform.rotation = QuarterTurn::Clockwise90;
        let prepared = prepare_annotations_with_assets(
            &manifest,
            &[FrameId::from_u128(1)].into(),
            &AnnotationRequest {
                mode: AnnotationMode::RecordedCursor,
                ..AnnotationRequest::default()
            },
            &AtomicBool::new(false),
            |_| {},
            &|_| Ok(source.clone()),
        )
        .unwrap();
        assert_eq!(prepared.assets.len(), 1);
        assert_ne!(prepared.assets[0].0.id, id);
        assert_eq!(
            prepared.assets[0].0.kind.raster_size(),
            Some(PhysicalSize::new(2, 2).unwrap())
        );
        assert_eq!(
            prepared.assets[0].1,
            [
                255, 255, 255, 255, 0, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 0
            ]
        );
    }

    #[test]
    fn recorded_cursor_transform_reads_once_and_caches_immutable_pixels() {
        let mut manifest = manifest();
        let source = RgbaSurface::new(
            PhysicalSize::new(2, 1).unwrap(),
            vec![255, 0, 0, 255, 0, 255, 0, 128],
        )
        .unwrap();
        let id = AssetStore::id_for_bytes(source.pixels());
        manifest.assets.insert(
            id,
            AssetDescriptor {
                id,
                byte_len: 8,
                kind: AssetKind::OverlayImage {
                    size: source.size(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        for frame in &mut manifest.timeline.frames {
            frame.capture_metadata.cursor_visible = true;
            frame.capture_metadata.cursor_asset = Some(id);
            frame.capture_metadata.cursor_position = Some(PhysicalPoint {
                x: PhysicalPx::new(10),
                y: PhysicalPx::new(5),
            });
            frame.transform.flip_horizontal = true;
        }
        let reads = std::cell::Cell::new(0);
        let prepared = prepare_annotations_with_assets(
            &manifest,
            &manifest
                .timeline
                .frames
                .iter()
                .map(|frame| frame.id)
                .collect(),
            &AnnotationRequest {
                mode: AnnotationMode::RecordedCursor,
                ..AnnotationRequest::default()
            },
            &AtomicBool::new(false),
            |_| {},
            &|asset| {
                assert_eq!(asset, id);
                reads.set(reads.get() + 1);
                Ok(source.clone())
            },
        )
        .unwrap();
        assert_eq!(reads.get(), 1);
        assert_eq!(prepared.assets.len(), 1);
        assert_eq!(prepared.assets[0].1, [0, 255, 0, 128, 255, 0, 0, 255]);
        assert!(
            matches!(&track(&prepared).items[0].content,OverlayContent::Cursor{position,hotspot,..} if position.x.get()==228 && *hotspot==PhysicalPoint::default())
        );
    }

    #[test]
    fn retained_click_positions_rebase_after_dynamic_recording_movement() {
        let mut manifest = manifest();
        manifest.timeline.frames[0].capture_metadata.capture_origin =
            Some(CaptureOrigin { x: -100, y: 0 });
        manifest.timeline.frames[1].capture_metadata.capture_origin =
            Some(CaptureOrigin { x: -110, y: 0 });
        manifest.timeline.frames[2].capture_metadata.capture_origin =
            Some(CaptureOrigin { x: 1000, y: 0 });
        manifest.timeline.frames[0]
            .capture_metadata
            .mouse_events
            .push(MouseInputEvent {
                at: TimeUs::ZERO,
                button: MouseButton::Left,
                pressed: true,
                position: Some(PhysicalPoint {
                    x: PhysicalPx::new(5),
                    y: PhysicalPx::new(5),
                }),
            });
        let prepared = prepare(
            &manifest,
            &[1, 2, 3],
            AnnotationRequest {
                mode: AnnotationMode::RecordedClicks,
                ..AnnotationRequest::default()
            },
        );
        assert_eq!(track(&prepared).items.len(), 2);
        assert!(
            matches!(&track(&prepared).items[1].content,OverlayContent::MouseClick{position,..} if position.x.get()==15)
        );
    }

    #[test]
    fn capture_positions_follow_crop_resize_rotation_and_flips() {
        let manifest = manifest();
        let mut frame = manifest.timeline.frames[0].clone();
        frame.transform = ClipTransform {
            crop: Some(PhysicalRect::new(10, 5, 20, 10).unwrap()),
            output_size: Some(PhysicalSize::new(40, 20).unwrap()),
            rotation: QuarterTurn::Clockwise90,
            flip_horizontal: true,
            flip_vertical: true,
        };
        assert_eq!(
            transform_point(
                &manifest,
                &frame,
                PhysicalPoint {
                    x: PhysicalPx::new(10),
                    y: PhysicalPx::new(5)
                }
            ),
            Some(PhysicalPoint {
                x: PhysicalPx::new(0),
                y: PhysicalPx::new(39)
            })
        );
        assert!(transform_point(&manifest, &frame, PhysicalPoint::default()).is_none());
    }

    #[test]
    fn sparse_samples_show_newly_delivered_events_then_expire_without_showing_future_events() {
        let mut manifest = manifest();
        for (frame, time) in manifest
            .timeline
            .frames
            .iter_mut()
            .zip([10_000_000, 10_100_000, 10_600_000])
        {
            frame.capture_metadata.captured_at = Some(TimeUs::new(time));
        }
        let first = &mut manifest.timeline.frames[0];
        first.capture_metadata.key_strokes = [(1_000_000, "Old"), (20_000_000, "Future")]
            .into_iter()
            .map(|(time, text)| KeyStroke {
                physical_key: text.to_owned(),
                display_text: Some(text.to_owned()),
                pressed: true,
                at: TimeUs::new(time),
                repeat: false,
                modifiers: 0,
            })
            .collect();
        first.capture_metadata.mouse_events = [1_000_000, 20_000_000]
            .into_iter()
            .map(|time| MouseInputEvent {
                at: TimeUs::new(time),
                button: MouseButton::Left,
                pressed: true,
                position: Some(PhysicalPoint::default()),
            })
            .collect();
        for mode in [AnnotationMode::RecordedKeys, AnnotationMode::RecordedClicks] {
            let prepared = prepare(
                &manifest,
                &[1, 2, 3],
                AnnotationRequest {
                    mode,
                    hold_ms: 500,
                    ..AnnotationRequest::default()
                },
            );
            assert_eq!(
                track(&prepared).items.len(),
                2,
                "one new event in first sample, then carry-over, then expiry"
            );
            assert_eq!(track(&prepared).items[0].span.start.get(), 0);
            assert_eq!(track(&prepared).items[1].span.start.get(), 100_000);
            if let OverlayContent::KeyStroke { text, .. } = &track(&prepared).items[0].content {
                assert_eq!(text, "Old");
            }
        }
    }

    #[test]
    fn empty_recorded_tasks_skip_unused_oversized_boxes_without_loading_assets() {
        let mut manifest = manifest();
        manifest.canvas.size = PhysicalSize::new(100, 50).unwrap();
        let selected = manifest
            .timeline
            .frames
            .iter()
            .map(|frame| frame.id)
            .collect();
        for mode in [
            AnnotationMode::RecordedKeys,
            AnnotationMode::RecordedClicks,
            AnnotationMode::RecordedCursor,
        ] {
            let request = AnnotationRequest {
                mode,
                ..AnnotationRequest::default()
            };
            let prepared = prepare_annotations_with_assets(
                &manifest,
                &selected,
                &request,
                &AtomicBool::new(false),
                |_| {},
                &|_| panic!("empty annotation task must not load pixels"),
            )
            .unwrap();
            assert!(prepared.commands.is_empty());
            assert!(prepared.assets.is_empty());
            assert_eq!(prepared.frames, 0);
        }
        manifest.timeline.frames[0]
            .capture_metadata
            .key_strokes
            .push(KeyStroke {
                physical_key: "C".to_owned(),
                display_text: Some("C".to_owned()),
                pressed: true,
                at: TimeUs::ZERO,
                repeat: false,
                modifiers: 2,
            });
        let request = AnnotationRequest {
            mode: AnnotationMode::RecordedKeys,
            ..AnnotationRequest::default()
        };
        assert!(
            prepare_annotations_with_assets(
                &manifest,
                &selected,
                &request,
                &AtomicBool::new(false),
                |_| {},
                &|_| panic!("invalid text box must fail before asset loading")
            )
            .unwrap_err()
            .contains("fit inside")
        );
    }

    #[test]
    fn cancellation_and_missing_events_leave_no_prepared_edit() {
        let manifest = manifest();
        let selected = [FrameId::from_u128(1)].into();
        assert!(
            prepare_annotations(
                &manifest,
                &selected,
                &AnnotationRequest::default(),
                &AtomicBool::new(true),
                |_| {}
            )
            .unwrap_err()
            .contains("cancelled")
        );
        assert!(
            prepare_annotations(
                &manifest,
                &selected,
                &AnnotationRequest {
                    mode: AnnotationMode::RecordedKeys,
                    ..AnnotationRequest::default()
                },
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap()
            .commands
            .is_empty()
        );
    }
}
