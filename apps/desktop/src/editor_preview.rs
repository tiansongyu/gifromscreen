//! Rendered editor-frame previews with a small revision-aware texture cache.

// The root UI integration lands separately; remove this once the cache is held
// by the editor view model.
#![allow(dead_code)]

use std::{
    collections::{BTreeMap, VecDeque},
    fs, io,
    path::PathBuf,
};

use eframe::egui;
use gif_from_screen_domain::{
    AssetId, AssetKind, FrameId, OverlayId, ProjectId, ProjectRevision, RasterEncoding, TimeUs,
};
use gif_from_screen_project::{ActiveProject, ProjectError};
use gif_from_screen_render::{
    AssetProviderError, CpuRenderer, FrameAssetProvider, NeverCancel, RenderError, RenderLimits,
    RgbaSurface, SurfaceError, active_raster_overlay_assets,
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
    max_size: [u32; 2],
}

#[derive(Clone)]
struct PreparedPreview {
    rendered_size: [u32; 2],
    preview_size: [u32; 2],
    rgba: Vec<u8>,
}

struct CacheEntry<K, V> {
    key: K,
    value: V,
    bytes: usize,
}

struct BoundedLru<K, V> {
    entries: VecDeque<CacheEntry<K, V>>,
    max_entries: usize,
    max_bytes: usize,
    cached_bytes: usize,
}

impl<K: Eq, V> BoundedLru<K, V> {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries,
            max_bytes,
            cached_bytes: 0,
        }
    }

    fn get(&mut self, key: &K) -> Option<&V> {
        let position = self.entries.iter().position(|entry| &entry.key == key)?;
        if position != 0 {
            let entry = self.entries.remove(position)?;
            self.entries.push_front(entry);
        }
        self.entries.front().map(|entry| &entry.value)
    }

    fn insert(&mut self, key: K, value: V, bytes: usize) {
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

    fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
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
        }
    }

    /// Returns a cached texture or strictly loads, renders, downsamples, and uploads it.
    pub(crate) fn preview(
        &mut self,
        project: &ActiveProject,
        frame_id: FrameId,
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
            max_size,
        };
        if let Some(preview) = self.cache.get(&key) {
            return Ok(preview.clone());
        }

        let prepared = prepare_preview(
            project,
            frame_id,
            max_size,
            self.render_surface_limit_bytes,
            self.cache.max_bytes,
        )?;
        let texture_bytes = prepared.rgba.len();
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [
                usize::try_from(prepared.preview_size[0]).map_err(|_| {
                    EditorPreviewError::PreviewByteLengthOverflow {
                        width: prepared.preview_size[0],
                        height: prepared.preview_size[1],
                    }
                })?,
                usize::try_from(prepared.preview_size[1]).map_err(|_| {
                    EditorPreviewError::PreviewByteLengthOverflow {
                        width: prepared.preview_size[0],
                        height: prepared.preview_size[1],
                    }
                })?,
            ],
            &prepared.rgba,
        );
        let texture = context.load_texture(
            format!(
                "editor-preview-{project_id}-{revision}-{frame_id}-{}x{}",
                max_size[0], max_size[1]
            ),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        let preview = EditorPreview {
            texture,
            rendered_size: prepared.rendered_size,
            preview_size: prepared.preview_size,
        };
        self.cache.insert(key, preview.clone(), texture_bytes);
        Ok(preview)
    }
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
    let (clip, sample_time, provider) =
        load_frame_sources(project, frame_id, render_surface_limit_bytes)?;
    let cpu_renderer = CpuRenderer::with_limits(RenderLimits {
        max_surface_bytes: render_surface_limit_bytes,
    });
    cpu_renderer
        .render_clip_with_raster_overlays(
            &clip,
            &project.manifest().timeline.overlay_tracks,
            sample_time,
            &provider,
            &NeverCancel,
        )
        .map_err(|source| EditorPreviewError::Render { frame_id, source })
}

fn load_frame_sources(
    project: &ActiveProject,
    frame_id: FrameId,
    render_surface_limit_bytes: usize,
) -> Result<
    (
        gif_from_screen_domain::FrameClip,
        TimeUs,
        PreviewAssetProvider,
    ),
    EditorPreviewError,
> {
    let clip = project
        .manifest()
        .timeline
        .frames
        .iter()
        .find(|clip| clip.id == frame_id)
        .ok_or(EditorPreviewError::FrameNotFound { frame_id })?
        .clone();
    let sample_time = project
        .manifest()
        .timeline
        .frame_start(frame_id)
        .ok_or(EditorPreviewError::FrameTimeOverflow { frame_id })?;
    let active_overlays = active_raster_overlay_assets(
        &project.manifest().timeline.overlay_tracks,
        sample_time,
        &NeverCancel,
    )
    .map_err(|source| EditorPreviewError::OverlayPlan {
        frame_id,
        time_us: sample_time.get(),
        source,
    })?;
    let mut provider = PreviewAssetProvider {
        assets: BTreeMap::new(),
    };
    let mut retained_bytes = 0_u64;
    load_preview_raster(
        project,
        clip.asset_id,
        PreviewRasterRole::Frame { frame_id },
        render_surface_limit_bytes,
        &mut retained_bytes,
        &mut provider.assets,
    )?;
    for overlay in active_overlays {
        load_preview_raster(
            project,
            overlay.asset_id,
            PreviewRasterRole::Overlay {
                overlay_id: overlay.overlay_id,
            },
            render_surface_limit_bytes,
            &mut retained_bytes,
            &mut provider.assets,
        )?;
    }
    Ok((clip, sample_time, provider))
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
    match (role, kind) {
        (PreviewRasterRole::Frame { .. }, AssetKind::Frame { size, encoding })
        | (
            PreviewRasterRole::Overlay { .. },
            AssetKind::Frame { size, encoding }
            | AssetKind::OverlayImage { size, encoding }
            | AssetKind::Mask { size, encoding },
        ) => Ok((*size, *encoding)),
        (PreviewRasterRole::Frame { .. }, kind) => Err(EditorPreviewError::InvalidAssetKind {
            asset_id,
            kind: kind.clone(),
        }),
        (PreviewRasterRole::Overlay { overlay_id }, kind) => {
            Err(EditorPreviewError::InvalidOverlayAssetKind {
                overlay_id,
                asset_id,
                kind: kind.clone(),
            })
        }
    }
}

fn load_preview_raster(
    project: &ActiveProject,
    asset_id: AssetId,
    role: PreviewRasterRole,
    render_surface_limit_bytes: usize,
    retained_bytes: &mut u64,
    assets: &mut BTreeMap<AssetId, RgbaSurface>,
) -> Result<(), EditorPreviewError> {
    let descriptor = project
        .manifest()
        .assets
        .get(&asset_id)
        .ok_or_else(|| match role {
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

    let asset_path = project.assets().asset_path(asset_id);
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

    let pixels = project
        .assets()
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
        ColorSpace, DurationUs, EditCommand, Effect, FrameClip, OverlayContent, OverlayId,
        OverlayItem, OverlayTrack, PhysicalPoint, PhysicalPx, PhysicalRect, PhysicalSize,
        ProjectManifest, TimelineSpan, TrackId, UnixTimeMs,
    };
    use tempfile::{TempDir, tempdir};

    fn key(project: u128, revision: u64, frame: u128, max_size: [u32; 2]) -> PreviewCacheKey {
        PreviewCacheKey {
            project_id: ProjectId::from_u128(project),
            revision: ProjectRevision::new(revision),
            frame_id: FrameId::from_u128(frame),
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
