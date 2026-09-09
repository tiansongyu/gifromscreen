//! Save an immutable project revision to a new, independently identified directory.

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, PREMULTIPLIED_SNAPSHOT_HEADER_LEN, PhysicalSize,
    ProjectId, ProjectManifest, ProjectRevision, RasterEncoding, UnixTimeMs,
};
use gif_from_screen_project::{ActiveProject, AssetStore};

const COPY_CHUNK: usize = 16 * 1024;
const MAX_ASSETS: usize = 100_000;
const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

/// One read-only revision; source assets remain immutable during normal editing.
#[derive(Clone, Debug)]
pub struct ProjectCopySnapshot {
    manifest: ProjectManifest,
    assets: AssetStore,
    root: PathBuf,
}

impl ProjectCopySnapshot {
    /// Freezes metadata without reading asset files or changing the source lock.
    pub fn from_active(project: &ActiveProject) -> Self {
        Self {
            manifest: project.manifest().clone(),
            assets: project.assets().clone(),
            root: project.layout().root.clone(),
        }
    }
}

/// Identity and destination assigned to a new copy.
#[derive(Clone, Debug)]
pub struct SaveProjectCopyOptions {
    /// Must identify a new .gfsproj directory outside the source project.
    pub target: PathBuf,
    /// A non-nil identity different from the source project.
    pub project_id: ProjectId,
    /// New copy's wall-clock creation time.
    pub created_at: UnixTimeMs,
}

/// Monotonic streaming-copy counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProjectCopyProgress {
    /// Assets verified and synchronized.
    pub assets_copied: usize,
    /// Total registered assets in the snapshot.
    pub total_assets: usize,
    /// Bytes streamed so far, including the current asset.
    pub bytes_copied: u64,
    /// Total declared bytes in the snapshot.
    pub total_bytes: u64,
}

/// A completed copy; callers choose explicitly whether to open it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectCopyReport {
    /// Canonical destination path.
    pub path: PathBuf,
    /// Independent copied-project identity.
    pub project_id: ProjectId,
    /// Original project's identity at capture time.
    pub source_project_id: ProjectId,
    /// Original revision captured, even if the editor subsequently changed.
    pub source_revision: ProjectRevision,
    /// Number of frames in the copied snapshot.
    pub frames: usize,
    /// Total copied and verified asset bytes.
    pub bytes_copied: u64,
}

/// Streams and verifies every registered asset before publishing a complete manifest.
/// Existing destinations are never opened for writing. Cancellation/failure removes
/// only this operation's newly claimed directory, when its identity is unchanged.
///
/// # Errors
/// Rejects invalid snapshots, aliases, existing targets, size limits, damaged assets,
/// cancellation, and filesystem failures without modifying the source project.
pub fn save_project_copy(
    snapshot: &ProjectCopySnapshot,
    options: &SaveProjectCopyOptions,
    cancellation: &AtomicBool,
    mut progress: impl FnMut(ProjectCopyProgress),
) -> Result<ProjectCopyReport, String> {
    cancelled(cancellation)?;
    let target = validate(snapshot, options)?;
    let source_id = snapshot.manifest.project_id;
    let source_revision = snapshot.manifest.revision;
    let mut manifest = snapshot.manifest.clone();
    let mut source_time = 0_u64;
    for frame in &mut manifest.timeline.frames {
        frame.freeze_capture_clock(gif_from_screen_domain::TimeUs::new(source_time));
        source_time = source_time
            .checked_add(frame.duration.get())
            .ok_or("Source capture timeline overflows.")?;
    }
    manifest.project_id = options.project_id;
    manifest.revision = ProjectRevision::ZERO;
    manifest.created_at = options.created_at;
    manifest.validate().map_err(|error| error.to_string())?;
    let total_bytes = manifest.assets.values().map(|asset| asset.byte_len).sum();
    let mut reported = ProjectCopyProgress {
        total_assets: manifest.assets.len(),
        total_bytes,
        ..ProjectCopyProgress::default()
    };
    cancelled(cancellation)?;
    fs::create_dir(&target).map_err(|error| {
        format!(
            "Could not claim new copy directory {}: {error}",
            target.display()
        )
    })?;
    let claim = CopyClaim::capture(&target)?;
    let result: Result<ProjectCopyReport, String> = (|| {
        let destination = AssetStore::open(&target).map_err(|error| error.to_string())?;
        for asset in manifest.assets.values() {
            copy_asset(
                &snapshot.assets,
                &destination,
                asset,
                cancellation,
                &mut reported,
                &mut progress,
            )?;
            reported.assets_copied += 1;
            progress(reported);
        }
        File::open(destination.directory())
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        cancelled(cancellation)?;
        claim.verify()?;
        let project =
            ActiveProject::create(&target, manifest).map_err(|error| error.to_string())?;
        let frames = project.manifest().timeline.frames.len();
        drop(project);
        cancelled(cancellation)?;
        Ok(ProjectCopyReport {
            path: target.clone(),
            project_id: options.project_id,
            source_project_id: source_id,
            source_revision,
            frames,
            bytes_copied: reported.bytes_copied,
        })
    })();
    match result {
        Ok(report) => Ok(report),
        Err(error) => match claim.remove() {
            Ok(()) => Err(format!(
                "{error}. The incomplete new copy was removed; the original project is unchanged"
            )),
            Err(cleanup) => Err(format!("{error}. Cleanup was not completed: {cleanup}")),
        },
    }
}

fn validate(
    snapshot: &ProjectCopySnapshot,
    options: &SaveProjectCopyOptions,
) -> Result<PathBuf, String> {
    snapshot
        .manifest
        .validate()
        .map_err(|error| error.to_string())?;
    if options.project_id.is_nil() || options.project_id == snapshot.manifest.project_id {
        return Err("The copy must have an independent, non-nil project identity".to_owned());
    }
    if snapshot.manifest.assets.len() > MAX_ASSETS {
        return Err("Project copy exceeds 100,000 registered assets".to_owned());
    }
    let mut bytes = 0_u64;
    for asset in snapshot.manifest.assets.values() {
        bytes = bytes
            .checked_add(asset.byte_len)
            .filter(|total| *total <= MAX_ASSET_BYTES)
            .ok_or_else(|| "Project copy exceeds the 64 GiB asset limit".to_owned())?;
        if let Some((size, RasterEncoding::Rgba8)) = asset.kind.raster_descriptor() {
            let expected = u64::from(size.width.get())
                .checked_mul(u64::from(size.height.get()))
                .and_then(|pixels| pixels.checked_mul(4));
            if expected != Some(asset.byte_len) {
                return Err(format!(
                    "Asset {} has inconsistent raw RGBA metadata",
                    asset.id
                ));
            }
        }
    }
    serde_json::to_writer(&mut MetadataBudget(0), &snapshot.manifest).map_err(|error| {
        format!("Project metadata exceeds the 32 MiB copy limit or is invalid: {error}")
    })?;
    if options
        .target
        .extension()
        .is_none_or(|extension| extension != "gfsproj")
    {
        return Err("Save As target must end in .gfsproj".to_owned());
    }
    let filename = options
        .target
        .file_name()
        .ok_or_else(|| "Choose a project directory name".to_owned())?;
    let parent = options
        .target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let target = fs::canonicalize(parent)
        .map_err(|error| format!("Copy parent directory must already exist: {error}"))?
        .join(filename);
    let source = fs::canonicalize(&snapshot.root).map_err(|error| error.to_string())?;
    if target.starts_with(&source) {
        return Err("Save As must use a directory outside the original project".to_owned());
    }
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err("Save As target already exists; choose a new directory".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    Ok(target)
}

struct MetadataBudget(u64);
impl Write for MetadataBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .filter(|total| *total <= MAX_MANIFEST_BYTES)
            .ok_or_else(|| std::io::Error::other("metadata limit exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn copy_asset(
    source: &AssetStore,
    destination: &AssetStore,
    asset: &AssetDescriptor,
    cancellation: &AtomicBool,
    reported: &mut ProjectCopyProgress,
    progress: &mut impl FnMut(ProjectCopyProgress),
) -> Result<(), String> {
    cancelled(cancellation)?;
    let path = source.asset_path(asset.id);
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("Could not inspect source asset {}: {error}", asset.id))?;
    if !metadata.is_file() || metadata.len() != asset.byte_len {
        return Err(format!(
            "Asset {} is missing, not a regular file, or has the wrong length",
            asset.id
        ));
    }
    let mut input = File::open(&path).map_err(|error| error.to_string())?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination.asset_path(asset.id))
        .map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    let mut typed = match asset.kind {
        AssetKind::PremultipliedSnapshot { size, .. } => {
            Some(PremultipliedCopyCheck::new(asset, size)?)
        }
        _ => None,
    };
    let mut copied = 0_u64;
    let mut buffer = [0_u8; COPY_CHUNK];
    loop {
        cancelled(cancellation)?;
        let read = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        copied += read as u64;
        if copied > asset.byte_len {
            return Err(format!("Asset {} changed during copy", asset.id));
        }
        if let Some(check) = &mut typed {
            check
                .consume(&buffer[..read])
                .map_err(|error| format!("Asset {}: {error}", asset.id))?;
        }
        hasher.update(&buffer[..read]);
        output
            .write_all(&buffer[..read])
            .map_err(|error| error.to_string())?;
        reported.bytes_copied += read as u64;
        progress(*reported);
    }
    if copied != asset.byte_len || AssetId::from_digest(*hasher.finalize().as_bytes()) != asset.id {
        return Err(format!(
            "Asset {} failed length or content-digest verification",
            asset.id
        ));
    }
    if let Some(check) = &typed {
        check
            .finish()
            .map_err(|error| format!("Asset {}: {error}", asset.id))?;
    }
    output.sync_all().map_err(|error| error.to_string())
}

/// Validate the same streamed bytes that are hashed and copied. The only
/// retained validation state is one header plus a possible split RGBA pixel.
struct PremultipliedCopyCheck {
    size: PhysicalSize,
    encoded_len: u64,
    header: [u8; PREMULTIPLIED_SNAPSHOT_HEADER_LEN],
    header_len: usize,
    pixel: [u8; 4],
    pixel_len: usize,
}

impl PremultipliedCopyCheck {
    fn new(asset: &AssetDescriptor, size: PhysicalSize) -> Result<Self, String> {
        gif_from_screen_domain::validate_premultiplied_snapshot_descriptor(asset)?;
        Ok(Self {
            size,
            encoded_len: asset.byte_len,
            header: [0; PREMULTIPLIED_SNAPSHOT_HEADER_LEN],
            header_len: 0,
            pixel: [0; 4],
            pixel_len: 0,
        })
    }

    fn consume(&mut self, mut bytes: &[u8]) -> Result<(), String> {
        if self.header_len < self.header.len() {
            let length = (self.header.len() - self.header_len).min(bytes.len());
            self.header[self.header_len..self.header_len + length]
                .copy_from_slice(&bytes[..length]);
            self.header_len += length;
            bytes = &bytes[length..];
            if self.header_len != self.header.len() {
                return Ok(());
            }
            let encoded_len =
                gif_from_screen_render::PremultipliedRgbaSurface::validate_encoded_header(
                    &self.header,
                    self.size,
                    usize::try_from(self.encoded_len).unwrap_or(usize::MAX),
                )
                .map_err(|error| error.to_string())?;
            if u64::try_from(encoded_len).ok() != Some(self.encoded_len) {
                return Err(
                    "Premultiplied snapshot header and descriptor lengths differ.".to_owned(),
                );
            }
        }
        if self.pixel_len != 0 {
            let length = (4 - self.pixel_len).min(bytes.len());
            self.pixel[self.pixel_len..self.pixel_len + length].copy_from_slice(&bytes[..length]);
            self.pixel_len += length;
            bytes = &bytes[length..];
            if self.pixel_len != 4 {
                return Ok(());
            }
            Self::pixel(self.pixel)?;
            self.pixel_len = 0;
        }
        let (pixels, tail) = bytes.as_chunks::<4>();
        for pixel in pixels {
            Self::pixel(*pixel)?;
        }
        self.pixel[..tail.len()].copy_from_slice(tail);
        self.pixel_len = tail.len();
        Ok(())
    }

    fn pixel(pixel: [u8; 4]) -> Result<(), String> {
        if pixel[..3].iter().any(|channel| *channel > pixel[3]) {
            Err("Premultiplied snapshot has RGB greater than alpha.".to_owned())
        } else {
            Ok(())
        }
    }

    fn finish(&self) -> Result<(), String> {
        if self.header_len != self.header.len() || self.pixel_len != 0 {
            Err("Premultiplied snapshot ended in a partial header or pixel.".to_owned())
        } else {
            Ok(())
        }
    }
}

fn cancelled(cancellation: &AtomicBool) -> Result<(), String> {
    if cancellation.load(Ordering::Acquire) {
        Err("Save As cancelled".to_owned())
    } else {
        Ok(())
    }
}

struct CopyClaim {
    root: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}
impl CopyClaim {
    fn capture(root: &Path) -> Result<Self, String> {
        let metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(
                "Copy destination changed before its directory could be claimed".to_owned(),
            );
        }
        Ok(Self {
            root: root.to_owned(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
    fn verify(&self) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&self.root).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        if metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            return Ok(());
        }
        Err("Copy directory changed externally; refusing to change replacement contents".to_owned())
    }
    fn remove(self) -> Result<(), String> {
        self.verify()?;
        fs::remove_dir_all(self.root).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_domain::{
        AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform, ColorSpace,
        DurationUs, EditCommand, FrameClip, FrameId, GifExportPreset, GifLoop, GifPaletteStrategy,
        GifPresetOptions, PhysicalSize, SourceProvenance,
    };
    use gif_from_screen_project::LockPolicy;

    fn source(root: &Path, width: u32, height: u32) -> ActiveProject {
        let size = PhysicalSize::new(width, height).unwrap();
        let mut manifest = ProjectManifest::new(
            ProjectId::from_u128(1),
            "copy-test",
            UnixTimeMs::new(0),
            Canvas {
                size,
                color_space: ColorSpace::Srgb,
                background: CanvasBackground::Transparent,
            },
        )
        .unwrap();
        manifest.source_provenance.push(SourceProvenance::Board);
        manifest.export_presets.insert(
            "Complete".to_owned(),
            GifExportPreset {
                colors: 256,
                palette: GifPaletteStrategy::PerFrame,
                repeat: GifLoop::Infinite,
                alpha_threshold: 1,
                options: Some(GifPresetOptions::default()),
            },
        );
        let mut project = ActiveProject::create(root, manifest).unwrap();
        let bytes = [255, 0, 0, 255].repeat(width as usize * height as usize);
        let id = project.assets().put(&bytes).unwrap();
        project
            .commit(EditCommand::Compound {
                commands: vec![
                    EditCommand::RegisterAsset {
                        asset: AssetDescriptor {
                            id,
                            byte_len: bytes.len() as u64,
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
                            capture_clock: Some(gif_from_screen_domain::CaptureClockContext {
                                id: Some(gif_from_screen_domain::CaptureClockId::from_u128(200)),
                                sampled_at: gif_from_screen_domain::TimeUs::ZERO,
                            }),
                            capture_binding: gif_from_screen_domain::CaptureBinding::Original,
                            id: FrameId::from_u128(1),
                            asset_id: id,
                            duration: DurationUs::new(100_000).unwrap(),
                            transform: ClipTransform::default(),
                            capture_metadata: CaptureMetadata::default(),
                            effects: Vec::new(),
                        }],
                    },
                ],
            })
            .unwrap();
        project
    }

    fn options(root: &Path) -> SaveProjectCopyOptions {
        SaveProjectCopyOptions {
            target: root.join("copy.gfsproj"),
            project_id: ProjectId::from_u128(2),
            created_at: UnixTimeMs::new(100),
        }
    }

    fn pm_container(size: PhysicalSize) -> Vec<u8> {
        let mut bytes = b"GFSPM8\0".to_vec();
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&size.width.get().to_le_bytes());
        bytes.extend_from_slice(&size.height.get().to_le_bytes());
        bytes.extend_from_slice(
            &[3, 6, 9, 128].repeat(usize::try_from(size.area().unwrap()).unwrap()),
        );
        bytes
    }

    fn register_pm(
        project: &mut ActiveProject,
        size: PhysicalSize,
        bytes: &[u8],
        attach: bool,
    ) -> AssetDescriptor {
        let id = project.assets().put(bytes).unwrap();
        let asset = AssetDescriptor {
            id,
            byte_len: u64::try_from(bytes.len()).unwrap(),
            kind: AssetKind::PremultipliedSnapshot {
                size,
                format_version: 1,
            },
        };
        let mut commands = vec![EditCommand::RegisterAsset {
            asset: asset.clone(),
        }];
        if attach {
            let mut frame = project.manifest().timeline.frames[0].clone();
            frame.render_steps = vec![
                gif_from_screen_domain::FrameRenderStep::composite(1),
                gif_from_screen_domain::FrameRenderStep::CinemagraphOverlay {
                    snapshot_asset: id,
                    snapshot_size: size,
                },
            ];
            commands.push(EditCommand::ReplaceFrame {
                frame_id: frame.id,
                replacement: Box::new(frame),
            });
        }
        project.commit(EditCommand::Compound { commands }).unwrap();
        asset
    }

    #[test]
    fn save_as_preserves_typed_pm_container_steps_and_independent_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let mut project = source(&directory.path().join("source.gfsproj"), 2, 3);
        let size = PhysicalSize::new(2, 3).unwrap();
        let bytes = pm_container(size);
        let asset = register_pm(&mut project, size, &bytes, true);
        let before = project.manifest().clone();
        let source_manifest = fs::read(&project.layout().manifest).unwrap();
        let source_journal = fs::read(&project.layout().journal).unwrap();
        let report = save_project_copy(
            &ProjectCopySnapshot::from_active(&project),
            &options(directory.path()),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let copied = ActiveProject::open(&report.path, LockPolicy::FailIfPresent).unwrap();
        assert!(copied.asset_issues.is_empty());
        assert!(before.schema_version >= 7);
        assert_eq!(
            copied.project.manifest().schema_version,
            before.schema_version
        );
        assert_ne!(copied.project.manifest().project_id, before.project_id);
        assert_eq!(copied.project.manifest().assets, before.assets);
        assert_eq!(copied.project.manifest().timeline, before.timeline);
        assert_eq!(copied.project.manifest().assets[&asset.id], asset);
        assert!(
            copied.project.manifest().assets[&asset.id]
                .kind
                .raster_descriptor()
                .is_none()
        );
        let restored = copied.project.assets().read(asset.id).unwrap();
        assert_eq!(restored, bytes);
        let decoded =
            gif_from_screen_render::PremultipliedRgbaSurface::decode(restored, size, 24).unwrap();
        assert_eq!(decoded.pixels(), &[3, 6, 9, 128].repeat(6));
        assert_eq!(project.manifest(), &before);
        assert_eq!(
            fs::read(&project.layout().manifest).unwrap(),
            source_manifest
        );
        assert_eq!(fs::read(&project.layout().journal).unwrap(), source_journal);
        assert!(ActiveProject::open(&project.layout().root, LockPolicy::FailIfPresent).is_err());
    }

    #[test]
    fn pm_semantic_corruption_with_a_matching_hash_rejects_and_removes_only_new_copy() {
        for corruption in ["magic", "version", "shape", "rgb_above_alpha", "hidden_rgb"] {
            let directory = tempfile::tempdir().unwrap();
            let mut project = source(&directory.path().join("source.gfsproj"), 2, 3);
            let size = PhysicalSize::new(2, 3).unwrap();
            let mut bytes = pm_container(size);
            let expected_error = match corruption {
                "magic" => {
                    bytes[0] = b'X';
                    "header"
                }
                "version" => {
                    bytes[7..9].copy_from_slice(&2_u16.to_le_bytes());
                    "version"
                }
                "shape" => {
                    bytes[9..13].copy_from_slice(&3_u32.to_le_bytes());
                    bytes[13..17].copy_from_slice(&2_u32.to_le_bytes());
                    "shape"
                }
                "rgb_above_alpha" => {
                    bytes[17..21].copy_from_slice(&[129, 6, 9, 128]);
                    "RGB greater than alpha"
                }
                "hidden_rgb" => {
                    bytes[17..21].copy_from_slice(&[1, 0, 0, 0]);
                    "RGB greater than alpha"
                }
                _ => unreachable!(),
            };
            // Store *these* malformed bytes under their correct digest. This
            // proves typed validation is independent from ordinary hash checks.
            let asset = register_pm(&mut project, size, &bytes, true);
            assert_eq!(AssetStore::id_for_bytes(&bytes), asset.id);
            project.assets().verify(asset.id).unwrap();
            let snapshot = ProjectCopySnapshot::from_active(&project);
            let before_manifest = fs::read(&project.layout().manifest).unwrap();
            let before_journal = fs::read(&project.layout().journal).unwrap();
            let keep = directory.path().join("unrelated.txt");
            fs::write(&keep, b"keep unrelated data").unwrap();
            let options = options(directory.path());
            let error = save_project_copy(&snapshot, &options, &AtomicBool::new(false), |_| {})
                .unwrap_err();
            assert!(error.contains(expected_error), "{corruption}: {error}");
            assert!(error.contains("incomplete new copy was removed"));
            assert!(!options.target.exists());
            assert_eq!(fs::read(&keep).unwrap(), b"keep unrelated data");
            assert_eq!(project.manifest(), &snapshot.manifest);
            assert_eq!(
                fs::read(&project.layout().manifest).unwrap(),
                before_manifest
            );
            assert_eq!(fs::read(&project.layout().journal).unwrap(), before_journal);
            assert_eq!(project.assets().read(asset.id).unwrap(), bytes);
        }
    }

    #[test]
    fn pm_stream_validator_accepts_split_headers_and_pixels_with_only_bounded_state() {
        let size = PhysicalSize::new(2, 3).unwrap();
        let bytes = pm_container(size);
        let asset = AssetDescriptor {
            id: AssetStore::id_for_bytes(&bytes),
            byte_len: u64::try_from(bytes.len()).unwrap(),
            kind: AssetKind::PremultipliedSnapshot {
                size,
                format_version: 1,
            },
        };
        for split in 0..=bytes.len() {
            let mut check = PremultipliedCopyCheck::new(&asset, size).unwrap();
            check.consume(&bytes[..split]).unwrap();
            check.consume(&bytes[split..]).unwrap();
            check.finish().unwrap();
        }
        for chunk in [1, 2, 3, 4, 5, 7, 16, 17, 19, COPY_CHUNK] {
            let mut check = PremultipliedCopyCheck::new(&asset, size).unwrap();
            for part in bytes.chunks(chunk) {
                check.consume(part).unwrap();
            }
            check.finish().unwrap();
        }
        for incomplete in [0, 1, 16, 18, 19, 20] {
            let mut check = PremultipliedCopyCheck::new(&asset, size).unwrap();
            check.consume(&bytes[..incomplete]).unwrap();
            assert!(check.finish().is_err());
        }
        assert!(
            std::mem::size_of::<PremultipliedCopyCheck>() < 128,
            "stream validation retains only a header and a partial pixel, not a surface"
        );
    }

    #[test]
    fn invalid_pm_pixel_crossing_the_actual_copy_chunk_is_not_missed() {
        let directory = tempfile::tempdir().unwrap();
        let mut project = source(&directory.path().join("source.gfsproj"), 1, 1);
        let size = PhysicalSize::new(64, 65).unwrap();
        let mut bytes = pm_container(size);
        let pixel_start = COPY_CHUNK - 3;
        assert_eq!((pixel_start - PREMULTIPLIED_SNAPSHOT_HEADER_LEN) % 4, 0);
        bytes[pixel_start..pixel_start + 4].copy_from_slice(&[129, 0, 0, 128]);
        let asset = register_pm(&mut project, size, &bytes, false);
        let mut check = PremultipliedCopyCheck::new(&asset, size).unwrap();
        check.consume(&bytes[..COPY_CHUNK]).unwrap();
        assert_eq!(check.pixel_len, 3);
        assert!(
            check
                .consume(&bytes[COPY_CHUNK..])
                .unwrap_err()
                .contains("RGB greater than alpha")
        );
        let options = options(directory.path());
        let error = save_project_copy(
            &ProjectCopySnapshot::from_active(&project),
            &options,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert!(error.contains("RGB greater than alpha"));
        assert!(!options.target.exists());
        assert_eq!(project.assets().read(asset.id).unwrap(), bytes);
    }

    #[test]
    fn cancellation_after_one_pm_chunk_removes_only_the_new_destination() {
        let directory = tempfile::tempdir().unwrap();
        let mut project = source(&directory.path().join("source.gfsproj"), 1, 1);
        let size = PhysicalSize::new(64, 65).unwrap();
        let bytes = pm_container(size);
        let asset = register_pm(&mut project, size, &bytes, false);
        let prior_bytes: u64 = project
            .manifest()
            .assets
            .values()
            .take_while(|item| item.id != asset.id)
            .map(|item| item.byte_len)
            .sum();
        let snapshot = ProjectCopySnapshot::from_active(&project);
        let options = options(directory.path());
        let keep = directory.path().join("unrelated.txt");
        fs::write(&keep, b"original unrelated file").unwrap();
        let cancellation = AtomicBool::new(false);
        let mut latest = 0;
        let error = save_project_copy(&snapshot, &options, &cancellation, |progress| {
            latest = progress.bytes_copied;
            if latest == prior_bytes + u64::try_from(COPY_CHUNK).unwrap() {
                cancellation.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
        assert!(cancellation.load(Ordering::Acquire));
        assert_eq!(latest, prior_bytes + u64::try_from(COPY_CHUNK).unwrap());
        assert!(error.contains("cancelled"));
        assert!(!options.target.exists());
        assert_eq!(fs::read(&keep).unwrap(), b"original unrelated file");
        assert_eq!(project.manifest(), &snapshot.manifest);
        assert_eq!(project.assets().read(asset.id).unwrap(), bytes);
    }

    #[test]
    fn save_as_preserves_frozen_content_with_independent_identity_and_source_lock() {
        let directory = tempfile::tempdir().unwrap();
        let source_root = directory.path().join("source.gfsproj");
        let mut source = source(&source_root, 1, 1);
        let snapshot = ProjectCopySnapshot::from_active(&source);
        source
            .commit(EditCommand::SetFrameDurations {
                changes: vec![gif_from_screen_domain::FrameDurationChange {
                    frame_id: FrameId::from_u128(1),
                    duration: DurationUs::new(900_000).unwrap(),
                }],
            })
            .unwrap();
        let original_after_edit = source.manifest().clone();
        let report = save_project_copy(
            &snapshot,
            &options(directory.path()),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(report.source_revision, snapshot.manifest.revision);
        assert_eq!(source.manifest(), &original_after_edit);
        assert!(ActiveProject::open(&source_root, LockPolicy::FailIfPresent).is_err());
        let copied = ActiveProject::open(&report.path, LockPolicy::FailIfPresent).unwrap();
        assert!(copied.asset_issues.is_empty());
        assert_eq!(
            copied.project.manifest().project_id,
            ProjectId::from_u128(2)
        );
        assert_eq!(copied.project.manifest().revision, ProjectRevision::ZERO);
        assert_eq!(
            copied.project.manifest().timeline,
            snapshot.manifest.timeline
        );
        assert_eq!(
            copied.project.manifest().export_presets,
            snapshot.manifest.export_presets
        );
        assert_eq!(
            copied.project.manifest().source_provenance,
            [SourceProvenance::Board]
        );
        let output = directory.path().join("copy.gif");
        crate::export_project_snapshot_to_gif(
            &crate::ProjectExportSnapshot::from_active(&copied.project),
            &output,
            &crate::ProjectGifExportOptions::default(),
            &gif_from_screen_gif::NeverCancel,
            &mut crate::NoopProjectExportProgress,
        )
        .unwrap();
        let gif = gif_from_screen_media::decode_gif(
            File::open(output).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!(gif.frames()[0].rgba(), [255, 0, 0, 255]);
        assert_eq!(gif.frames()[0].duration_us(), 100_000);
    }

    #[test]
    fn corruption_and_mid_asset_cancellation_remove_only_the_new_incomplete_copy() {
        for corrupt in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let source = source(&directory.path().join("source.gfsproj"), 256, 256);
            let snapshot = ProjectCopySnapshot::from_active(&source);
            if corrupt {
                let id = *source.manifest().assets.keys().next().unwrap();
                fs::write(source.assets().asset_path(id), vec![1; 256 * 256 * 4]).unwrap();
            }
            let cancelled = AtomicBool::new(false);
            let options = options(directory.path());
            let result = save_project_copy(&snapshot, &options, &cancelled, |progress| {
                if !corrupt && progress.bytes_copied >= COPY_CHUNK as u64 {
                    cancelled.store(true, Ordering::Release);
                }
            });
            assert!(result.is_err());
            assert!(!options.target.exists());
            assert!(source.layout().manifest.exists());
            assert_eq!(source.manifest(), &snapshot.manifest);
        }
    }

    #[test]
    fn existing_alias_nested_targets_and_initial_cancellation_never_mutate_original() {
        let directory = tempfile::tempdir().unwrap();
        let source = source(&directory.path().join("source.gfsproj"), 1, 1);
        let snapshot = ProjectCopySnapshot::from_active(&source);
        let mut options = options(directory.path());
        fs::create_dir(&options.target).unwrap();
        fs::write(options.target.join("keep"), "original").unwrap();
        assert!(save_project_copy(&snapshot, &options, &AtomicBool::new(false), |_| {}).is_err());
        assert_eq!(
            fs::read_to_string(options.target.join("keep")).unwrap(),
            "original"
        );
        options.target = source.layout().root.join("nested.gfsproj");
        assert!(save_project_copy(&snapshot, &options, &AtomicBool::new(false), |_| {}).is_err());
        assert!(!options.target.exists());
        #[cfg(unix)]
        {
            options.target = directory.path().join("alias.gfsproj");
            std::os::unix::fs::symlink(source.layout().root.clone(), &options.target).unwrap();
            assert!(
                save_project_copy(&snapshot, &options, &AtomicBool::new(false), |_| {}).is_err()
            );
        }
        options.target = directory.path().join("cancel.gfsproj");
        assert!(save_project_copy(&snapshot, &options, &AtomicBool::new(true), |_| {}).is_err());
        assert!(!options.target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn replacement_directory_is_not_removed_during_cancel_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let source = source(&directory.path().join("source.gfsproj"), 128, 128);
        let snapshot = ProjectCopySnapshot::from_active(&source);
        let options = options(directory.path());
        let cancellation = AtomicBool::new(false);
        let moved = directory.path().join("moved-copy.gfsproj");
        let mut replaced = false;
        let result = save_project_copy(&snapshot, &options, &cancellation, |_| {
            if !replaced {
                fs::rename(&options.target, &moved).unwrap();
                fs::create_dir(&options.target).unwrap();
                fs::write(options.target.join("keep"), "replacement").unwrap();
                replaced = true;
                cancellation.store(true, Ordering::Release);
            }
        });
        assert!(result.unwrap_err().contains("changed externally"));
        assert_eq!(
            fs::read_to_string(options.target.join("keep")).unwrap(),
            "replacement"
        );
    }
}
