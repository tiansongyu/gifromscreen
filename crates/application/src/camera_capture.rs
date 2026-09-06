//! Explicitly activated Linux V4L2 camera capture with a live preview.

mod control;

use std::{
    cell::RefCell,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use gif_from_screen_domain::{PhysicalSize, SourceProvenance};
use gif_from_screen_project::ActiveProject;

use crate::{
    LiveFrameSubmission, LiveRecordingOptions, LiveRecordingOutcome, LiveRecordingProgress,
    LiveRgbaRecorder, VideoImportError,
    video_import::process::{self, OutputMode},
};

pub use control::{CameraControl, CameraPhase};

/// A Linux camera node discovered without opening the camera.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CameraDevice {
    /// Strict `/dev/videoN` character-device path.
    pub path: PathBuf,
    /// Device's sysfs label, or the node name if unavailable.
    pub name: String,
}

/// Camera mode and durable project configuration selected before activation.
#[derive(Clone, Debug)]
pub struct CameraCaptureOptions {
    /// Explicitly selected camera; never an implicit default device.
    pub device: CameraDevice,
    /// Requested capture and preview sampling rate, in 1..=60.
    pub fps: u32,
    /// New project metadata, canvas and disk limits. Camera provenance is enforced.
    pub recording: LiveRecordingOptions,
}

/// Latest camera frame; reference-counting avoids cloning full pixels during UI polling.
#[derive(Clone, Debug)]
pub struct CameraPreviewFrame {
    /// Monotonic preview sequence used to avoid redundant texture uploads.
    pub sequence: u64,
    /// Fixed packed-RGBA canvas.
    pub size: PhysicalSize,
    /// Straight-alpha sRGB pixels.
    pub pixels: Arc<[u8]>,
}

/// Replaceable preview/progress snapshot, not an accumulating frame queue.
#[derive(Clone, Debug)]
pub struct CameraCaptureProgress {
    /// Most recently decoded preview frame, including during pause.
    pub preview: CameraPreviewFrame,
    /// Durable writer statistics, if recording has started.
    pub recording: Option<LiveRecordingProgress>,
    /// Frames skipped when the bounded writer queue was full.
    pub dropped_frames: u64,
}

/// Enumerates device nodes and sysfs names without opening any camera.
///
/// # Errors
///
/// Returns a diagnostic if the platform is unsupported or `/dev` is unreadable.
pub fn enumerate_camera_devices() -> Result<Vec<CameraDevice>, String> {
    if !cfg!(target_os = "linux") {
        return Err("Camera capture is currently available on Linux only.".to_owned());
    }
    let mut devices = Vec::new();
    for entry in
        fs::read_dir("/dev").map_err(|error| format!("Cannot list camera devices: {error}"))?
    {
        let entry = entry.map_err(|error| format!("Cannot inspect camera device: {error}"))?;
        let path = entry.path();
        if validate_camera_device(&path).is_err() {
            continue;
        }
        let name = device_name(&path);
        devices.push(CameraDevice { path, name });
    }
    devices.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(devices)
}

fn device_name(path: &Path) -> String {
    let node = path.file_name().unwrap_or_default().to_string_lossy();
    let mut label = String::new();
    let name_path = Path::new("/sys/class/video4linux")
        .join(node.as_ref())
        .join("name");
    if let Ok(file) = fs::File::open(name_path) {
        let _ = file.take(512).read_to_string(&mut label);
    }
    if label.trim().is_empty() {
        node.into_owned()
    } else {
        label.trim().to_owned()
    }
}

fn validate_camera_device(path: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let valid_name = name.strip_prefix("video").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.bytes().all(|byte| byte.is_ascii_digit())
            && suffix.parse::<u32>().is_ok()
    });
    if path.parent() != Some(Path::new("/dev")) || !valid_name {
        return Err(
            "Select a /dev/videoN camera device; files, URLs and aliases are not accepted."
                .to_owned(),
        );
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Camera {} is unavailable: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if metadata.file_type().is_char_device() {
            return Ok(());
        }
    }
    Err(format!(
        "{} is not a camera character device.",
        path.display()
    ))
}

/// Runs an explicitly requested camera preview/recording until stopped.
///
/// The device is opened only when this function is called. Pausing drains camera
/// frames into the replaceable preview but submits nothing to storage; its clock
/// excludes the paused interval. Stop and discard interrupt even a frozen camera
/// through the child supervisor's independent 20 ms cancellation check. Ordinary
/// cancellation/shutdown saves accepted frames; only [`CameraControl::discard`]
/// requests deletion of this session's newly created project.
///
/// # Errors
///
/// Reports invalid devices/modes, missing `FFmpeg`, driver errors and storage
/// failures. If capture fails after frames were saved, the recovery path is retained.
pub fn run_camera_capture(
    mut options: CameraCaptureOptions,
    control: &CameraControl,
    cancellation: &AtomicBool,
    progress: impl FnMut(CameraCaptureProgress),
) -> Result<Option<ActiveProject>, String> {
    validate_camera_device(&options.device.path)?;
    validate_options(&options)?;
    options.recording.provenance = SourceProvenance::Camera {
        device_label: Some(options.device.name.clone()),
    };
    let command = camera_command(&options);
    run_camera_command(options, command, control, cancellation, progress)
}

fn validate_options(options: &CameraCaptureOptions) -> Result<usize, String> {
    let size = options.recording.canvas;
    if !(1..=60).contains(&options.fps)
        || size.width.get() == 0
        || size.height.get() == 0
        || size.width.get() > 4096
        || size.height.get() > 4096
    {
        return Err(
            "Camera mode must be 1–60 fps and dimensions between 1 and 4096 pixels.".to_owned(),
        );
    }
    usize::try_from(u64::from(size.width.get()) * u64::from(size.height.get()) * 4)
        .map_err(|_| "Camera frame is too large for this platform.".to_owned())
}

fn camera_command(options: &CameraCaptureOptions) -> Command {
    let mut command = Command::new("ffmpeg");
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-xerror",
            "-max_alloc",
            "67108864",
            "-threads",
            "1",
            "-max_pixels",
            "16777216",
            "-f",
            "v4l2",
            "-framerate",
            &options.fps.to_string(),
            "-video_size",
            &format!(
                "{}x{}",
                options.recording.canvas.width.get(),
                options.recording.canvas.height.get()
            ),
            "-i",
        ])
        .arg(&options.device.path);
    output_options(&mut command, options);
    command
}

fn output_options(command: &mut Command, options: &CameraCaptureOptions) {
    let size = options.recording.canvas;
    // V4L2 drivers may silently substitute another size. This full-frame crop
    // rejects a substituted canvas before any raw bytes can be mis-framed.
    let filter = format!(
        "crop=w='if(eq(iw,{})*eq(ih,{}),iw,iw+1)':h=ih:x=0:y=0:exact=1,fps={}",
        size.width.get(),
        size.height.get(),
        options.fps
    );
    command.args([
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-dn",
        "-vf",
        &filter,
        "-filter_threads",
        "1",
        "-threads",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "pipe:1",
    ]);
}

struct CameraWriter {
    options: LiveRecordingOptions,
    recorder: Option<LiveRgbaRecorder>,
    first_time_us: Option<u64>,
    dropped_frames: u64,
    finished: Option<Result<LiveRecordingOutcome, String>>,
}

impl CameraWriter {
    fn poll_finished(&mut self) -> bool {
        if self.finished.is_none() {
            self.finished = self.recorder.as_mut().and_then(LiveRgbaRecorder::poll);
        }
        self.finished.is_some()
    }

    fn submit(&mut self, active_us: u64, pixels: Vec<u8>) -> Result<(), String> {
        if self.recorder.is_none() {
            self.recorder = Some(LiveRgbaRecorder::start(self.options.clone())?);
        }
        let first = *self.first_time_us.get_or_insert(active_us);
        let recorder = self.recorder.as_ref().expect("recorder was created");
        match recorder.try_frame(active_us.saturating_sub(first), pixels)? {
            LiveFrameSubmission::Accepted | LiveFrameSubmission::Finishing => {}
            LiveFrameSubmission::Backpressure => self.dropped_frames += 1,
        }
        Ok(())
    }

    fn finish(mut self, control: &CameraControl) -> Result<Option<ActiveProject>, String> {
        if let Some(result) = self.finished.take() {
            return camera_outcome(result?, control);
        }
        let Some(mut recorder) = self.recorder.take() else {
            return Ok(None);
        };
        control.stop();
        if control.phase() == CameraPhase::Discarding {
            recorder.discard();
        } else {
            recorder.stop_at(
                control
                    .active_time_us()
                    .saturating_sub(self.first_time_us.unwrap_or(0)),
            );
        }
        loop {
            if control.phase() == CameraPhase::Discarding {
                recorder.discard();
            }
            if let Some(result) = recorder.poll() {
                return camera_outcome(result?, control);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn camera_outcome(
    outcome: LiveRecordingOutcome,
    control: &CameraControl,
) -> Result<Option<ActiveProject>, String> {
    match outcome {
        LiveRecordingOutcome::Saved(project) if control.phase() == CameraPhase::Discarding => {
            Err(format!(
                "The camera recording finished saving before discard was accepted. Its project was retained at {}.",
                project.layout().root.display()
            ))
        }
        LiveRecordingOutcome::Saved(project) => Ok(Some(project)),
        LiveRecordingOutcome::Discarded => Ok(None),
    }
}

fn run_camera_command(
    options: CameraCaptureOptions,
    command: Command,
    control: &CameraControl,
    cancellation: &AtomicBool,
    mut progress: impl FnMut(CameraCaptureProgress),
) -> Result<Option<ActiveProject>, String> {
    let bytes = validate_options(&options)?;
    let size = options.recording.canvas;
    // Both callbacks run on this supervisor thread. Interior mutability permits
    // its periodic stop check to observe writer failure/limits even when the
    // camera freezes and never calls the frame callback again.
    let writer = RefCell::new(CameraWriter {
        options: options.recording,
        recorder: None,
        first_time_us: None,
        dropped_frames: 0,
        finished: None,
    });
    let mut sequence = 0;
    let result = process::run_with_check(
        command,
        "ffmpeg",
        OutputMode::Frames {
            bytes,
            limit: usize::MAX,
        },
        || {
            if writer.borrow_mut().poll_finished() {
                control.stop();
            }
            control.stop_requested() || cancellation.load(Ordering::Acquire)
        },
        Duration::from_secs(3600),
        |pixels| {
            let mut writer = writer.borrow_mut();
            sequence += 1;
            let preview = CameraPreviewFrame {
                sequence,
                size,
                pixels: Arc::from(pixels.as_slice()),
            };
            let (phase, active_us) = control.snapshot();
            if phase == CameraPhase::Recording {
                writer
                    .submit(active_us, pixels)
                    .map_err(VideoImportError::InvalidVideo)?;
            }
            let recording = writer.recorder.as_ref().map(LiveRgbaRecorder::progress);
            if recording
                .as_ref()
                .is_some_and(|status| status.limit_reached)
            {
                control.stop();
            }
            let update = CameraCaptureProgress {
                preview,
                recording,
                dropped_frames: writer.dropped_frames,
            };
            drop(writer);
            progress(update);
            Ok(())
        },
    );
    let requested_stop = control.stop_requested() || cancellation.load(Ordering::Acquire);
    control.stop();
    let saved = writer.into_inner().finish(control)?;
    match result {
        Ok(()) | Err(VideoImportError::Cancelled) if requested_stop => Ok(saved),
        other => {
            let message = match other {
                Ok(()) => "The camera stopped delivering frames unexpectedly.".to_owned(),
                Err(error) => camera_error(error),
            };
            Err(if let Some(project) = saved {
                format!(
                    "{message} Completed frames were saved in {}.",
                    project.layout().root.display()
                )
            } else {
                message
            })
        }
    }
}

fn camera_error(error: VideoImportError) -> String {
    match error {
        VideoImportError::StartProcess { source, .. } => format!(
            "Cannot start the camera decoder: {source}. Install FFmpeg and ensure it is on PATH."
        ),
        VideoImportError::ProcessFailed { detail, .. } => format!(
            "Camera mode could not be captured. Try a supported resolution/frame rate, check camera permissions and whether another application is using it. FFmpeg: {detail}"
        ),
        VideoImportError::Timeout { .. } => {
            "Camera session reached its one-hour safety timeout; capture was stopped.".to_owned()
        }
        VideoImportError::InvalidVideo(message) => message,
        other => format!("Camera capture failed: {other}"),
    }
}

#[cfg(test)]
mod tests;
