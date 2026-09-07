use std::collections::BTreeMap;

use gif_from_screen_domain::{
    AssetId, BlendMode, CompositePrecision, FrameClip, FrameId, FrameRenderStep, OverlayContent,
    OverlayId, OverlayItem, OverlayTrack, PhysicalPoint, PhysicalRect, PhysicalSize,
    ProgressDirection, ProgressStyle, Rgba, ShapeKind, StrokePoint, TimeUs, TimelineSpan,
    validate_frame_render_steps,
};

use crate::{
    CancellationToken, CpuRenderer, FrameAssetProvider, RenderError, RenderLimits, RgbaSurface,
    SurfaceError, surface::checked_byte_len,
};

const CANCELLATION_PIXEL_INTERVAL: u32 = 1_024;

#[path = "event_overlay.rs"]
mod events;

#[cfg(test)]
#[path = "stage_precision_tests.rs"]
mod precision_tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaintBlend {
    Legacy(BlendMode),
    WpfSourceOver,
}

impl From<BlendMode> for PaintBlend {
    fn from(mode: BlendMode) -> Self {
        Self::Legacy(mode)
    }
}

/// Freezes one legacy timed item's content at a half-open sampling point.
///
/// Visibility, track opacity and raster opacity deliberately do not filter this
/// operation: hidden artwork must survive an explicit representation change.
/// Existing fixed styles and static content are copied unchanged. Legacy time
/// progress retains its millionths-then-ceiling pixel rule, not exact-ratio rounding.
/// This performs no asset, font or pixel I/O and does not create mark identities.
///
/// # Errors
/// Returns an overflowing-span or unsupported-content error. In particular,
/// legacy frame-number progress without a frozen label remains unsupported.
/// `Ok(None)` means only that the item is inactive at this sampling point.
pub fn freeze_timed_overlay_content(
    item: &OverlayItem,
    sample_time: TimeUs,
) -> Result<Option<OverlayContent>, RenderError> {
    let end = item.span.end().ok_or(RenderError::OverlaySpanOverflow {
        overlay_id: item.id,
    })?;
    if sample_time < item.span.start || sample_time >= end {
        return Ok(None);
    }
    validate_overlay_content(item.id, &item.content, true)?;
    let mut content = item.content.clone();
    if let OverlayContent::Progress { style, .. } = &mut content
        && style.is_none()
    {
        *style = Some(ProgressStyle {
            amount_millionths: events::legacy_progress_amount(item.span, sample_time),
            fraction: None,
            direction: ProgressDirection::LeftToRight,
            label: None,
            label_position: PhysicalPoint::default(),
            label_text: String::new(),
        });
    }
    Ok(Some(content))
}

/// Stable identity pair for one active raster overlay and its immutable asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RasterOverlayAsset {
    /// Overlay item referencing the asset.
    pub overlay_id: OverlayId,
    /// Immutable RGBA8 raster asset.
    pub asset_id: AssetId,
}

#[derive(Clone, Copy, Debug)]
struct OverlayLayer<'a> {
    id: OverlayId,
    content: &'a OverlayContent,
    span: Option<TimelineSpan>,
    stage: Option<u32>,
    z_index: i32,
    track_opacity: u8,
    blend_mode: PaintBlend,
    track_index: usize,
    item_index: usize,
}

/// A detached, frame-specific drawing plan. Only active drawing content is copied;
/// project timelines, authoring scopes, recipes and other frames are not retained.
#[derive(Clone, Debug)]
pub struct OverlayRenderPlan {
    frame_id: FrameId,
    sample_time: TimeUs,
    layers: Vec<OwnedOverlayLayer>,
}

#[derive(Clone, Debug)]
struct OwnedOverlayLayer {
    id: OverlayId,
    content: OverlayContent,
    span: Option<TimelineSpan>,
    stage: Option<u32>,
    track_opacity: u8,
    blend_mode: PaintBlend,
}

impl OverlayRenderPlan {
    /// Resolves timed items and whole-frame marks through the same ordering used
    /// by direct CPU rendering. Local authoring scopes never clip visible marks.
    ///
    /// # Errors
    /// Returns errors for unsupported active content, invalid spans, cancellation
    /// or a drawing-plan allocation failure.
    pub fn for_frame<C: CancellationToken + ?Sized>(
        tracks: &[OverlayTrack],
        frame_id: FrameId,
        sample_time: TimeUs,
        cancellation: &C,
    ) -> Result<Self, RenderError> {
        let resolved = active_overlay_layers(tracks, Some(frame_id), sample_time, cancellation)?;
        let mut layers = Vec::new();
        layers.try_reserve_exact(resolved.len()).map_err(|_| {
            RenderError::OverlayPlanAllocationFailed {
                requested: resolved.len(),
            }
        })?;
        for layer in resolved {
            check_cancelled(cancellation)?;
            layers.push(OwnedOverlayLayer {
                id: layer.id,
                content: layer.content.clone(),
                span: layer.span,
                stage: layer.stage,
                track_opacity: layer.track_opacity,
                blend_mode: layer.blend_mode,
            });
        }
        Ok(Self {
            frame_id,
            sample_time,
            layers,
        })
    }

    /// All active stages' immutable assets, retaining repeated identities.
    /// Enumeration is stable z/track/item order; chronological stage order is
    /// resolved only when this plan is rendered with its owner's step vector.
    pub fn raster_assets(&self) -> impl Iterator<Item = RasterOverlayAsset> + '_ {
        self.layers.iter().filter_map(|layer| {
            Some(RasterOverlayAsset {
                overlay_id: layer.id,
                asset_id: layer.content.referenced_asset()?,
            })
        })
    }

    fn layers(&self) -> impl Iterator<Item = OverlayLayer<'_>> {
        self.layers
            .iter()
            .enumerate()
            .map(|(index, layer)| OverlayLayer {
                id: layer.id,
                content: &layer.content,
                span: layer.span,
                stage: layer.stage,
                // The detached plan has already been sorted. These fields are not
                // consulted again by the compositor.
                z_index: 0,
                track_opacity: layer.track_opacity,
                blend_mode: layer.blend_mode,
                track_index: 0,
                item_index: index,
            })
    }
}

impl CpuRenderer {
    /// Renders one clip and composites all supported overlays active at `sample_time`.
    ///
    /// Legacy overlay spans use half-open point sampling: an item is active when
    /// `span.start <= sample_time < span.end()`. Visible non-zero-opacity Raster, Shape, and Drawing
    /// items and the matching `clip.id`'s frame-owned marks are ordered globally by
    /// `(z_index, track order, item order)` within each Composite stage. Legacy
    /// timed items join only the first stage; unanchored marks paint after the
    /// ordered steps. Empty steps preserve the old global ordering. A frame-owned mark is painted once for the whole
    /// frame, regardless of its authoring scopes or sample time. Raster images are
    /// nearest-neighbor sampled without a resized allocation. Shapes and drawings are hard-edged,
    /// clipped directly to the destination, and allocate no geometry-sized buffers. Track opacity
    /// and raster item opacity multiply source alpha. Legacy stages preserve deterministic
    /// straight-alpha Normal/Multiply/Screen composition. Explicit WPF stages require Normal,
    /// convert the complete surface to premultiplied bytes once, compose every active mark,
    /// then cross one WIC-compatible straight-RGBA boundary. Empty stages do not quantize.
    /// Drawing `width` is a base diameter: each normalized
    /// `pressure_milli` in `0..=1000` scales it with half-up integer rounding, positive pressure is
    /// at least one pixel, zero pressure is invisible, and width interpolates linearly along each
    /// segment. Line and Arrow run from the centers of the bounds' top-left and bottom-right pixels;
    /// Arrow adds a two-edge head and optionally fills its triangular interior. Rectangle and
    /// Ellipse strokes are drawn inward, with the optional fill beneath them. Active unsupported
    /// content returns an error instead of silently disappearing from preview or export.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] for base or overlay asset loading failures, invalid/oversized source
    /// surfaces, overflowing spans or plan sizes, allocation failure, or cancellation. The output
    /// is not observable on failure.
    pub fn render_clip_with_overlays<P, C>(
        &self,
        clip: &FrameClip,
        tracks: &[OverlayTrack],
        sample_time: TimeUs,
        provider: &P,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError>
    where
        P: FrameAssetProvider + ?Sized,
        C: CancellationToken + ?Sized,
    {
        self.render_staged_overlays(
            clip,
            active_overlay_layers(tracks, Some(clip.id), sample_time, cancellation)?,
            sample_time,
            provider,
            cancellation,
        )
    }

    /// Renders a detached plan produced by [`OverlayRenderPlan::for_frame`].
    ///
    /// # Errors
    /// Returns normal rendering errors, or rejects a plan belonging to another frame.
    pub fn render_clip_with_overlay_plan<P, C>(
        &self,
        clip: &FrameClip,
        plan: &OverlayRenderPlan,
        provider: &P,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError>
    where
        P: FrameAssetProvider + ?Sized,
        C: CancellationToken + ?Sized,
    {
        check_cancelled(cancellation)?;
        if clip.id != plan.frame_id {
            return Err(RenderError::OverlayPlanFrameMismatch {
                expected: plan.frame_id,
                actual: clip.id,
            });
        }
        self.render_staged_overlays(
            clip,
            plan.layers(),
            plan.sample_time,
            provider,
            cancellation,
        )
    }

    fn render_staged_overlays<'a, P: FrameAssetProvider + ?Sized, C: CancellationToken + ?Sized>(
        &self,
        clip: &FrameClip,
        layers: impl IntoIterator<Item = OverlayLayer<'a>>,
        sample_time: TimeUs,
        provider: &P,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError> {
        let mut groups = stage_overlay_layers(clip, layers, cancellation)?;
        let mut surface = self.render_clip_prefix(clip, provider, cancellation)?;
        for step in &clip.render_steps {
            check_cancelled(cancellation)?;
            if let FrameRenderStep::Composite {
                stage_id,
                precision,
            } = step
            {
                if let Some(layers) = groups.remove(&Some(*stage_id)) {
                    composite_stage(
                        &mut surface,
                        layers,
                        *precision,
                        sample_time,
                        provider,
                        self.limits(),
                        cancellation,
                    )?;
                }
            } else {
                surface = self.apply_render_step(surface, step, provider, cancellation)?;
            }
        }
        if let Some(layers) = groups.remove(&None) {
            composite_overlay_layers(
                &mut surface,
                layers,
                sample_time,
                provider,
                self.limits(),
                cancellation,
            )?;
        }
        debug_assert!(
            groups.is_empty(),
            "every validated stage is consumed exactly once"
        );
        Ok(surface)
    }

    /// Compatibility alias for [`Self::render_clip_with_overlays`].
    ///
    /// Despite its historical name this composites all supported content, including frame-owned
    /// marks, through the complete shared overlay path.
    ///
    /// # Errors
    ///
    /// Returns the same [`RenderError`] values as [`Self::render_clip_with_overlays`].
    pub fn render_clip_with_raster_overlays<P, C>(
        &self,
        clip: &FrameClip,
        tracks: &[OverlayTrack],
        sample_time: TimeUs,
        provider: &P,
        cancellation: &C,
    ) -> Result<RgbaSurface, RenderError>
    where
        P: FrameAssetProvider + ?Sized,
        C: CancellationToken + ?Sized,
    {
        self.render_clip_with_overlays(clip, tracks, sample_time, provider, cancellation)
    }
}

fn stage_overlay_layers<'a, C: CancellationToken + ?Sized>(
    clip: &FrameClip,
    layers: impl IntoIterator<Item = OverlayLayer<'a>>,
    cancellation: &C,
) -> Result<BTreeMap<Option<u32>, Vec<OverlayLayer<'a>>>, RenderError> {
    check_cancelled(cancellation)?;
    validate_frame_render_steps(&clip.render_steps).map_err(|reason| {
        RenderError::InvalidRenderSteps {
            frame_id: clip.id,
            reason,
        }
    })?;
    let stages: BTreeMap<_, _> = clip
        .render_steps
        .iter()
        .filter_map(|step| match step {
            FrameRenderStep::Composite {
                stage_id,
                precision,
            } => Some((*stage_id, *precision)),
            _ => None,
        })
        .collect();
    let first = match clip.render_steps.first() {
        Some(FrameRenderStep::Composite { stage_id, .. }) => Some(*stage_id),
        _ => None,
    };
    let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for mut layer in layers {
        check_cancelled(cancellation)?;
        let stage = if layer.span.is_some() {
            first
        } else {
            layer.stage
        };
        if let Some(stage_id) = stage
            && !stages.contains_key(&stage_id)
        {
            return Err(RenderError::OverlayStageMissing {
                frame_id: clip.id,
                overlay_id: layer.id,
                stage_id,
            });
        }
        if let Some(stage_id) = stage
            && stages.get(&stage_id) == Some(&CompositePrecision::WpfPbgra8PngV1)
        {
            if layer.blend_mode != PaintBlend::Legacy(BlendMode::Normal) {
                return Err(RenderError::InvalidRenderSteps {
                    frame_id: clip.id,
                    reason: format!(
                        "WPF paint stage {stage_id} requires Normal blend for overlay {}.",
                        layer.id
                    ),
                });
            }
            layer.blend_mode = PaintBlend::WpfSourceOver;
        }
        let group = groups.entry(stage).or_default();
        group
            .try_reserve(1)
            .map_err(|_| RenderError::OverlayPlanAllocationFailed {
                requested: group.len().saturating_add(1),
            })?;
        group.push(layer);
    }
    Ok(groups)
}

/// Returns raster assets from visible, non-zero-opacity overlays active at `sample_time`.
///
/// Results include all active stages in deterministic z/track/item order.
/// Stage chronology requires an owner's render steps and is resolved during
/// rendering. Repeated asset identities are retained so
/// callers can preserve overlay context or deduplicate explicitly.
///
/// # Errors
///
/// Returns [`RenderError`] for an overflowing active span, plan size/allocation failure, or
/// cancellation. Visible frame-owned content returns
/// [`RenderError::OverlayFrameIdentityRequired`]; callers handling new projects must use
/// [`active_raster_overlay_assets_for_frame`] instead of guessing a frame from time.
pub fn active_raster_overlay_assets<C>(
    tracks: &[OverlayTrack],
    sample_time: TimeUs,
    cancellation: &C,
) -> Result<Vec<RasterOverlayAsset>, RenderError>
where
    C: CancellationToken + ?Sized,
{
    raster_assets_from_layers(active_overlay_layers(
        tracks,
        None,
        sample_time,
        cancellation,
    )?)
}

/// Returns active raster assets for a specific frame, including its whole-frame marks.
///
/// # Errors
/// Returns the same planning errors as [`OverlayRenderPlan::for_frame`].
pub fn active_raster_overlay_assets_for_frame<C: CancellationToken + ?Sized>(
    tracks: &[OverlayTrack],
    frame_id: FrameId,
    sample_time: TimeUs,
    cancellation: &C,
) -> Result<Vec<RasterOverlayAsset>, RenderError> {
    raster_assets_from_layers(active_overlay_layers(
        tracks,
        Some(frame_id),
        sample_time,
        cancellation,
    )?)
}

fn raster_assets_from_layers(
    layers: Vec<OverlayLayer<'_>>,
) -> Result<Vec<RasterOverlayAsset>, RenderError> {
    let mut assets = Vec::new();
    assets.try_reserve_exact(layers.len()).map_err(|_| {
        RenderError::OverlayPlanAllocationFailed {
            requested: layers.len(),
        }
    })?;
    assets.extend(layers.into_iter().filter_map(|layer| {
        let asset_id = layer.content.referenced_asset()?;
        Some(RasterOverlayAsset {
            overlay_id: layer.id,
            asset_id,
        })
    }));
    Ok(assets)
}

fn composite_overlay_layers<'a, P, C>(
    destination: &mut RgbaSurface,
    layers: impl IntoIterator<Item = OverlayLayer<'a>>,
    sample_time: TimeUs,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError>
where
    P: FrameAssetProvider + ?Sized,
    C: CancellationToken + ?Sized,
{
    for layer in layers {
        check_cancelled(cancellation)?;
        match layer.content {
            OverlayContent::Text {
                position,
                raster: Some(raster),
                ..
            }
            | OverlayContent::KeyStroke {
                position,
                raster: Some(raster),
                ..
            } => composite_raster_overlay(
                destination,
                layer.id,
                raster.asset_id,
                *position,
                raster.size,
                255,
                layer.track_opacity,
                layer.blend_mode,
                provider,
                limits,
                cancellation,
            )?,
            OverlayContent::Raster {
                asset_id,
                position,
                size,
                opacity,
            } => composite_raster_overlay(
                destination,
                layer.id,
                *asset_id,
                *position,
                *size,
                *opacity,
                layer.track_opacity,
                layer.blend_mode,
                provider,
                limits,
                cancellation,
            )?,
            OverlayContent::Shape {
                kind,
                bounds,
                stroke_width,
                stroke,
                fill,
            } => composite_shape(
                destination,
                layer.id,
                *kind,
                *bounds,
                *stroke_width,
                *stroke,
                *fill,
                layer.track_opacity,
                layer.blend_mode,
                cancellation,
            )?,
            OverlayContent::Drawing {
                points,
                width,
                color,
            } => composite_drawing(
                destination,
                layer.id,
                points,
                *width,
                *color,
                layer.track_opacity,
                layer.blend_mode,
                cancellation,
            )?,
            OverlayContent::Progress { .. }
            | OverlayContent::MouseClick { .. }
            | OverlayContent::Cursor { .. } => events::composite_event(
                destination,
                layer,
                sample_time,
                provider,
                limits,
                cancellation,
            )?,
            content => return Err(unsupported_overlay(layer.id, content)),
        }
    }
    Ok(())
}

fn composite_stage<P: FrameAssetProvider + ?Sized, C: CancellationToken + ?Sized>(
    destination: &mut RgbaSurface,
    layers: Vec<OverlayLayer<'_>>,
    precision: CompositePrecision,
    sample_time: TimeUs,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError> {
    if layers.is_empty() {
        return Ok(());
    }
    if precision == CompositePrecision::WpfPbgra8PngV1 {
        convert_surface_precision(destination, crate::wpf_pixels::premultiply, cancellation)?;
    }
    composite_overlay_layers(
        destination,
        layers,
        sample_time,
        provider,
        limits,
        cancellation,
    )?;
    if precision == CompositePrecision::WpfPbgra8PngV1 {
        convert_surface_precision(destination, crate::wpf_pixels::unpremultiply, cancellation)?;
    }
    Ok(())
}

fn convert_surface_precision<C: CancellationToken + ?Sized>(
    surface: &mut RgbaSurface,
    convert: fn([u8; 4]) -> [u8; 4],
    cancellation: &C,
) -> Result<(), RenderError> {
    check_cancelled(cancellation)?;
    for (index, pixel) in surface
        .pixels_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        if index.is_multiple_of(CANCELLATION_PIXEL_INTERVAL as usize) {
            check_cancelled(cancellation)?;
        }
        *pixel = convert(*pixel);
    }
    check_cancelled(cancellation)
}

fn active_overlay_layers<'a, C>(
    tracks: &'a [OverlayTrack],
    frame_id: Option<FrameId>,
    sample_time: TimeUs,
    cancellation: &C,
) -> Result<Vec<OverlayLayer<'a>>, RenderError>
where
    C: CancellationToken + ?Sized,
{
    let mut layers = Vec::new();
    for (track_index, track) in tracks.iter().enumerate() {
        check_cancelled(cancellation)?;
        if !track.visible || track.opacity == 0 {
            continue;
        }
        for (item_index, item) in track.items.iter().enumerate() {
            check_cancelled(cancellation)?;
            if matches!(item.content, OverlayContent::Raster { opacity: 0, .. }) {
                continue;
            }
            let end = item.span.end().ok_or(RenderError::OverlaySpanOverflow {
                overlay_id: item.id,
            })?;
            if sample_time < item.span.start || sample_time >= end {
                continue;
            }
            push_layer(
                &mut layers,
                OverlayLayer {
                    id: item.id,
                    content: &item.content,
                    span: Some(item.span),
                    stage: None,
                    z_index: item.z_index,
                    track_opacity: track.opacity,
                    blend_mode: track.blend_mode.into(),
                    track_index,
                    item_index,
                },
            )?;
        }
        let mut item_index = track.items.len();
        for cell in track.frame_cells.iter().flatten() {
            check_cancelled(cancellation)?;
            if let Some(frame_id) = frame_id {
                if cell.frame_id != frame_id {
                    continue;
                }
            } else if cell
                .marks
                .iter()
                .any(|mark| !matches!(mark.content, OverlayContent::Raster { opacity: 0, .. }))
            {
                return Err(RenderError::OverlayFrameIdentityRequired);
            }
            for mark in &cell.marks {
                check_cancelled(cancellation)?;
                push_layer(
                    &mut layers,
                    OverlayLayer {
                        id: mark.id,
                        content: &mark.content,
                        span: None,
                        stage: cell.stage,
                        z_index: mark.z_index,
                        track_opacity: track.opacity,
                        blend_mode: track.blend_mode.into(),
                        track_index,
                        item_index,
                    },
                )?;
                item_index = item_index
                    .checked_add(1)
                    .ok_or(RenderError::OverlayPlanSizeOverflow)?;
            }
        }
    }
    layers.sort_unstable_by_key(|layer| (layer.z_index, layer.track_index, layer.item_index));
    check_cancelled(cancellation)?;
    Ok(layers)
}

fn push_layer<'a>(
    layers: &mut Vec<OverlayLayer<'a>>,
    layer: OverlayLayer<'a>,
) -> Result<(), RenderError> {
    if matches!(layer.content, OverlayContent::Raster { opacity: 0, .. }) {
        return Ok(());
    }
    validate_overlay_content(layer.id, layer.content, layer.span.is_some())?;
    let requested = layers
        .len()
        .checked_add(1)
        .ok_or(RenderError::OverlayPlanSizeOverflow)?;
    layers
        .try_reserve(1)
        .map_err(|_| RenderError::OverlayPlanAllocationFailed { requested })?;
    layers.push(layer);
    Ok(())
}

fn validate_overlay_content(
    id: OverlayId,
    content: &OverlayContent,
    timed: bool,
) -> Result<(), RenderError> {
    if !timed && matches!(content, OverlayContent::Progress { style: None, .. }) {
        return Err(RenderError::InvalidOverlayGeometry {
            overlay_id: id,
            reason: "frame-owned progress requires a frozen style",
        });
    }
    if !matches!(
        content,
        OverlayContent::Raster { .. }
            | OverlayContent::Text {
                raster: Some(_),
                ..
            }
            | OverlayContent::KeyStroke {
                raster: Some(_),
                ..
            }
            | OverlayContent::Cursor { .. }
            | OverlayContent::MouseClick { .. }
            | OverlayContent::Progress {
                show_frame_number: false,
                ..
            }
            | OverlayContent::Progress { style: Some(_), .. }
            | OverlayContent::Shape { .. }
            | OverlayContent::Drawing { .. }
    ) {
        return Err(unsupported_overlay(id, content));
    }
    Ok(())
}

fn unsupported_overlay(overlay_id: OverlayId, content: &OverlayContent) -> RenderError {
    let kind = match content {
        OverlayContent::Text { .. } => "text",
        OverlayContent::KeyStroke { .. } => "keystroke",
        OverlayContent::Cursor { .. } => "cursor",
        OverlayContent::MouseClick { .. } => "mouse click",
        OverlayContent::Progress { .. } => "progress",
        OverlayContent::Raster { .. } => "raster",
        OverlayContent::Shape { .. } => "shape",
        OverlayContent::Drawing { .. } => "drawing",
    };
    RenderError::UnsupportedOverlay { overlay_id, kind }
}

#[allow(
    clippy::too_many_arguments,
    reason = "overlay identity, geometry, opacity, blend, provider, limits, and cancellation remain explicit"
)]
fn composite_raster_overlay<P, C>(
    destination: &mut RgbaSurface,
    overlay_id: OverlayId,
    asset_id: AssetId,
    position: PhysicalPoint,
    size: PhysicalSize,
    item_opacity: u8,
    track_opacity: u8,
    blend_mode: impl Into<PaintBlend>,
    provider: &P,
    limits: RenderLimits,
    cancellation: &C,
) -> Result<(), RenderError>
where
    P: FrameAssetProvider + ?Sized,
    C: CancellationToken + ?Sized,
{
    let blend_mode = blend_mode.into();
    if position.x.get() >= destination.width() || position.y.get() >= destination.height() {
        return Ok(());
    }
    if size.width.get() == 0 || size.height.get() == 0 {
        return Err(SurfaceError::EmptyDimensions {
            width: size.width.get(),
            height: size.height.get(),
        }
        .into());
    }
    let source = provider
        .load_rgba8(asset_id)
        .map_err(|source| RenderError::OverlayAssetLoad {
            overlay_id,
            asset_id,
            source,
        })?;
    let requested = checked_byte_len(source.size())?;
    if requested > limits.max_surface_bytes {
        return Err(RenderError::SurfaceLimitExceeded {
            requested,
            limit: limits.max_surface_bytes,
        });
    }
    let start_x = position.x.get();
    let start_y = position.y.get();
    let visible_width = size.width.get().min(destination.width() - start_x);
    let visible_height = size.height.get().min(destination.height() - start_y);
    let scaled_width = u64::from(size.width.get());
    let scaled_height = u64::from(size.height.get());
    for local_y in 0..visible_height {
        check_cancelled(cancellation)?;
        let source_y =
            u32::try_from(u64::from(local_y) * u64::from(source.height()) / scaled_height)
                .expect("nearest overlay y coordinate is bounded by source height");
        for local_x in 0..visible_width {
            if local_x.is_multiple_of(CANCELLATION_PIXEL_INTERVAL) {
                check_cancelled(cancellation)?;
            }
            let source_x =
                u32::try_from(u64::from(local_x) * u64::from(source.width()) / scaled_width)
                    .expect("nearest overlay x coordinate is bounded by source width");
            let source_offset = source.byte_offset(source_x, source_y);
            let destination_offset = destination.byte_offset(start_x + local_x, start_y + local_y);
            let source_pixel = &source.pixels()[source_offset..source_offset + 4];
            let destination_pixel =
                &mut destination.pixels_mut()[destination_offset..destination_offset + 4];
            blend_pixel(
                destination_pixel,
                source_pixel,
                item_opacity,
                track_opacity,
                blend_mode,
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ScanBounds {
    start_x: u32,
    start_y: u32,
    end_x: u32,
    end_y: u32,
}

#[allow(
    clippy::too_many_arguments,
    reason = "shape identity, geometry, colors, track composition, and cancellation remain explicit"
)]
fn composite_shape<C>(
    destination: &mut RgbaSurface,
    overlay_id: OverlayId,
    kind: ShapeKind,
    bounds: PhysicalRect,
    stroke_width: u16,
    stroke: Rgba,
    fill: Option<Rgba>,
    track_opacity: u8,
    blend_mode: impl Into<PaintBlend>,
    cancellation: &C,
) -> Result<(), RenderError>
where
    C: CancellationToken + ?Sized,
{
    let blend_mode = blend_mode.into();
    validate_shape(overlay_id, kind, bounds, stroke_width, fill)?;
    let padding = match kind {
        ShapeKind::Line => u32::from(stroke_width).div_ceil(2),
        ShapeKind::Arrow => u32::from(stroke_width).saturating_mul(3).max(6),
        ShapeKind::Rectangle | ShapeKind::Ellipse => 0,
    };
    let Some(scan) = scan_bounds_for_rect(bounds, padding, destination) else {
        return Ok(());
    };
    for y in scan.start_y..scan.end_y {
        check_cancelled(cancellation)?;
        for x in scan.start_x..scan.end_x {
            if x.wrapping_sub(scan.start_x)
                .is_multiple_of(CANCELLATION_PIXEL_INTERVAL)
            {
                check_cancelled(cancellation)?;
            }
            if let Some(color) = shape_pixel(kind, bounds, stroke_width, stroke, fill, x, y) {
                paint_pixel(destination, x, y, color, track_opacity, blend_mode);
            }
        }
    }
    Ok(())
}

fn validate_shape(
    overlay_id: OverlayId,
    kind: ShapeKind,
    bounds: PhysicalRect,
    stroke_width: u16,
    fill: Option<Rgba>,
) -> Result<(), RenderError> {
    if bounds.size.width.get() == 0 || bounds.size.height.get() == 0 {
        return Err(RenderError::InvalidOverlayGeometry {
            overlay_id,
            reason: "shape bounds must be non-empty",
        });
    }
    if bounds.end_x().is_none() || bounds.end_y().is_none() {
        return Err(RenderError::InvalidOverlayGeometry {
            overlay_id,
            reason: "shape bounds coordinates overflow",
        });
    }
    let requires_stroke = matches!(kind, ShapeKind::Line | ShapeKind::Arrow) || fill.is_none();
    if requires_stroke && stroke_width == 0 {
        return Err(RenderError::InvalidOverlayGeometry {
            overlay_id,
            reason: "a visible shape stroke must have positive width",
        });
    }
    Ok(())
}

fn scan_bounds_for_rect(
    bounds: PhysicalRect,
    padding: u32,
    destination: &RgbaSurface,
) -> Option<ScanBounds> {
    let end_x = bounds.end_x()?;
    let end_y = bounds.end_y()?;
    let start_x = bounds.origin.x.get().saturating_sub(padding);
    let start_y = bounds.origin.y.get().saturating_sub(padding);
    let end_x = end_x.saturating_add(padding).min(destination.width());
    let end_y = end_y.saturating_add(padding).min(destination.height());
    let start_x = start_x.min(destination.width());
    let start_y = start_y.min(destination.height());
    (start_x < end_x && start_y < end_y).then_some(ScanBounds {
        start_x,
        start_y,
        end_x,
        end_y,
    })
}

fn shape_pixel(
    kind: ShapeKind,
    bounds: PhysicalRect,
    stroke_width: u16,
    stroke: Rgba,
    fill: Option<Rgba>,
    x: u32,
    y: u32,
) -> Option<Rgba> {
    match kind {
        ShapeKind::Line => line_shape_pixel(bounds, stroke_width, stroke, x, y),
        ShapeKind::Arrow => arrow_shape_pixel(bounds, stroke_width, stroke, fill, x, y),
        ShapeKind::Rectangle => rectangle_shape_pixel(bounds, stroke_width, stroke, fill, x, y),
        ShapeKind::Ellipse => ellipse_shape_pixel(bounds, stroke_width, stroke, fill, x, y),
    }
}

fn line_shape_pixel(
    bounds: PhysicalRect,
    stroke_width: u16,
    stroke: Rgba,
    x: u32,
    y: u32,
) -> Option<Rgba> {
    let (start, end) = diagonal_endpoints(bounds);
    (stroke.alpha != 0
        && point_segment_distance_squared(pixel_center(x, y), start, end).0
            <= stroke_radius(stroke_width).powi(2))
    .then_some(stroke)
}

fn arrow_shape_pixel(
    bounds: PhysicalRect,
    stroke_width: u16,
    stroke: Rgba,
    fill: Option<Rgba>,
    x: u32,
    y: u32,
) -> Option<Rgba> {
    let point = pixel_center(x, y);
    let (start, end) = diagonal_endpoints(bounds);
    let (left, right) = arrow_head(start, end, stroke_width);
    let radius_squared = stroke_radius(stroke_width).powi(2);
    let on_stroke = stroke.alpha != 0
        && [
            point_segment_distance_squared(point, start, end).0,
            point_segment_distance_squared(point, end, left).0,
            point_segment_distance_squared(point, end, right).0,
        ]
        .into_iter()
        .any(|distance| distance <= radius_squared);
    if on_stroke {
        Some(stroke)
    } else if point_in_triangle(point, end, left, right) {
        fill.filter(|color| color.alpha != 0)
    } else {
        None
    }
}

fn rectangle_shape_pixel(
    bounds: PhysicalRect,
    stroke_width: u16,
    stroke: Rgba,
    fill: Option<Rgba>,
    x: u32,
    y: u32,
) -> Option<Rgba> {
    let end_x = bounds.end_x()?;
    let end_y = bounds.end_y()?;
    if x < bounds.origin.x.get() || x >= end_x || y < bounds.origin.y.get() || y >= end_y {
        return None;
    }
    let local_x = f64::from(x - bounds.origin.x.get()) + 0.5;
    let local_y = f64::from(y - bounds.origin.y.get()) + 0.5;
    let edge_distance = local_x
        .min(f64::from(bounds.size.width.get()) - local_x)
        .min(local_y)
        .min(f64::from(bounds.size.height.get()) - local_y);
    if stroke.alpha != 0 && edge_distance <= f64::from(stroke_width) {
        Some(stroke)
    } else {
        fill.filter(|color| color.alpha != 0)
    }
}

fn ellipse_shape_pixel(
    bounds: PhysicalRect,
    stroke_width: u16,
    stroke: Rgba,
    fill: Option<Rgba>,
    x: u32,
    y: u32,
) -> Option<Rgba> {
    let radius_x = f64::from(bounds.size.width.get()) / 2.0;
    let radius_y = f64::from(bounds.size.height.get()) / 2.0;
    let center_x = f64::from(bounds.origin.x.get()) + radius_x;
    let center_y = f64::from(bounds.origin.y.get()) + radius_y;
    let delta_x = f64::from(x) + 0.5 - center_x;
    let delta_y = f64::from(y) + 0.5 - center_y;
    let outer = (delta_x / radius_x).powi(2) + (delta_y / radius_y).powi(2);
    if outer > 1.0 {
        return None;
    }
    let inner_radius_x = radius_x - f64::from(stroke_width);
    let inner_radius_y = radius_y - f64::from(stroke_width);
    let in_stroke = stroke_width != 0
        && stroke.alpha != 0
        && (inner_radius_x <= 0.0
            || inner_radius_y <= 0.0
            || (delta_x / inner_radius_x).powi(2) + (delta_y / inner_radius_y).powi(2) >= 1.0);
    if in_stroke {
        Some(stroke)
    } else {
        fill.filter(|color| color.alpha != 0)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "drawing identity, path, styling, track composition, and cancellation remain explicit"
)]
fn composite_drawing<C>(
    destination: &mut RgbaSurface,
    overlay_id: OverlayId,
    points: &[StrokePoint],
    width: u16,
    color: Rgba,
    track_opacity: u8,
    blend_mode: impl Into<PaintBlend>,
    cancellation: &C,
) -> Result<(), RenderError>
where
    C: CancellationToken + ?Sized,
{
    let blend_mode = blend_mode.into();
    if points.is_empty() || color.alpha == 0 {
        return Ok(());
    }
    if width == 0 {
        return Err(RenderError::InvalidOverlayGeometry {
            overlay_id,
            reason: "drawing base width must be positive",
        });
    }
    for (point_index, point) in points.iter().enumerate() {
        check_cancelled(cancellation)?;
        if point.pressure_milli > 1_000 {
            return Err(RenderError::InvalidDrawingPressure {
                overlay_id,
                point_index,
                pressure_milli: point.pressure_milli,
                maximum: 1_000,
            });
        }
    }
    let Some(scan) = drawing_scan_bounds(points, width, destination) else {
        return Ok(());
    };
    for y in scan.start_y..scan.end_y {
        check_cancelled(cancellation)?;
        for x in scan.start_x..scan.end_x {
            if x.wrapping_sub(scan.start_x)
                .is_multiple_of(CANCELLATION_PIXEL_INTERVAL)
            {
                check_cancelled(cancellation)?;
            }
            if drawing_covers_pixel(points, width, pixel_center(x, y), cancellation)? {
                paint_pixel(destination, x, y, color, track_opacity, blend_mode);
            }
        }
    }
    Ok(())
}

fn drawing_scan_bounds(
    points: &[StrokePoint],
    width: u16,
    destination: &RgbaSurface,
) -> Option<ScanBounds> {
    let padding = u32::from(width).div_ceil(2);
    let min_x = points
        .iter()
        .map(|point| point.point.x.get())
        .min()?
        .saturating_sub(padding)
        .min(destination.width());
    let min_y = points
        .iter()
        .map(|point| point.point.y.get())
        .min()?
        .saturating_sub(padding)
        .min(destination.height());
    let max_x = points
        .iter()
        .map(|point| point.point.x.get())
        .max()?
        .saturating_add(padding)
        .saturating_add(1)
        .min(destination.width());
    let max_y = points
        .iter()
        .map(|point| point.point.y.get())
        .max()?
        .saturating_add(padding)
        .saturating_add(1)
        .min(destination.height());
    (min_x < max_x && min_y < max_y).then_some(ScanBounds {
        start_x: min_x,
        start_y: min_y,
        end_x: max_x,
        end_y: max_y,
    })
}

fn drawing_covers_pixel<C>(
    points: &[StrokePoint],
    width: u16,
    pixel: (f64, f64),
    cancellation: &C,
) -> Result<bool, RenderError>
where
    C: CancellationToken + ?Sized,
{
    if let [point] = points {
        let radius = f64::from(pressure_width(width, point.pressure_milli)) / 2.0;
        return Ok(
            radius > 0.0 && squared_distance(pixel, stroke_point_center(point)) <= radius.powi(2)
        );
    }
    for (segment_index, segment) in points.windows(2).enumerate() {
        if segment_index.is_multiple_of(256) {
            check_cancelled(cancellation)?;
        }
        let start = stroke_point_center(&segment[0]);
        let end = stroke_point_center(&segment[1]);
        let (distance_squared, progress) = point_segment_distance_squared(pixel, start, end);
        let start_width = f64::from(pressure_width(width, segment[0].pressure_milli));
        let end_width = f64::from(pressure_width(width, segment[1].pressure_milli));
        let interpolated_width = start_width + (end_width - start_width) * progress;
        let radius = f64::midpoint(0.0, interpolated_width);
        if radius > 0.0 && distance_squared <= radius.powi(2) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn pressure_width(base_width: u16, pressure_milli: u16) -> u32 {
    if pressure_milli == 0 {
        return 0;
    }
    let rounded = (u32::from(base_width) * u32::from(pressure_milli) + 500) / 1_000;
    rounded.max(1)
}

fn diagonal_endpoints(bounds: PhysicalRect) -> ((f64, f64), (f64, f64)) {
    (
        (
            f64::from(bounds.origin.x.get()) + 0.5,
            f64::from(bounds.origin.y.get()) + 0.5,
        ),
        (
            f64::from(bounds.end_x().unwrap_or(u32::MAX)) - 0.5,
            f64::from(bounds.end_y().unwrap_or(u32::MAX)) - 0.5,
        ),
    )
}

fn arrow_head(start: (f64, f64), end: (f64, f64), stroke_width: u16) -> ((f64, f64), (f64, f64)) {
    let delta_x = end.0 - start.0;
    let delta_y = end.1 - start.1;
    let length = (delta_x * delta_x + delta_y * delta_y).sqrt();
    if length == 0.0 {
        return (end, end);
    }
    let head_length = f64::from(u32::from(stroke_width).saturating_mul(3).max(6)).min(length);
    let unit_x = delta_x / length;
    let unit_y = delta_y / length;
    let base_x = end.0 - unit_x * head_length;
    let base_y = end.1 - unit_y * head_length;
    let half_width = head_length / 2.0;
    (
        (base_x - unit_y * half_width, base_y + unit_x * half_width),
        (base_x + unit_y * half_width, base_y - unit_x * half_width),
    )
}

fn pixel_center(x: u32, y: u32) -> (f64, f64) {
    (f64::from(x) + 0.5, f64::from(y) + 0.5)
}

fn stroke_point_center(point: &StrokePoint) -> (f64, f64) {
    pixel_center(point.point.x.get(), point.point.y.get())
}

fn stroke_radius(stroke_width: u16) -> f64 {
    f64::from(stroke_width) / 2.0
}

fn squared_distance(first: (f64, f64), second: (f64, f64)) -> f64 {
    (first.0 - second.0).powi(2) + (first.1 - second.1).powi(2)
}

fn point_segment_distance_squared(
    point: (f64, f64),
    start: (f64, f64),
    end: (f64, f64),
) -> (f64, f64) {
    let delta_x = end.0 - start.0;
    let delta_y = end.1 - start.1;
    let length_squared = delta_x * delta_x + delta_y * delta_y;
    if length_squared == 0.0 {
        return (squared_distance(point, start), 0.0);
    }
    let progress = (((point.0 - start.0) * delta_x + (point.1 - start.1) * delta_y)
        / length_squared)
        .clamp(0.0, 1.0);
    let closest = (start.0 + progress * delta_x, start.1 + progress * delta_y);
    (squared_distance(point, closest), progress)
}

fn point_in_triangle(
    point: (f64, f64),
    first: (f64, f64),
    second: (f64, f64),
    third: (f64, f64),
) -> bool {
    fn sign(point: (f64, f64), first: (f64, f64), second: (f64, f64)) -> f64 {
        (point.0 - second.0) * (first.1 - second.1) - (first.0 - second.0) * (point.1 - second.1)
    }
    let first_sign = sign(point, first, second);
    let second_sign = sign(point, second, third);
    let third_sign = sign(point, third, first);
    let has_negative = first_sign < 0.0 || second_sign < 0.0 || third_sign < 0.0;
    let has_positive = first_sign > 0.0 || second_sign > 0.0 || third_sign > 0.0;
    !(has_negative && has_positive)
}

fn paint_pixel(
    destination: &mut RgbaSurface,
    x: u32,
    y: u32,
    color: Rgba,
    track_opacity: u8,
    blend_mode: impl Into<PaintBlend>,
) {
    let offset = destination.byte_offset(x, y);
    blend_pixel(
        &mut destination.pixels_mut()[offset..offset + 4],
        &[color.red, color.green, color.blue, color.alpha],
        255,
        track_opacity,
        blend_mode,
    );
}

fn blend_pixel(
    destination: &mut [u8],
    source: &[u8],
    item_opacity: u8,
    track_opacity: u8,
    blend_mode: impl Into<PaintBlend>,
) {
    match blend_mode.into() {
        PaintBlend::Legacy(mode) => {
            blend_legacy_pixel(destination, source, item_opacity, track_opacity, mode);
        }
        PaintBlend::WpfSourceOver => {
            let mut source =
                crate::wpf_pixels::premultiply([source[0], source[1], source[2], source[3]]);
            // WPF quantizes each opacity application in premultiplied space;
            // multiplying the alphas first is observably a different operation.
            for opacity in [item_opacity, track_opacity] {
                source = source.map(|channel| crate::wpf_pixels::mul_byte(channel, opacity));
            }
            let output = crate::wpf_pixels::over(
                source,
                [
                    destination[0],
                    destination[1],
                    destination[2],
                    destination[3],
                ],
            );
            destination.copy_from_slice(&output);
        }
    }
}

fn blend_legacy_pixel(
    destination: &mut [u8],
    source: &[u8],
    item_opacity: u8,
    track_opacity: u8,
    blend_mode: BlendMode,
) {
    let opacity_denominator = u64::from(u8::MAX).pow(2);
    let source_alpha = (u64::from(source[3]) * u64::from(item_opacity) * u64::from(track_opacity)
        + opacity_denominator / 2)
        / opacity_denominator;
    if source_alpha == 0 {
        return;
    }
    let destination_alpha = u64::from(destination[3]);
    let inverse_source_alpha = u64::from(u8::MAX) - source_alpha;
    let output_alpha_numerator =
        source_alpha * u64::from(u8::MAX) + destination_alpha * inverse_source_alpha;
    if output_alpha_numerator == 0 {
        destination.copy_from_slice(&[0; 4]);
        return;
    }
    for channel in 0..3 {
        let source_channel = u64::from(source[channel]);
        let destination_channel = u64::from(destination[channel]);
        let blended = u64::from(blend_channel(
            source[channel],
            destination[channel],
            blend_mode,
        ));
        let premultiplied_numerator =
            inverse_source_alpha * destination_channel * destination_alpha
                + (u64::from(u8::MAX) - destination_alpha) * source_channel * source_alpha
                + source_alpha * destination_alpha * blended;
        destination[channel] = u8::try_from(
            (premultiplied_numerator + output_alpha_numerator / 2) / output_alpha_numerator,
        )
        .expect("bounded blend inputs produce an RGBA8 channel");
    }
    destination[3] =
        u8::try_from((output_alpha_numerator + u64::from(u8::MAX) / 2) / u64::from(u8::MAX))
            .expect("source-over alpha remains within RGBA8");
}

fn blend_channel(source: u8, destination: u8, mode: BlendMode) -> u8 {
    match mode {
        BlendMode::Normal => source,
        BlendMode::Multiply => {
            let product = u16::from(source) * u16::from(destination);
            u8::try_from((product + 127) / 255).expect("multiplication blend remains within u8")
        }
        BlendMode::Screen => {
            let inverse_product = u16::from(255 - source) * u16::from(255 - destination);
            255 - u8::try_from((inverse_product + 127) / 255)
                .expect("screen blend remains within u8")
        }
    }
}

fn check_cancelled<C: CancellationToken + ?Sized>(cancellation: &C) -> Result<(), RenderError> {
    if cancellation.is_cancelled() {
        Err(RenderError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use gif_from_screen_domain::{
        CaptureMetadata, ClipTransform, DurationUs, FrameAuthoringSpan, FrameLocalSpan,
        FrameOverlayCell, FrameOverlayMark, OverlayItem, PhysicalPx, TimelineSpan, TrackId,
    };

    use super::*;
    use crate::{AssetProviderError, NeverCancel};

    fn freeze_track(track: &OverlayTrack, owner: FrameId, time: TimeUs) -> OverlayTrack {
        let marks = track
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                freeze_timed_overlay_content(item, time)
                    .unwrap()
                    .map(|content| FrameOverlayMark {
                        id: OverlayId::from_u128(
                            10_000
                                + u128::from_be_bytes(*track.id.as_bytes()) * 100
                                + u128::try_from(index).unwrap(),
                        ),
                        z_index: item.z_index,
                        content,
                    })
            })
            .collect();
        let mut output = track.clone();
        output.items.clear();
        output.frame_cells = Some(vec![FrameOverlayCell {
            stage: None,
            frame_id: owner,
            scopes: Vec::new(),
            marks,
            input_replay: None,
        }]);
        output
    }

    #[test]
    fn frozen_legacy_progress_preserves_pixels_after_clipping_and_all_track_blends() {
        let owner = clip(asset(1));
        let provider = |_id| -> Result<RgbaSurface, AssetProviderError> {
            Ok(surface(10, 2, &[20, 50, 90, 255].repeat(20)))
        };
        for (width, duration, elapsed) in [
            (1, 2, 1),
            (10, 3, 1),
            (17, 7, 6),
            (u32::MAX, u64::MAX, 1),
            (10, 1, 0),
        ] {
            for x in [0, 8, u32::MAX - width] {
                if x.checked_add(width).is_none() {
                    continue;
                }
                for opacity in [0, 87, 255] {
                    for blend in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen] {
                        let item = OverlayItem {
                            id: OverlayId::from_u128(1),
                            span: TimelineSpan {
                                start: TimeUs::ZERO,
                                duration: DurationUs::new(duration).unwrap(),
                            },
                            z_index: -2,
                            content: OverlayContent::Progress {
                                bounds: PhysicalRect::new(x, 0, width, 2).unwrap(),
                                foreground: rgba(200, 30, 40, 133),
                                background: rgba(1, 100, 200, 127),
                                show_frame_number: false,
                                style: None,
                            },
                        };
                        let legacy = track(1, true, opacity, blend, vec![item]);
                        let frozen = freeze_track(&legacy, owner.id, TimeUs::new(elapsed));
                        let expected = CpuRenderer::default()
                            .render_clip_with_overlays(
                                &owner,
                                &[legacy],
                                TimeUs::new(elapsed),
                                &provider,
                                &NeverCancel,
                            )
                            .unwrap();
                        for time in [0, elapsed, u64::MAX] {
                            let actual = CpuRenderer::default()
                                .render_clip_with_overlays(
                                    &owner,
                                    std::slice::from_ref(&frozen),
                                    TimeUs::new(time),
                                    &provider,
                                    &NeverCancel,
                                )
                                .unwrap();
                            assert_eq!(
                                actual, expected,
                                "width={width}, x={x}, opacity={opacity}, blend={blend:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn timed_content_freeze_uses_half_open_points_and_keeps_unsupported_legacy_errors() {
        let mut item = OverlayItem {
            id: OverlayId::from_u128(2),
            z_index: 0,
            span: TimelineSpan {
                start: TimeUs::new(5),
                duration: DurationUs::new(3).unwrap(),
            },
            content: OverlayContent::Progress {
                bounds: PhysicalRect::new(0, 0, 10, 1).unwrap(),
                foreground: rgba(255, 0, 0, 255),
                background: Rgba::TRANSPARENT,
                show_frame_number: false,
                style: None,
            },
        };
        for time in [0, 4, 8, u64::MAX] {
            assert!(
                freeze_timed_overlay_content(&item, TimeUs::new(time))
                    .unwrap()
                    .is_none()
            );
        }
        let Some(OverlayContent::Progress {
            style: Some(style), ..
        }) = freeze_timed_overlay_content(&item, TimeUs::new(6)).unwrap()
        else {
            panic!("frozen progress");
        };
        assert_eq!(style.amount_millionths, 333_333);
        assert!(style.fraction.is_none() && style.label.is_none());
        if let OverlayContent::Progress {
            show_frame_number, ..
        } = &mut item.content
        {
            *show_frame_number = true;
        }
        assert!(matches!(
            freeze_timed_overlay_content(&item, TimeUs::new(6)),
            Err(RenderError::UnsupportedOverlay {
                kind: "progress",
                ..
            })
        ));
        item.span.start = TimeUs::new(u64::MAX);
        assert!(matches!(
            freeze_timed_overlay_content(&item, TimeUs::new(u64::MAX)),
            Err(RenderError::OverlaySpanOverflow { .. })
        ));
    }

    fn exact_styled_progress() -> OverlayItem {
        use gif_from_screen_domain::{ProgressFraction, TextRaster};
        OverlayItem {
            id: OverlayId::from_u128(1),
            z_index: 5,
            span: TimelineSpan {
                start: TimeUs::ZERO,
                duration: DurationUs::new(10).unwrap(),
            },
            content: OverlayContent::Progress {
                bounds: PhysicalRect::new(0, 0, 4, 2).unwrap(),
                foreground: rgba(255, 10, 10, 120),
                background: rgba(0, 0, 0, 80),
                show_frame_number: true,
                style: Some(ProgressStyle {
                    amount_millionths: u32::MAX,
                    fraction: ProgressFraction::new(1, 6),
                    direction: ProgressDirection::RightToLeft,
                    label: Some(TextRaster {
                        asset_id: asset(2),
                        size: PhysicalSize::new(1, 1).unwrap(),
                    }),
                    label_position: point(2, 0),
                    label_text: "keep exactly".to_owned(),
                }),
            },
        }
    }

    #[test]
    fn frozen_existing_progress_keeps_fraction_direction_label_and_stacking_order() {
        let owner = clip(asset(1));
        let progress = exact_styled_progress();
        assert_eq!(
            freeze_timed_overlay_content(&progress, TimeUs::new(1)).unwrap(),
            Some(progress.content.clone())
        );
        let first = track(
            1,
            true,
            100,
            BlendMode::Screen,
            vec![
                progress,
                raster_item(
                    2,
                    asset(2),
                    5,
                    0,
                    10,
                    point(0, 0),
                    PhysicalSize::new(2, 2).unwrap(),
                    127,
                ),
            ],
        );
        let second = track(
            2,
            true,
            230,
            BlendMode::Multiply,
            vec![raster_item(
                3,
                asset(2),
                5,
                0,
                10,
                point(1, 0),
                PhysicalSize::new(2, 2).unwrap(),
                150,
            )],
        );
        let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
            Ok(if id == asset(2) {
                surface(1, 1, &[30, 190, 70, 128])
            } else {
                surface(4, 2, &[10, 20, 90, 255].repeat(8))
            })
        };
        let tracks = [first, second];
        let frozen = tracks
            .iter()
            .map(|track| freeze_track(track, owner.id, TimeUs::new(1)))
            .collect::<Vec<_>>();
        let expected = CpuRenderer::default()
            .render_clip_with_overlays(&owner, &tracks, TimeUs::new(1), &provider, &NeverCancel)
            .unwrap();
        let actual = CpuRenderer::default()
            .render_clip_with_overlays(&owner, &frozen, TimeUs::new(100), &provider, &NeverCancel)
            .unwrap();
        assert_eq!(actual, expected);
        let mut hidden = tracks[0].clone();
        hidden.visible = false;
        hidden.opacity = 0;
        assert_eq!(
            freeze_track(&hidden, owner.id, TimeUs::new(1))
                .frame_cells
                .unwrap()[0]
                .marks
                .len(),
            2
        );
        let transparent = raster_item(
            9,
            asset(2),
            0,
            0,
            10,
            point(0, 0),
            PhysicalSize::new(1, 1).unwrap(),
            0,
        );
        assert_eq!(
            freeze_timed_overlay_content(&transparent, TimeUs::ZERO).unwrap(),
            Some(transparent.content)
        );
    }

    fn asset(number: u8) -> AssetId {
        AssetId::from_digest([number; 32])
    }

    fn point(x: u32, y: u32) -> PhysicalPoint {
        PhysicalPoint {
            x: PhysicalPx::new(x),
            y: PhysicalPx::new(y),
        }
    }

    fn surface(width: u32, height: u32, pixels: &[u8]) -> RgbaSurface {
        RgbaSurface::new(PhysicalSize::new(width, height).unwrap(), pixels.to_vec()).unwrap()
    }

    fn clip(asset_id: AssetId) -> FrameClip {
        FrameClip {
            render_steps: Vec::new(),
            capture_clock: None,
            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
            id: gif_from_screen_domain::FrameId::from_u128(1),
            asset_id,
            duration: DurationUs::new(10).unwrap(),
            transform: ClipTransform::default(),
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn raster_item(
        number: u128,
        asset_id: AssetId,
        z_index: i32,
        start: u64,
        duration: u64,
        position: PhysicalPoint,
        size: PhysicalSize,
        opacity: u8,
    ) -> OverlayItem {
        OverlayItem {
            id: OverlayId::from_u128(number),
            span: TimelineSpan {
                start: TimeUs::new(start),
                duration: DurationUs::new(duration).unwrap(),
            },
            z_index,
            content: OverlayContent::Raster {
                asset_id,
                position,
                size,
                opacity,
            },
        }
    }

    fn track(
        number: u128,
        visible: bool,
        opacity: u8,
        blend_mode: BlendMode,
        items: Vec<OverlayItem>,
    ) -> OverlayTrack {
        OverlayTrack {
            frame_cells: None,
            annotation: None,
            annotation_scope: None,
            id: TrackId::from_u128(number),
            name: format!("track {number}"),
            visible,
            opacity,
            blend_mode,
            items,
        }
    }

    fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Rgba {
        Rgba {
            red,
            green,
            blue,
            alpha,
        }
    }

    fn owned_track(owner: FrameId, number: u128, items: Vec<OverlayItem>) -> OverlayTrack {
        let mut result = track(number, true, 255, BlendMode::Normal, Vec::new());
        result.frame_cells = Some(vec![FrameOverlayCell {
            stage: None,
            input_replay: None,
            frame_id: owner,
            // Two disjoint authoring intervals must still paint each mark only
            // once, including at times outside both intervals.
            scopes: vec![
                FrameAuthoringSpan {
                    run_id: 1,
                    span: FrameLocalSpan::new(2, 3, DurationUs::new(10).unwrap()).unwrap(),
                },
                FrameAuthoringSpan {
                    run_id: 2,
                    span: FrameLocalSpan::new(7, 8, DurationUs::new(10).unwrap()).unwrap(),
                },
            ],
            marks: items
                .into_iter()
                .map(|item| FrameOverlayMark {
                    id: item.id,
                    z_index: item.z_index,
                    content: item.content,
                })
                .collect(),
        }]);
        result
    }

    #[test]
    fn frame_marks_and_timed_items_share_stable_order_without_scope_double_paint() {
        let one = PhysicalSize::new(1, 1).unwrap();
        let owner = clip(asset(1));
        let item =
            |number, source| raster_item(number, asset(source), 0, 0, 10, point(0, 0), one, 255);
        let tracks = vec![
            track(1, true, 255, BlendMode::Normal, vec![item(1, 2)]),
            owned_track(owner.id, 2, vec![item(2, 3), item(3, 4)]),
        ];
        let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
            let color = if id == asset(1) {
                [255, 0, 0, 255]
            } else if id == asset(2) {
                [0, 255, 0, 255]
            } else if id == asset(3) {
                [0, 0, 255, 128]
            } else {
                [255, 0, 0, 128]
            };
            Ok(surface(1, 1, &color))
        };
        let renderer = CpuRenderer::default();
        for time in [0, 2, 5, 7, 9] {
            let time = TimeUs::new(time);
            let direct = renderer
                .render_clip_with_overlays(&owner, &tracks, time, &provider, &NeverCancel)
                .unwrap();
            assert_eq!(direct.pixels(), &[128, 63, 64, 255]);
            let plan = OverlayRenderPlan::for_frame(&tracks, owner.id, time, &NeverCancel).unwrap();
            assert_eq!(
                plan.raster_assets()
                    .map(|asset| asset.overlay_id)
                    .collect::<Vec<_>>(),
                vec![
                    OverlayId::from_u128(1),
                    OverlayId::from_u128(2),
                    OverlayId::from_u128(3)
                ]
            );
            assert_eq!(
                renderer
                    .render_clip_with_overlay_plan(&owner, &plan, &provider, &NeverCancel)
                    .unwrap(),
                direct
            );
        }
    }

    #[test]
    fn frame_identity_not_sample_time_selects_marks_after_reverse_and_retime() {
        let one = PhysicalSize::new(1, 1).unwrap();
        let mut owner = clip(asset(1));
        let tracks = vec![owned_track(
            owner.id,
            1,
            vec![raster_item(1, asset(2), 0, 0, 10, point(0, 0), one, 255)],
        )];
        let other = FrameClip {
            id: FrameId::from_u128(2),
            ..owner.clone()
        };
        let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
            Ok(surface(
                1,
                1,
                if id == asset(1) {
                    &[0, 0, 0, 255]
                } else {
                    &[0, 255, 0, 255]
                },
            ))
        };
        owner.duration = DurationUs::new(1_000_000).unwrap();
        let renderer = CpuRenderer::default();
        for (frame, time, expected) in [
            (&other, 0, [0, 0, 0, 255]),
            (&owner, 10, [0, 255, 0, 255]),
            (&owner, 1_000_001, [0, 255, 0, 255]),
        ] {
            assert_eq!(
                renderer
                    .render_clip_with_overlays(
                        frame,
                        &tracks,
                        TimeUs::new(time),
                        &provider,
                        &NeverCancel
                    )
                    .unwrap()
                    .pixels(),
                expected
            );
        }
    }

    #[test]
    fn legacy_planning_cannot_silently_omit_frame_marks_and_plan_rejects_wrong_owner() {
        let owner = clip(asset(1));
        let one = PhysicalSize::new(1, 1).unwrap();
        let mut tracks = vec![owned_track(
            owner.id,
            1,
            vec![raster_item(1, asset(2), 0, 0, 10, point(0, 0), one, 255)],
        )];
        assert!(matches!(
            active_raster_overlay_assets(&tracks, TimeUs::ZERO, &NeverCancel),
            Err(RenderError::OverlayFrameIdentityRequired)
        ));
        let plan =
            OverlayRenderPlan::for_frame(&tracks, owner.id, TimeUs::ZERO, &NeverCancel).unwrap();
        let never_load = |_id| -> Result<RgbaSurface, AssetProviderError> {
            panic!("wrong-owner plan must not read pixels")
        };
        let other = FrameClip {
            id: FrameId::from_u128(2),
            ..owner.clone()
        };
        assert!(matches!(
            CpuRenderer::default().render_clip_with_overlay_plan(
                &other,
                &plan,
                &never_load,
                &NeverCancel
            ),
            Err(RenderError::OverlayPlanFrameMismatch { .. })
        ));
        assert!(
            active_raster_overlay_assets_for_frame(&tracks, other.id, TimeUs::ZERO, &NeverCancel)
                .unwrap()
                .is_empty()
        );
        tracks[0].visible = false;
        assert!(
            active_raster_overlay_assets(&tracks, TimeUs::ZERO, &NeverCancel)
                .unwrap()
                .is_empty()
        );
        tracks[0].visible = true;
        tracks[0].opacity = 0;
        assert!(
            active_raster_overlay_assets(&tracks, TimeUs::ZERO, &NeverCancel)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            plan.raster_assets().count(),
            1,
            "detached plan retains its source revision"
        );
    }

    #[test]
    fn frame_progress_requires_frozen_style_but_legacy_time_progress_is_unchanged() {
        let owner = clip(asset(1));
        let item = OverlayItem {
            id: OverlayId::from_u128(1),
            z_index: 0,
            span: TimelineSpan {
                start: TimeUs::ZERO,
                duration: DurationUs::new(10).unwrap(),
            },
            content: OverlayContent::Progress {
                bounds: PhysicalRect::new(0, 0, 2, 1).unwrap(),
                foreground: rgba(0, 255, 0, 255),
                background: rgba(0, 0, 0, 255),
                show_frame_number: false,
                style: None,
            },
        };
        let tracks = vec![owned_track(owner.id, 1, vec![item.clone()])];
        assert!(matches!(
            OverlayRenderPlan::for_frame(&tracks, owner.id, TimeUs::new(5), &NeverCancel),
            Err(RenderError::InvalidOverlayGeometry { .. })
        ));
        let legacy = vec![track(1, true, 255, BlendMode::Normal, vec![item])];
        let provider =
            |_id| -> Result<RgbaSurface, AssetProviderError> { Ok(surface(2, 1, &[0; 8])) };
        assert_eq!(
            CpuRenderer::default()
                .render_clip_with_overlays(&owner, &legacy, TimeUs::new(5), &provider, &NeverCancel)
                .unwrap()
                .pixels(),
            &[0, 255, 0, 255, 0, 0, 0, 255]
        );
    }

    #[test]
    fn frame_progress_fraction_and_label_are_frozen_at_every_presentation_time() {
        use gif_from_screen_domain::{
            ProgressDirection, ProgressFraction, ProgressStyle, TextRaster,
        };
        let mut owner = clip(asset(1));
        owner.duration = DurationUs::new(90_000).unwrap();
        let item = OverlayItem {
            id: OverlayId::from_u128(1),
            z_index: 0,
            span: TimelineSpan {
                start: TimeUs::ZERO,
                duration: DurationUs::new(10).unwrap(),
            },
            content: OverlayContent::Progress {
                bounds: PhysicalRect::new(0, 0, 2, 1).unwrap(),
                foreground: rgba(0, 255, 0, 255),
                background: rgba(0, 0, 0, 255),
                show_frame_number: true,
                style: Some(ProgressStyle {
                    amount_millionths: 500_000,
                    fraction: ProgressFraction::new(1, 2),
                    direction: ProgressDirection::LeftToRight,
                    label: Some(TextRaster {
                        asset_id: asset(2),
                        size: PhysicalSize::new(1, 1).unwrap(),
                    }),
                    label_position: point(2, 0),
                    label_text: "2 / 4".to_owned(),
                }),
            },
        };
        let tracks = vec![owned_track(owner.id, 1, vec![item])];
        let provider = |id| -> Result<RgbaSurface, AssetProviderError> {
            Ok(if id == asset(2) {
                surface(1, 1, &[255, 0, 0, 255])
            } else {
                surface(3, 1, &[0, 0, 255, 255].repeat(3))
            })
        };
        for time in [0, 5, 100, 90_000] {
            let plan =
                OverlayRenderPlan::for_frame(&tracks, owner.id, TimeUs::new(time), &NeverCancel)
                    .unwrap();
            assert_eq!(
                plan.raster_assets()
                    .map(|asset| asset.asset_id)
                    .collect::<Vec<_>>(),
                vec![asset(2)]
            );
            assert_eq!(
                CpuRenderer::default()
                    .render_clip_with_overlay_plan(&owner, &plan, &provider, &NeverCancel)
                    .unwrap()
                    .pixels(),
                &[0, 255, 0, 255, 0, 0, 0, 255, 255, 0, 0, 255]
            );
        }
    }

    fn color_mask(surface: &RgbaSurface) -> String {
        let mut mask = String::new();
        for (index, pixel) in surface.pixels().as_chunks::<4>().0.iter().enumerate() {
            if index != 0 && index.is_multiple_of(surface.width() as usize) {
                mask.push('\n');
            }
            mask.push(match pixel {
                [255, 0, 0, 255] => 'R',
                [0, 0, 255, 255] => 'B',
                [255, 255, 255, 255] => '#',
                [0, 0, 0, 255] => '.',
                _ => '?',
            });
        }
        mask
    }

    #[test]
    fn active_assets_use_half_open_time_and_global_stable_layer_order() {
        let size = PhysicalSize::new(1, 1).unwrap();
        let tracks = vec![
            track(
                1,
                true,
                255,
                BlendMode::Normal,
                vec![
                    raster_item(1, asset(1), 5, 10, 10, point(0, 0), size, 255),
                    raster_item(2, asset(2), 0, 10, 10, point(0, 0), size, 255),
                    raster_item(3, asset(3), -5, 10, 10, point(0, 0), size, 0),
                ],
            ),
            track(
                2,
                true,
                255,
                BlendMode::Screen,
                vec![raster_item(4, asset(4), 0, 10, 10, point(0, 0), size, 255)],
            ),
            track(
                3,
                false,
                255,
                BlendMode::Normal,
                vec![raster_item(
                    5,
                    asset(5),
                    -10,
                    10,
                    10,
                    point(0, 0),
                    size,
                    255,
                )],
            ),
        ];

        assert!(
            active_raster_overlay_assets(&tracks, TimeUs::new(9), &NeverCancel)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            active_raster_overlay_assets(&tracks, TimeUs::new(10), &NeverCancel).unwrap(),
            [
                RasterOverlayAsset {
                    overlay_id: OverlayId::from_u128(2),
                    asset_id: asset(2),
                },
                RasterOverlayAsset {
                    overlay_id: OverlayId::from_u128(4),
                    asset_id: asset(4),
                },
                RasterOverlayAsset {
                    overlay_id: OverlayId::from_u128(1),
                    asset_id: asset(1),
                },
            ]
        );
        assert!(
            active_raster_overlay_assets(&tracks, TimeUs::new(20), &NeverCancel)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn frozen_text_loads_its_immutable_pixels_without_resolving_fonts() {
        let size = PhysicalSize::new(1, 1).unwrap();
        let item = OverlayItem {
            id: OverlayId::from_u128(42),
            span: TimelineSpan {
                start: TimeUs::ZERO,
                duration: DurationUs::new(10).unwrap(),
            },
            z_index: 0,
            content: OverlayContent::Text {
                text: "你好".to_owned(),
                font_family: "Font missing on this computer".to_owned(),
                font_size_px: 20,
                position: point(0, 0),
                max_width: Some(size.width),
                foreground: rgba(255, 0, 0, 255),
                background: None,
                alignment: gif_from_screen_domain::HorizontalAlignment::Start,
                raster: Some(gif_from_screen_domain::TextRaster {
                    asset_id: asset(2),
                    size,
                }),
            },
        };
        let tracks = [track(1, true, 255, BlendMode::Normal, vec![item])];
        let provider = |id| -> Result<RgbaSurface, crate::AssetProviderError> {
            Ok(if id == asset(2) {
                surface(1, 1, &[255, 0, 0, 255])
            } else {
                surface(1, 1, &[0, 0, 0, 255])
            })
        };
        let output = CpuRenderer::default()
            .render_clip_with_overlays(
                &clip(asset(1)),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            )
            .unwrap();
        assert_eq!(output.pixels(), [255, 0, 0, 255]);
        assert_eq!(
            active_raster_overlay_assets(&tracks, TimeUs::ZERO, &NeverCancel).unwrap()[0].asset_id,
            asset(2)
        );
    }

    #[test]
    fn active_unsupported_content_is_reported_but_hidden_and_inactive_items_are_safe() {
        let item = OverlayItem {
            id: OverlayId::from_u128(42),
            span: TimelineSpan {
                start: TimeUs::new(10),
                duration: DurationUs::new(10).unwrap(),
            },
            z_index: 0,
            content: OverlayContent::KeyStroke {
                text: "Ctrl+C".to_owned(),
                position: point(0, 0),
                raster: None,
            },
        };
        let mut tracks = vec![track(1, true, 255, BlendMode::Normal, vec![item])];
        assert!(matches!(
            active_raster_overlay_assets(&tracks, TimeUs::new(10), &NeverCancel),
            Err(RenderError::UnsupportedOverlay { overlay_id, kind: "keystroke" })
                if overlay_id == OverlayId::from_u128(42)
        ));
        for sample in [9, 20] {
            assert!(
                active_raster_overlay_assets(&tracks, TimeUs::new(sample), &NeverCancel)
                    .unwrap()
                    .is_empty()
            );
        }
        tracks[0].visible = false;
        assert!(
            active_raster_overlay_assets(&tracks, TimeUs::new(10), &NeverCancel)
                .unwrap()
                .is_empty()
        );
        tracks[0].visible = true;
        tracks[0].opacity = 0;
        assert!(
            active_raster_overlay_assets(&tracks, TimeUs::new(10), &NeverCancel)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn render_resizes_nearest_clips_right_edge_and_preserves_straight_alpha() {
        let base_id = asset(1);
        let overlay_id = asset(2);
        let sources = BTreeMap::from([
            (base_id, surface(4, 1, &[0, 0, 255, 255].repeat(4))),
            (overlay_id, surface(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128])),
        ]);
        let provider = move |asset_id| {
            sources.get(&asset_id).cloned().ok_or_else(|| {
                Box::new(io::Error::new(io::ErrorKind::NotFound, "missing"))
                    as crate::AssetProviderError
            })
        };
        let tracks = [track(
            1,
            true,
            255,
            BlendMode::Normal,
            vec![raster_item(
                1,
                overlay_id,
                0,
                0,
                10,
                point(1, 0),
                PhysicalSize::new(4, 1).unwrap(),
                255,
            )],
        )];

        let rendered = CpuRenderer::new()
            .render_clip_with_raster_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            )
            .unwrap();

        assert_eq!(
            rendered.pixels(),
            [
                0, 0, 255, 255, // untouched
                255, 0, 0, 255, // scaled source x=0
                255, 0, 0, 255, // scaled source x=0
                0, 128, 127, 255, // clipped scaled source x=1, alpha-composited
            ]
        );
    }

    #[test]
    fn z_order_and_off_canvas_layers_control_loading_and_pixels() {
        let base_id = asset(1);
        let low_id = asset(2);
        let high_id = asset(3);
        let sources = BTreeMap::from([
            (base_id, surface(1, 1, &[0, 0, 0, 255])),
            (low_id, surface(1, 1, &[255, 0, 0, 255])),
            (high_id, surface(1, 1, &[0, 255, 0, 255])),
        ]);
        let provider = move |asset_id| {
            sources.get(&asset_id).cloned().ok_or_else(|| {
                Box::new(io::Error::new(io::ErrorKind::NotFound, "must not load"))
                    as crate::AssetProviderError
            })
        };
        let size = PhysicalSize::new(1, 1).unwrap();
        let tracks = [
            track(
                1,
                true,
                255,
                BlendMode::Normal,
                vec![
                    raster_item(1, high_id, 10, 0, 10, point(0, 0), size, 255),
                    raster_item(2, asset(99), 20, 0, 10, point(9, 9), size, 255),
                ],
            ),
            track(
                2,
                true,
                255,
                BlendMode::Normal,
                vec![raster_item(3, low_id, -10, 0, 10, point(0, 0), size, 255)],
            ),
        ];
        let rendered = CpuRenderer::new()
            .render_clip_with_raster_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            )
            .unwrap();
        assert_eq!(rendered.pixels(), [0, 255, 0, 255]);
    }

    #[test]
    fn blend_modes_and_combined_opacity_are_integer_deterministic() {
        let source = [200, 100, 50, 255];
        let original = [100, 150, 200, 255];
        for (mode, expected) in [
            (BlendMode::Normal, [200, 100, 50, 255]),
            (BlendMode::Multiply, [78, 59, 39, 255]),
            (BlendMode::Screen, [222, 191, 211, 255]),
        ] {
            let mut destination = original;
            blend_pixel(&mut destination, &source, 255, 255, mode);
            assert_eq!(destination, expected);
        }

        let mut destination = [0, 0, 0, 255];
        blend_pixel(
            &mut destination,
            &[255, 255, 255, 255],
            128,
            128,
            BlendMode::Normal,
        );
        assert_eq!(destination, [64, 64, 64, 255]);

        let mut transparent = [99, 88, 77, 0];
        blend_pixel(
            &mut transparent,
            &[255, 0, 0, 128],
            255,
            255,
            BlendMode::Multiply,
        );
        assert_eq!(transparent, [255, 0, 0, 128]);
    }

    #[test]
    fn rectangle_ellipse_and_line_have_hard_edged_golden_pixels() {
        let black = [0, 0, 0, 255].repeat(25);
        let mut rectangle = surface(5, 5, &black);
        composite_shape(
            &mut rectangle,
            OverlayId::from_u128(1),
            ShapeKind::Rectangle,
            PhysicalRect::new(1, 1, 3, 3).unwrap(),
            1,
            rgba(255, 0, 0, 255),
            Some(rgba(0, 0, 255, 255)),
            255,
            BlendMode::Normal,
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(color_mask(&rectangle), ".....\n.RRR.\n.RBR.\n.RRR.\n.....");

        let mut ellipse = surface(5, 5, &black);
        composite_shape(
            &mut ellipse,
            OverlayId::from_u128(2),
            ShapeKind::Ellipse,
            PhysicalRect::new(0, 0, 5, 5).unwrap(),
            1,
            rgba(255, 0, 0, 255),
            Some(rgba(0, 0, 255, 255)),
            255,
            BlendMode::Normal,
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(color_mask(&ellipse), ".RRR.\nRBBBR\nRBBBR\nRBBBR\n.RRR.");

        let mut line = surface(5, 5, &black);
        composite_shape(
            &mut line,
            OverlayId::from_u128(3),
            ShapeKind::Line,
            PhysicalRect::new(0, 0, 5, 5).unwrap(),
            1,
            rgba(255, 255, 255, 255),
            None,
            255,
            BlendMode::Normal,
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(color_mask(&line), "#....\n.#...\n..#..\n...#.\n....#");
    }

    #[test]
    fn arrow_and_pressure_drawing_cover_endpoints_without_geometry_allocations() {
        let black = [0, 0, 0, 255].repeat(49);
        let mut arrow = surface(7, 7, &black);
        composite_shape(
            &mut arrow,
            OverlayId::from_u128(1),
            ShapeKind::Arrow,
            PhysicalRect::new(0, 0, 7, 7).unwrap(),
            1,
            rgba(255, 255, 255, 255),
            Some(rgba(0, 0, 255, 255)),
            255,
            BlendMode::Normal,
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(
            color_mask(&arrow),
            "#...#..\n.#.B#..\n..#BB#.\n.BB#B#.\n##BB##.\n..#####\n.....##"
        );

        assert_eq!(pressure_width(4, 0), 0);
        assert_eq!(pressure_width(4, 250), 1);
        assert_eq!(pressure_width(4, 500), 2);
        assert_eq!(pressure_width(4, 1_000), 4);
        let mut drawing = surface(7, 5, &[0, 0, 0, 255].repeat(35));
        composite_drawing(
            &mut drawing,
            OverlayId::from_u128(2),
            &[
                StrokePoint {
                    point: point(1, 2),
                    pressure_milli: 1_000,
                },
                StrokePoint {
                    point: point(5, 2),
                    pressure_milli: 1_000,
                },
            ],
            1,
            rgba(255, 255, 255, 255),
            255,
            BlendMode::Normal,
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(
            color_mask(&drawing),
            ".......\n.......\n.#####.\n.......\n......."
        );
    }

    #[test]
    fn shapes_and_drawings_share_z_track_opacity_and_blend_semantics() {
        let base_id = asset(1);
        let provider = move |asset_id| {
            if asset_id == base_id {
                Ok(surface(1, 1, &[0, 0, 0, 255]))
            } else {
                Err(Box::new(io::Error::new(io::ErrorKind::NotFound, "missing"))
                    as crate::AssetProviderError)
            }
        };
        let span = TimelineSpan {
            start: TimeUs::ZERO,
            duration: DurationUs::new(1).unwrap(),
        };
        let tracks = [track(
            1,
            true,
            128,
            BlendMode::Screen,
            vec![
                OverlayItem {
                    id: OverlayId::from_u128(1),
                    span,
                    z_index: 10,
                    content: OverlayContent::Drawing {
                        points: vec![StrokePoint {
                            point: point(0, 0),
                            pressure_milli: 1_000,
                        }],
                        width: 1,
                        color: rgba(0, 255, 0, 255),
                    },
                },
                OverlayItem {
                    id: OverlayId::from_u128(2),
                    span,
                    z_index: -10,
                    content: OverlayContent::Shape {
                        kind: ShapeKind::Rectangle,
                        bounds: PhysicalRect::new(0, 0, 1, 1).unwrap(),
                        stroke_width: 0,
                        stroke: Rgba::TRANSPARENT,
                        fill: Some(rgba(255, 0, 0, 255)),
                    },
                },
            ],
        )];

        let rendered = CpuRenderer::new()
            .render_clip_with_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            )
            .unwrap();
        assert_eq!(rendered.pixels(), [128, 128, 0, 255]);
    }

    #[test]
    fn invalid_and_extreme_vector_geometry_is_bounded_and_typed() {
        let mut destination = surface(2, 2, &[0, 0, 0, 255].repeat(4));
        let overflowing = PhysicalRect {
            origin: point(u32::MAX, u32::MAX),
            size: PhysicalSize::new(1, 1).unwrap(),
        };
        assert!(matches!(
            composite_shape(
                &mut destination,
                OverlayId::from_u128(1),
                ShapeKind::Rectangle,
                overflowing,
                1,
                rgba(255, 0, 0, 255),
                None,
                255,
                BlendMode::Normal,
                &NeverCancel,
            ),
            Err(RenderError::InvalidOverlayGeometry { .. })
        ));
        assert!(matches!(
            composite_drawing(
                &mut destination,
                OverlayId::from_u128(2),
                &[StrokePoint {
                    point: point(0, 0),
                    pressure_milli: 1_001,
                }],
                1,
                rgba(255, 255, 255, 255),
                255,
                BlendMode::Normal,
                &NeverCancel,
            ),
            Err(RenderError::InvalidDrawingPressure {
                point_index: 0,
                pressure_milli: 1_001,
                maximum: 1_000,
                ..
            })
        ));
        assert!(matches!(
            composite_shape(
                &mut destination,
                OverlayId::from_u128(4),
                ShapeKind::Line,
                PhysicalRect::new(0, 0, 1, 1).unwrap(),
                0,
                rgba(255, 0, 0, 255),
                None,
                255,
                BlendMode::Normal,
                &NeverCancel,
            ),
            Err(RenderError::InvalidOverlayGeometry { .. })
        ));
        assert!(matches!(
            composite_drawing(
                &mut destination,
                OverlayId::from_u128(5),
                &[StrokePoint {
                    point: point(0, 0),
                    pressure_milli: 1_000,
                }],
                0,
                rgba(255, 255, 255, 255),
                255,
                BlendMode::Normal,
                &NeverCancel,
            ),
            Err(RenderError::InvalidOverlayGeometry { .. })
        ));

        composite_drawing(
            &mut destination,
            OverlayId::from_u128(3),
            &[StrokePoint {
                point: point(u32::MAX, u32::MAX),
                pressure_milli: 1_000,
            }],
            u16::MAX,
            rgba(255, 255, 255, 255),
            255,
            BlendMode::Normal,
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(destination.pixels(), [0, 0, 0, 255].repeat(4));
    }

    #[test]
    fn span_provider_and_limit_errors_are_typed() {
        let size = PhysicalSize::new(1, 1).unwrap();
        let invalid_span = [track(
            1,
            true,
            255,
            BlendMode::Normal,
            vec![raster_item(
                9,
                asset(2),
                0,
                u64::MAX,
                1,
                point(0, 0),
                size,
                255,
            )],
        )];
        assert!(matches!(
            active_raster_overlay_assets(&invalid_span, TimeUs::new(u64::MAX), &NeverCancel),
            Err(RenderError::OverlaySpanOverflow { overlay_id })
                if overlay_id == OverlayId::from_u128(9)
        ));

        let base_id = asset(1);
        let missing_id = asset(2);
        let provider = move |asset_id| {
            if asset_id == base_id {
                Ok(surface(1, 1, &[0, 0, 0, 255]))
            } else {
                Err(Box::new(io::Error::new(io::ErrorKind::NotFound, "missing"))
                    as crate::AssetProviderError)
            }
        };
        let tracks = [track(
            1,
            true,
            255,
            BlendMode::Normal,
            vec![raster_item(4, missing_id, 0, 0, 1, point(0, 0), size, 255)],
        )];
        assert!(matches!(
            CpuRenderer::new().render_clip_with_raster_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            ),
            Err(RenderError::OverlayAssetLoad {
                overlay_id,
                asset_id,
                ..
            }) if overlay_id == OverlayId::from_u128(4) && asset_id == missing_id
        ));

        let sources = BTreeMap::from([
            (base_id, surface(1, 1, &[0, 0, 0, 255])),
            (missing_id, surface(2, 1, &[0; 8])),
        ]);
        let provider = move |asset_id| {
            sources.get(&asset_id).cloned().ok_or_else(|| {
                Box::new(io::Error::new(io::ErrorKind::NotFound, "missing"))
                    as crate::AssetProviderError
            })
        };
        assert!(matches!(
            CpuRenderer::with_limits(RenderLimits {
                max_surface_bytes: 4,
            })
            .render_clip_with_raster_overlays(
                &clip(base_id),
                &tracks,
                TimeUs::ZERO,
                &provider,
                &NeverCancel,
            ),
            Err(RenderError::SurfaceLimitExceeded {
                requested: 8,
                limit: 4,
            })
        ));
    }

    #[test]
    fn pixel_loop_observes_cancellation_without_returning_partial_output() {
        struct CancelAfter {
            checks: AtomicUsize,
            cancel_at: usize,
        }
        impl CancellationToken for CancelAfter {
            fn is_cancelled(&self) -> bool {
                self.checks.fetch_add(1, Ordering::Relaxed) >= self.cancel_at
            }
        }

        let size = PhysicalSize::new(2_048, 1).unwrap();
        let mut destination = RgbaSurface::new(size, [0, 0, 0, 255].repeat(2_048)).unwrap();
        let source = RgbaSurface::new(size, [255, 255, 255, 255].repeat(2_048)).unwrap();
        let provider = move |_asset_id| Ok::<_, crate::AssetProviderError>(source.clone());
        let cancellation = CancelAfter {
            checks: AtomicUsize::new(0),
            cancel_at: 2,
        };

        assert!(matches!(
            composite_raster_overlay(
                &mut destination,
                OverlayId::from_u128(1),
                asset(1),
                point(0, 0),
                size,
                255,
                255,
                BlendMode::Normal,
                &provider,
                RenderLimits::default(),
                &cancellation,
            ),
            Err(RenderError::Cancelled)
        ));
        assert_eq!(cancellation.checks.load(Ordering::Relaxed), 3);
        assert_eq!(&destination.pixels()[0..4], &[255, 255, 255, 255]);
        assert_eq!(
            &destination.pixels()[1_500 * 4..1_500 * 4 + 4],
            &[0, 0, 0, 255]
        );
    }

    #[test]
    fn shape_and_drawing_pixel_loops_observe_cancellation() {
        struct CancelAfter {
            checks: AtomicUsize,
            cancel_at: usize,
        }
        impl CancellationToken for CancelAfter {
            fn is_cancelled(&self) -> bool {
                self.checks.fetch_add(1, Ordering::Relaxed) >= self.cancel_at
            }
        }

        let size = PhysicalSize::new(2_048, 1).unwrap();
        let mut shape_surface = RgbaSurface::new(size, [0, 0, 0, 255].repeat(2_048)).unwrap();
        let shape_cancel = CancelAfter {
            checks: AtomicUsize::new(0),
            cancel_at: 2,
        };
        assert!(matches!(
            composite_shape(
                &mut shape_surface,
                OverlayId::from_u128(1),
                ShapeKind::Rectangle,
                PhysicalRect::new(0, 0, 2_048, 1).unwrap(),
                0,
                Rgba::TRANSPARENT,
                Some(rgba(255, 0, 0, 255)),
                255,
                BlendMode::Normal,
                &shape_cancel,
            ),
            Err(RenderError::Cancelled)
        ));

        let mut drawing_surface = RgbaSurface::new(size, [0, 0, 0, 255].repeat(2_048)).unwrap();
        let drawing_cancel = CancelAfter {
            checks: AtomicUsize::new(0),
            cancel_at: 6,
        };
        assert!(matches!(
            composite_drawing(
                &mut drawing_surface,
                OverlayId::from_u128(2),
                &[
                    StrokePoint {
                        point: point(0, 0),
                        pressure_milli: 1_000,
                    },
                    StrokePoint {
                        point: point(2_047, 0),
                        pressure_milli: 1_000,
                    },
                ],
                1,
                rgba(255, 255, 255, 255),
                255,
                BlendMode::Normal,
                &drawing_cancel,
            ),
            Err(RenderError::Cancelled)
        ));
    }
}
