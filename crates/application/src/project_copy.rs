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
    AssetDescriptor, AssetId, ProjectId, ProjectManifest, ProjectRevision, RasterEncoding,
    UnixTimeMs,
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
    output.sync_all().map_err(|error| error.to_string())
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
