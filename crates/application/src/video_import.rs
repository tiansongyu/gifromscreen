//! Local-video import through a supervised system `FFmpeg` process.

pub(crate) mod process;

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use gif_from_screen_domain::{FrameId, PhysicalSize, ProjectId, SourceProvenance, UnixTimeMs};
use gif_from_screen_gif::RgbaFrame;
use gif_from_screen_project::ActiveProject;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    IncrementalRecordingProject, IncrementalRecordingProjectError,
    IncrementalRecordingProjectOptions,
};
use process::OutputMode;

// Deliberately excludes playlist, image-sequence, device and network demuxers.
const FORMATS: &str = "mov,matroska,webm,avi,asf,mpeg,mpegts,ogg,flv,nut";
const MAX_SOURCE_PIXELS: u64 = 16_777_216;
const MAX_DURATION: Duration = Duration::from_secs(300);
const MAX_START: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_FRAMES: usize = 10_000;
const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;
const PROBE_LIMIT: usize = 64 * 1024;

/// Resource limits for local-video conversion. Limits may be lowered by callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoImportLimits {
    /// Maximum frames accepted, at most 10,000.
    pub max_frames: usize,
    /// Maximum bytes per packed RGBA frame, at most 64 MiB.
    pub max_frame_bytes: u64,
    /// Maximum cumulative raw RGBA bytes written (before content deduplication).
    pub max_total_bytes: u64,
    /// Maximum wall-clock runtime for each decoder/probe process.
    pub process_timeout: Duration,
}

impl Default for VideoImportLimits {
    fn default() -> Self {
        Self {
            max_frames: MAX_FRAMES,
            max_frame_bytes: MAX_FRAME_BYTES,
            max_total_bytes: 2 * 1024 * 1024 * 1024,
            process_timeout: Duration::from_secs(300),
        }
    }
}

/// Inputs for converting a selected video interval into an editable project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoImportOptions {
    /// Local regular video file; URLs, devices and playlists are not accepted.
    pub input: PathBuf,
    /// New project directory. Its parent must exist; this path must not exist.
    pub project_path: PathBuf,
    /// Stable identity of the new project.
    pub project_id: ProjectId,
    /// Application version persisted in the project manifest.
    pub app_version: String,
    /// Wall-clock project creation timestamp.
    pub created_at: UnixTimeMs,
    /// Accurate seek position, relative to the start of the input video.
    pub start: Duration,
    /// Requested interval, greater than zero and at most 300 seconds.
    pub duration: Duration,
    /// Constant output sampling rate, in 1..=60 frames per second.
    pub fps: u32,
    /// Optional fixed canvas. `None` preserves auto-rotated source dimensions.
    pub output_size: Option<PhysicalSize>,
    /// Explicit decoding and disk-growth limits.
    pub limits: VideoImportLimits,
}

/// Progress emitted only after a complete frame is durably journaled.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VideoImportProgress {
    /// Frames already recoverable on disk.
    pub frames: usize,
    /// Maximum expected frame count for the selected interval.
    pub expected_frames: usize,
    /// Presentation duration already recoverable, in microseconds.
    pub duration_us: u64,
}

/// A failed video import; partial projects are never silently discarded.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VideoImportError {
    /// Caller supplied an invalid or unsafe request.
    #[error("invalid video import options: {0}")]
    InvalidOptions(String),
    /// Metadata, stream contents or decoded output violated an import invariant.
    #[error("cannot import video: {0}")]
    InvalidVideo(String),
    /// A local file or process pipe operation failed.
    #[error("could not {operation}: {source}")]
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },
    /// The required external process could not be started.
    #[error(
        "could not start {program}: {source}. Install the system FFmpeg package (including ffprobe) and ensure both programs are on PATH"
    )]
    StartProcess {
        /// Missing or unavailable executable.
        program: &'static str,
        /// Operating-system process creation error.
        #[source]
        source: io::Error,
    },
    /// The decoder returned a nonzero status; only bounded stderr is retained.
    #[error("{program} failed: {detail}")]
    ProcessFailed {
        /// Failing executable.
        program: &'static str,
        /// Last at most 8192 bytes of diagnostic output.
        detail: String,
    },
    /// Cancellation was acknowledged and the child process reaped.
    #[error("video import cancelled")]
    Cancelled,
    /// A stalled or excessively slow process was killed and reaped.
    #[error("{program} exceeded the video import timeout and was stopped")]
    Timeout {
        /// Process that exceeded its deadline.
        program: &'static str,
    },
    /// Frame journaling or finalization failed.
    #[error(transparent)]
    Project(#[from] IncrementalRecordingProjectError),
    /// The failure left a new project directory for recovery, never an overwrite.
    #[error(
        "{source}. The partial import was retained at {path}; open this project to recover completed frames"
    )]
    Partial {
        /// Exclusively created project path belonging to this import.
        path: PathBuf,
        /// Original decoding, cancellation or persistence failure.
        #[source]
        source: Box<Self>,
    },
}

impl VideoImportError {
    /// Whether this failure represents a user cancellation.
    pub fn is_cancelled(&self) -> bool {
        match self {
            Self::Cancelled => true,
            Self::Partial { source, .. } => source.is_cancelled(),
            _ => false,
        }
    }

    /// The newly-created directory retained after a partial import, if any.
    pub fn partial_project_path(&self) -> Option<&Path> {
        match self {
            Self::Partial { path, .. } => Some(path),
            _ => None,
        }
    }
}

/// Imports local video without retaining the decoded movie in RAM.
///
/// `FFmpeg` and ffprobe are launched directly (never through a shell), restricted
/// to local-file protocols and a container whitelist. Only two RGBA frames can
/// be live in this Rust pipeline. Cancellation is checked every 20 ms while
/// waiting on the child, and between durable frame writes. `FFmpeg` has its own
/// decoder buffers; the per-allocation/pixel limits are not an OS memory sandbox.
///
/// A new directory is created exclusively only when the first complete frame
/// arrives. Later errors retain its journal and report the recovery path. No
/// existing path is overwritten, and no project is removed by this function.
///
/// # Errors
///
/// Returns [`VideoImportError`] for invalid options, unsafe/unsupported inputs,
/// missing system tools, cancellation, bounded-output violations or storage errors.
pub fn import_video_project(
    mut options: VideoImportOptions,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(VideoImportProgress),
) -> Result<ActiveProject, VideoImportError> {
    validate_options(&options)?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(VideoImportError::Cancelled);
    }
    let input = fs::canonicalize(&options.input).map_err(|source| VideoImportError::Io {
        operation: "resolve local video file",
        source,
    })?;
    if !input.is_file() {
        return Err(VideoImportError::InvalidOptions(
            "input must be a local regular file".to_owned(),
        ));
    }
    if options
        .project_path
        .try_exists()
        .map_err(|source| VideoImportError::Io {
            operation: "inspect video project destination",
            source,
        })?
    {
        return Err(VideoImportError::InvalidOptions(
            "project destination already exists; choose a new directory".to_owned(),
        ));
    }
    let metadata = probe(&input, cancelled, options.limits.process_timeout)?;
    let expected_frames = trim_to_source(&mut options, metadata.duration)?;
    let output = options.output_size.unwrap_or(metadata.size);
    let frame_bytes = validate_size(output, options.limits.max_frame_bytes)?;
    let total_bytes = u64::try_from(expected_frames)
        .ok()
        .and_then(|count| frame_bytes.checked_mul(count))
        .ok_or_else(|| VideoImportError::InvalidOptions("video size overflow".to_owned()))?;
    if total_bytes > options.limits.max_total_bytes {
        return Err(VideoImportError::InvalidOptions(format!(
            "selected interval could write {total_bytes} RGBA bytes, above the {}-byte limit; reduce size, duration or fps",
            options.limits.max_total_bytes
        )));
    }
    let command = decoder_command(&input, &options, output, expected_frames);
    let mut sink = ImportSink {
        options: &options,
        output,
        provenance: SourceProvenance::Imported {
            display_name: input
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            media_type: format!("video/{}", metadata.format),
        },
        writer: None,
        owns_directory: false,
        frames: 0,
        expected_frames,
    };
    let result = process::run(
        command,
        "ffmpeg",
        OutputMode::Frames {
            bytes: usize::try_from(frame_bytes).map_err(|_| {
                VideoImportError::InvalidOptions("frame size cannot be represented".to_owned())
            })?,
            limit: expected_frames,
        },
        cancelled,
        options.limits.process_timeout,
        |pixels| {
            progress(sink.append(pixels)?);
            Ok(())
        },
    );
    let result = result.and_then(|()| {
        if cancelled.load(Ordering::Relaxed) {
            return Err(VideoImportError::Cancelled);
        }
        sink.writer
            .take()
            .ok_or_else(|| {
                VideoImportError::InvalidVideo(
                    "selected interval contains no decodable video frames".to_owned(),
                )
            })?
            .finish()
            .map_err(VideoImportError::from)
    });
    let owns_directory = sink.owns_directory;
    drop(sink);
    result.map_err(|source| {
        if owns_directory {
            VideoImportError::Partial {
                path: options.project_path,
                source: Box::new(source),
            }
        } else {
            source
        }
    })
}

fn trim_to_source(
    options: &mut VideoImportOptions,
    duration: Option<Duration>,
) -> Result<usize, VideoImportError> {
    if let Some(duration) = duration {
        let available = duration.saturating_sub(options.start);
        if available.is_zero() {
            return Err(VideoImportError::InvalidVideo(
                "selected start is at or beyond the end of the video".to_owned(),
            ));
        }
        options.duration = options.duration.min(available);
    }
    validate_options(options)
}

struct ImportSink<'a> {
    options: &'a VideoImportOptions,
    output: PhysicalSize,
    provenance: SourceProvenance,
    writer: Option<IncrementalRecordingProject>,
    owns_directory: bool,
    frames: usize,
    expected_frames: usize,
}

impl ImportSink<'_> {
    fn append(&mut self, pixels: Vec<u8>) -> Result<VideoImportProgress, VideoImportError> {
        let options = self.options;
        if self.writer.is_none() {
            fs::create_dir(&options.project_path).map_err(|source| VideoImportError::Io {
                operation: "exclusively create video project directory",
                source,
            })?;
            self.owns_directory = true;
            self.writer = Some(IncrementalRecordingProject::create_with_provenance(
                &options.project_path,
                self.output,
                IncrementalRecordingProjectOptions {
                    project_id: options.project_id,
                    app_version: options.app_version.clone(),
                    created_at: options.created_at,
                    source_label: None,
                },
                self.provenance.clone(),
            )?);
        }
        let duration_us = u64::try_from(options.duration.as_micros()).expect("validated duration");
        let begin = frame_boundary(self.frames, options.fps);
        let end = frame_boundary(self.frames + 1, options.fps).min(duration_us);
        let frame = RgbaFrame::new(
            u16::try_from(self.output.width.get()).expect("validated width"),
            u16::try_from(self.output.height.get()).expect("validated height"),
            pixels,
            end - begin,
        )
        .map_err(|error| VideoImportError::InvalidVideo(error.to_string()))?;
        self.writer.as_mut().expect("created writer").append_frame(
            FrameId::from_bytes(*uuid::Uuid::new_v4().as_bytes()),
            &frame,
        )?;
        self.frames += 1;
        Ok(VideoImportProgress {
            frames: self.frames,
            expected_frames: self.expected_frames,
            duration_us: end,
        })
    }
}

fn validate_options(options: &VideoImportOptions) -> Result<usize, VideoImportError> {
    let invalid = |message: &str| VideoImportError::InvalidOptions(message.to_owned());
    if options.project_id.is_nil() || options.app_version.trim().is_empty() {
        return Err(invalid(
            "project identity and application version are required",
        ));
    }
    if options.duration.as_micros() == 0 || options.duration > MAX_DURATION {
        return Err(invalid(
            "duration must be between 1 microsecond and 300 seconds",
        ));
    }
    if options.start > MAX_START || !(1..=60).contains(&options.fps) {
        return Err(invalid(
            "start must be within 7 days; fps must be between 1 and 60",
        ));
    }
    let limits = options.limits;
    if limits.max_frames == 0
        || limits.max_frames > MAX_FRAMES
        || limits.max_frame_bytes == 0
        || limits.max_frame_bytes > MAX_FRAME_BYTES
        || limits.max_total_bytes == 0
        || limits.process_timeout.is_zero()
        || limits.process_timeout > Duration::from_secs(3600)
    {
        return Err(invalid(
            "invalid frame, allocation, disk or process-timeout limits",
        ));
    }
    let frames = (options.duration.as_micros() * u128::from(options.fps)).div_ceil(1_000_000);
    if frames > limits.max_frames as u128 {
        return Err(invalid(
            "selected duration and fps exceed the maximum frame count",
        ));
    }
    if let Some(size) = options.output_size {
        validate_size(size, limits.max_frame_bytes)?;
    }
    usize::try_from(frames).map_err(|_| invalid("frame count cannot be represented"))
}

fn validate_size(size: PhysicalSize, limit: u64) -> Result<u64, VideoImportError> {
    let width = size.width.get();
    let height = size.height.get();
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4));
    if width == 0
        || height == 0
        || width > u32::from(u16::MAX)
        || height > u32::from(u16::MAX)
        || bytes.is_none_or(|bytes| bytes > limit)
    {
        return Err(VideoImportError::InvalidOptions(format!(
            "video canvas {width}x{height} is invalid or exceeds the {limit}-byte frame limit"
        )));
    }
    Ok(bytes.expect("validated byte size"))
}

fn frame_boundary(index: usize, fps: u32) -> u64 {
    u64::try_from(index).expect("bounded frame count") * 1_000_000 / u64::from(fps)
}

fn seconds(duration: Duration) -> String {
    format!("{}.{:06}", duration.as_secs(), duration.subsec_micros())
}

fn input_options(command: &mut Command) {
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-max_alloc",
        "67108864",
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        FORMATS,
        "-threads",
        "1",
        "-max_pixels",
        "16777216",
        "-probesize",
        "5000000",
        "-analyzeduration",
        "5000000",
    ]);
}

fn decoder_command(
    input: &Path,
    options: &VideoImportOptions,
    output: PhysicalSize,
    frames: usize,
) -> Command {
    let mut command = Command::new("ffmpeg");
    input_options(&mut command);
    command.args(["-nostdin", "-xerror", "-ss", &seconds(options.start)]);
    command.arg("-i").arg(input);
    command.args([
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-dn",
        "-t",
        &seconds(options.duration),
        "-vf",
        &format!(
            "fps={}:start_time=0:eof_action=pass,scale={}:{}:flags=lanczos,setsar=1",
            options.fps,
            output.width.get(),
            output.height.get()
        ),
        "-filter_threads",
        "1",
        "-threads",
        "1",
        "-frames:v",
        &frames.to_string(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "pipe:1",
    ]);
    command
}

struct VideoMetadata {
    size: PhysicalSize,
    format: String,
    duration: Option<Duration>,
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(Deserialize)]
struct ProbeStream {
    width: u32,
    height: u32,
    duration: Option<String>,
    #[serde(default)]
    disposition: ProbeDisposition,
    #[serde(default)]
    side_data_list: Vec<ProbeSideData>,
    #[serde(default)]
    tags: ProbeTags,
}

#[derive(Default, Deserialize)]
struct ProbeDisposition {
    #[serde(default)]
    attached_pic: u8,
}

#[derive(Deserialize)]
struct ProbeSideData {
    rotation: Option<i32>,
}

#[derive(Default, Deserialize)]
struct ProbeTags {
    rotate: Option<String>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    format_name: String,
    duration: Option<String>,
}

fn probe(
    input: &Path,
    cancelled: &AtomicBool,
    timeout: Duration,
) -> Result<VideoMetadata, VideoImportError> {
    let mut command = Command::new("ffprobe");
    input_options(&mut command);
    command.args([
        "-select_streams", "v:0", "-show_entries",
        "stream=width,height,duration:stream_disposition=attached_pic:side_data=rotation:stream_tags=rotate:format=format_name,duration",
        "-of", "json", "-i",
    ]).arg(input);
    let mut output = Vec::new();
    process::run(
        command,
        "ffprobe",
        OutputMode::Probe { limit: PROBE_LIMIT },
        cancelled,
        timeout.min(Duration::from_secs(30)),
        |bytes| {
            output = bytes;
            Ok(())
        },
    )?;
    parse_probe(&output)
}

fn parse_probe(bytes: &[u8]) -> Result<VideoMetadata, VideoImportError> {
    let metadata: ProbeOutput = serde_json::from_slice(bytes).map_err(|error| {
        VideoImportError::InvalidVideo(format!("invalid video metadata: {error}"))
    })?;
    let stream = metadata.streams.first().ok_or_else(|| {
        VideoImportError::InvalidVideo("input contains no video stream".to_owned())
    })?;
    if stream.disposition.attached_pic != 0 {
        return Err(VideoImportError::InvalidVideo(
            "first video stream is attached cover art, not a movie".to_owned(),
        ));
    }
    if u64::from(stream.width) * u64::from(stream.height) > MAX_SOURCE_PIXELS {
        return Err(VideoImportError::InvalidVideo(
            "source video exceeds the 16 megapixel decoder limit".to_owned(),
        ));
    }
    let rotation = stream
        .side_data_list
        .iter()
        .find_map(|side| side.rotation)
        .or_else(|| {
            stream
                .tags
                .rotate
                .as_ref()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
        .rem_euclid(360);
    if ![0, 90, 180, 270].contains(&rotation) {
        return Err(VideoImportError::InvalidVideo(
            "only right-angle video rotation metadata is currently supported".to_owned(),
        ));
    }
    let (width, height) = if rotation == 90 || rotation == 270 {
        (stream.height, stream.width)
    } else {
        (stream.width, stream.height)
    };
    let size = PhysicalSize::new(width, height)
        .map_err(|error| VideoImportError::InvalidVideo(error.to_string()))?;
    validate_size(size, MAX_FRAME_BYTES)?;
    let format = metadata
        .format
        .format_name
        .split(',')
        .next()
        .unwrap_or_default();
    if !FORMATS.split(',').any(|allowed| allowed == format) {
        return Err(VideoImportError::InvalidVideo(
            "unsupported or unsafe video container".to_owned(),
        ));
    }
    Ok(VideoMetadata {
        size,
        format: format.to_owned(),
        duration: stream
            .duration
            .as_deref()
            .and_then(parse_seconds)
            .or_else(|| metadata.format.duration.as_deref().and_then(parse_seconds)),
    })
}

// Metadata is not trusted. Parse finite nonnegative decimal seconds exactly,
// without floating-point casts, exponent syntax or unbounded magnitudes.
fn parse_seconds(text: &str) -> Option<Duration> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.parse::<u64>().ok()?;
    let mut micros = 0_u64;
    for index in 0..6 {
        micros *= 10;
        if let Some(&byte) = fraction.as_bytes().get(index) {
            micros += u64::from(byte - b'0');
        }
    }
    Some(Duration::from_micros(
        whole.checked_mul(1_000_000)?.checked_add(micros)?,
    ))
}

#[cfg(test)]
mod tests;
