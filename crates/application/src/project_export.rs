use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gif_from_screen_domain::{
    AssetId, AssetKind, FrameClip, FrameId, MAX_TRANSITION_STEPS, OverlayId, OverlayTrack,
    ProjectId, ProjectManifest, ProjectRevision, RasterEncoding, TimeUs, Transition, TransitionId,
};
use gif_from_screen_gif::{
    BuiltinGifEncoder, CancellationToken as GifCancellationToken, EncodeOptions, EncodeProgress,
    EncodeReport, FixedPaletteQuantizer, FrameError, GifEncodeError, GifEncoder,
    IteratorFrameSource, ProgressSink, QuantizationError, RgbaFrame, Transparency,
};
use gif_from_screen_project::{ActiveProject, AssetStore, ProjectError};
use gif_from_screen_render::{
    AssetProviderError, CancellationToken as RenderCancellationToken, CpuRenderer,
    FrameAssetProvider, RenderError, RenderLimits, RgbaSurface, SurfaceError, TransitionProgress,
    active_raster_overlay_assets, render_transition,
};
use tempfile::NamedTempFile;
use thiserror::Error;

/// Lock-free, read-only inputs needed to export one project revision.
///
/// The manifest and content-addressed store handle are cloned from an active
/// project. The snapshot owns no project lock, so it can be moved to a
/// background thread after the [`ActiveProject`] is dropped.
#[derive(Clone, Debug)]
pub struct ProjectExportSnapshot {
    manifest: ProjectManifest,
    assets: AssetStore,
}

impl ProjectExportSnapshot {
    /// Captures the current manifest revision and immutable asset-store handle.
    pub fn from_active(project: &ActiveProject) -> Self {
        Self {
            manifest: project.manifest().clone(),
            assets: project.assets().clone(),
        }
    }

    /// Returns the immutable manifest revision represented by this snapshot.
    pub const fn manifest(&self) -> &ProjectManifest {
        &self.manifest
    }
}

/// Frames included in an export and their presentation order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProjectFrameSelection {
    /// Export the complete timeline in manifest order.
    #[default]
    All,
    /// Export exactly these frames in the supplied order.
    Ordered(Vec<FrameId>),
}

/// Immutable caller-supplied GIF palette in tightly packed RGB byte order.
///
/// The optional transparent index designates one existing RGB entry. Duplicate RGB entries are
/// accepted so a transparent entry can intentionally share its visible color with an opaque one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomGifPalette {
    packed_rgb: Vec<u8>,
    transparent_index: Option<u8>,
}

impl CustomGifPalette {
    /// Validates and constructs a palette containing between 2 and 256 complete RGB entries.
    ///
    /// # Errors
    ///
    /// Returns [`CustomGifPaletteError`] when the byte length is not divisible by three, the entry
    /// count is outside GIF's supported range, or `transparent_index` does not name an entry.
    pub fn new(
        packed_rgb: Vec<u8>,
        transparent_index: Option<u8>,
    ) -> Result<Self, CustomGifPaletteError> {
        if !packed_rgb.len().is_multiple_of(3) {
            return Err(CustomGifPaletteError::IncompleteRgbEntry {
                byte_len: packed_rgb.len(),
            });
        }
        let color_count = packed_rgb.len() / 3;
        if !(2..=256).contains(&color_count) {
            return Err(CustomGifPaletteError::ColorCountOutOfRange { color_count });
        }
        if let Some(index) = transparent_index
            && usize::from(index) >= color_count
        {
            return Err(CustomGifPaletteError::TransparentIndexOutOfRange { index, color_count });
        }
        Ok(Self {
            packed_rgb,
            transparent_index,
        })
    }

    /// Returns tightly packed RGB entries in stable palette-index order.
    pub fn packed_rgb(&self) -> &[u8] {
        &self.packed_rgb
    }

    /// Returns the designated transparent palette index, when configured.
    pub const fn transparent_index(&self) -> Option<u8> {
        self.transparent_index
    }

    /// Returns the number of RGB entries, including the optional transparent entry.
    pub fn color_count(&self) -> usize {
        self.packed_rgb.len() / 3
    }
}

/// Invalid construction input for [`CustomGifPalette`].
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum CustomGifPaletteError {
    /// The packed bytes end inside an RGB triplet.
    #[error("custom GIF palette byte length {byte_len} is not divisible by three")]
    IncompleteRgbEntry {
        /// Rejected packed byte length.
        byte_len: usize,
    },
    /// GIF palettes require between two and 256 entries.
    #[error("custom GIF palette must contain 2..=256 colors, got {color_count}")]
    ColorCountOutOfRange {
        /// Rejected number of complete RGB entries.
        color_count: usize,
    },
    /// The transparent index does not name an existing RGB entry.
    #[error("custom GIF transparent index {index} is outside the {color_count}-color palette")]
    TransparentIndexOutOfRange {
        /// Rejected palette index.
        index: u8,
        /// Number of entries in the palette.
        color_count: usize,
    },
}

/// Configuration for a project-to-GIF export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectGifExportOptions {
    /// Complete timeline or an explicitly ordered subset.
    pub frames: ProjectFrameSelection,
    /// Palette, timing, loop, transparency, delta, and dithering configuration.
    pub encoding: EncodeOptions,
    /// Optional immutable palette overriding `encoding.quantizer` while retaining palette mode,
    /// transparency, dithering, timing, delta, and loop settings.
    pub custom_palette: Option<CustomGifPalette>,
    /// Whether a successfully encoded GIF may atomically replace an existing file.
    pub overwrite_existing: bool,
    /// Maximum combined bytes retained for source assets and rendered frames.
    pub render_buffer_limit_bytes: u64,
}

impl Default for ProjectGifExportOptions {
    fn default() -> Self {
        Self {
            frames: ProjectFrameSelection::All,
            encoding: EncodeOptions::default(),
            custom_palette: None,
            overwrite_existing: false,
            render_buffer_limit_bytes: 512 * 1024 * 1024,
        }
    }
}

/// High-level phase of a project export.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProjectExportPhase {
    /// Validate selection and load immutable assets.
    Preparing,
    /// Apply clip transforms and effects with the deterministic CPU renderer.
    Rendering,
    /// Quantize and encode rendered frames as GIF.
    Encoding,
    /// Flush and synchronize the completed temporary file.
    Syncing,
    /// Atomically publish the synchronized file.
    Committing,
    /// Export and directory synchronization completed.
    Complete,
}

/// Monotonic stage and frame counters reported during export.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectExportProgress {
    /// Current export phase.
    pub phase: ProjectExportPhase,
    /// Logical project frames fully rendered so far.
    pub frames_rendered: u64,
    /// Logical rendered frames consumed by the GIF encoder so far.
    pub frames_encoded: u64,
    /// Exact output-frame count, including generated transition intermediates.
    pub total_frames: u64,
}

/// Receives project export progress on the worker thread.
pub trait ProjectExportProgressSink {
    /// Reports a monotonic export progress snapshot.
    fn report(&mut self, progress: ProjectExportProgress);
}

impl<F> ProjectExportProgressSink for F
where
    F: FnMut(ProjectExportProgress),
{
    fn report(&mut self, progress: ProjectExportProgress) {
        self(progress);
    }
}

/// Progress sink for callers that do not need updates.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopProjectExportProgress;

impl ProjectExportProgressSink for NoopProjectExportProgress {
    fn report(&mut self, _progress: ProjectExportProgress) {}
}

/// Successful atomic GIF export metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectGifExportReport {
    /// Project identifier captured by the export snapshot.
    pub project_id: ProjectId,
    /// Exact manifest revision captured by the export snapshot.
    pub revision: ProjectRevision,
    /// Number of selected logical frames.
    pub selected_frames: u64,
    /// Built-in encoder statistics.
    pub encoding: EncodeReport,
    /// Final committed output path.
    pub output_path: PathBuf,
    /// Synchronized temporary-file length immediately before commit.
    pub bytes_written: u64,
}

/// Failure while selecting, rendering, encoding, or atomically committing GIF output.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProjectGifExportError {
    /// No timeline frame was selected.
    #[error("project GIF export requires at least one frame")]
    EmptySelection,

    /// An explicitly selected frame id does not exist in the snapshot.
    #[error("selected frame {frame_id} at position {selection_index} is not in the project")]
    UnknownFrame {
        /// Zero-based position in the explicit selection.
        selection_index: usize,
        /// Unknown stable frame identifier.
        frame_id: FrameId,
    },

    /// An explicit selection contains the same frame more than once.
    #[error(
        "selected frame {frame_id} is duplicated at positions {first_index} and {duplicate_index}"
    )]
    DuplicateFrame {
        /// Repeated stable frame identifier.
        frame_id: FrameId,
        /// First selection position using the identifier.
        first_index: usize,
        /// Later selection position using the identifier.
        duplicate_index: usize,
    },

    /// A selected clip references no descriptor in the snapshot manifest.
    #[error("frame {frame_id} references missing asset descriptor {asset_id}")]
    MissingAssetDescriptor {
        /// Selected frame referencing the descriptor.
        frame_id: FrameId,
        /// Missing content-addressed asset identifier.
        asset_id: AssetId,
    },

    /// A selected clip references an asset that is not a frame raster.
    #[error("frame {frame_id} references non-frame asset {asset_id}: {kind:?}")]
    InvalidAssetKind {
        /// Selected frame referencing the asset.
        frame_id: FrameId,
        /// Incompatible asset identifier.
        asset_id: AssetId,
        /// Incompatible descriptor kind.
        kind: AssetKind,
    },

    /// The CPU renderer currently requires raw RGBA8 frame assets.
    #[error("frame {frame_id} asset {asset_id} uses unsupported raster encoding {encoding:?}")]
    UnsupportedAssetEncoding {
        /// Selected frame referencing the asset.
        frame_id: FrameId,
        /// Incompatible asset identifier.
        asset_id: AssetId,
        /// Unsupported persisted encoding.
        encoding: RasterEncoding,
    },

    /// A selected immutable asset file is missing.
    #[error("frame {frame_id} asset file {asset_id} is missing at {path}", path = path.display())]
    MissingAssetFile {
        /// Selected frame referencing the missing file.
        frame_id: FrameId,
        /// Missing content-addressed asset identifier.
        asset_id: AssetId,
        /// Expected immutable asset path.
        path: PathBuf,
    },

    /// A selected asset no longer matches its content digest.
    #[error("frame {frame_id} asset {asset_id} is corrupt: {source}")]
    CorruptAsset {
        /// Selected frame referencing the corrupt asset.
        frame_id: FrameId,
        /// Corrupt content-addressed asset identifier.
        asset_id: AssetId,
        /// Original project-store integrity error.
        #[source]
        source: ProjectError,
    },

    /// A selected asset could not be read or verified.
    #[error("could not read frame {frame_id} asset {asset_id}: {source}")]
    ReadAsset {
        /// Selected frame referencing the asset.
        frame_id: FrameId,
        /// Unreadable content-addressed asset identifier.
        asset_id: AssetId,
        /// Original project-store failure.
        #[source]
        source: ProjectError,
    },

    /// Descriptor byte length differs from verified asset content.
    #[error(
        "frame {frame_id} asset {asset_id} has {actual} bytes but its descriptor declares {expected}"
    )]
    AssetLengthMismatch {
        /// Selected frame referencing the asset.
        frame_id: FrameId,
        /// Inconsistent content-addressed asset identifier.
        asset_id: AssetId,
        /// Byte length persisted in the descriptor.
        expected: u64,
        /// Verified asset-file byte length.
        actual: u64,
    },

    /// Verified asset bytes do not form the descriptor's RGBA surface.
    #[error("frame {frame_id} asset {asset_id} is not a valid RGBA surface: {source}")]
    InvalidAssetSurface {
        /// Selected frame referencing the asset.
        frame_id: FrameId,
        /// Invalid content-addressed asset identifier.
        asset_id: AssetId,
        /// Renderer surface validation failure.
        #[source]
        source: SurfaceError,
    },

    /// An active raster overlay references no descriptor in the snapshot manifest.
    #[error("raster overlay {overlay_id} references missing asset descriptor {asset_id}")]
    MissingOverlayAssetDescriptor {
        /// Active raster overlay.
        overlay_id: OverlayId,
        /// Missing content-addressed asset identifier.
        asset_id: AssetId,
    },

    /// An active raster overlay references a non-raster asset kind.
    #[error("raster overlay {overlay_id} references non-raster asset {asset_id}: {kind:?}")]
    InvalidOverlayAssetKind {
        /// Active raster overlay.
        overlay_id: OverlayId,
        /// Incompatible content-addressed asset identifier.
        asset_id: AssetId,
        /// Incompatible descriptor kind.
        kind: AssetKind,
    },

    /// The raster-overlay compositor currently requires raw RGBA8 assets.
    #[error("raster overlay {overlay_id} asset {asset_id} uses unsupported encoding {encoding:?}")]
    UnsupportedOverlayAssetEncoding {
        /// Active raster overlay.
        overlay_id: OverlayId,
        /// Incompatible content-addressed asset identifier.
        asset_id: AssetId,
        /// Unsupported persisted encoding.
        encoding: RasterEncoding,
    },

    /// An active immutable raster-overlay asset file is missing.
    #[error(
        "raster overlay {overlay_id} asset {asset_id} is missing at {path}",
        path = path.display()
    )]
    MissingOverlayAssetFile {
        /// Active raster overlay.
        overlay_id: OverlayId,
        /// Missing content-addressed asset identifier.
        asset_id: AssetId,
        /// Expected immutable asset path.
        path: PathBuf,
    },

    /// An active raster-overlay asset no longer matches its content digest.
    #[error("raster overlay {overlay_id} asset {asset_id} is corrupt: {source}")]
    CorruptOverlayAsset {
        /// Active raster overlay.
        overlay_id: OverlayId,
        /// Corrupt content-addressed asset identifier.
        asset_id: AssetId,
        /// Original project-store integrity error.
        #[source]
        source: ProjectError,
    },

    /// An active raster-overlay asset could not be read or verified.
    #[error("could not read raster overlay {overlay_id} asset {asset_id}: {source}")]
    ReadOverlayAsset {
        /// Active raster overlay.
        overlay_id: OverlayId,
        /// Unreadable content-addressed asset identifier.
        asset_id: AssetId,
        /// Original project-store failure.
        #[source]
        source: ProjectError,
    },

    /// An overlay descriptor byte length differs from verified asset content.
    #[error(
        "raster overlay {overlay_id} asset {asset_id} has {actual} bytes but its descriptor declares {expected}"
    )]
    OverlayAssetLengthMismatch {
        /// Active raster overlay.
        overlay_id: OverlayId,
        /// Inconsistent content-addressed asset identifier.
        asset_id: AssetId,
        /// Byte length persisted in the descriptor.
        expected: u64,
        /// Verified asset-file byte length.
        actual: u64,
    },

    /// Verified raster-overlay bytes do not form the descriptor's RGBA surface.
    #[error("raster overlay {overlay_id} asset {asset_id} is not valid RGBA8: {source}")]
    InvalidOverlayAssetSurface {
        /// Active raster overlay.
        overlay_id: OverlayId,
        /// Invalid content-addressed asset identifier.
        asset_id: AssetId,
        /// Renderer surface validation failure.
        #[source]
        source: SurfaceError,
    },

    /// Resolving active raster overlays failed for one selected frame time.
    #[error("could not plan raster overlays for frame {frame_id} at {time_us}us: {source}")]
    PlanRasterOverlays {
        /// Selected project frame.
        frame_id: FrameId,
        /// Original project-relative frame start.
        time_us: u64,
        /// Renderer planning failure.
        #[source]
        source: RenderError,
    },

    /// CPU transform/effect rendering failed for one frame.
    #[error("could not render selected frame {frame_id} at position {selection_index}: {source}")]
    RenderFrame {
        /// Zero-based position in the resolved selection.
        selection_index: usize,
        /// Stable frame identifier being rendered.
        frame_id: FrameId,
        /// Deterministic renderer failure.
        #[source]
        source: RenderError,
    },

    /// A rendered surface exceeds GIF's 16-bit dimensions.
    #[error("rendered frame {frame_id} dimensions {width}x{height} exceed the GIF canvas limit")]
    RenderedDimensionsOutOfRange {
        /// Stable frame identifier being rendered.
        frame_id: FrameId,
        /// Rendered width.
        width: u32,
        /// Rendered height.
        height: u32,
    },

    /// A rendered surface could not be converted into an encoder frame.
    #[error("rendered frame {frame_id} is invalid for GIF encoding: {source}")]
    BuildGifFrame {
        /// Stable frame identifier being converted.
        frame_id: FrameId,
        /// Encoder frame validation failure.
        #[source]
        source: FrameError,
    },

    /// Retained source and rendered RGBA buffers exceed the configured bound.
    #[error(
        "project export would retain {required_bytes} RGBA bytes, above the configured {limit_bytes}-byte limit"
    )]
    RenderBufferLimitExceeded {
        /// Resident bytes required after accepting the current buffer.
        required_bytes: u64,
        /// Caller-configured resident RGBA bound.
        limit_bytes: u64,
    },

    /// Resident render-buffer accounting overflowed `u64` before comparison with the limit.
    #[error("project export render-buffer byte accounting overflowed")]
    RenderBufferSizeOverflow,

    /// More than one transition targets the same ordered frame pair.
    #[error(
        "transitions {first_transition_id} and {duplicate_transition_id} both target frames {from_frame} -> {to_frame}"
    )]
    DuplicateTransitionEndpoints {
        /// Identifier of the first transition for this pair.
        first_transition_id: TransitionId,
        /// Identifier of the later ambiguous transition.
        duplicate_transition_id: TransitionId,
        /// Outgoing endpoint.
        from_frame: FrameId,
        /// Incoming endpoint.
        to_frame: FrameId,
    },

    /// An applicable transition has an invalid intermediate-frame count.
    #[error("transition {} has invalid step count {steps}", transition.id)]
    InvalidTransitionSteps {
        /// Invalid applicable transition.
        transition: Transition,
        /// Rejected intermediate-frame count.
        steps: u16,
    },

    /// An applicable transition cannot give every intermediate frame a positive duration.
    #[error(
        "transition {} duration {duration_us}us is shorter than its {steps} steps",
        transition.id
    )]
    TransitionDurationTooShort {
        /// Invalid applicable transition.
        transition: Transition,
        /// Total duration available for intermediates.
        duration_us: u64,
        /// Positive intermediate-frame count.
        steps: u16,
    },

    /// Selected originals plus transition frames exceed a representable count.
    #[error("selected output frame count overflows this platform")]
    OutputFrameCountOverflow,

    /// Selected originals plus transition durations exceed `u64` microseconds.
    #[error("selected output duration exceeds the supported microsecond range")]
    OutputDurationOverflow,

    /// Growing the rendered-frame metadata vector failed.
    #[error("could not allocate metadata for {requested} rendered GIF frames")]
    OutputFrameAllocationFailed {
        /// Metadata entries requested at the failed growth point.
        requested: usize,
    },

    /// Rendering one transition intermediate failed.
    #[error("could not render transition {} step {step}: {source}", transition.id)]
    RenderTransition {
        /// Transition being rendered.
        transition: Transition,
        /// One-based intermediate step.
        step: u16,
        /// Deterministic renderer failure.
        #[source]
        source: Box<RenderError>,
    },

    /// A custom palette was paired with an invalid encoder color limit.
    #[error("custom palette requires max_colors in 2..=256, got {max_colors}")]
    InvalidCustomPaletteColorLimit {
        /// Rejected encoder limit.
        max_colors: u16,
    },

    /// A fixed custom palette cannot be truncated to the requested encoder limit.
    #[error("custom palette contains {palette_colors} colors, above max_colors={max_colors}")]
    CustomPaletteExceedsColorLimit {
        /// Number of caller-supplied palette entries.
        palette_colors: usize,
        /// Encoder color limit.
        max_colors: u16,
    },

    /// Rendered transparency requires a designated entry in the custom palette.
    #[error(
        "rendered frame {frame_index} contains alpha below {alpha_threshold}, but the custom palette has no transparent index"
    )]
    CustomPaletteMissingTransparency {
        /// Zero-based rendered output-frame position containing transparency.
        frame_index: usize,
        /// Strict alpha threshold used by the encoder.
        alpha_threshold: u8,
    },

    /// The GIF quantizer unexpectedly rejected a previously validated application palette.
    #[error("validated custom palette could not initialize the GIF quantizer: {source}")]
    CustomPaletteQuantizerRejected {
        /// Lower-level fixed-palette validation failure.
        #[source]
        source: QuantizationError,
    },

    /// Built-in GIF encoding failed.
    #[error("GIF encoding failed: {source}")]
    Encode {
        /// Built-in encoder or quantizer failure.
        #[source]
        source: GifEncodeError,
    },

    /// Cooperative cancellation was observed before atomic commit.
    #[error("project GIF export was cancelled")]
    Cancelled,

    /// Output path has no usable file name.
    #[error("GIF output path does not identify a file: {}", .0.display())]
    InvalidOutputPath(PathBuf),

    /// Existing output is protected by the default no-overwrite policy.
    #[error("refusing to overwrite existing GIF output {}", .0.display())]
    ExistingOutput(PathBuf),

    /// Temporary-file creation, synchronization, commit, or directory sync failed.
    #[error("could not {operation} {}: {source}", path.display())]
    Io {
        /// Stable failing operation name.
        operation: &'static str,
        /// Relevant output or parent path.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: io::Error,
    },

    /// Atomic rename succeeded, but synchronizing its parent directory failed.
    #[error(
        "GIF output {} was committed, but its directory could not be synchronized; durability is uncertain: {source}",
        path.display()
    )]
    CommitDurabilityUnknown {
        /// Output path already published by the successful rename.
        path: PathBuf,
        /// Parent-directory synchronization failure.
        #[source]
        source: io::Error,
    },
}

/// Renders a lock-free project snapshot and atomically commits a GIF file.
///
/// This synchronous use case owns no UI or runtime state and is safe to move as
/// a whole onto an application-managed background thread. The final path is
/// untouched until rendering and encoding succeed, the temporary file is
/// flushed and synchronized, and cancellation is checked one last time.
/// Temporary files are automatically removed on every pre-commit failure.
/// When replacement is enabled, the old output likewise remains untouched by
/// every rendering, encoding, synchronization, and cancellation failure before
/// the atomic rename. A failure to synchronize the directory after that rename
/// is reported separately as [`ProjectGifExportError::CommitDurabilityUnknown`]
/// because the new output is already visible and its crash durability is
/// uncertain.
///
/// # Errors
///
/// Returns [`ProjectGifExportError`] for invalid selections, asset integrity or
/// encoding mismatches, rendering/encoding failures, cancellation, protected
/// existing output, or filesystem failures.
pub fn export_project_snapshot_to_gif(
    snapshot: &ProjectExportSnapshot,
    output: impl AsRef<Path>,
    options: &ProjectGifExportOptions,
    cancellation: &dyn GifCancellationToken,
    progress: &mut dyn ProjectExportProgressSink,
) -> Result<ProjectGifExportReport, ProjectGifExportError> {
    let output = output.as_ref().to_path_buf();
    let parent = output_parent(&output)?;
    ensure_not_cancelled(cancellation)?;
    if !options.overwrite_existing && try_exists(&output, "inspect GIF output")? {
        return Err(ProjectGifExportError::ExistingOutput(output));
    }

    let clips = select_clips(&snapshot.manifest, &options.frames)?;
    let frame_times = selected_frame_start_times(&snapshot.manifest, &clips)?;
    let selected_frames =
        u64::try_from(clips.len()).map_err(|_| ProjectGifExportError::OutputFrameCountOverflow)?;
    let transitions = applicable_transitions(&snapshot.manifest, &clips)?;
    let total_frames = expanded_frame_count(clips.len(), &transitions)?;
    validate_expanded_duration(&clips, &transitions)?;
    let mut execution = ExportExecution::new(cancellation, progress, total_frames);
    execution.report_phase(ProjectExportPhase::Preparing);
    let (assets, source_bytes) = load_selected_assets(
        snapshot,
        &clips,
        &frame_times,
        options.render_buffer_limit_bytes,
        cancellation,
    )?;
    let provider = LoadedAssetProvider { assets };
    let gif_frames = render_selected_frames(
        &clips,
        &frame_times,
        &transitions,
        &snapshot.manifest.timeline.overlay_tracks,
        &provider,
        source_bytes,
        options.render_buffer_limit_bytes,
        &mut execution,
    )?;
    let (encoding, bytes_written) =
        encode_and_commit(gif_frames, &output, parent, options, &mut execution)?;
    execution.report_phase(ProjectExportPhase::Complete);
    Ok(ProjectGifExportReport {
        project_id: snapshot.manifest.project_id,
        revision: snapshot.manifest.revision,
        selected_frames,
        encoding,
        output_path: output,
        bytes_written,
    })
}

struct ExportExecution<'a> {
    cancellation: &'a dyn GifCancellationToken,
    progress: &'a mut dyn ProjectExportProgressSink,
    state: ProjectExportProgress,
}

impl<'a> ExportExecution<'a> {
    fn new(
        cancellation: &'a dyn GifCancellationToken,
        progress: &'a mut dyn ProjectExportProgressSink,
        total_frames: u64,
    ) -> Self {
        Self {
            cancellation,
            progress,
            state: ProjectExportProgress {
                phase: ProjectExportPhase::Preparing,
                frames_rendered: 0,
                frames_encoded: 0,
                total_frames,
            },
        }
    }

    fn report_phase(&mut self, phase: ProjectExportPhase) {
        self.state.phase = phase;
        self.progress.report(self.state);
    }
}

fn applicable_transitions(
    manifest: &ProjectManifest,
    clips: &[FrameClip],
) -> Result<Vec<Option<Transition>>, ProjectGifExportError> {
    let positions: BTreeMap<_, _> = manifest
        .timeline
        .frames
        .iter()
        .enumerate()
        .map(|(index, frame)| (frame.id, index))
        .collect();
    let mut transitions_by_endpoint = BTreeMap::new();
    for transition in &manifest.timeline.transitions {
        let endpoints = (transition.from_frame, transition.to_frame);
        if let Some(first) = transitions_by_endpoint.insert(endpoints, transition) {
            return Err(ProjectGifExportError::DuplicateTransitionEndpoints {
                first_transition_id: first.id,
                duplicate_transition_id: transition.id,
                from_frame: transition.from_frame,
                to_frame: transition.to_frame,
            });
        }
    }
    clips
        .windows(2)
        .map(|pair| {
            let forward_adjacent = positions
                .get(&pair[0].id)
                .zip(positions.get(&pair[1].id))
                .is_some_and(|(from, to)| from.checked_add(1) == Some(*to));
            if !forward_adjacent {
                return Ok(None);
            }
            let transition = transitions_by_endpoint
                .get(&(pair[0].id, pair[1].id))
                .map(|transition| (*transition).clone());
            if let Some(transition) = &transition {
                validate_transition_timing(transition)?;
            }
            Ok(transition)
        })
        .collect()
}

fn validate_transition_timing(transition: &Transition) -> Result<(), ProjectGifExportError> {
    if transition.steps == 0 || transition.steps > MAX_TRANSITION_STEPS {
        return Err(ProjectGifExportError::InvalidTransitionSteps {
            transition: transition.clone(),
            steps: transition.steps,
        });
    }
    if transition.duration.get() < u64::from(transition.steps) {
        return Err(ProjectGifExportError::TransitionDurationTooShort {
            transition: transition.clone(),
            duration_us: transition.duration.get(),
            steps: transition.steps,
        });
    }
    Ok(())
}

fn expanded_frame_count(
    selected_frames: usize,
    transitions: &[Option<Transition>],
) -> Result<u64, ProjectGifExportError> {
    let count = transitions
        .iter()
        .flatten()
        .try_fold(selected_frames, |total, transition| {
            total.checked_add(usize::from(transition.steps))
        })
        .ok_or(ProjectGifExportError::OutputFrameCountOverflow)?;
    u64::try_from(count).map_err(|_| ProjectGifExportError::OutputFrameCountOverflow)
}

fn validate_expanded_duration(
    clips: &[FrameClip],
    transitions: &[Option<Transition>],
) -> Result<(), ProjectGifExportError> {
    let mut total = 0_u64;
    for duration_us in clips.iter().map(|clip| clip.duration.get()).chain(
        transitions
            .iter()
            .flatten()
            .map(|transition| transition.duration.get()),
    ) {
        total = total
            .checked_add(duration_us)
            .ok_or(ProjectGifExportError::OutputDurationOverflow)?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the bounded two-surface lookahead keeps output ordering and memory accounting auditable"
)]
fn render_selected_frames(
    clips: &[FrameClip],
    frame_times: &[TimeUs],
    transitions: &[Option<Transition>],
    overlay_tracks: &[OverlayTrack],
    provider: &LoadedAssetProvider,
    source_bytes: u64,
    buffer_limit_bytes: u64,
    execution: &mut ExportExecution<'_>,
) -> Result<Vec<RgbaFrame>, ProjectGifExportError> {
    let cpu_renderer = CpuRenderer::with_limits(RenderLimits {
        max_surface_bytes: usize::try_from(buffer_limit_bytes).unwrap_or(usize::MAX),
    });
    let render_cancellation = RenderCancellationAdapter(execution.cancellation);
    let mut gif_frames = Vec::new();
    let mut rendered_bytes = 0_u64;
    execution.report_phase(ProjectExportPhase::Rendering);
    let mut current = render_clip_surface(
        &cpu_renderer,
        &clips[0],
        frame_times[0],
        0,
        overlay_tracks,
        provider,
        &render_cancellation,
        execution,
    )?;
    ensure_render_buffer(
        source_bytes,
        rendered_bytes,
        &[u64::try_from(current.pixels().len()).unwrap_or(u64::MAX)],
        buffer_limit_bytes,
    )?;

    for selection_index in 0..clips.len().saturating_sub(1) {
        let next_clip = &clips[selection_index + 1];
        let next = render_clip_surface(
            &cpu_renderer,
            next_clip,
            frame_times[selection_index + 1],
            selection_index + 1,
            overlay_tracks,
            provider,
            &render_cancellation,
            execution,
        )?;
        let current_bytes = u64::try_from(current.pixels().len()).unwrap_or(u64::MAX);
        let next_bytes = u64::try_from(next.pixels().len()).unwrap_or(u64::MAX);
        ensure_render_buffer(
            source_bytes,
            rendered_bytes,
            &[current_bytes, next_bytes],
            buffer_limit_bytes,
        )?;

        let mut intermediate_frames = Vec::new();
        let mut intermediate_bytes = 0_u64;
        if let Some(transition) = &transitions[selection_index] {
            intermediate_frames
                .try_reserve_exact(usize::from(transition.steps))
                .map_err(|_| ProjectGifExportError::OutputFrameAllocationFailed {
                    requested: usize::from(transition.steps),
                })?;
            let denominator = u32::from(transition.steps) + 1;
            for step in 1..=transition.steps {
                ensure_not_cancelled(execution.cancellation)?;
                ensure_render_buffer(
                    source_bytes,
                    rendered_bytes,
                    &[current_bytes, next_bytes, intermediate_bytes, current_bytes],
                    buffer_limit_bytes,
                )?;
                let progress =
                    TransitionProgress::new(u32::from(step), denominator).map_err(|source| {
                        ProjectGifExportError::RenderTransition {
                            transition: transition.clone(),
                            step,
                            source: Box::new(source),
                        }
                    })?;
                let surface = render_transition(
                    &current,
                    &next,
                    &transition.kind,
                    progress,
                    &render_cancellation,
                )
                .map_err(|source| {
                    if execution.cancellation.is_cancelled() {
                        ProjectGifExportError::Cancelled
                    } else {
                        ProjectGifExportError::RenderTransition {
                            transition: transition.clone(),
                            step,
                            source: Box::new(source),
                        }
                    }
                })?;
                let duration_us = transition_step_duration(transition, step - 1);
                intermediate_bytes = intermediate_bytes
                    .checked_add(u64::try_from(surface.pixels().len()).unwrap_or(u64::MAX))
                    .ok_or(ProjectGifExportError::RenderBufferSizeOverflow)?;
                intermediate_frames.push(surface_to_gif_frame(
                    surface,
                    transition.from_frame,
                    duration_us,
                )?);
            }
        }

        let current_clip = &clips[selection_index];
        append_gif_frame(
            &mut gif_frames,
            surface_to_gif_frame(current, current_clip.id, current_clip.duration.get())?,
            &mut rendered_bytes,
            execution,
        )?;
        for frame in intermediate_frames {
            append_gif_frame(&mut gif_frames, frame, &mut rendered_bytes, execution)?;
        }
        current = next;
    }
    let last = clips.last().ok_or(ProjectGifExportError::EmptySelection)?;
    append_gif_frame(
        &mut gif_frames,
        surface_to_gif_frame(current, last.id, last.duration.get())?,
        &mut rendered_bytes,
        execution,
    )?;
    Ok(gif_frames)
}

fn surface_to_gif_frame(
    surface: RgbaSurface,
    frame_id: FrameId,
    duration_us: u64,
) -> Result<RgbaFrame, ProjectGifExportError> {
    let width = u16::try_from(surface.width()).map_err(|_| {
        ProjectGifExportError::RenderedDimensionsOutOfRange {
            frame_id,
            width: surface.width(),
            height: surface.height(),
        }
    })?;
    let height = u16::try_from(surface.height()).map_err(|_| {
        ProjectGifExportError::RenderedDimensionsOutOfRange {
            frame_id,
            width: surface.width(),
            height: surface.height(),
        }
    })?;
    RgbaFrame::new(width, height, surface.into_pixels(), duration_us)
        .map_err(|source| ProjectGifExportError::BuildGifFrame { frame_id, source })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the helper keeps frame identity, timeline sample, provider, and cancellation context explicit"
)]
fn render_clip_surface(
    renderer: &CpuRenderer,
    clip: &FrameClip,
    sample_time: TimeUs,
    selection_index: usize,
    overlay_tracks: &[OverlayTrack],
    provider: &LoadedAssetProvider,
    cancellation: &RenderCancellationAdapter<'_>,
    execution: &ExportExecution<'_>,
) -> Result<RgbaSurface, ProjectGifExportError> {
    ensure_not_cancelled(execution.cancellation)?;
    renderer
        .render_clip_with_raster_overlays(clip, overlay_tracks, sample_time, provider, cancellation)
        .map_err(|source| {
            if execution.cancellation.is_cancelled() {
                ProjectGifExportError::Cancelled
            } else {
                ProjectGifExportError::RenderFrame {
                    selection_index,
                    frame_id: clip.id,
                    source,
                }
            }
        })
}

fn transition_step_duration(transition: &Transition, zero_based_step: u16) -> u64 {
    let steps = u64::from(transition.steps);
    let base = transition.duration.get() / steps;
    let remainder = transition.duration.get() % steps;
    base + u64::from(u64::from(zero_based_step) < remainder)
}

fn ensure_render_buffer(
    source_bytes: u64,
    rendered_bytes: u64,
    working_buffers: &[u64],
    limit_bytes: u64,
) -> Result<(), ProjectGifExportError> {
    let required_bytes = working_buffers.iter().try_fold(
        source_bytes
            .checked_add(rendered_bytes)
            .ok_or(ProjectGifExportError::RenderBufferSizeOverflow)?,
        |total, bytes| {
            total
                .checked_add(*bytes)
                .ok_or(ProjectGifExportError::RenderBufferSizeOverflow)
        },
    )?;
    if required_bytes > limit_bytes {
        Err(ProjectGifExportError::RenderBufferLimitExceeded {
            required_bytes,
            limit_bytes,
        })
    } else {
        Ok(())
    }
}

fn append_gif_frame(
    frames: &mut Vec<RgbaFrame>,
    frame: RgbaFrame,
    rendered_bytes: &mut u64,
    execution: &mut ExportExecution<'_>,
) -> Result<(), ProjectGifExportError> {
    ensure_not_cancelled(execution.cancellation)?;
    let requested = frames
        .len()
        .checked_add(1)
        .ok_or(ProjectGifExportError::OutputFrameCountOverflow)?;
    frames
        .try_reserve(1)
        .map_err(|_| ProjectGifExportError::OutputFrameAllocationFailed { requested })?;
    *rendered_bytes = rendered_bytes
        .checked_add(u64::try_from(frame.pixels().len()).unwrap_or(u64::MAX))
        .ok_or(ProjectGifExportError::RenderBufferSizeOverflow)?;
    frames.push(frame);
    execution.state.frames_rendered = u64::try_from(frames.len()).unwrap_or(u64::MAX);
    execution.progress.report(execution.state);
    Ok(())
}

fn encode_and_commit(
    gif_frames: Vec<RgbaFrame>,
    output: &Path,
    parent: &Path,
    options: &ProjectGifExportOptions,
    execution: &mut ExportExecution<'_>,
) -> Result<(EncodeReport, u64), ProjectGifExportError> {
    ensure_not_cancelled(execution.cancellation)?;
    let encoder = encoder_for_options(options, &gif_frames, execution.cancellation)?;
    ensure_not_cancelled(execution.cancellation)?;
    let mut temporary = create_temporary(parent, output)?;
    execution.report_phase(ProjectExportPhase::Encoding);
    let mut frame_source = IteratorFrameSource::new(gif_frames.into_iter());
    let cancellation = execution.cancellation;
    let encoding = {
        let mut encode_progress = GifProgressAdapter {
            sink: execution.progress,
            state: &mut execution.state,
        };
        encoder
            .encode(
                &mut frame_source,
                temporary.as_file_mut(),
                &options.encoding,
                cancellation,
                &mut encode_progress,
            )
            .map_err(|source| {
                if cancellation.is_cancelled() {
                    ProjectGifExportError::Cancelled
                } else {
                    ProjectGifExportError::Encode { source }
                }
            })?
    };

    ensure_not_cancelled(cancellation)?;
    execution.state.frames_encoded = execution.state.total_frames;
    execution.report_phase(ProjectExportPhase::Syncing);
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| ProjectGifExportError::Io {
            operation: "synchronize temporary GIF",
            path: temporary.path().to_path_buf(),
            source,
        })?;
    let bytes_written = temporary
        .as_file()
        .metadata()
        .map_err(|source| ProjectGifExportError::Io {
            operation: "inspect synchronized temporary GIF",
            path: temporary.path().to_path_buf(),
            source,
        })?
        .len();
    ensure_not_cancelled(cancellation)?;
    execution.report_phase(ProjectExportPhase::Committing);
    persist_temporary(temporary, output, options.overwrite_existing)?;
    sync_directory_after_commit(parent, output)?;
    Ok((encoding, bytes_written))
}

fn encoder_for_options(
    options: &ProjectGifExportOptions,
    frames: &[RgbaFrame],
    cancellation: &dyn GifCancellationToken,
) -> Result<BuiltinGifEncoder, ProjectGifExportError> {
    let Some(palette) = &options.custom_palette else {
        return Ok(BuiltinGifEncoder::default());
    };
    let max_colors = options.encoding.max_colors;
    if !(2..=256).contains(&max_colors) {
        return Err(ProjectGifExportError::InvalidCustomPaletteColorLimit { max_colors });
    }
    if palette.color_count() > usize::from(max_colors) {
        return Err(ProjectGifExportError::CustomPaletteExceedsColorLimit {
            palette_colors: palette.color_count(),
            max_colors,
        });
    }
    if palette.transparent_index().is_none()
        && let Transparency::AlphaThreshold(alpha_threshold) = options.encoding.transparency
        && let Some(frame_index) =
            first_frame_requiring_transparency(frames, alpha_threshold, cancellation)?
    {
        return Err(ProjectGifExportError::CustomPaletteMissingTransparency {
            frame_index,
            alpha_threshold,
        });
    }
    let quantizer =
        FixedPaletteQuantizer::new(palette.packed_rgb().to_vec(), palette.transparent_index())
            .map_err(|source| ProjectGifExportError::CustomPaletteQuantizerRejected { source })?;
    Ok(BuiltinGifEncoder::new(Box::new(quantizer)))
}

fn first_frame_requiring_transparency(
    frames: &[RgbaFrame],
    alpha_threshold: u8,
    cancellation: &dyn GifCancellationToken,
) -> Result<Option<usize>, ProjectGifExportError> {
    ensure_not_cancelled(cancellation)?;
    if alpha_threshold == 0 {
        return Ok(None);
    }
    for (frame_index, frame) in frames.iter().enumerate() {
        ensure_not_cancelled(cancellation)?;
        let row_bytes = usize::from(frame.width()) * 4;
        for row in frame.pixels().chunks_exact(row_bytes) {
            ensure_not_cancelled(cancellation)?;
            if row
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] < alpha_threshold)
            {
                return Ok(Some(frame_index));
            }
        }
    }
    ensure_not_cancelled(cancellation)?;
    Ok(None)
}

fn select_clips(
    manifest: &ProjectManifest,
    selection: &ProjectFrameSelection,
) -> Result<Vec<FrameClip>, ProjectGifExportError> {
    match selection {
        ProjectFrameSelection::All => {
            if manifest.timeline.frames.is_empty() {
                return Err(ProjectGifExportError::EmptySelection);
            }
            Ok(manifest.timeline.frames.clone())
        }
        ProjectFrameSelection::Ordered(frame_ids) => {
            if frame_ids.is_empty() {
                return Err(ProjectGifExportError::EmptySelection);
            }
            let available: BTreeMap<_, _> = manifest
                .timeline
                .frames
                .iter()
                .map(|clip| (clip.id, clip))
                .collect();
            let mut first_positions = BTreeMap::new();
            let mut clips = Vec::with_capacity(frame_ids.len());
            for (selection_index, frame_id) in frame_ids.iter().copied().enumerate() {
                if let Some(first_index) = first_positions.insert(frame_id, selection_index) {
                    return Err(ProjectGifExportError::DuplicateFrame {
                        frame_id,
                        first_index,
                        duplicate_index: selection_index,
                    });
                }
                let clip = available
                    .get(&frame_id)
                    .ok_or(ProjectGifExportError::UnknownFrame {
                        selection_index,
                        frame_id,
                    })?;
                clips.push((*clip).clone());
            }
            Ok(clips)
        }
    }
}

fn selected_frame_start_times(
    manifest: &ProjectManifest,
    clips: &[FrameClip],
) -> Result<Vec<TimeUs>, ProjectGifExportError> {
    let mut starts = BTreeMap::new();
    let mut start_us = 0_u64;
    for frame in &manifest.timeline.frames {
        starts.insert(frame.id, TimeUs::new(start_us));
        start_us = start_us
            .checked_add(frame.duration.get())
            .ok_or(ProjectGifExportError::OutputDurationOverflow)?;
    }
    let mut selected = Vec::new();
    selected.try_reserve_exact(clips.len()).map_err(|_| {
        ProjectGifExportError::OutputFrameAllocationFailed {
            requested: clips.len(),
        }
    })?;
    for (selection_index, clip) in clips.iter().enumerate() {
        selected.push(
            *starts
                .get(&clip.id)
                .ok_or(ProjectGifExportError::UnknownFrame {
                    selection_index,
                    frame_id: clip.id,
                })?,
        );
    }
    Ok(selected)
}

#[allow(
    clippy::too_many_lines,
    reason = "frame and active-overlay assets share one ordered aggregate memory budget"
)]
fn load_selected_assets(
    snapshot: &ProjectExportSnapshot,
    clips: &[FrameClip],
    frame_times: &[TimeUs],
    buffer_limit_bytes: u64,
    cancellation: &dyn GifCancellationToken,
) -> Result<(BTreeMap<AssetId, RgbaSurface>, u64), ProjectGifExportError> {
    let mut loaded = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let mut loaded_bytes = 0_u64;
    for clip in clips {
        ensure_not_cancelled(cancellation)?;
        if !visited.insert(clip.asset_id) {
            continue;
        }
        let descriptor = snapshot.manifest.assets.get(&clip.asset_id).ok_or(
            ProjectGifExportError::MissingAssetDescriptor {
                frame_id: clip.id,
                asset_id: clip.asset_id,
            },
        )?;
        let Some((size, encoding)) = descriptor.kind.raster_descriptor() else {
            return Err(ProjectGifExportError::InvalidAssetKind {
                frame_id: clip.id,
                asset_id: clip.asset_id,
                kind: descriptor.kind.clone(),
            });
        };
        if encoding != RasterEncoding::Rgba8 {
            return Err(ProjectGifExportError::UnsupportedAssetEncoding {
                frame_id: clip.id,
                asset_id: clip.asset_id,
                encoding,
            });
        }
        let asset_path = snapshot.assets.asset_path(clip.asset_id);
        let actual = asset_file_length(&asset_path, clip)?;
        if actual != descriptor.byte_len {
            return Err(ProjectGifExportError::AssetLengthMismatch {
                frame_id: clip.id,
                asset_id: clip.asset_id,
                expected: descriptor.byte_len,
                actual,
            });
        }
        let required_bytes = loaded_bytes
            .checked_add(actual)
            .ok_or(ProjectGifExportError::RenderBufferSizeOverflow)?;
        if required_bytes > buffer_limit_bytes {
            return Err(ProjectGifExportError::RenderBufferLimitExceeded {
                required_bytes,
                limit_bytes: buffer_limit_bytes,
            });
        }
        let pixels = read_asset(snapshot, clip)?;
        let surface = RgbaSurface::new(size, pixels).map_err(|source| {
            ProjectGifExportError::InvalidAssetSurface {
                frame_id: clip.id,
                asset_id: clip.asset_id,
                source,
            }
        })?;
        loaded.insert(clip.asset_id, surface);
        loaded_bytes = required_bytes;
    }
    let render_cancellation = RenderCancellationAdapter(cancellation);
    let mut overlay_assets = BTreeMap::new();
    for (clip, sample_time) in clips.iter().zip(frame_times.iter().copied()) {
        ensure_not_cancelled(cancellation)?;
        let active = active_raster_overlay_assets(
            &snapshot.manifest.timeline.overlay_tracks,
            sample_time,
            &render_cancellation,
        )
        .map_err(|source| {
            if cancellation.is_cancelled() || matches!(&source, RenderError::Cancelled) {
                ProjectGifExportError::Cancelled
            } else {
                ProjectGifExportError::PlanRasterOverlays {
                    frame_id: clip.id,
                    time_us: sample_time.get(),
                    source,
                }
            }
        })?;
        for reference in active {
            overlay_assets
                .entry(reference.asset_id)
                .or_insert(reference.overlay_id);
        }
    }
    for (asset_id, overlay_id) in overlay_assets {
        ensure_not_cancelled(cancellation)?;
        let descriptor = snapshot.manifest.assets.get(&asset_id).ok_or(
            ProjectGifExportError::MissingOverlayAssetDescriptor {
                overlay_id,
                asset_id,
            },
        )?;
        let Some((size, encoding)) = descriptor.kind.raster_descriptor() else {
            return Err(ProjectGifExportError::InvalidOverlayAssetKind {
                overlay_id,
                asset_id,
                kind: descriptor.kind.clone(),
            });
        };
        if encoding != RasterEncoding::Rgba8 {
            return Err(ProjectGifExportError::UnsupportedOverlayAssetEncoding {
                overlay_id,
                asset_id,
                encoding,
            });
        }
        if loaded.contains_key(&asset_id) {
            continue;
        }
        let asset_path = snapshot.assets.asset_path(asset_id);
        let actual = overlay_asset_file_length(&asset_path, overlay_id, asset_id)?;
        if actual != descriptor.byte_len {
            return Err(ProjectGifExportError::OverlayAssetLengthMismatch {
                overlay_id,
                asset_id,
                expected: descriptor.byte_len,
                actual,
            });
        }
        let required_bytes = loaded_bytes
            .checked_add(actual)
            .ok_or(ProjectGifExportError::RenderBufferSizeOverflow)?;
        if required_bytes > buffer_limit_bytes {
            return Err(ProjectGifExportError::RenderBufferLimitExceeded {
                required_bytes,
                limit_bytes: buffer_limit_bytes,
            });
        }
        let pixels = read_overlay_asset(snapshot, overlay_id, asset_id)?;
        let surface = RgbaSurface::new(size, pixels).map_err(|source| {
            ProjectGifExportError::InvalidOverlayAssetSurface {
                overlay_id,
                asset_id,
                source,
            }
        })?;
        loaded.insert(asset_id, surface);
        loaded_bytes = required_bytes;
    }
    Ok((loaded, loaded_bytes))
}

fn overlay_asset_file_length(
    path: &Path,
    overlay_id: OverlayId,
    asset_id: AssetId,
) -> Result<u64, ProjectGifExportError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            Err(ProjectGifExportError::MissingOverlayAssetFile {
                overlay_id,
                asset_id,
                path: path.to_path_buf(),
            })
        }
        Err(source) => Err(ProjectGifExportError::Io {
            operation: "inspect raster overlay asset",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_overlay_asset(
    snapshot: &ProjectExportSnapshot,
    overlay_id: OverlayId,
    asset_id: AssetId,
) -> Result<Vec<u8>, ProjectGifExportError> {
    match snapshot.assets.read(asset_id) {
        Ok(pixels) => Ok(pixels),
        Err(error) => {
            if matches!(
                &error,
                ProjectError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound
            ) {
                return Err(ProjectGifExportError::MissingOverlayAssetFile {
                    overlay_id,
                    asset_id,
                    path: snapshot.assets.asset_path(asset_id),
                });
            }
            if matches!(error, ProjectError::CorruptAsset { .. }) {
                return Err(ProjectGifExportError::CorruptOverlayAsset {
                    overlay_id,
                    asset_id,
                    source: error,
                });
            }
            Err(ProjectGifExportError::ReadOverlayAsset {
                overlay_id,
                asset_id,
                source: error,
            })
        }
    }
}

fn asset_file_length(path: &Path, clip: &FrameClip) -> Result<u64, ProjectGifExportError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            Err(ProjectGifExportError::MissingAssetFile {
                frame_id: clip.id,
                asset_id: clip.asset_id,
                path: path.to_path_buf(),
            })
        }
        Err(source) => Err(ProjectGifExportError::Io {
            operation: "inspect frame asset",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_asset(
    snapshot: &ProjectExportSnapshot,
    clip: &FrameClip,
) -> Result<Vec<u8>, ProjectGifExportError> {
    match snapshot.assets.read(clip.asset_id) {
        Ok(pixels) => Ok(pixels),
        Err(error) => {
            if matches!(
                &error,
                ProjectError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound
            ) {
                return Err(ProjectGifExportError::MissingAssetFile {
                    frame_id: clip.id,
                    asset_id: clip.asset_id,
                    path: snapshot.assets.asset_path(clip.asset_id),
                });
            }
            if matches!(error, ProjectError::CorruptAsset { .. }) {
                return Err(ProjectGifExportError::CorruptAsset {
                    frame_id: clip.id,
                    asset_id: clip.asset_id,
                    source: error,
                });
            }
            Err(ProjectGifExportError::ReadAsset {
                frame_id: clip.id,
                asset_id: clip.asset_id,
                source: error,
            })
        }
    }
}

#[derive(Debug)]
struct LoadedAssetProvider {
    assets: BTreeMap<AssetId, RgbaSurface>,
}

impl FrameAssetProvider for LoadedAssetProvider {
    fn load_rgba8(&self, asset_id: AssetId) -> Result<RgbaSurface, AssetProviderError> {
        self.assets.get(&asset_id).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("preloaded frame asset {asset_id} is unavailable"),
            )
            .into()
        })
    }
}

struct RenderCancellationAdapter<'a>(&'a dyn GifCancellationToken);

impl RenderCancellationToken for RenderCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct GifProgressAdapter<'a> {
    sink: &'a mut dyn ProjectExportProgressSink,
    state: &'a mut ProjectExportProgress,
}

impl ProgressSink for GifProgressAdapter<'_> {
    fn report(&mut self, progress: EncodeProgress) {
        self.state.phase = ProjectExportPhase::Encoding;
        self.state.frames_encoded = progress.frames_read.min(self.state.total_frames);
        self.sink.report(*self.state);
    }
}

fn ensure_not_cancelled(
    cancellation: &dyn GifCancellationToken,
) -> Result<(), ProjectGifExportError> {
    if cancellation.is_cancelled() {
        Err(ProjectGifExportError::Cancelled)
    } else {
        Ok(())
    }
}

fn output_parent(output: &Path) -> Result<&Path, ProjectGifExportError> {
    output
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ProjectGifExportError::InvalidOutputPath(output.to_path_buf()))?;
    Ok(output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new(".")))
}

fn try_exists(path: &Path, operation: &'static str) -> Result<bool, ProjectGifExportError> {
    path.try_exists()
        .map_err(|source| ProjectGifExportError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })
}

fn create_temporary(parent: &Path, output: &Path) -> Result<NamedTempFile, ProjectGifExportError> {
    tempfile::Builder::new()
        .prefix(".gif-from-screen-")
        .suffix(".partial")
        .tempfile_in(parent)
        .map_err(|source| ProjectGifExportError::Io {
            operation: "create temporary GIF",
            path: output.to_path_buf(),
            source,
        })
}

fn persist_temporary(
    temporary: NamedTempFile,
    output: &Path,
    overwrite_existing: bool,
) -> Result<(), ProjectGifExportError> {
    let result = if overwrite_existing {
        temporary.persist(output)
    } else {
        temporary.persist_noclobber(output)
    };
    match result {
        Ok(file) => {
            drop(file);
            Ok(())
        }
        Err(error) if !overwrite_existing && error.error.kind() == io::ErrorKind::AlreadyExists => {
            Err(ProjectGifExportError::ExistingOutput(output.to_path_buf()))
        }
        Err(error) => Err(ProjectGifExportError::Io {
            operation: "atomically commit GIF",
            path: output.to_path_buf(),
            source: error.error,
        }),
    }
}

fn sync_directory_after_commit(
    directory: &Path,
    output: &Path,
) -> Result<(), ProjectGifExportError> {
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|source| ProjectGifExportError::CommitDurabilityUnknown {
            path: output.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use gif_from_screen_domain::{
        AssetDescriptor, BlendMode, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
        ColorSpace, DurationUs, EdgeWidths, EditCommand, Effect, FrameClip, OverlayContent,
        OverlayItem, OverlayTrack, PhysicalPoint, PhysicalPx, PhysicalSize, ProjectId,
        ProjectManifest, Rgba, ShapeKind, SlideDirection, StrokePoint, TimelineSpan, TrackId,
        Transition, TransitionId, TransitionKind, UnixTimeMs,
    };
    use gif_from_screen_gif::{CancellationFlag, DitherMode, PaletteMode};
    use gif_from_screen_project::LockPolicy;
    use tempfile::tempdir;

    use super::*;

    #[derive(Clone)]
    struct TestClip {
        id: FrameId,
        pixels: Vec<u8>,
        duration_us: u64,
        transform: ClipTransform,
        effects: Vec<Effect>,
        encoding: RasterEncoding,
    }

    impl TestClip {
        fn rgba(id: u128, pixels: &[u8], duration_us: u64) -> Self {
            Self {
                id: FrameId::from_u128(id),
                pixels: pixels.to_vec(),
                duration_us,
                transform: ClipTransform::default(),
                effects: Vec::new(),
                encoding: RasterEncoding::Rgba8,
            }
        }
    }

    fn snapshot(
        root: &Path,
        size: PhysicalSize,
        specs: &[TestClip],
    ) -> (ProjectExportSnapshot, Vec<AssetId>) {
        let manifest = ProjectManifest::new(
            ProjectId::from_u128(99),
            "export-test",
            UnixTimeMs::new(123),
            Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        let mut project = ActiveProject::create(root, manifest).unwrap();
        let mut descriptors = BTreeMap::new();
        let mut clips = Vec::new();
        let mut asset_ids = Vec::new();
        for spec in specs {
            let asset_id = project.assets().put(&spec.pixels).unwrap();
            asset_ids.push(asset_id);
            descriptors.entry(asset_id).or_insert(AssetDescriptor {
                id: asset_id,
                byte_len: u64::try_from(spec.pixels.len()).unwrap(),
                kind: AssetKind::Frame {
                    size,
                    encoding: spec.encoding,
                },
            });
            clips.push(FrameClip {
                id: spec.id,
                asset_id,
                duration: DurationUs::new(spec.duration_us).unwrap(),
                transform: spec.transform,
                capture_metadata: CaptureMetadata::default(),
                effects: spec.effects.clone(),
            });
        }
        let mut commands: Vec<_> = descriptors
            .into_values()
            .map(|asset| EditCommand::RegisterAsset { asset })
            .collect();
        commands.push(EditCommand::InsertFrames {
            index: 0,
            frames: clips,
        });
        project.commit(EditCommand::Compound { commands }).unwrap();
        project.checkpoint_and_compact().unwrap();
        let snapshot = ProjectExportSnapshot::from_active(&project);
        drop(project);
        (snapshot, asset_ids)
    }

    fn decode_rgba(path: &Path) -> Vec<(u16, Vec<u8>)> {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(File::open(path).unwrap()).unwrap();
        let mut frames = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            frames.push((frame.delay, frame.buffer.to_vec()));
        }
        frames
    }

    fn export(
        snapshot: &ProjectExportSnapshot,
        output: &Path,
        options: &ProjectGifExportOptions,
    ) -> Result<ProjectGifExportReport, ProjectGifExportError> {
        let mut progress = NoopProjectExportProgress;
        export_project_snapshot_to_gif(
            snapshot,
            output,
            options,
            &gif_from_screen_gif::NeverCancel,
            &mut progress,
        )
    }

    fn partial_files(directory: &Path) -> usize {
        fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
            .count()
    }

    fn add_transition(
        snapshot: &mut ProjectExportSnapshot,
        from: u128,
        to: u128,
        duration_us: u64,
        steps: u16,
        kind: TransitionKind,
    ) {
        snapshot.manifest.timeline.transitions.push(Transition {
            id: TransitionId::from_u128(from * 100 + to),
            from_frame: FrameId::from_u128(from),
            to_frame: FrameId::from_u128(to),
            duration: DurationUs::new(duration_us).unwrap(),
            steps,
            kind,
        });
    }

    fn add_overlay_asset(
        snapshot: &mut ProjectExportSnapshot,
        size: PhysicalSize,
        pixels: &[u8],
    ) -> AssetId {
        let asset_id = snapshot.assets.put(pixels).unwrap();
        snapshot.manifest.assets.insert(
            asset_id,
            AssetDescriptor {
                id: asset_id,
                byte_len: u64::try_from(pixels.len()).unwrap(),
                kind: AssetKind::OverlayImage {
                    size,
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        asset_id
    }

    fn raster_overlay_track(
        asset_id: AssetId,
        span: TimelineSpan,
        position: PhysicalPoint,
        size: PhysicalSize,
    ) -> OverlayTrack {
        OverlayTrack {
            id: TrackId::from_u128(1),
            name: "watermark".to_owned(),
            visible: true,
            opacity: 255,
            blend_mode: BlendMode::Normal,
            items: vec![OverlayItem {
                id: OverlayId::from_u128(1),
                span,
                z_index: 0,
                content: OverlayContent::Raster {
                    asset_id,
                    position,
                    size,
                    opacity: 255,
                },
            }],
        }
    }

    #[test]
    fn raster_roles_reopen_and_export_one_shared_frame_and_overlay_asset() {
        let size = PhysicalSize::new(1, 1).unwrap();
        let pixel = [12, 34, 56, 255];
        for kind in [
            AssetKind::Frame {
                size,
                encoding: RasterEncoding::Rgba8,
            },
            AssetKind::OverlayImage {
                size,
                encoding: RasterEncoding::Rgba8,
            },
            AssetKind::Mask {
                size,
                encoding: RasterEncoding::Rgba8,
            },
        ] {
            let directory = tempdir().unwrap();
            let (mut source, ids) = snapshot(
                &directory.path().join("fixture"),
                size,
                &[TestClip::rgba(1, &pixel, 10_000)],
            );
            source.manifest.assets.get_mut(&ids[0]).unwrap().kind = kind.clone();
            source
                .manifest
                .timeline
                .overlay_tracks
                .push(raster_overlay_track(
                    ids[0],
                    TimelineSpan {
                        start: TimeUs::ZERO,
                        duration: DurationUs::new(10_000).unwrap(),
                    },
                    PhysicalPoint::default(),
                    size,
                ));
            let root = directory.path().join("shared.gfsproj");
            let project = ActiveProject::create(&root, source.manifest).unwrap();
            assert_eq!(project.assets().put(&pixel).unwrap(), ids[0]);
            drop(project);
            let reopened = ActiveProject::open(&root, LockPolicy::FailIfPresent).unwrap();
            assert!(reopened.asset_issues.is_empty());
            assert_eq!(reopened.project.manifest().assets.len(), 1);
            assert_eq!(reopened.project.manifest().assets[&ids[0]].kind, kind);
            let snapshot = ProjectExportSnapshot::from_active(&reopened.project);
            let output = directory.path().join("shared.gif");
            export(
                &snapshot,
                &output,
                &ProjectGifExportOptions {
                    render_buffer_limit_bytes: 8,
                    ..ProjectGifExportOptions::default()
                },
            )
            .unwrap();
            assert_eq!(decode_rgba(&output), vec![(1, pixel.to_vec())]);
        }
    }

    #[test]
    fn shared_raster_roles_still_reject_non_rasters_encoding_and_size_mismatches() {
        let directory = tempdir().unwrap();
        let size = PhysicalSize::new(1, 1).unwrap();
        let (mut snapshot, ids) = snapshot(
            &directory.path().join("project"),
            size,
            &[TestClip::rgba(1, &[12, 34, 56, 255], 10_000)],
        );
        let output = directory.path().join("invalid.gif");
        for media_type in ["image/png", "video/mp4", "audio/wav", "font/ttf"] {
            snapshot.manifest.assets.get_mut(&ids[0]).unwrap().kind = AssetKind::ImportedSource {
                media_type: media_type.into(),
            };
            assert!(matches!(
                export(&snapshot, &output, &ProjectGifExportOptions::default()),
                Err(ProjectGifExportError::InvalidAssetKind { .. })
            ));
        }
        snapshot.manifest.assets.get_mut(&ids[0]).unwrap().kind = AssetKind::OverlayImage {
            size,
            encoding: RasterEncoding::Png,
        };
        assert!(matches!(
            export(&snapshot, &output, &ProjectGifExportOptions::default()),
            Err(ProjectGifExportError::UnsupportedAssetEncoding { .. })
        ));
        snapshot.manifest.assets.get_mut(&ids[0]).unwrap().kind = AssetKind::Mask {
            size: PhysicalSize::new(2, 1).unwrap(),
            encoding: RasterEncoding::Rgba8,
        };
        assert!(matches!(
            export(&snapshot, &output, &ProjectGifExportOptions::default()),
            Err(ProjectGifExportError::InvalidAssetSurface { .. })
        ));
        let descriptor = snapshot.manifest.assets.get_mut(&ids[0]).unwrap();
        descriptor.kind = AssetKind::OverlayImage {
            size,
            encoding: RasterEncoding::Rgba8,
        };
        descriptor.byte_len = 8;
        assert!(matches!(
            export(&snapshot, &output, &ProjectGifExportOptions::default()),
            Err(ProjectGifExportError::AssetLengthMismatch { .. })
        ));
        assert!(!output.exists());
        assert_eq!(partial_files(directory.path()), 0);
    }

    #[test]
    fn raster_overlay_export_uses_original_half_open_frame_times_in_any_selection_order() {
        let directory = tempdir().unwrap();
        let blue = [0, 0, 255, 255].repeat(2);
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(2, 1).unwrap(),
            &[
                TestClip::rgba(1, &blue, 10_000),
                TestClip::rgba(2, &blue, 10_000),
            ],
        );
        let overlay_asset = add_overlay_asset(
            &mut snapshot,
            PhysicalSize::new(1, 1).unwrap(),
            &[255, 0, 0, 255],
        );
        snapshot
            .manifest
            .timeline
            .overlay_tracks
            .push(raster_overlay_track(
                overlay_asset,
                TimelineSpan {
                    start: TimeUs::ZERO,
                    duration: DurationUs::new(10_000).unwrap(),
                },
                PhysicalPoint {
                    x: PhysicalPx::new(1),
                    y: PhysicalPx::ZERO,
                },
                PhysicalSize::new(1, 1).unwrap(),
            ));

        let forward = directory.path().join("overlay-forward.gif");
        export(&snapshot, &forward, &ProjectGifExportOptions::default()).unwrap();
        assert_eq!(
            decode_rgba(&forward),
            [(1, vec![0, 0, 255, 255, 255, 0, 0, 255]), (1, blue.clone()),]
        );

        let reverse = directory.path().join("overlay-reverse.gif");
        let options = ProjectGifExportOptions {
            frames: ProjectFrameSelection::Ordered(vec![
                FrameId::from_u128(2),
                FrameId::from_u128(1),
            ]),
            ..ProjectGifExportOptions::default()
        };
        export(&snapshot, &reverse, &options).unwrap();
        assert_eq!(
            decode_rgba(&reverse),
            [(1, blue), (1, vec![0, 0, 255, 255, 255, 0, 0, 255]),]
        );

        add_transition(&mut snapshot, 1, 2, 10_000, 1, TransitionKind::FadeToNext);
        let transitioned = directory.path().join("overlay-transition.gif");
        export(
            &snapshot,
            &transitioned,
            &ProjectGifExportOptions::default(),
        )
        .unwrap();
        assert_eq!(
            decode_rgba(&transitioned),
            [
                (1, vec![0, 0, 255, 255, 255, 0, 0, 255]),
                (1, vec![0, 0, 255, 255, 128, 0, 128, 255]),
                (1, vec![0, 0, 255, 255, 0, 0, 255, 255]),
            ]
        );
    }

    #[test]
    fn raster_overlay_reuses_an_identical_frame_asset_without_kind_or_memory_conflict() {
        let directory = tempdir().unwrap();
        let pixels = [255, 0, 0, 255, 0, 255, 0, 255];
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(2, 1).unwrap(),
            &[TestClip::rgba(1, &pixels, 10_000)],
        );
        let frame_asset = snapshot.manifest.timeline.frames[0].asset_id;
        snapshot
            .manifest
            .timeline
            .overlay_tracks
            .push(raster_overlay_track(
                frame_asset,
                TimelineSpan {
                    start: TimeUs::ZERO,
                    duration: DurationUs::new(10_000).unwrap(),
                },
                PhysicalPoint {
                    x: PhysicalPx::new(1),
                    y: PhysicalPx::ZERO,
                },
                PhysicalSize::new(1, 1).unwrap(),
            ));
        let output = directory.path().join("same-frame-overlay.gif");
        let options = ProjectGifExportOptions {
            // One retained 8-byte asset plus one 8-byte rendered surface; the shared asset must
            // not be loaded or counted twice for its overlay role.
            render_buffer_limit_bytes: 16,
            ..ProjectGifExportOptions::default()
        };

        export(&snapshot, &output, &options).unwrap();
        assert_eq!(
            decode_rgba(&output),
            [(1, vec![255, 0, 0, 255, 255, 0, 0, 255])]
        );
    }

    #[test]
    fn shape_and_drawing_overlays_flow_through_project_export() {
        let directory = tempdir().unwrap();
        let black = [0, 0, 0, 255].repeat(3);
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(3, 1).unwrap(),
            &[TestClip::rgba(1, &black, 10_000)],
        );
        let span = TimelineSpan {
            start: TimeUs::ZERO,
            duration: DurationUs::new(10_000).unwrap(),
        };
        snapshot
            .manifest
            .timeline
            .overlay_tracks
            .push(OverlayTrack {
                id: TrackId::from_u128(1),
                name: "vector watermark".to_owned(),
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
                            bounds: gif_from_screen_domain::PhysicalRect::new(0, 0, 3, 1).unwrap(),
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
            });
        let output = directory.path().join("vector-overlays.gif");

        export(&snapshot, &output, &ProjectGifExportOptions::default()).unwrap();

        assert_eq!(
            decode_rgba(&output),
            [(1, vec![255, 0, 0, 255, 0, 255, 0, 255, 255, 0, 0, 255])]
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario verifies every pre-output overlay asset rejection and memory boundary"
    )]
    fn active_overlay_asset_validation_and_memory_fail_before_output_creation() {
        let directory = tempdir().unwrap();
        let blue = [0, 0, 255, 255].repeat(2);
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(2, 1).unwrap(),
            &[TestClip::rgba(1, &blue, 10_000)],
        );
        let overlay_asset = add_overlay_asset(
            &mut snapshot,
            PhysicalSize::new(1, 1).unwrap(),
            &[255, 0, 0, 255],
        );
        snapshot
            .manifest
            .timeline
            .overlay_tracks
            .push(raster_overlay_track(
                overlay_asset,
                TimelineSpan {
                    start: TimeUs::ZERO,
                    duration: DurationUs::new(10_000).unwrap(),
                },
                PhysicalPoint::default(),
                PhysicalSize::new(1, 1).unwrap(),
            ));
        let descriptor = snapshot.manifest.assets[&overlay_asset].clone();

        let ignored_asset = AssetId::from_digest([88; 32]);
        let mut ignored = snapshot.clone();
        ignored.manifest.assets.insert(
            ignored_asset,
            AssetDescriptor {
                id: ignored_asset,
                byte_len: 4,
                kind: AssetKind::OverlayImage {
                    size: PhysicalSize::new(1, 1).unwrap(),
                    encoding: RasterEncoding::Rgba8,
                },
            },
        );
        let OverlayContent::Raster { opacity, .. } =
            &mut ignored.manifest.timeline.overlay_tracks[0].items[0].content
        else {
            unreachable!();
        };
        *opacity = 0;
        ignored.manifest.timeline.overlay_tracks[0]
            .items
            .push(OverlayItem {
                id: OverlayId::from_u128(3),
                span: TimelineSpan {
                    start: TimeUs::new(5_000),
                    duration: DurationUs::new(5_000).unwrap(),
                },
                z_index: 2,
                content: OverlayContent::Raster {
                    asset_id: ignored_asset,
                    position: PhysicalPoint::default(),
                    size: PhysicalSize::new(1, 1).unwrap(),
                    opacity: 255,
                },
            });
        ignored.manifest.timeline.overlay_tracks.push(OverlayTrack {
            id: TrackId::from_u128(2),
            name: "hidden missing watermark".to_owned(),
            visible: false,
            opacity: 255,
            blend_mode: BlendMode::Normal,
            items: vec![OverlayItem {
                id: OverlayId::from_u128(2),
                span: TimelineSpan {
                    start: TimeUs::ZERO,
                    duration: DurationUs::new(10_000).unwrap(),
                },
                z_index: 1,
                content: OverlayContent::Raster {
                    asset_id: ignored_asset,
                    position: PhysicalPoint::default(),
                    size: PhysicalSize::new(1, 1).unwrap(),
                    opacity: 255,
                },
            }],
        });
        let ignored_output = directory.path().join("ignored-overlays.gif");
        export(
            &ignored,
            &ignored_output,
            &ProjectGifExportOptions::default(),
        )
        .unwrap();
        assert!(ignored_output.is_file());

        let bounded = ProjectGifExportOptions {
            render_buffer_limit_bytes: 11,
            ..ProjectGifExportOptions::default()
        };
        let output = directory.path().join("overlay-bounded.gif");
        assert!(matches!(
            export(&snapshot, &output, &bounded),
            Err(ProjectGifExportError::RenderBufferLimitExceeded {
                required_bytes: 12,
                limit_bytes: 11,
            })
        ));
        assert!(!output.exists());

        let joint = ProjectGifExportOptions {
            render_buffer_limit_bytes: 19,
            ..ProjectGifExportOptions::default()
        };
        let output = directory.path().join("overlay-source-plus-render.gif");
        assert!(matches!(
            export(&snapshot, &output, &joint),
            Err(ProjectGifExportError::RenderBufferLimitExceeded {
                required_bytes: 20,
                limit_bytes: 19,
            })
        ));
        assert!(!output.exists());

        snapshot.manifest.assets.remove(&overlay_asset);
        assert!(matches!(
            export(
                &snapshot,
                &directory.path().join("overlay-no-descriptor.gif"),
                &ProjectGifExportOptions::default(),
            ),
            Err(ProjectGifExportError::MissingOverlayAssetDescriptor {
                overlay_id,
                asset_id,
            }) if overlay_id == OverlayId::from_u128(1) && asset_id == overlay_asset
        ));
        snapshot
            .manifest
            .assets
            .insert(overlay_asset, descriptor.clone());

        snapshot
            .manifest
            .assets
            .get_mut(&overlay_asset)
            .unwrap()
            .kind = AssetKind::ImportedSource {
            media_type: "image/png".to_owned(),
        };
        assert!(matches!(
            export(
                &snapshot,
                &directory.path().join("overlay-wrong-kind.gif"),
                &ProjectGifExportOptions::default(),
            ),
            Err(ProjectGifExportError::InvalidOverlayAssetKind { .. })
        ));
        snapshot
            .manifest
            .assets
            .get_mut(&overlay_asset)
            .unwrap()
            .kind = AssetKind::Mask {
            size: PhysicalSize::new(1, 1).unwrap(),
            encoding: RasterEncoding::Rgba8,
        };
        let mask_output = directory.path().join("overlay-mask-kind.gif");
        export(&snapshot, &mask_output, &ProjectGifExportOptions::default()).unwrap();
        assert!(mask_output.is_file());
        snapshot
            .manifest
            .assets
            .get_mut(&overlay_asset)
            .unwrap()
            .kind = AssetKind::OverlayImage {
            size: PhysicalSize::new(1, 1).unwrap(),
            encoding: RasterEncoding::Png,
        };
        assert!(matches!(
            export(
                &snapshot,
                &directory.path().join("overlay-encoding.gif"),
                &ProjectGifExportOptions::default(),
            ),
            Err(ProjectGifExportError::UnsupportedOverlayAssetEncoding {
                encoding: RasterEncoding::Png,
                ..
            })
        ));
        snapshot.manifest.assets.insert(overlay_asset, descriptor);
        fs::remove_file(snapshot.assets.asset_path(overlay_asset)).unwrap();
        assert!(matches!(
            export(
                &snapshot,
                &directory.path().join("overlay-missing.gif"),
                &ProjectGifExportOptions::default(),
            ),
            Err(ProjectGifExportError::MissingOverlayAssetFile { .. })
        ));
        assert_eq!(partial_files(directory.path()), 0);
    }

    #[test]
    fn custom_palette_constructor_validates_packed_entries_and_transparent_index() {
        for byte_len in [1, 4, 7] {
            assert_eq!(
                CustomGifPalette::new(vec![0; byte_len], None),
                Err(CustomGifPaletteError::IncompleteRgbEntry { byte_len })
            );
        }
        for color_count in [0, 1, 257] {
            assert_eq!(
                CustomGifPalette::new(vec![0; color_count * 3], None),
                Err(CustomGifPaletteError::ColorCountOutOfRange { color_count })
            );
        }
        assert_eq!(
            CustomGifPalette::new(vec![0; 2 * 3], Some(2)),
            Err(CustomGifPaletteError::TransparentIndexOutOfRange {
                index: 2,
                color_count: 2,
            })
        );

        let minimum = CustomGifPalette::new(vec![0, 0, 0, 255, 255, 255], Some(1)).unwrap();
        assert_eq!(minimum.color_count(), 2);
        assert_eq!(minimum.transparent_index(), Some(1));
        assert_eq!(minimum.packed_rgb(), [0, 0, 0, 255, 255, 255]);
        assert_eq!(minimum.clone(), minimum);
        let maximum = CustomGifPalette::new(vec![0; 256 * 3], Some(255)).unwrap();
        assert_eq!(maximum.color_count(), 256);
    }

    #[test]
    fn custom_palette_roundtrips_local_global_and_every_dither_mode() {
        let directory = tempdir().unwrap();
        let first = [
            0, 0, 0, 0, 32, 32, 32, 255, 224, 224, 224, 255, 255, 0, 0, 255,
        ];
        let second = [
            255, 0, 0, 255, 0, 0, 0, 0, 96, 96, 96, 255, 255, 255, 255, 255,
        ];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(2, 2).unwrap(),
            &[
                TestClip::rgba(1, &first, 10_000),
                TestClip::rgba(2, &second, 20_000),
            ],
        );
        let packed_rgb = vec![
            0, 0, 0, // transparent black
            0, 0, 0, // opaque black
            255, 255, 255, // white
            255, 0, 0, // red
        ];
        let palette = CustomGifPalette::new(packed_rgb.clone(), Some(0)).unwrap();
        let dithers = [
            DitherMode::None,
            DitherMode::Bayer4x4,
            DitherMode::FloydSteinberg,
            DitherMode::Atkinson,
            DitherMode::Burkes,
            DitherMode::SierraLite,
            DitherMode::TwoRowSierra,
            DitherMode::Sierra,
            DitherMode::JarvisJudiceNinke,
            DitherMode::Stucki,
            DitherMode::StevensonArce,
        ];

        for palette_mode in [PaletteMode::LocalPerFrame, PaletteMode::Global] {
            for dither in dithers {
                let output = directory
                    .path()
                    .join(format!("custom-{palette_mode:?}-{dither:?}.gif"));
                let options = ProjectGifExportOptions {
                    encoding: EncodeOptions {
                        max_colors: 4,
                        merge_duplicate_frames: false,
                        transparency: Transparency::AlphaThreshold(128),
                        palette_mode,
                        dither,
                        ..EncodeOptions::default()
                    },
                    custom_palette: Some(palette.clone()),
                    ..ProjectGifExportOptions::default()
                };
                let report = export(&snapshot, &output, &options).unwrap();
                assert_eq!(report.encoding.input_frames, 2);

                let mut decoder = gif::DecodeOptions::new()
                    .read_info(File::open(&output).unwrap())
                    .unwrap();
                if palette_mode == PaletteMode::Global {
                    assert_eq!(decoder.global_palette(), Some(packed_rgb.as_slice()));
                }
                let mut decoded_frames = 0;
                while let Some(frame) = decoder.read_next_frame().unwrap() {
                    assert_eq!(
                        frame.palette.is_some(),
                        palette_mode == PaletteMode::LocalPerFrame
                    );
                    if let Some(local) = frame.palette.as_deref() {
                        assert_eq!(local, packed_rgb);
                    }
                    assert_eq!(frame.transparent, Some(0));
                    decoded_frames += 1;
                }
                assert_eq!(decoded_frames, 2);

                let rgba = decode_rgba(&output);
                assert_eq!(rgba[0].1[3], 0);
                assert_eq!(rgba[1].1[7], 0);
            }
        }
    }

    #[test]
    fn custom_palette_conflicts_are_typed_before_temporary_file_creation() {
        let directory = tempdir().unwrap();
        let transparent_pixel = [10, 20, 30, 0];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[TestClip::rgba(1, &transparent_pixel, 10_000)],
        );
        let output = directory.path().join("preserved.gif");
        fs::write(&output, b"previous GIF").unwrap();
        let three_colors =
            CustomGifPalette::new(vec![0, 0, 0, 128, 128, 128, 255, 255, 255], Some(0)).unwrap();
        let conflict = ProjectGifExportOptions {
            encoding: EncodeOptions {
                max_colors: 2,
                ..EncodeOptions::default()
            },
            custom_palette: Some(three_colors),
            overwrite_existing: true,
            ..ProjectGifExportOptions::default()
        };
        assert!(matches!(
            export(&snapshot, &output, &conflict),
            Err(ProjectGifExportError::CustomPaletteExceedsColorLimit {
                palette_colors: 3,
                max_colors: 2,
            })
        ));
        assert_eq!(fs::read(&output).unwrap(), b"previous GIF");
        assert_eq!(partial_files(directory.path()), 0);

        let no_transparency = CustomGifPalette::new(vec![0, 0, 0, 255, 255, 255], None).unwrap();
        let missing_output = directory.path().join("missing-transparency.gif");
        let missing = ProjectGifExportOptions {
            custom_palette: Some(no_transparency.clone()),
            ..ProjectGifExportOptions::default()
        };
        assert!(matches!(
            export(&snapshot, &missing_output, &missing),
            Err(ProjectGifExportError::CustomPaletteMissingTransparency {
                frame_index: 0,
                alpha_threshold: 1,
            })
        ));
        assert!(!missing_output.exists());
        assert_eq!(partial_files(directory.path()), 0);

        let invalid_limit_output = directory.path().join("invalid-limit.gif");
        let invalid_limit = ProjectGifExportOptions {
            encoding: EncodeOptions {
                max_colors: 300,
                transparency: Transparency::Opaque,
                ..EncodeOptions::default()
            },
            custom_palette: Some(no_transparency.clone()),
            ..ProjectGifExportOptions::default()
        };
        assert!(matches!(
            export(&snapshot, &invalid_limit_output, &invalid_limit),
            Err(ProjectGifExportError::InvalidCustomPaletteColorLimit { max_colors: 300 })
        ));
        assert!(!invalid_limit_output.exists());
        assert_eq!(partial_files(directory.path()), 0);

        let opaque_output = directory.path().join("opaque-custom.gif");
        let opaque = ProjectGifExportOptions {
            encoding: EncodeOptions {
                max_colors: 2,
                transparency: Transparency::Opaque,
                ..EncodeOptions::default()
            },
            custom_palette: Some(no_transparency),
            ..ProjectGifExportOptions::default()
        };
        export(&snapshot, &opaque_output, &opaque).unwrap();
        assert!(opaque_output.is_file());
    }

    #[test]
    fn custom_palette_transparency_preflight_checks_cancellation_by_row() {
        struct CancelAfterChecks {
            checks: AtomicUsize,
            cancel_at: usize,
        }

        impl GifCancellationToken for CancelAfterChecks {
            fn is_cancelled(&self) -> bool {
                self.checks.fetch_add(1, Ordering::Relaxed) >= self.cancel_at
            }
        }

        let frame = RgbaFrame::new(2, 3, [10, 20, 30, 255].repeat(6), 10_000).unwrap();
        let options = ProjectGifExportOptions {
            encoding: EncodeOptions {
                max_colors: 2,
                transparency: Transparency::AlphaThreshold(255),
                ..EncodeOptions::default()
            },
            custom_palette: Some(
                CustomGifPalette::new(vec![0, 0, 0, 255, 255, 255], None).unwrap(),
            ),
            ..ProjectGifExportOptions::default()
        };
        let cancellation = CancelAfterChecks {
            checks: AtomicUsize::new(0),
            cancel_at: 3,
        };

        assert!(matches!(
            encoder_for_options(&options, &[frame], &cancellation),
            Err(ProjectGifExportError::Cancelled)
        ));
        assert_eq!(cancellation.checks.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn fade_transition_exports_intermediates_with_exact_added_timing_and_progress() {
        let directory = tempdir().unwrap();
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[
                TestClip::rgba(1, &red, 10_000),
                TestClip::rgba(2, &blue, 20_000),
            ],
        );
        add_transition(&mut snapshot, 1, 2, 20_000, 2, TransitionKind::FadeToNext);
        let output = directory.path().join("fade.gif");
        let mut updates = Vec::new();
        let report = export_project_snapshot_to_gif(
            &snapshot,
            &output,
            &ProjectGifExportOptions::default(),
            &gif_from_screen_gif::NeverCancel,
            &mut |update| updates.push(update),
        )
        .unwrap();

        assert_eq!(report.selected_frames, 2);
        assert_eq!(report.encoding.input_frames, 4);
        assert_eq!(report.encoding.input_duration_us, 50_000);
        assert_eq!(
            decode_rgba(&output),
            [
                (1, red.to_vec()),
                (1, vec![170, 0, 85, 255]),
                (1, vec![85, 0, 170, 255]),
                (2, blue.to_vec()),
            ]
        );
        assert_eq!(updates.last().unwrap().total_frames, 4);
        assert_eq!(updates.last().unwrap().frames_rendered, 4);
        assert_eq!(updates.last().unwrap().frames_encoded, 4);
    }

    #[test]
    fn slide_transition_pixels_and_duration_remainder_are_deterministic() {
        let directory = tempdir().unwrap();
        let red_green = [255, 0, 0, 255, 0, 255, 0, 255];
        let blue_yellow = [0, 0, 255, 255, 255, 255, 0, 255];
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(2, 1).unwrap(),
            &[
                TestClip::rgba(1, &red_green, 10_000),
                TestClip::rgba(2, &blue_yellow, 10_000),
            ],
        );
        add_transition(
            &mut snapshot,
            1,
            2,
            10_000,
            1,
            TransitionKind::Slide {
                direction: SlideDirection::Left,
            },
        );
        let output = directory.path().join("slide.gif");
        export(&snapshot, &output, &ProjectGifExportOptions::default()).unwrap();
        let frames = decode_rgba(&output);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1].0, 1);
        assert_eq!(frames[1].1, [0, 255, 0, 255, 0, 0, 255, 255]);

        let transition = Transition {
            id: TransitionId::from_u128(1),
            from_frame: FrameId::from_u128(1),
            to_frame: FrameId::from_u128(2),
            duration: DurationUs::new(10).unwrap(),
            steps: 3,
            kind: TransitionKind::FadeToNext,
        };
        assert_eq!(
            (0..3)
                .map(|step| transition_step_duration(&transition, step))
                .collect::<Vec<_>>(),
            [4, 3, 3]
        );
    }

    #[test]
    fn reverse_and_nonadjacent_selections_do_not_apply_transitions() {
        let directory = tempdir().unwrap();
        let colors = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]];
        let specs = colors
            .iter()
            .enumerate()
            .map(|(index, color)| TestClip::rgba(u128::try_from(index).unwrap() + 1, color, 10_000))
            .collect::<Vec<_>>();
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &specs,
        );
        add_transition(&mut snapshot, 1, 2, 10_000, 1, TransitionKind::FadeToNext);
        add_transition(&mut snapshot, 2, 3, 10_000, 1, TransitionKind::FadeToNext);

        for (name, selection) in [
            (
                "reverse",
                vec![FrameId::from_u128(2), FrameId::from_u128(1)],
            ),
            ("gap", vec![FrameId::from_u128(1), FrameId::from_u128(3)]),
        ] {
            let output = directory.path().join(format!("{name}.gif"));
            let options = ProjectGifExportOptions {
                frames: ProjectFrameSelection::Ordered(selection),
                ..ProjectGifExportOptions::default()
            };
            let report = export(&snapshot, &output, &options).unwrap();
            assert_eq!(report.encoding.input_frames, 2);
            assert_eq!(decode_rgba(&output).len(), 2);
        }
    }

    #[test]
    fn transition_memory_and_cancellation_are_checked_before_encoding() {
        let directory = tempdir().unwrap();
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[
                TestClip::rgba(1, &red, 10_000),
                TestClip::rgba(2, &blue, 10_000),
            ],
        );
        add_transition(&mut snapshot, 1, 2, 10_000, 1, TransitionKind::FadeToNext);
        let bounded_output = directory.path().join("bounded-transition.gif");
        let bounded = ProjectGifExportOptions {
            render_buffer_limit_bytes: 19,
            ..ProjectGifExportOptions::default()
        };
        assert!(matches!(
            export(&snapshot, &bounded_output, &bounded),
            Err(ProjectGifExportError::RenderBufferLimitExceeded {
                required_bytes: 20,
                limit_bytes: 19,
            })
        ));
        assert!(!bounded_output.exists());

        let cancellation = CancellationFlag::default();
        let cancel_from_progress = cancellation.clone();
        let cancelled_output = directory.path().join("cancel-transition.gif");
        let error = export_project_snapshot_to_gif(
            &snapshot,
            &cancelled_output,
            &ProjectGifExportOptions::default(),
            &cancellation,
            &mut move |progress: ProjectExportProgress| {
                if progress.phase == ProjectExportPhase::Rendering && progress.frames_rendered == 1
                {
                    cancel_from_progress.cancel();
                }
            },
        )
        .unwrap_err();
        assert!(matches!(error, ProjectGifExportError::Cancelled));
        assert!(!cancelled_output.exists());
    }

    #[test]
    fn expanded_output_duration_overflow_is_typed_before_rendering() {
        let directory = tempdir().unwrap();
        let pixel = [1, 2, 3, 255];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[
                TestClip::rgba(1, &pixel, 10_000),
                TestClip::rgba(2, &pixel, 10_000),
            ],
        );
        let mut clips = snapshot.manifest.timeline.frames.clone();
        clips[0].duration = DurationUs::new(u64::MAX).unwrap();
        clips[1].duration = DurationUs::new(1).unwrap();

        assert!(matches!(
            validate_expanded_duration(&clips, &[None]),
            Err(ProjectGifExportError::OutputDurationOverflow)
        ));
        assert!(matches!(
            ensure_render_buffer(u64::MAX, 1, &[], u64::MAX),
            Err(ProjectGifExportError::RenderBufferSizeOverflow)
        ));
        let transition = Transition {
            id: TransitionId::from_u128(1),
            from_frame: FrameId::from_u128(1),
            to_frame: FrameId::from_u128(2),
            duration: DurationUs::new(1).unwrap(),
            steps: 1,
            kind: TransitionKind::FadeToNext,
        };
        assert!(matches!(
            expanded_frame_count(usize::MAX, &[Some(transition)]),
            Err(ProjectGifExportError::OutputFrameCountOverflow)
        ));
    }

    #[test]
    fn duplicate_transition_endpoints_are_rejected_in_export_snapshot() {
        let directory = tempdir().unwrap();
        let pixel = [1, 2, 3, 255];
        let (mut snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[
                TestClip::rgba(1, &pixel, 10_000),
                TestClip::rgba(2, &pixel, 10_000),
            ],
        );
        add_transition(&mut snapshot, 1, 2, 10_000, 1, TransitionKind::FadeToNext);
        let mut duplicate = snapshot.manifest.timeline.transitions[0].clone();
        duplicate.id = TransitionId::from_u128(999);
        snapshot.manifest.timeline.transitions.push(duplicate);

        assert!(matches!(
            export(
                &snapshot,
                &directory.path().join("duplicate-transition.gif"),
                &ProjectGifExportOptions::default(),
            ),
            Err(ProjectGifExportError::DuplicateTransitionEndpoints { .. })
        ));
    }

    #[test]
    fn explicit_selection_preserves_reverse_order_and_variable_durations() {
        let directory = tempdir().unwrap();
        let project_root = directory.path().join("project");
        let red = [255, 0, 0, 255];
        let green = [0, 255, 0, 255];
        let blue = [0, 0, 255, 255];
        let (snapshot, _) = snapshot(
            &project_root,
            PhysicalSize::new(1, 1).unwrap(),
            &[
                TestClip::rgba(1, &red, 10_000),
                TestClip::rgba(2, &green, 20_000),
                TestClip::rgba(3, &blue, 30_000),
            ],
        );
        let output = directory.path().join("reverse.gif");
        let options = ProjectGifExportOptions {
            frames: ProjectFrameSelection::Ordered(vec![
                FrameId::from_u128(3),
                FrameId::from_u128(1),
            ]),
            ..ProjectGifExportOptions::default()
        };

        // Holding a newly opened project proves the snapshot/export path does
        // not try to reacquire or retain the ActiveProject lock.
        let _reopened = ActiveProject::open(&project_root, LockPolicy::FailIfPresent).unwrap();
        let report = export(&snapshot, &output, &options).unwrap();
        assert_eq!(report.selected_frames, 2);
        assert_eq!(report.revision, ProjectRevision::new(1));
        let frames = decode_rgba(&output);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], (3, blue.to_vec()));
        assert_eq!(frames[1], (1, red.to_vec()));
    }

    #[test]
    fn cpu_renderer_applies_transform_then_effects_before_encoding() {
        let directory = tempdir().unwrap();
        let mut clip = TestClip::rgba(1, &[255, 0, 0, 255, 0, 0, 255, 255], 10_000);
        clip.transform.flip_horizontal = true;
        clip.effects.push(Effect::Border {
            widths: EdgeWidths {
                left: 1,
                ..EdgeWidths::default()
            },
            color: Rgba {
                red: 0,
                green: 255,
                blue: 0,
                alpha: 255,
            },
        });
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(2, 1).unwrap(),
            &[clip],
        );
        let output = directory.path().join("rendered.gif");

        export(&snapshot, &output, &ProjectGifExportOptions::default()).unwrap();

        let frames = decode_rgba(&output);
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].1,
            [0, 255, 0, 255, 255, 0, 0, 255],
            "left border is green and the flipped right pixel is original red"
        );
    }

    #[test]
    fn selection_rejects_empty_unknown_and_duplicate_ids() {
        let directory = tempdir().unwrap();
        let pixel = [1, 2, 3, 255];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[TestClip::rgba(1, &pixel, 10_000)],
        );
        let output = directory.path().join("selection.gif");

        for selection in [
            ProjectFrameSelection::Ordered(Vec::new()),
            ProjectFrameSelection::Ordered(vec![FrameId::from_u128(2)]),
            ProjectFrameSelection::Ordered(vec![FrameId::from_u128(1), FrameId::from_u128(1)]),
        ] {
            let options = ProjectGifExportOptions {
                frames: selection,
                ..ProjectGifExportOptions::default()
            };
            let error = export(&snapshot, &output, &options).unwrap_err();
            assert!(matches!(
                error,
                ProjectGifExportError::EmptySelection
                    | ProjectGifExportError::UnknownFrame { .. }
                    | ProjectGifExportError::DuplicateFrame { .. }
            ));
            assert!(!output.exists());
        }
    }

    #[test]
    fn cancellation_during_encoding_cleans_partial_and_does_not_publish_output() {
        let directory = tempdir().unwrap();
        let pixel = [1, 2, 3, 255];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[TestClip::rgba(1, &pixel, 10_000)],
        );
        let output = directory.path().join("cancelled.gif");
        let cancellation = CancellationFlag::default();
        let cancel_from_progress = cancellation.clone();
        let mut progress = move |snapshot: ProjectExportProgress| {
            if snapshot.phase == ProjectExportPhase::Encoding {
                cancel_from_progress.cancel();
            }
        };

        let error = export_project_snapshot_to_gif(
            &snapshot,
            &output,
            &ProjectGifExportOptions::default(),
            &cancellation,
            &mut progress,
        )
        .unwrap_err();
        assert!(matches!(error, ProjectGifExportError::Cancelled));
        assert!(!output.exists());
        assert_eq!(partial_files(directory.path()), 0);
    }

    #[test]
    fn cancelled_overwrite_preserves_the_previous_file() {
        let directory = tempdir().unwrap();
        let pixel = [1, 2, 3, 255];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[TestClip::rgba(1, &pixel, 10_000)],
        );
        let output = directory.path().join("preserved.gif");
        fs::write(&output, b"previous GIF").unwrap();
        let cancellation = CancellationFlag::default();
        let cancel_from_progress = cancellation.clone();
        let mut progress = move |snapshot: ProjectExportProgress| {
            if snapshot.phase == ProjectExportPhase::Encoding {
                cancel_from_progress.cancel();
            }
        };
        let options = ProjectGifExportOptions {
            overwrite_existing: true,
            ..ProjectGifExportOptions::default()
        };

        let error = export_project_snapshot_to_gif(
            &snapshot,
            &output,
            &options,
            &cancellation,
            &mut progress,
        )
        .unwrap_err();
        assert!(matches!(error, ProjectGifExportError::Cancelled));
        assert_eq!(fs::read(&output).unwrap(), b"previous GIF");
        assert_eq!(partial_files(directory.path()), 0);
    }

    #[test]
    fn resident_render_buffers_are_bounded_before_encoding() {
        let directory = tempdir().unwrap();
        let pixel = [1, 2, 3, 255];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[TestClip::rgba(1, &pixel, 10_000)],
        );
        let output = directory.path().join("bounded.gif");
        let options = ProjectGifExportOptions {
            // Four source bytes plus four rendered bytes require eight.
            render_buffer_limit_bytes: 7,
            ..ProjectGifExportOptions::default()
        };

        let error = export(&snapshot, &output, &options).unwrap_err();
        assert!(matches!(
            error,
            ProjectGifExportError::RenderBufferLimitExceeded {
                required_bytes: 8,
                limit_bytes: 7
            }
        ));
        assert!(!output.exists());
        assert_eq!(partial_files(directory.path()), 0);
    }

    #[test]
    fn progress_reports_ordered_phases_and_monotonic_frame_counts() {
        let directory = tempdir().unwrap();
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[
                TestClip::rgba(1, &red, 10_000),
                TestClip::rgba(2, &blue, 20_000),
            ],
        );
        let output = directory.path().join("progress.gif");
        let mut updates = Vec::new();
        let mut progress = |update| updates.push(update);

        export_project_snapshot_to_gif(
            &snapshot,
            &output,
            &ProjectGifExportOptions::default(),
            &gif_from_screen_gif::NeverCancel,
            &mut progress,
        )
        .unwrap();

        let phases = [
            ProjectExportPhase::Preparing,
            ProjectExportPhase::Rendering,
            ProjectExportPhase::Encoding,
            ProjectExportPhase::Syncing,
            ProjectExportPhase::Committing,
            ProjectExportPhase::Complete,
        ];
        let mut previous_position = None;
        for phase in phases {
            let position = updates
                .iter()
                .position(|update| update.phase == phase)
                .expect("each export phase is reported");
            assert!(previous_position.is_none_or(|previous| position > previous));
            previous_position = Some(position);
        }
        assert!(
            updates
                .windows(2)
                .all(|pair| pair[0].frames_rendered <= pair[1].frames_rendered
                    && pair[0].frames_encoded <= pair[1].frames_encoded)
        );
        assert_eq!(updates.last().unwrap().total_frames, 2);
        assert_eq!(updates.last().unwrap().frames_rendered, 2);
        assert_eq!(updates.last().unwrap().frames_encoded, 2);
    }

    #[test]
    fn default_policy_preserves_existing_output_without_partial_files() {
        let directory = tempdir().unwrap();
        let pixel = [1, 2, 3, 255];
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[TestClip::rgba(1, &pixel, 10_000)],
        );
        let output = directory.path().join("existing.gif");
        fs::write(&output, b"old output").unwrap();

        let error = export(&snapshot, &output, &ProjectGifExportOptions::default()).unwrap_err();
        assert!(matches!(error, ProjectGifExportError::ExistingOutput(path) if path == output));
        assert_eq!(fs::read(&output).unwrap(), b"old output");
        assert_eq!(partial_files(directory.path()), 0);
    }

    #[test]
    fn corrupt_and_missing_assets_are_rejected_before_output_creation() {
        let pixel = [1, 2, 3, 255];

        let corrupt_directory = tempdir().unwrap();
        let (corrupt_snapshot, corrupt_ids) = snapshot(
            &corrupt_directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[TestClip::rgba(1, &pixel, 10_000)],
        );
        fs::write(corrupt_snapshot.assets.asset_path(corrupt_ids[0]), b"xxxx").unwrap();
        let corrupt_output = corrupt_directory.path().join("corrupt.gif");
        assert!(matches!(
            export(
                &corrupt_snapshot,
                &corrupt_output,
                &ProjectGifExportOptions::default()
            ),
            Err(ProjectGifExportError::CorruptAsset { .. })
        ));
        assert!(!corrupt_output.exists());

        let missing_directory = tempdir().unwrap();
        let (missing_snapshot, missing_ids) = snapshot(
            &missing_directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[TestClip::rgba(1, &pixel, 10_000)],
        );
        fs::remove_file(missing_snapshot.assets.asset_path(missing_ids[0])).unwrap();
        let missing_output = missing_directory.path().join("missing.gif");
        assert!(matches!(
            export(
                &missing_snapshot,
                &missing_output,
                &ProjectGifExportOptions::default()
            ),
            Err(ProjectGifExportError::MissingAssetFile { .. })
        ));
        assert!(!missing_output.exists());
    }

    #[test]
    fn unsupported_persisted_frame_encoding_is_rejected() {
        let directory = tempdir().unwrap();
        let pixel = [1, 2, 3, 255];
        let mut clip = TestClip::rgba(1, &pixel, 10_000);
        clip.encoding = RasterEncoding::Qoi;
        let (snapshot, _) = snapshot(
            &directory.path().join("project"),
            PhysicalSize::new(1, 1).unwrap(),
            &[clip],
        );
        let output = directory.path().join("unsupported.gif");

        assert!(matches!(
            export(&snapshot, &output, &ProjectGifExportOptions::default()),
            Err(ProjectGifExportError::UnsupportedAssetEncoding {
                encoding: RasterEncoding::Qoi,
                ..
            })
        ));
        assert!(!output.exists());
    }
}
