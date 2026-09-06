//! Rendered editor-frame previews with a small revision-aware texture cache.

use std::{
    collections::{BTreeMap, VecDeque},
    fs, io,
    path::PathBuf,
};

use eframe::egui;
use gif_from_screen_application::{PresentationTransitionStep, transition_step_progress};
use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, FrameClip, FrameId, OverlayId, ProjectId, ProjectRevision,
    RasterEncoding, TimeUs, Transition, TransitionId,
};
use gif_from_screen_project::{ActiveProject, AssetStore, ProjectError};
use gif_from_screen_render::{
    AssetProviderError, CancellationToken, CpuRenderer, FrameAssetProvider, NeverCancel,
    OverlayRenderPlan, RenderError, RenderLimits, RgbaSurface, SurfaceError, render_transition,
};
use thiserror::Error;

const DEFAULT_CACHE_ENTRIES: usize = 16;
const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RENDER_SURFACE_BYTES: usize = 512 * 1024 * 1024;

/// A texture plus the rendered and downsampled dimensions represented by it.
#[derive(Clone)]
pub(crate) struct EditorPreview {
    pub(crate) texture: egui::TextureHandle,
    pub(crate) rendered_size: [u32; 2],
    pub(crate) preview_size: [u32; 2],
}

/// Typed failures while loading, rendering, resizing, or uploading a preview.
#[derive(Debug, Error)]
pub(crate) enum EditorPreviewError {
    #[error("transition {transition_id} is no longer available for this preview")]
    TransitionNotFound { transition_id: TransitionId },
    #[error("could not render transition {transition_id} step {step}: {source}")]
    TransitionRender {
        transition_id: TransitionId,
        step: u16,
        #[source]
        source: RenderError,
    },
    #[error("preview bounds must be non-zero, got {width}x{height}")]
    InvalidPreviewBounds { width: u32, height: u32 },
    #[error("frame {frame_id} is not present in the active project")]
    FrameNotFound { frame_id: FrameId },
    #[error("project time overflows before frame {frame_id}")]
    FrameTimeOverflow { frame_id: FrameId },
    #[error("frame {frame_id} references missing asset descriptor {asset_id}")]
    MissingAssetDescriptor {
        frame_id: FrameId,
        asset_id: AssetId,
    },
    #[error("raster overlay {overlay_id} references missing asset descriptor {asset_id}")]
    MissingOverlayAssetDescriptor {
        overlay_id: OverlayId,
        asset_id: AssetId,
    },
    #[error("asset map key {asset_id} does not match descriptor id {descriptor_id}")]
    DescriptorIdMismatch {
        asset_id: AssetId,
        descriptor_id: AssetId,
    },
    #[error("asset {asset_id} is not a frame asset: {kind:?}")]
    InvalidAssetKind { asset_id: AssetId, kind: AssetKind },
    #[error("raster overlay {overlay_id} asset {asset_id} is not a raster: {kind:?}")]
    InvalidOverlayAssetKind {
        overlay_id: OverlayId,
        asset_id: AssetId,
        kind: AssetKind,
    },
    #[error("frame asset {asset_id} uses unsupported encoding {encoding:?}; expected raw RGBA8")]
    UnsupportedAssetEncoding {
        asset_id: AssetId,
        encoding: RasterEncoding,
    },
    #[error("raster overlay {overlay_id} asset {asset_id} uses unsupported encoding {encoding:?}")]
    UnsupportedOverlayAssetEncoding {
        overlay_id: OverlayId,
        asset_id: AssetId,
        encoding: RasterEncoding,
    },
    #[error("could not plan raster overlays for frame {frame_id} at {time_us}us: {source}")]
    OverlayPlan {
        frame_id: FrameId,
        time_us: u64,
        #[source]
        source: RenderError,
    },
    #[error("raw RGBA8 byte length overflows for asset {asset_id} at {width}x{height}")]
    SourceByteLengthOverflow {
        asset_id: AssetId,
        width: u32,
        height: u32,
    },
    #[error(
        "asset descriptor {asset_id} declares {actual} bytes, but {width}x{height} RGBA8 requires {expected}"
    )]
    DescriptorByteLengthMismatch {
        asset_id: AssetId,
        width: u32,
        height: u32,
        expected: u64,
        actual: u64,
    },
    #[error("could not inspect frame asset {asset_id} at {}: {source}", path.display())]
    AssetMetadata {
        asset_id: AssetId,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("asset file {asset_id} has {actual} bytes, but its descriptor declares {expected}")]
    AssetFileLengthMismatch {
        asset_id: AssetId,
        expected: u64,
        actual: u64,
    },
    #[error(
        "loading asset {asset_id} would retain {required} bytes, above the {limit}-byte preview source limit"
    )]
    SourceMemoryLimitExceeded {
        asset_id: AssetId,
        required: u64,
        limit: usize,
    },
    #[error("preview source-memory accounting overflowed while adding asset {asset_id}")]
    SourceMemorySizeOverflow { asset_id: AssetId },
    #[error("could not read and verify frame asset {asset_id}: {source}")]
    AssetRead {
        asset_id: AssetId,
        #[source]
        source: ProjectError,
    },
    #[error("frame asset {asset_id} is not a valid RGBA8 surface: {source}")]
    InvalidSurface {
        asset_id: AssetId,
        #[source]
        source: SurfaceError,
    },
    #[error("could not render frame {frame_id}: {source}")]
    Render {
        frame_id: FrameId,
        #[source]
        source: RenderError,
    },
    #[error("preview RGBA byte length overflows for {width}x{height}")]
    PreviewByteLengthOverflow { width: u32, height: u32 },
    #[error("rendered preview dimensions must be non-zero, got {width}x{height}")]
    InvalidRenderedSize { width: u32, height: u32 },
    #[error("resize source has {actual} RGBA bytes, expected {expected}")]
    ResizeSourceLengthMismatch { expected: usize, actual: usize },
    #[error("preview needs {required} bytes, above the {limit}-byte cache limit")]
    PreviewMemoryLimitExceeded { required: usize, limit: usize },
    #[error("could not allocate {requested} bytes for the downsampled preview")]
    PreviewAllocationFailed { requested: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreviewCacheKey {
    project_id: ProjectId,
    revision: ProjectRevision,
    frame_id: FrameId,
    transition: Option<PresentationTransitionStep>,
    max_size: [u32; 2],
}

#[derive(Clone)]
pub(crate) struct PreparedPreview {
    pub(crate) rendered_size: [u32; 2],
    pub(crate) preview_size: [u32; 2],
    pub(crate) rgba: Vec<u8>,
}

struct CacheEntry<K, V> {
    key: K,
    value: V,
    bytes: usize,
}

pub(crate) struct BoundedLru<K, V> {
    entries: VecDeque<CacheEntry<K, V>>,
    max_entries: usize,
    max_bytes: usize,
    cached_bytes: usize,
}

impl<K: Eq, V> BoundedLru<K, V> {
    pub(crate) fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
            max_bytes,
            cached_bytes: 0,
        }
    }

    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        let position = self.entries.iter().position(|entry| &entry.key == key)?;
        if position != 0 {
            let entry = self.entries.remove(position)?;
            self.entries.push_front(entry);
        }
        self.entries.front().map(|entry| &entry.value)
    }

    pub(crate) fn insert(&mut self, key: K, value: V, bytes: usize) {
        if let Some(position) = self.entries.iter().position(|entry| entry.key == key)
            && let Some(replaced) = self.entries.remove(position)
        {
            self.cached_bytes -= replaced.bytes;
        }
        if self.max_entries == 0 || bytes > self.max_bytes {
            return;
        }
        while self.entries.len() >= self.max_entries
            || self.cached_bytes.saturating_add(bytes) > self.max_bytes
        {
            let Some(evicted) = self.entries.pop_back() else {
                break;
            };
            self.cached_bytes -= evicted.bytes;
        }
        self.cached_bytes += bytes;
        self.entries.push_front(CacheEntry { key, value, bytes });
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.entries.retain(|entry| keep(&entry.key));
        self.cached_bytes = self.entries.iter().map(|entry| entry.bytes).sum();
    }

    #[cfg(test)]
    fn contains(&self, key: &K) -> bool {
        self.entries.iter().any(|entry| &entry.key == key)
    }
}

/// Revision-aware, count- and byte-bounded LRU of editor preview textures.
pub(crate) struct EditorPreviewCache {
    cache: BoundedLru<PreviewCacheKey, EditorPreview>,
    render_surface_limit_bytes: usize,
    transition_endpoints: Option<CachedTransitionEndpoints>,
}

impl Default for EditorPreviewCache {
    fn default() -> Self {
        Self::with_limits(
            DEFAULT_CACHE_ENTRIES,
            DEFAULT_CACHE_BYTES,
            MAX_RENDER_SURFACE_BYTES,
        )
    }
}

impl EditorPreviewCache {
    /// Creates the default 16-entry, 64 MiB preview texture cache.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Creates a cache with explicit entry, texture-byte, and render-surface limits.
    pub(crate) fn with_limits(
        max_entries: usize,
        max_cached_bytes: usize,
        render_surface_limit_bytes: usize,
    ) -> Self {
        Self {
            cache: BoundedLru::new(max_entries, max_cached_bytes),
            render_surface_limit_bytes,
            transition_endpoints: None,
        }
    }

    /// Returns a cached texture or strictly loads, renders, downsamples, and uploads it.
    #[cfg(test)]
    pub(crate) fn preview(
        &mut self,
        project: &ActiveProject,
        frame_id: FrameId,
        context: &egui::Context,
        max_size: [u32; 2],
    ) -> Result<EditorPreview, EditorPreviewError> {
        self.presentation_preview(project, frame_id, None, context, max_size)
    }

    /// Renders only the requested original or generated intermediate. Generated
    /// textures share the same bounded LRU as ordinary editing previews.
    pub(crate) fn presentation_preview(
        &mut self,
        project: &ActiveProject,
        frame_id: FrameId,
        transition: Option<PresentationTransitionStep>,
        context: &egui::Context,
        max_size: [u32; 2],
    ) -> Result<EditorPreview, EditorPreviewError> {
        validate_preview_bounds(max_size)?;
        let project_id = project.manifest().project_id;
        let revision = project.manifest().revision;
        invalidate_stale_project_revisions(&mut self.cache, project_id, revision);
        let key = PreviewCacheKey {
            project_id,
            revision,
            frame_id,
            transition,
            max_size,
        };
        if let Some(preview) = self.cache.get(&key) {
            return Ok(preview.clone());
        }

        let prepared = if let Some(step) = transition {
            if !self
                .transition_endpoints
                .as_ref()
                .is_some_and(|endpoints| endpoints.matches(project, step))
            {
                // Drop both previous endpoints before allocating a different pair.
                self.transition_endpoints = None;
                self.transition_endpoints = Some(CachedTransitionEndpoints::new(
                    project,
                    step,
                    self.render_surface_limit_bytes,
                )?);
            }
            let surface = self
                .transition_endpoints
                .as_ref()
                .ok_or(EditorPreviewError::TransitionNotFound {
                    transition_id: step.transition_id,
                })?
                .render(step)?;
            downsample_preview(surface, max_size, self.cache.max_bytes)?
        } else {
            // Ordinary editing previews regain the complete per-surface budget.
            self.transition_endpoints = None;
            prepare_preview(
                project,
                frame_id,
                max_size,
                self.render_surface_limit_bytes,
                self.cache.max_bytes,
            )?
        };
        let texture_bytes = prepared.rgba.len();
        let preview = upload_preview(
            &prepared,
            context,
            format!(
                "editor-preview-{project_id}-{revision}-{frame_id}-{transition:?}-{}x{}",
                max_size[0], max_size[1],
            ),
        )?;
        self.cache.insert(key, preview.clone(), texture_bytes);
        Ok(preview)
    }
}

struct CachedTransitionEndpoints {
    project_id: ProjectId,
    revision: ProjectRevision,
    transition: Transition,
    from: RgbaSurface,
    to: RgbaSurface,
}

impl CachedTransitionEndpoints {
    fn matches(&self, project: &ActiveProject, step: PresentationTransitionStep) -> bool {
        self.project_id == project.manifest().project_id
            && self.revision == project.manifest().revision
            && self.transition.id == step.transition_id
            && self.transition.from_frame == step.from_frame
            && self.transition.to_frame == step.to_frame
    }

    fn new(
        project: &ActiveProject,
        step: PresentationTransitionStep,
        working_limit_bytes: usize,
    ) -> Result<Self, EditorPreviewError> {
        let transition = project
            .manifest()
            .timeline
            .transitions
            .iter()
            .find(|transition| {
                transition.id == step.transition_id
                    && transition.from_frame == step.from_frame
                    && transition.to_frame == step.to_frame
            })
            .ok_or(EditorPreviewError::TransitionNotFound {
                transition_id: step.transition_id,
            })?;
        transition_step_progress(transition, step.step).map_err(|source| {
            EditorPreviewError::TransitionRender {
                transition_id: step.transition_id,
                step: step.step,
                source,
            }
        })?;
        // Two final endpoints and the generated output coexist. Each endpoint
        // provider is dropped before rendering the following surface.
        let endpoint_limit = working_limit_bytes / 3;
        let from = render_frame_surface(project, step.from_frame, endpoint_limit)?;
        let to = render_frame_surface(project, step.to_frame, endpoint_limit)?;
        Ok(Self {
            project_id: project.manifest().project_id,
            revision: project.manifest().revision,
            transition: transition.clone(),
            from,
            to,
        })
    }

    fn render(&self, step: PresentationTransitionStep) -> Result<RgbaSurface, EditorPreviewError> {
        let map_error = |source| EditorPreviewError::TransitionRender {
            transition_id: step.transition_id,
            step: step.step,
            source,
        };
        let progress = transition_step_progress(&self.transition, step.step).map_err(map_error)?;
        render_transition(
            &self.from,
            &self.to,
            &self.transition.kind,
            progress,
            &NeverCancel,
        )
        .map_err(map_error)
    }
}

#[cfg(test)]
fn render_transition_surface(
    project: &ActiveProject,
    step: PresentationTransitionStep,
    working_limit_bytes: usize,
) -> Result<RgbaSurface, EditorPreviewError> {
    CachedTransitionEndpoints::new(project, step, working_limit_bytes)?.render(step)
}

pub(crate) fn upload_preview(
    prepared: &PreparedPreview,
    context: &egui::Context,
    name: String,
) -> Result<EditorPreview, EditorPreviewError> {
    let overflow = || EditorPreviewError::PreviewByteLengthOverflow {
        width: prepared.preview_size[0],
        height: prepared.preview_size[1],
    };
    let size = [
        usize::try_from(prepared.preview_size[0]).map_err(|_| overflow())?,
        usize::try_from(prepared.preview_size[1]).map_err(|_| overflow())?,
    ];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &prepared.rgba);
    Ok(EditorPreview {
        texture: context.load_texture(name, image, egui::TextureOptions::LINEAR),
        rendered_size: prepared.rendered_size,
        preview_size: prepared.preview_size,
    })
}

fn invalidate_stale_project_revisions<V>(
    cache: &mut BoundedLru<PreviewCacheKey, V>,
    project_id: ProjectId,
    revision: ProjectRevision,
) {
    cache.retain(|key| key.project_id != project_id || key.revision == revision);
}

fn prepare_preview(
    project: &ActiveProject,
    frame_id: FrameId,
    max_size: [u32; 2],
    render_surface_limit_bytes: usize,
    preview_limit_bytes: usize,
) -> Result<PreparedPreview, EditorPreviewError> {
    validate_preview_bounds(max_size)?;
    let rendered_surface = render_frame_surface(project, frame_id, render_surface_limit_bytes)?;
    downsample_preview(rendered_surface, max_size, preview_limit_bytes)
}

fn downsample_preview(
    rendered_surface: RgbaSurface,
    max_size: [u32; 2],
    preview_limit_bytes: usize,
) -> Result<PreparedPreview, EditorPreviewError> {
    let rendered_size = [rendered_surface.width(), rendered_surface.height()];
    let preview_size = fit_preview_dimensions(rendered_size, max_size)?;
    let preview_bytes = checked_rgba_byte_len(preview_size)?;
    if preview_bytes > preview_limit_bytes {
        return Err(EditorPreviewError::PreviewMemoryLimitExceeded {
            required: preview_bytes,
            limit: preview_limit_bytes,
        });
    }
    let rgba = if preview_size == rendered_size {
        rendered_surface.into_pixels()
    } else {
        resize_nearest_rgba(rendered_surface.pixels(), rendered_size, preview_size)?
    };
    Ok(PreparedPreview {
        rendered_size,
        preview_size,
        rgba,
    })
}

/// Safely loads and CPU-renders one frame with a strict per-surface memory limit.
///
/// This is the shared final-pixel path used by previews and exact duplicate detection. It verifies
/// descriptor shape, file length, content digest, raw RGBA encoding, aggregate provider limits,
/// and renderer limits before returning the fully transformed/effected/overlaid surface.
pub(crate) fn render_frame_surface(
    project: &ActiveProject,
    frame_id: FrameId,
    render_surface_limit_bytes: usize,
) -> Result<RgbaSurface, EditorPreviewError> {
    let clip = project
        .manifest()
        .timeline
        .frames
        .iter()
        .find(|clip| clip.id == frame_id)
        .ok_or(EditorPreviewError::FrameNotFound { frame_id })?;
    let sample_time = project
        .manifest()
        .timeline
        .frame_start(frame_id)
        .ok_or(EditorPreviewError::FrameTimeOverflow { frame_id })?;
    PreviewRenderPlan::new(project, clip, sample_time)?
        .render(render_surface_limit_bytes, &NeverCancel)
}

/// Immutable metadata for one frame. Creating a plan never reads pixel files
/// and never clones the project timeline, so workers need no project lock.
pub(crate) struct PreviewRenderPlan {
    clip: FrameClip,
    sample_time: TimeUs,
    overlays: OverlayRenderPlan,
    descriptors: BTreeMap<AssetId, AssetDescriptor>,
    store: AssetStore,
}

impl PreviewRenderPlan {
    pub(crate) fn new(
        project: &ActiveProject,
        clip: &FrameClip,
        sample_time: TimeUs,
    ) -> Result<Self, EditorPreviewError> {
        let frame_id = clip.id;
        let overlays = OverlayRenderPlan::for_frame(
            &project.manifest().timeline.overlay_tracks,
            frame_id,
            sample_time,
            &NeverCancel,
        )
        .map_err(|source| EditorPreviewError::OverlayPlan {
            frame_id,
            time_us: sample_time.get(),
            source,
        })?;
        let mut descriptors = BTreeMap::new();
        let descriptor = project.manifest().assets.get(&clip.asset_id).ok_or(
            EditorPreviewError::MissingAssetDescriptor {
                frame_id,
                asset_id: clip.asset_id,
            },
        )?;
        descriptors.insert(clip.asset_id, descriptor.clone());
        for overlay in overlays.raster_assets() {
            let descriptor = project.manifest().assets.get(&overlay.asset_id).ok_or(
                EditorPreviewError::MissingOverlayAssetDescriptor {
                    overlay_id: overlay.overlay_id,
                    asset_id: overlay.asset_id,
                },
            )?;
            descriptors.insert(overlay.asset_id, descriptor.clone());
        }
        Ok(Self {
            clip: clip.clone(),
            sample_time,
            overlays,
            descriptors,
            store: project.assets().clone(),
        })
    }

    pub(crate) fn prepare(
        &self,
        max_size: [u32; 2],
        render_surface_limit_bytes: usize,
        preview_limit_bytes: usize,
        cancellation: &dyn CancellationToken,
    ) -> Result<PreparedPreview, EditorPreviewError> {
        validate_preview_bounds(max_size)?;
        let surface = self.render(render_surface_limit_bytes, cancellation)?;
        downsample_preview(surface, max_size, preview_limit_bytes)
    }

    pub(crate) fn render(
        &self,
        render_surface_limit_bytes: usize,
        cancellation: &dyn CancellationToken,
    ) -> Result<RgbaSurface, EditorPreviewError> {
        let frame_id = self.clip.id;
        if cancellation.is_cancelled() {
            return Err(EditorPreviewError::OverlayPlan {
                frame_id,
                time_us: self.sample_time.get(),
                source: RenderError::Cancelled,
            });
        }
        let mut provider = PreviewAssetProvider {
            assets: BTreeMap::new(),
        };
        let mut retained_bytes = 0_u64;
        load_preview_raster(
            &self.store,
            &self.descriptors,
            self.clip.asset_id,
            PreviewRasterRole::Frame { frame_id },
            render_surface_limit_bytes,
            &mut retained_bytes,
            &mut provider.assets,
        )?;
        for overlay in self.overlays.raster_assets() {
            if cancellation.is_cancelled() {
                return Err(EditorPreviewError::Render {
                    frame_id,
                    source: RenderError::Cancelled,
                });
            }
            load_preview_raster(
                &self.store,
                &self.descriptors,
                overlay.asset_id,
                PreviewRasterRole::Overlay {
                    overlay_id: overlay.overlay_id,
                },
                render_surface_limit_bytes,
                &mut retained_bytes,
                &mut provider.assets,
            )?;
        }
        CpuRenderer::with_limits(RenderLimits {
            max_surface_bytes: render_surface_limit_bytes,
        })
        .render_clip_with_overlay_plan(&self.clip, &self.overlays, &provider, cancellation)
        .map_err(|source| EditorPreviewError::Render { frame_id, source })
    }
}

#[derive(Clone, Copy)]
enum PreviewRasterRole {
    Frame { frame_id: FrameId },
    Overlay { overlay_id: OverlayId },
}

fn preview_raster_shape(
    kind: &AssetKind,
    role: PreviewRasterRole,
    asset_id: AssetId,
) -> Result<(gif_from_screen_domain::PhysicalSize, RasterEncoding), EditorPreviewError> {
    if let Some(descriptor) = kind.raster_descriptor() {
        return Ok(descriptor);
    }
    match role {
        PreviewRasterRole::Frame { .. } => Err(EditorPreviewError::InvalidAssetKind {
            asset_id,
            kind: kind.clone(),
        }),
        PreviewRasterRole::Overlay { overlay_id } => {
            Err(EditorPreviewError::InvalidOverlayAssetKind {
                overlay_id,
                asset_id,
                kind: kind.clone(),
            })
        }
    }
}

fn load_preview_raster(
    store: &AssetStore,
    descriptors: &BTreeMap<AssetId, AssetDescriptor>,
    asset_id: AssetId,
    role: PreviewRasterRole,
    render_surface_limit_bytes: usize,
    retained_bytes: &mut u64,
    assets: &mut BTreeMap<AssetId, RgbaSurface>,
) -> Result<(), EditorPreviewError> {
    let descriptor = descriptors.get(&asset_id).ok_or_else(|| match role {
        PreviewRasterRole::Frame { frame_id } => {
            EditorPreviewError::MissingAssetDescriptor { frame_id, asset_id }
        }
        PreviewRasterRole::Overlay { overlay_id } => {
            EditorPreviewError::MissingOverlayAssetDescriptor {
                overlay_id,
                asset_id,
            }
        }
    })?;
    if descriptor.id != asset_id {
        return Err(EditorPreviewError::DescriptorIdMismatch {
            asset_id,
            descriptor_id: descriptor.id,
        });
    }
    let (source_size, encoding) = preview_raster_shape(&descriptor.kind, role, asset_id)?;
    if encoding != RasterEncoding::Rgba8 {
        return Err(match role {
            PreviewRasterRole::Frame { .. } => {
                EditorPreviewError::UnsupportedAssetEncoding { asset_id, encoding }
            }
            PreviewRasterRole::Overlay { overlay_id } => {
                EditorPreviewError::UnsupportedOverlayAssetEncoding {
                    overlay_id,
                    asset_id,
                    encoding,
                }
            }
        });
    }
    if assets.contains_key(&asset_id) {
        return Ok(());
    }

    let width = source_size.width.get();
    let height = source_size.height.get();
    let expected_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|area| area.checked_mul(4))
        .ok_or(EditorPreviewError::SourceByteLengthOverflow {
            asset_id,
            width,
            height,
        })?;
    if descriptor.byte_len != expected_bytes {
        return Err(EditorPreviewError::DescriptorByteLengthMismatch {
            asset_id,
            width,
            height,
            expected: expected_bytes,
            actual: descriptor.byte_len,
        });
    }

    let asset_path = store.asset_path(asset_id);
    let file_bytes = fs::metadata(&asset_path)
        .map_err(|source| EditorPreviewError::AssetMetadata {
            asset_id,
            path: asset_path,
            source,
        })?
        .len();
    if file_bytes != descriptor.byte_len {
        return Err(EditorPreviewError::AssetFileLengthMismatch {
            asset_id,
            expected: descriptor.byte_len,
            actual: file_bytes,
        });
    }
    let required = retained_bytes
        .checked_add(file_bytes)
        .ok_or(EditorPreviewError::SourceMemorySizeOverflow { asset_id })?;
    if required > u64::try_from(render_surface_limit_bytes).unwrap_or(u64::MAX) {
        return Err(EditorPreviewError::SourceMemoryLimitExceeded {
            asset_id,
            required,
            limit: render_surface_limit_bytes,
        });
    }

    let pixels = store
        .read(asset_id)
        .map_err(|source| EditorPreviewError::AssetRead { asset_id, source })?;
    let source = RgbaSurface::new(source_size, pixels)
        .map_err(|source| EditorPreviewError::InvalidSurface { asset_id, source })?;
    assets.insert(asset_id, source);
    *retained_bytes = required;
    Ok(())
}

#[derive(Debug, Error)]
#[error("renderer requested asset {actual} outside the bounded preview provider")]
struct UnexpectedAsset {
    actual: AssetId,
}

struct PreviewAssetProvider {
    assets: BTreeMap<AssetId, RgbaSurface>,
}

impl FrameAssetProvider for PreviewAssetProvider {
    fn load_rgba8(&self, asset_id: AssetId) -> Result<RgbaSurface, AssetProviderError> {
        self.assets
            .get(&asset_id)
            .cloned()
            .ok_or_else(|| Box::new(UnexpectedAsset { actual: asset_id }) as AssetProviderError)
    }
}

fn validate_preview_bounds(max_size: [u32; 2]) -> Result<(), EditorPreviewError> {
    if max_size[0] == 0 || max_size[1] == 0 {
        Err(EditorPreviewError::InvalidPreviewBounds {
            width: max_size[0],
            height: max_size[1],
        })
    } else {
        Ok(())
    }
}

fn fit_preview_dimensions(
    source: [u32; 2],
    max_size: [u32; 2],
) -> Result<[u32; 2], EditorPreviewError> {
    validate_preview_bounds(max_size)?;
    if source[0] == 0 || source[1] == 0 {
        return Err(EditorPreviewError::InvalidRenderedSize {
            width: source[0],
            height: source[1],
        });
    }
    if source[0] <= max_size[0] && source[1] <= max_size[1] {
        return Ok(source);
    }

    let width_limited = u64::from(max_size[0]) * u64::from(source[1])
        <= u64::from(max_size[1]) * u64::from(source[0]);
    if width_limited {
        let height = (u64::from(source[1]) * u64::from(max_size[0]) + u64::from(source[0]) / 2)
            / u64::from(source[0]);
        Ok([
            max_size[0],
            u32::try_from(height)
                .unwrap_or(u32::MAX)
                .clamp(1, max_size[1]),
        ])
    } else {
        let width = (u64::from(source[0]) * u64::from(max_size[1]) + u64::from(source[1]) / 2)
            / u64::from(source[1]);
        Ok([
            u32::try_from(width)
                .unwrap_or(u32::MAX)
                .clamp(1, max_size[0]),
            max_size[1],
        ])
    }
}

fn checked_rgba_byte_len(size: [u32; 2]) -> Result<usize, EditorPreviewError> {
    let bytes = u64::from(size[0])
        .checked_mul(u64::from(size[1]))
        .and_then(|area| area.checked_mul(4))
        .ok_or(EditorPreviewError::PreviewByteLengthOverflow {
            width: size[0],
            height: size[1],
        })?;
    usize::try_from(bytes).map_err(|_| EditorPreviewError::PreviewByteLengthOverflow {
        width: size[0],
        height: size[1],
    })
}

fn resize_nearest_rgba(
    source: &[u8],
    source_size: [u32; 2],
    target_size: [u32; 2],
) -> Result<Vec<u8>, EditorPreviewError> {
    validate_preview_bounds(target_size)?;
    if source_size[0] == 0 || source_size[1] == 0 {
        return Err(EditorPreviewError::InvalidRenderedSize {
            width: source_size[0],
            height: source_size[1],
        });
    }
    let expected_source = checked_rgba_byte_len(source_size)?;
    if source.len() != expected_source {
        return Err(EditorPreviewError::ResizeSourceLengthMismatch {
            expected: expected_source,
            actual: source.len(),
        });
    }
    let requested = checked_rgba_byte_len(target_size)?;
    let mut target = Vec::new();
    target
        .try_reserve_exact(requested)
        .map_err(|_| EditorPreviewError::PreviewAllocationFailed { requested })?;
    for target_y in 0..target_size[1] {
        let source_y = u64::from(target_y) * u64::from(source_size[1]) / u64::from(target_size[1]);
        for target_x in 0..target_size[0] {
            let source_x =
                u64::from(target_x) * u64::from(source_size[0]) / u64::from(target_size[0]);
            let source_offset = usize::try_from(
                (source_y * u64::from(source_size[0]) + source_x) * 4,
            )
            .map_err(|_| EditorPreviewError::PreviewByteLengthOverflow {
                width: source_size[0],
                height: source_size[1],
            })?;
            target.extend_from_slice(&source[source_offset..source_offset + 4]);
        }
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::{
        AssetDescriptor, BlendMode, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
        ColorSpace, DurationUs, EditCommand, Effect, FrameAuthoringSpan, FrameClip,
        FrameDurationChange, FrameLocalSpan, FrameOverlayCell, FrameOverlayMark, OverlayContent,
        OverlayId, OverlayItem, OverlayTrack, PhysicalPoint, PhysicalPx, PhysicalRect,
        PhysicalSize, ProjectManifest, Rgba, ShapeKind, SlideDirection, StrokePoint, TimelineSpan,
        TrackId, Transition, TransitionKind, UnixTimeMs,
    };
    use tempfile::{TempDir, tempdir};

    fn key(project: u128, revision: u64, frame: u128, max_size: [u32; 2]) -> PreviewCacheKey {
        PreviewCacheKey {
            project_id: ProjectId::from_u128(project),
            revision: ProjectRevision::new(revision),
            frame_id: FrameId::from_u128(frame),
            transition: None,
            max_size,
        }
    }

    fn project_with_frame(
        pixels: &[u8],
        size: PhysicalSize,
        transform: ClipTransform,
        effects: Vec<Effect>,
    ) -> (TempDir, ActiveProject, FrameId, AssetId) {
        let scratch = tempdir().unwrap();
        let canvas_size = transform.output_size.unwrap_or(size);
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "test",
            UnixTimeMs::new(0),
            Canvas {
                size: canvas_size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        let mut project = ActiveProject::create(scratch.path(), manifest).unwrap();
        let asset_id = project.assets().put(pixels).unwrap();
        let frame_id = FrameId::from_u128(1);
        project
            .commit(EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: AssetDescriptor {
                            id: asset_id,
                            byte_len: pixels.len() as u64,
                            kind: AssetKind::Frame {
                                size,
                                encoding: RasterEncoding::Rgba8,
                            },
                        },
                    },
                    EditCommand::InsertFrames {
                        index: 0,
                        frames: vec![FrameClip {
                            render_steps: Vec::new(),
                            capture_clock: None,
                            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
                            id: frame_id,
                            asset_id,
                            duration: DurationUs::new(10_000).unwrap(),
                            transform,
                            capture_metadata: CaptureMetadata::default(),
                            effects,
                        }],
                    },
                ],
            })
            .unwrap();
        (scratch, project, frame_id, asset_id)
    }

    fn add_raster_overlay(
        project: &mut ActiveProject,
        pixels: &[u8],
        size: PhysicalSize,
    ) -> (AssetId, OverlayTrack) {
        let asset_id = project.assets().put(pixels).unwrap();
        let track = OverlayTrack {
            frame_cells: None,
            annotation: None,
            annotation_scope: None,
            id: TrackId::from_u128(1),
            name: "preview watermark".to_owned(),
            visible: true,
            opacity: 255,
            blend_mode: BlendMode::Normal,
            items: vec![OverlayItem {
                id: OverlayId::from_u128(1),
                span: TimelineSpan {
                    start: TimeUs::ZERO,
                    duration: DurationUs::new(10_000).unwrap(),
                },
                z_index: 0,
                content: OverlayContent::Raster {
                    asset_id,
                    position: PhysicalPoint {
                        x: PhysicalPx::new(1),
                        y: PhysicalPx::ZERO,
                    },
                    size,
                    opacity: 255,
                },
            }],
        };
        project
            .commit(EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: AssetDescriptor {
                            id: asset_id,
                            byte_len: u64::try_from(pixels.len()).unwrap(),
                            kind: AssetKind::OverlayImage {
                                size,
                                encoding: RasterEncoding::Rgba8,
                            },
                        },
                    },
                    EditCommand::UpsertOverlayTrack {
                        track: track.clone(),
                    },
                ],
            })
            .unwrap();
        (asset_id, track)
    }

    #[test]
    fn fits_dimensions_without_upscaling() {
        assert_eq!(
            fit_preview_dimensions([1_920, 1_080], [640, 640]).unwrap(),
            [640, 360]
        );
        assert_eq!(
            fit_preview_dimensions([1_080, 1_920], [640, 480]).unwrap(),
            [270, 480]
        );
        assert_eq!(
            fit_preview_dimensions([320, 200], [640, 480]).unwrap(),
            [320, 200]
        );
        assert!(matches!(
            fit_preview_dimensions([320, 200], [0, 480]),
            Err(EditorPreviewError::InvalidPreviewBounds { .. })
        ));
        assert!(matches!(
            fit_preview_dimensions([0, 200], [320, 200]),
            Err(EditorPreviewError::InvalidRenderedSize { .. })
        ));
    }

    fn transition_project(
        kind: TransitionKind,
    ) -> (TempDir, ActiveProject, PresentationTransitionStep) {
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let green = [0, 255, 0, 255];
        let size = PhysicalSize::new(2, 1).unwrap();
        let (scratch, mut project, first, _) =
            project_with_frame(&red.repeat(2), size, ClipTransform::default(), Vec::new());
        let pixels = blue.repeat(2);
        let asset_id = project.assets().put(&pixels).unwrap();
        let second = FrameId::from_u128(2);
        project
            .commit(EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: AssetDescriptor {
                            id: asset_id,
                            byte_len: 8,
                            kind: AssetKind::Frame {
                                size,
                                encoding: RasterEncoding::Rgba8,
                            },
                        },
                    },
                    EditCommand::InsertFrames {
                        index: 1,
                        frames: vec![FrameClip {
                            render_steps: Vec::new(),
                            capture_clock: None,
                            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
                            id: second,
                            asset_id,
                            duration: DurationUs::new(30_000).unwrap(),
                            transform: ClipTransform::default(),
                            capture_metadata: CaptureMetadata::default(),
                            effects: Vec::new(),
                        }],
                    },
                ],
            })
            .unwrap();
        // Active only on the outgoing original, never on the incoming endpoint.
        add_raster_overlay(&mut project, &green, PhysicalSize::new(1, 1).unwrap());
        let transition_id = TransitionId::from_u128(1);
        project
            .commit(EditCommand::SetTransitions {
                transitions: vec![Transition {
                    id: transition_id,
                    from_frame: first,
                    to_frame: second,
                    duration: DurationUs::new(20_000).unwrap(),
                    steps: 1,
                    kind,
                }],
            })
            .unwrap();
        (
            scratch,
            project,
            PresentationTransitionStep {
                transition_id,
                from_frame: first,
                to_frame: second,
                step: 1,
            },
        )
    }

    #[test]
    fn transition_preview_uses_final_endpoint_overlays_at_original_frame_times() {
        for (kind, pixels) in [
            (
                TransitionKind::FadeToNext,
                vec![128, 0, 128, 255, 0, 128, 128, 255],
            ),
            (
                TransitionKind::Slide {
                    direction: SlideDirection::Left,
                },
                vec![0, 255, 0, 255, 0, 0, 255, 255],
            ),
            (
                TransitionKind::FadeToColor {
                    color: Rgba {
                        red: 17,
                        green: 23,
                        blue: 42,
                        alpha: 255,
                    },
                },
                vec![17, 23, 42, 255, 17, 23, 42, 255],
            ),
        ] {
            let (_scratch, project, step) = transition_project(kind);
            let preview = render_transition_surface(&project, step, 1024).unwrap();
            assert_eq!(preview.pixels(), pixels);
        }
    }

    fn add_incoming_frame_mark(project: &mut ActiveProject) {
        let mut track = project.manifest().timeline.overlay_tracks[0].clone();
        track.id = TrackId::from_u128(2);
        track.items.clear();
        track.frame_cells = Some(vec![FrameOverlayCell {
            stage: None,
            input_replay: None,
            frame_id: FrameId::from_u128(2),
            scopes: vec![FrameAuthoringSpan {
                run_id: 1,
                span: FrameLocalSpan::new(15_000, 20_000, DurationUs::new(30_000).unwrap())
                    .unwrap(),
            }],
            marks: vec![FrameOverlayMark {
                id: OverlayId::from_u128(2),
                z_index: 0,
                content: OverlayContent::Raster {
                    asset_id: project.manifest().timeline.frames[0].asset_id,
                    position: PhysicalPoint::default(),
                    size: PhysicalSize::new(1, 1).unwrap(),
                    opacity: 255,
                },
            }],
        }]);
        project
            .commit(EditCommand::UpsertOverlayTrack { track })
            .unwrap();
    }

    #[test]
    fn frame_owned_preview_and_gif_share_whole_frame_transition_endpoints() {
        let (directory, mut project, step) = transition_project(TransitionKind::FadeToNext);
        add_incoming_frame_mark(&mut project);
        let first = render_frame_surface(&project, step.from_frame, 1024).unwrap();
        let second = render_frame_surface(&project, step.to_frame, 1024).unwrap();
        let transition = render_transition_surface(&project, step, 1024).unwrap();
        assert_eq!(first.pixels(), &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(second.pixels(), &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert_eq!(transition.pixels(), &[255, 0, 0, 255, 0, 128, 128, 255]);
        let output = directory.path().join("frame-owned.gif");
        gif_from_screen_application::export_project_snapshot_to_gif(
            &gif_from_screen_application::ProjectExportSnapshot::from_active(&project),
            &output,
            &gif_from_screen_application::ProjectGifExportOptions::default(),
            &gif_from_screen_gif::NeverCancel,
            &mut gif_from_screen_application::NoopProjectExportProgress,
        )
        .unwrap();
        let decoded = gif_from_screen_media::decode_gif(
            fs::File::open(output).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!(decoded.frames().len(), 3);
        for (frame, expected) in decoded.frames().iter().zip([first, transition, second]) {
            assert_eq!(frame.rgba(), expected.pixels());
        }
    }

    #[test]
    fn frame_owned_detached_preview_survives_reverse_retime_and_reopen() {
        let (directory, mut project, step) = transition_project(TransitionKind::FadeToNext);
        add_incoming_frame_mark(&mut project);
        let plan = PreviewRenderPlan::new(
            &project,
            &project.manifest().timeline.frames[1],
            TimeUs::new(10_000),
        )
        .unwrap();
        let frozen = plan.render(1024, &NeverCancel).unwrap();
        let original_cells = project.manifest().timeline.overlay_tracks[1]
            .frame_cells
            .clone();
        project
            .commit(EditCommand::SetTransitions {
                transitions: Vec::new(),
            })
            .unwrap();
        project
            .commit(EditCommand::ReorderFrames {
                order: vec![step.to_frame, step.from_frame],
            })
            .unwrap();
        project
            .commit(EditCommand::SetFrameDurations {
                changes: vec![FrameDurationChange {
                    frame_id: step.to_frame,
                    duration: DurationUs::new(90_000).unwrap(),
                }],
            })
            .unwrap();
        assert_eq!(
            original_cells,
            project.manifest().timeline.overlay_tracks[1].frame_cells
        );
        // The old timed green mark remains at time zero; the red frame-owned
        // mark follows frame 2. The detached earlier revision stays red/blue.
        assert_eq!(
            render_frame_surface(&project, step.to_frame, 1024)
                .unwrap()
                .pixels(),
            &[255, 0, 0, 255, 0, 255, 0, 255]
        );
        assert_eq!(plan.render(1024, &NeverCancel).unwrap(), frozen);
        let expected = render_frame_surface(&project, step.to_frame, 1024).unwrap();
        drop(project);
        let reopened = ActiveProject::open(
            directory.path(),
            gif_from_screen_project::LockPolicy::FailIfPresent,
        )
        .unwrap();
        assert_eq!(
            render_frame_surface(&reopened.project, step.to_frame, 1024).unwrap(),
            expected
        );
    }

    #[test]
    fn transition_preview_does_not_reuse_original_texture_and_obeys_working_limit() {
        let (_scratch, project, step) = transition_project(TransitionKind::FadeToNext);
        let context = egui::Context::default();
        let mut cache = EditorPreviewCache::with_limits(2, 1024, 1024);
        let original = cache
            .preview(&project, step.from_frame, &context, [2, 1])
            .unwrap();
        let intermediate = cache
            .presentation_preview(&project, step.from_frame, Some(step), &context, [2, 1])
            .unwrap();
        assert_ne!(original.texture.id(), intermediate.texture.id());
        let repeated = cache
            .presentation_preview(&project, step.from_frame, Some(step), &context, [2, 1])
            .unwrap();
        assert_eq!(repeated.texture.id(), intermediate.texture.id());
        assert!(render_transition_surface(&project, step, 8).is_err());
        assert!(matches!(
            render_transition_surface(
                &project,
                PresentationTransitionStep { step: 0, ..step },
                1024
            ),
            Err(EditorPreviewError::TransitionRender { .. })
        ));
        assert!(matches!(
            render_transition_surface(
                &project,
                PresentationTransitionStep {
                    transition_id: TransitionId::from_u128(2),
                    ..step
                },
                1024
            ),
            Err(EditorPreviewError::TransitionNotFound { .. })
        ));
    }

    #[test]
    fn transition_endpoint_cache_reuses_pixels_across_steps_and_invalidates_revisions() {
        let (_scratch, mut project, step) = transition_project(TransitionKind::FadeToNext);
        let mut transition = project.manifest().timeline.transitions[0].clone();
        transition.steps = 2;
        project
            .commit(EditCommand::SetTransitions {
                transitions: vec![transition],
            })
            .unwrap();
        let context = egui::Context::default();
        let mut cache = EditorPreviewCache::with_limits(2, 1024, 1024);
        cache
            .presentation_preview(&project, step.from_frame, Some(step), &context, [2, 1])
            .unwrap();
        let asset = project.manifest().timeline.frames[0].asset_id;
        fs::remove_file(project.assets().asset_path(asset)).unwrap();
        // A different intermediate is not texture-cached, but its two endpoint
        // surfaces are. No file access or repeated overlay rendering is needed.
        cache
            .presentation_preview(
                &project,
                step.from_frame,
                Some(PresentationTransitionStep { step: 2, ..step }),
                &context,
                [2, 1],
            )
            .unwrap();
        project
            .commit(EditCommand::SetTransitions {
                transitions: Vec::new(),
            })
            .unwrap();
        assert!(matches!(
            cache.presentation_preview(&project, step.from_frame, Some(step), &context, [2, 1]),
            Err(EditorPreviewError::TransitionNotFound { .. })
        ));
        assert!(cache.transition_endpoints.is_none());
    }

    #[test]
    fn nearest_resize_preserves_straight_rgba_channels() {
        let source = [
            1, 2, 3, 0, 10, 20, 30, 64, 40, 50, 60, 128, 70, 80, 90, 255, 4, 5, 6, 7, 11, 21, 31,
            65, 41, 51, 61, 129, 71, 81, 91, 254,
        ];
        let resized = resize_nearest_rgba(&source, [4, 2], [2, 1]).unwrap();
        assert_eq!(resized, [1, 2, 3, 0, 40, 50, 60, 128]);
        assert!(matches!(
            resize_nearest_rgba(&source[..4], [4, 2], [2, 1]),
            Err(EditorPreviewError::ResizeSourceLengthMismatch { .. })
        ));
    }

    #[test]
    fn detached_plan_matches_final_preview_and_preserves_its_revision() {
        let (_directory, mut project, frame_id, _) = project_with_frame(
            &[10, 20, 30, 255, 40, 50, 60, 255],
            PhysicalSize::new(2, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        add_raster_overlay(
            &mut project,
            &[255, 0, 0, 128],
            PhysicalSize::new(1, 1).unwrap(),
        );
        let plan = PreviewRenderPlan::new(
            &project,
            &project.manifest().timeline.frames[0],
            TimeUs::ZERO,
        )
        .unwrap();
        let expected = prepare_preview(&project, frame_id, [2, 1], 1024, 1024).unwrap();
        let original = plan.prepare([2, 1], 1024, 1024, &NeverCancel).unwrap();
        assert_eq!(original.rgba, expected.rgba);
        project
            .commit(EditCommand::RemoveOverlayTrack {
                track_id: TrackId::from_u128(1),
            })
            .unwrap();
        let unchanged = plan.prepare([2, 1], 1024, 1024, &NeverCancel).unwrap();
        let edited = prepare_preview(&project, frame_id, [2, 1], 1024, 1024).unwrap();
        assert_eq!(unchanged.rgba, original.rgba);
        assert_ne!(unchanged.rgba, edited.rgba);
    }

    #[test]
    fn planning_never_reads_pixels_and_cancelled_render_never_loads_missing_assets() {
        struct Cancelled;
        impl CancellationToken for Cancelled {
            fn is_cancelled(&self) -> bool {
                true
            }
        }
        let (_directory, project, frame_id, asset_id) = project_with_frame(
            &[1, 2, 3, 255],
            PhysicalSize::new(1, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        fs::remove_file(project.assets().asset_path(asset_id)).unwrap();
        let plan = PreviewRenderPlan::new(
            &project,
            &project.manifest().timeline.frames[0],
            TimeUs::ZERO,
        )
        .unwrap();
        assert!(matches!(
            plan.prepare([10, 10], 1024, 1024, &Cancelled),
            Err(EditorPreviewError::OverlayPlan {
                source: RenderError::Cancelled,
                ..
            })
        ));
        assert!(matches!(
            render_frame_surface(&project, frame_id, 1024),
            Err(EditorPreviewError::AssetMetadata { .. })
        ));
    }

    #[test]
    fn cache_key_and_lru_include_revision_frame_and_bounds() {
        let first = key(1, 1, 1, [320, 200]);
        assert_ne!(first, key(1, 2, 1, [320, 200]));
        assert_ne!(first, key(1, 1, 2, [320, 200]));
        assert_ne!(first, key(1, 1, 1, [640, 400]));

        let second = key(1, 1, 2, [320, 200]);
        let third = key(1, 1, 3, [320, 200]);
        let mut cache = BoundedLru::new(2, 8);
        cache.insert(first, "first", 4);
        cache.insert(second, "second", 4);
        assert_eq!(cache.get(&first), Some(&"first"));
        cache.insert(third, "third", 4);
        assert!(cache.contains(&first));
        assert!(!cache.contains(&second));
        assert!(cache.contains(&third));
        assert_eq!(cache.cached_bytes, 8);

        let mut byte_limited = BoundedLru::new(4, 6);
        byte_limited.insert(first, "first", 4);
        byte_limited.insert(second, "second", 4);
        assert!(!byte_limited.contains(&first));
        assert!(byte_limited.contains(&second));
        assert_eq!(byte_limited.cached_bytes, 4);
    }

    #[test]
    fn revision_change_removes_only_stale_entries_for_that_project() {
        let stale = key(1, 1, 1, [320, 200]);
        let current = key(1, 2, 1, [320, 200]);
        let other_project = key(2, 1, 1, [320, 200]);
        let mut cache = BoundedLru::new(16, 64);
        cache.insert(stale, 1, 4);
        cache.insert(current, 2, 4);
        cache.insert(other_project, 3, 4);

        invalidate_stale_project_revisions(
            &mut cache,
            ProjectId::from_u128(1),
            ProjectRevision::new(2),
        );
        assert!(!cache.contains(&stale));
        assert!(cache.contains(&current));
        assert!(cache.contains(&other_project));
        assert_eq!(cache.cached_bytes, 8);
    }

    #[test]
    fn preparation_applies_transform_effects_and_preserves_alpha() {
        let size = PhysicalSize::new(2, 1).unwrap();
        let transform = ClipTransform {
            output_size: Some(PhysicalSize::new(4, 2).unwrap()),
            flip_horizontal: true,
            ..ClipTransform::default()
        };
        let effects = vec![Effect::Darken {
            region: PhysicalRect::new(0, 0, 4, 2).unwrap(),
            amount_percent: 50,
        }];
        let (_scratch, project, frame_id, _) = project_with_frame(
            &[200, 100, 50, 255, 20, 40, 60, 64],
            size,
            transform,
            effects,
        );

        let prepared = prepare_preview(
            &project,
            frame_id,
            [4, 2],
            MAX_RENDER_SURFACE_BYTES,
            DEFAULT_CACHE_BYTES,
        )
        .unwrap();
        assert_eq!(prepared.rendered_size, [4, 2]);
        assert_eq!(prepared.preview_size, [4, 2]);
        assert_eq!(
            prepared.rgba,
            [
                10, 20, 30, 64, 10, 20, 30, 64, 100, 50, 25, 255, 100, 50, 25, 255, 10, 20, 30, 64,
                10, 20, 30, 64, 100, 50, 25, 255, 100, 50, 25, 255,
            ]
        );
    }

    #[test]
    fn preview_uses_shared_raster_compositor_and_bounded_multi_asset_provider() {
        let base_size = PhysicalSize::new(2, 1).unwrap();
        let (_scratch, mut project, frame_id, _) = project_with_frame(
            &[0, 0, 255, 255, 0, 0, 255, 255],
            base_size,
            ClipTransform::default(),
            Vec::new(),
        );
        let (overlay_asset, mut track) = add_raster_overlay(
            &mut project,
            &[255, 0, 0, 255],
            PhysicalSize::new(1, 1).unwrap(),
        );

        assert!(matches!(
            render_frame_surface(&project, frame_id, 11),
            Err(EditorPreviewError::SourceMemoryLimitExceeded {
                asset_id,
                required: 12,
                limit: 11,
            }) if asset_id == overlay_asset
        ));
        let rendered = render_frame_surface(&project, frame_id, 12).unwrap();
        assert_eq!(rendered.pixels(), [0, 0, 255, 255, 255, 0, 0, 255]);

        track.visible = false;
        project
            .commit(EditCommand::UpsertOverlayTrack { track })
            .unwrap();
        fs::remove_file(project.assets().asset_path(overlay_asset)).unwrap();
        let hidden = render_frame_surface(&project, frame_id, 8).unwrap();
        assert_eq!(hidden.pixels(), [0, 0, 255, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn preview_overlay_can_reuse_the_identical_content_addressed_frame_asset() {
        let size = PhysicalSize::new(2, 1).unwrap();
        let (_scratch, mut project, frame_id, frame_asset) = project_with_frame(
            &[255, 0, 0, 255, 0, 255, 0, 255],
            size,
            ClipTransform::default(),
            Vec::new(),
        );
        let track = OverlayTrack {
            frame_cells: None,
            annotation: None,
            annotation_scope: None,
            id: TrackId::from_u128(1),
            name: "shared asset watermark".to_owned(),
            visible: true,
            opacity: 255,
            blend_mode: BlendMode::Normal,
            items: vec![OverlayItem {
                id: OverlayId::from_u128(1),
                span: TimelineSpan {
                    start: TimeUs::ZERO,
                    duration: DurationUs::new(10_000).unwrap(),
                },
                z_index: 0,
                content: OverlayContent::Raster {
                    asset_id: frame_asset,
                    position: PhysicalPoint {
                        x: PhysicalPx::new(1),
                        y: PhysicalPx::ZERO,
                    },
                    size: PhysicalSize::new(1, 1).unwrap(),
                    opacity: 255,
                },
            }],
        };
        project
            .commit(EditCommand::UpsertOverlayTrack { track })
            .unwrap();

        let rendered = render_frame_surface(&project, frame_id, 16).unwrap();
        assert_eq!(rendered.pixels(), [255, 0, 0, 255, 255, 0, 0, 255]);
        let mut plan = PreviewRenderPlan::new(
            &project,
            &project.manifest().timeline.frames[0],
            TimeUs::ZERO,
        )
        .unwrap();
        for kind in [
            AssetKind::OverlayImage {
                size,
                encoding: RasterEncoding::Rgba8,
            },
            AssetKind::Mask {
                size,
                encoding: RasterEncoding::Rgba8,
            },
        ] {
            plan.descriptors.get_mut(&frame_asset).unwrap().kind = kind;
            assert_eq!(
                plan.render(16, &NeverCancel).unwrap().pixels(),
                rendered.pixels()
            );
        }
    }

    #[test]
    fn preview_raster_role_reuse_preserves_kind_encoding_and_byte_validation() {
        let size = PhysicalSize::new(1, 1).unwrap();
        let (_scratch, project, _, asset_id) = project_with_frame(
            &[12, 34, 56, 255],
            size,
            ClipTransform::default(),
            Vec::new(),
        );
        let mut plan = PreviewRenderPlan::new(
            &project,
            &project.manifest().timeline.frames[0],
            TimeUs::ZERO,
        )
        .unwrap();
        for media_type in ["image/png", "video/mp4", "audio/wav", "font/ttf"] {
            plan.descriptors.get_mut(&asset_id).unwrap().kind = AssetKind::ImportedSource {
                media_type: media_type.into(),
            };
            assert!(matches!(
                plan.render(16, &NeverCancel),
                Err(EditorPreviewError::InvalidAssetKind { .. })
            ));
        }
        plan.descriptors.get_mut(&asset_id).unwrap().kind = AssetKind::OverlayImage {
            size,
            encoding: RasterEncoding::Qoi,
        };
        assert!(matches!(
            plan.render(16, &NeverCancel),
            Err(EditorPreviewError::UnsupportedAssetEncoding { .. })
        ));
        plan.descriptors.get_mut(&asset_id).unwrap().kind = AssetKind::Mask {
            size: PhysicalSize::new(2, 1).unwrap(),
            encoding: RasterEncoding::Rgba8,
        };
        assert!(matches!(
            plan.render(16, &NeverCancel),
            Err(EditorPreviewError::DescriptorByteLengthMismatch { .. })
        ));
    }

    #[test]
    fn shape_and_drawing_overlays_flow_through_editor_preview() {
        let size = PhysicalSize::new(3, 1).unwrap();
        let (_scratch, mut project, frame_id, _) = project_with_frame(
            &[0, 0, 0, 255].repeat(3),
            size,
            ClipTransform::default(),
            Vec::new(),
        );
        let span = TimelineSpan {
            start: TimeUs::ZERO,
            duration: DurationUs::new(10_000).unwrap(),
        };
        project
            .commit(EditCommand::UpsertOverlayTrack {
                track: OverlayTrack {
                    frame_cells: None,
                    annotation: None,
                    annotation_scope: None,
                    id: TrackId::from_u128(1),
                    name: "vector preview".to_owned(),
                    visible: true,
                    opacity: 255,
                    blend_mode: BlendMode::Normal,
                    items: vec![
                        OverlayItem {
                            id: OverlayId::from_u128(1),
                            span,
                            z_index: 0,
                            content: OverlayContent::Shape {
                                kind: ShapeKind::Rectangle,
                                bounds: PhysicalRect::new(0, 0, 3, 1).unwrap(),
                                stroke_width: 0,
                                stroke: Rgba::TRANSPARENT,
                                fill: Some(Rgba {
                                    red: 255,
                                    green: 0,
                                    blue: 0,
                                    alpha: 255,
                                }),
                            },
                        },
                        OverlayItem {
                            id: OverlayId::from_u128(2),
                            span,
                            z_index: 1,
                            content: OverlayContent::Drawing {
                                points: vec![StrokePoint {
                                    point: PhysicalPoint {
                                        x: PhysicalPx::new(1),
                                        y: PhysicalPx::ZERO,
                                    },
                                    pressure_milli: 1_000,
                                }],
                                width: 1,
                                color: Rgba {
                                    red: 0,
                                    green: 255,
                                    blue: 0,
                                    alpha: 255,
                                },
                            },
                        },
                    ],
                },
            })
            .unwrap();

        let rendered = render_frame_surface(&project, frame_id, 12).unwrap();
        assert_eq!(
            rendered.pixels(),
            [255, 0, 0, 255, 0, 255, 0, 255, 255, 0, 0, 255]
        );
    }

    #[test]
    fn corrupted_asset_digest_is_rejected_before_rendering() {
        let pixels = vec![255, 0, 0, 255, 0, 0, 255, 64];
        let (_scratch, project, frame_id, asset_id) = project_with_frame(
            &pixels,
            PhysicalSize::new(2, 1).unwrap(),
            ClipTransform::default(),
            Vec::new(),
        );
        fs::write(project.assets().asset_path(asset_id), [0_u8; 8]).unwrap();

        assert!(matches!(
            prepare_preview(
                &project,
                frame_id,
                [320, 200],
                MAX_RENDER_SURFACE_BYTES,
                DEFAULT_CACHE_BYTES,
            ),
            Err(EditorPreviewError::AssetRead {
                source: ProjectError::CorruptAsset { .. },
                ..
            })
        ));
    }
}
