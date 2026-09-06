//! Bounded, nonblocking RGBA delivery to one durable background project writer.

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::Duration,
};

use gif_from_screen_domain::{
    DurationUs, FrameId, PhysicalSize, ProjectId, SourceProvenance, UnixTimeMs,
};
use gif_from_screen_gif::RgbaFrame;
use gif_from_screen_project::ActiveProject;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use uuid::Uuid;

use crate::{IncrementalRecordingProject, IncrementalRecordingProjectOptions};

const MAX_EDGE: u32 = 4096;
const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Fixed recording limits and destination. No existing directory is replaced.
#[derive(Clone, Debug)]
pub struct LiveRecordingOptions {
    /// A directory which must not already exist when the first frame arrives.
    pub project_path: PathBuf,
    /// Fixed raw RGBA dimensions, at most 4096 pixels per edge.
    pub canvas: PhysicalSize,
    /// Fresh identity for the recorded project.
    pub project_id: ProjectId,
    /// Application version attached to the manifest.
    pub app_version: String,
    /// Wall-clock creation timestamp.
    pub created_at: UnixTimeMs,
    /// Screen, camera, or drawing-board origin.
    pub provenance: SourceProvenance,
    /// Positive recoverable tail until the next frame or explicit stop arrives.
    pub provisional_duration_us: u64,
    /// Maximum saved frames before automatic finalization, at most 100,000.
    pub max_frames: usize,
    /// Sum of submitted raw frame sizes, before content-addressed deduplication.
    pub max_frame_bytes_total: u64,
}

/// Submission never waits for disk I/O or an available queue slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveFrameSubmission {
    /// Enqueued; persistence completes asynchronously.
    Accepted,
    /// Both queue slots were occupied; the caller may retry this frame.
    Backpressure,
    /// Recording is terminating or has already completed.
    Finishing,
}

/// Latest durable counters, never a queue of stale progress events.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LiveRecordingProgress {
    /// Frames already committed to the journal.
    pub frames: usize,
    /// Duration including the current provisional final-frame tail.
    pub duration_us: u64,
    /// True when a frame or raw-data limit automatically stopped recording.
    pub limit_reached: bool,
}

/// One terminal result, delivered after the writer has released or transferred its lock.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "one bounded terminal message transfers the locked project without another allocation"
)]
pub enum LiveRecordingOutcome {
    /// Finished project, with its exclusive lock transferred to the caller.
    Saved(ActiveProject),
    /// Explicit discard, or a stop before any frame arrived (no directory created).
    Discarded,
}

#[derive(Clone, Copy, Debug, Default)]
enum Terminal {
    #[default]
    Running,
    Stop(Option<u64>),
    Discard,
}

struct QueuedFrame {
    at_us: u64,
    rgba: Vec<u8>,
}

/// One writer with a two-frame queue and an independent termination channel.
pub struct LiveRgbaRecorder {
    sender: SyncSender<QueuedFrame>,
    terminal: Arc<Mutex<Terminal>>,
    progress: Arc<Mutex<LiveRecordingProgress>>,
    result: Option<Receiver<Result<LiveRecordingOutcome, String>>>,
    frame_bytes: usize,
    claim: Arc<Mutex<Option<DirectoryClaim>>>,
}

impl LiveRgbaRecorder {
    /// Starts a worker. The destination is created lazily at the first frame.
    ///
    /// # Errors
    /// Returns invalid options or thread creation failures without filesystem writes.
    pub fn start(options: LiveRecordingOptions) -> Result<Self, String> {
        let frame_bytes = validate_options(&options)?;
        let (sender, receiver) = mpsc::sync_channel(2);
        let (result_sender, result) = mpsc::sync_channel(1);
        let terminal = Arc::new(Mutex::new(Terminal::Running));
        let progress = Arc::new(Mutex::new(LiveRecordingProgress::default()));
        let worker_terminal = Arc::clone(&terminal);
        let worker_progress = Arc::clone(&progress);
        let claim = Arc::new(Mutex::new(None));
        let worker_claim = Arc::clone(&claim);
        thread::Builder::new()
            .name("gfs-live-rgba-writer".to_owned())
            .spawn(move || {
                let root = options.project_path.clone();
                let result = run(
                    &options,
                    &receiver,
                    &worker_terminal,
                    &worker_progress,
                    &worker_claim,
                )
                .map_err(|error| {
                    format!(
                        "{error}. Any saved frames remain recoverable at {}",
                        root.display()
                    )
                });
                let _ = result_sender.send(result);
            })
            .map_err(|error| format!("Could not start recording writer: {error}"))?;
        Ok(Self {
            sender,
            terminal,
            progress,
            result: Some(result),
            frame_bytes,
            claim,
        })
    }

    /// Enqueues one tightly packed frame at a strictly increasing active-time timestamp.
    ///
    /// # Errors
    /// Returns an error if the byte length differs from the fixed canvas.
    pub fn try_frame(
        &self,
        active_time_us: u64,
        rgba: Vec<u8>,
    ) -> Result<LiveFrameSubmission, String> {
        self.try_frame_retaining(active_time_us, rgba)
            .map(|(submission, _)| submission)
    }

    /// Like [`Self::try_frame`], but returns pixels on rejection for allocation-free retries.
    /// Accepted submissions return an empty vector.
    ///
    /// # Errors
    /// Returns an error if the byte length differs from the fixed canvas.
    pub fn try_frame_retaining(
        &self,
        active_time_us: u64,
        rgba: Vec<u8>,
    ) -> Result<(LiveFrameSubmission, Vec<u8>), String> {
        if rgba.len() != self.frame_bytes {
            return Err("Recording frame dimensions do not match its fixed canvas".to_owned());
        }
        if !matches!(
            *self.terminal.lock().unwrap_or_else(PoisonError::into_inner),
            Terminal::Running
        ) {
            return Ok((LiveFrameSubmission::Finishing, rgba));
        }
        Ok(
            match self.sender.try_send(QueuedFrame {
                at_us: active_time_us,
                rgba,
            }) {
                Ok(()) => (LiveFrameSubmission::Accepted, Vec::new()),
                Err(TrySendError::Full(frame)) => (LiveFrameSubmission::Backpressure, frame.rgba),
                Err(TrySendError::Disconnected(frame)) => {
                    (LiveFrameSubmission::Finishing, frame.rgba)
                }
            },
        )
    }

    /// Finishes after draining queued frames, assigning the final actual active duration.
    pub fn stop_at(&self, active_time_us: u64) {
        let mut terminal = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(*terminal, Terminal::Running) {
            *terminal = Terminal::Stop(Some(active_time_us));
        }
    }

    /// Explicit user discard. Only a directory freshly claimed by this worker is deleted.
    pub fn discard(&self) {
        *self.terminal.lock().unwrap_or_else(PoisonError::into_inner) = Terminal::Discard;
    }

    /// True until the caller consumes the terminal result.
    pub fn is_active(&self) -> bool {
        self.result.is_some()
    }

    /// Reads the latest durable counters without performing I/O.
    pub fn progress(&self) -> LiveRecordingProgress {
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes a terminal result if ready, without waiting for the worker.
    pub fn poll(&mut self) -> Option<Result<LiveRecordingOutcome, String>> {
        let result = match self.result.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("Recording writer exited unexpectedly; recover any saved project from its target path".to_owned()),
        };
        if matches!(
            *self.terminal.lock().unwrap_or_else(PoisonError::into_inner),
            Terminal::Discard
        ) && let Ok(LiveRecordingOutcome::Saved(project)) = result
        {
            let claim = self
                .claim
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let (sender, receiver) = mpsc::sync_channel(1);
            match thread::Builder::new()
                .name("gfs-discard-live-result".to_owned())
                .spawn(move || {
                    let root = project.layout().root.clone();
                    drop(project);
                    let result = claim
                        .ok_or_else(|| "Cannot discard an unclaimed recording directory".to_owned())
                        .and_then(|claim| claim.verify())
                        .and_then(|()| fs::remove_dir_all(&root).map_err(|error| error.to_string()))
                        .map(|()| LiveRecordingOutcome::Discarded)
                        .map_err(|error| {
                            format!(
                                "Could not remove discarded recording {}: {error}",
                                root.display()
                            )
                        });
                    let _ = sender.send(result);
                }) {
                Ok(_) => {
                    self.result = Some(receiver);
                    return None;
                }
                Err(error) => {
                    self.result = None;
                    return Some(Err(format!(
                        "Could not start discard cleanup; the saved project remains: {error}"
                    )));
                }
            }
        }
        self.result = None;
        Some(result)
    }
}

impl Drop for LiveRgbaRecorder {
    fn drop(&mut self) {
        let mut terminal = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(*terminal, Terminal::Running) {
            *terminal = Terminal::Stop(None);
        }
    }
}

fn validate_options(options: &LiveRecordingOptions) -> Result<usize, String> {
    let width = options.canvas.width.get();
    let height = options.canvas.height.get();
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
        return Err(
            "Live recording canvas must be 1..4096 pixels per edge and at most 64 MiB".to_owned(),
        );
    }
    let bytes = u64::from(width) * u64::from(height) * 4;
    if bytes > MAX_FRAME_BYTES {
        return Err("Live recording frame exceeds 64 MiB".to_owned());
    }
    if options.project_id.is_nil()
        || options.app_version.trim().is_empty()
        || options.provisional_duration_us == 0
        || options.provisional_duration_us > 3_600_000_000
        || options.max_frames == 0
        || options.max_frames > 100_000
        || options.max_frame_bytes_total < bytes
    {
        return Err("Live recording metadata, timing or storage limits are invalid".to_owned());
    }
    if options
        .project_path
        .extension()
        .is_none_or(|extension| extension != "gfsproj")
    {
        return Err("Live recording must use a new .gfsproj directory".to_owned());
    }
    usize::try_from(bytes).map_err(|error| error.to_string())
}

struct Writer {
    project: IncrementalRecordingProject,
    last: Option<(FrameId, u64)>,
    bytes: u64,
}

#[derive(Clone)]
struct DirectoryClaim {
    root: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl DirectoryClaim {
    fn capture(root: &std::path::Path) -> Result<Self, String> {
        let metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Recording directory is not a newly claimed regular directory".to_owned());
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
        Err(
            "Recording directory changed externally; refusing to remove replacement contents"
                .to_owned(),
        )
    }
}

fn run(
    options: &LiveRecordingOptions,
    frames: &Receiver<QueuedFrame>,
    terminal: &Mutex<Terminal>,
    progress: &Mutex<LiveRecordingProgress>,
    claim: &Mutex<Option<DirectoryClaim>>,
) -> Result<LiveRecordingOutcome, String> {
    let mut writer: Option<Writer> = None;
    loop {
        let requested = *terminal.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(requested, Terminal::Discard) {
            return discard(writer, claim);
        }
        let frame = if matches!(requested, Terminal::Running) {
            match frames.recv_timeout(POLL_INTERVAL) {
                Ok(frame) => Some(frame),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return finish(writer, None, progress),
            }
        } else {
            frames.try_recv().ok()
        };
        let Some(frame) = frame else {
            let at_us = match requested {
                Terminal::Stop(at) => at,
                _ => None,
            };
            return finish(writer, at_us, progress);
        };
        if matches!(
            *terminal.lock().unwrap_or_else(PoisonError::into_inner),
            Terminal::Discard
        ) {
            return discard(writer, claim);
        }
        if writer.is_none() {
            fs::create_dir(&options.project_path)
                .map_err(|error| format!("Could not claim new recording directory: {error}"))?;
            *claim.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(DirectoryClaim::capture(&options.project_path)?);
            let project = IncrementalRecordingProject::create_with_provenance(
                &options.project_path,
                options.canvas,
                IncrementalRecordingProjectOptions {
                    project_id: options.project_id,
                    app_version: options.app_version.clone(),
                    created_at: options.created_at,
                    source_label: None,
                },
                options.provenance.clone(),
            )
            .map_err(|error| error.to_string())?;
            writer = Some(Writer {
                project,
                last: None,
                bytes: 0,
            });
        }
        let current = writer
            .as_mut()
            .ok_or_else(|| "Recording writer was not created".to_owned())?;
        append(current, frame, options)?;
        let summary = current.project.summary();
        let limit_reached = summary.frames >= options.max_frames
            || current.bytes.saturating_add(
                u64::from(options.canvas.width.get()) * u64::from(options.canvas.height.get()) * 4,
            ) > options.max_frame_bytes_total;
        *progress.lock().unwrap_or_else(PoisonError::into_inner) = LiveRecordingProgress {
            frames: summary.frames,
            duration_us: summary.duration_us,
            limit_reached,
        };
        if limit_reached {
            return finish(writer, None, progress);
        }
    }
}

fn append(
    writer: &mut Writer,
    frame: QueuedFrame,
    options: &LiveRecordingOptions,
) -> Result<(), String> {
    if let Some((id, previous)) = writer.last {
        let duration = frame
            .at_us
            .checked_sub(previous)
            .and_then(DurationUs::new)
            .ok_or_else(|| "Recording frame timestamps must strictly increase".to_owned())?;
        writer
            .project
            .set_frame_duration(id, duration)
            .map_err(|error| error.to_string())?;
    }
    let frame_id = FrameId::from_u128(Uuid::new_v4().as_u128());
    let bytes = u64::try_from(frame.rgba.len()).map_err(|error| error.to_string())?;
    let image = RgbaFrame::new(
        u16::try_from(options.canvas.width.get()).map_err(|error| error.to_string())?,
        u16::try_from(options.canvas.height.get()).map_err(|error| error.to_string())?,
        frame.rgba,
        options.provisional_duration_us,
    )
    .map_err(|error| error.to_string())?;
    writer
        .project
        .append_frame(frame_id, &image)
        .map_err(|error| error.to_string())?;
    writer.last = Some((frame_id, frame.at_us));
    writer.bytes = writer
        .bytes
        .checked_add(bytes)
        .ok_or_else(|| "Recording byte limit overflow".to_owned())?;
    Ok(())
}

fn finish(
    writer: Option<Writer>,
    at_us: Option<u64>,
    progress: &Mutex<LiveRecordingProgress>,
) -> Result<LiveRecordingOutcome, String> {
    let Some(mut writer) = writer else {
        return Ok(LiveRecordingOutcome::Discarded);
    };
    if let (Some(at_us), Some((id, previous))) = (at_us, writer.last) {
        let duration = at_us
            .checked_sub(previous)
            .and_then(DurationUs::new)
            .unwrap_or_else(|| DurationUs::new(1).expect("one microsecond is nonzero"));
        writer
            .project
            .set_frame_duration(id, duration)
            .map_err(|error| error.to_string())?;
    }
    let summary = writer.project.summary();
    let mut reported = progress.lock().unwrap_or_else(PoisonError::into_inner);
    reported.frames = summary.frames;
    reported.duration_us = summary.duration_us;
    drop(reported);
    writer
        .project
        .finish()
        .map(LiveRecordingOutcome::Saved)
        .map_err(|error| error.to_string())
}

fn discard(
    writer: Option<Writer>,
    claim: &Mutex<Option<DirectoryClaim>>,
) -> Result<LiveRecordingOutcome, String> {
    if let Some(writer) = writer {
        let root = writer.project.root().to_owned();
        drop(writer);
        claim
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .ok_or_else(|| "Cannot discard an unclaimed recording directory".to_owned())?
            .verify()?;
        fs::remove_dir_all(&root).map_err(|error| {
            format!(
                "Could not remove discarded recording {}: {error}",
                root.display()
            )
        })?;
    }
    Ok(LiveRecordingOutcome::Discarded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gif_from_screen_project::LockPolicy;
    use std::time::Instant;

    fn options(root: &std::path::Path) -> LiveRecordingOptions {
        LiveRecordingOptions {
            project_path: root.join("recording.gfsproj"),
            canvas: PhysicalSize::new(1, 1).unwrap(),
            project_id: ProjectId::from_u128(1),
            app_version: "live-test".to_owned(),
            created_at: UnixTimeMs::new(0),
            provenance: SourceProvenance::Board,
            provisional_duration_us: 100_000,
            max_frames: 1000,
            max_frame_bytes_total: 4096,
        }
    }

    #[test]
    fn maximum_integer_dimensions_are_rejected_without_arithmetic_overflow() {
        let directory = tempfile::tempdir().unwrap();
        let mut options = options(directory.path());
        options.canvas = PhysicalSize::new(u32::MAX, u32::MAX).unwrap();
        assert!(LiveRgbaRecorder::start(options).is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    fn outcome(recorder: &mut LiveRgbaRecorder) -> Result<LiveRecordingOutcome, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = recorder.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "writer did not finish");
            thread::yield_now();
        }
    }

    fn wait_frames(recorder: &LiveRgbaRecorder, frames: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while recorder.progress().frames < frames {
            assert!(Instant::now() < deadline, "frame was not persisted");
            thread::yield_now();
        }
    }

    #[test]
    fn actual_active_frame_intervals_survive_recovery_and_export() {
        let directory = tempfile::tempdir().unwrap();
        let request = options(directory.path());
        let path = request.project_path.clone();
        let mut recorder = LiveRgbaRecorder::start(request).unwrap();
        assert_eq!(
            recorder.try_frame(100_000, vec![255, 0, 0, 255]).unwrap(),
            LiveFrameSubmission::Accepted
        );
        wait_frames(&recorder, 1);
        recorder.try_frame(350_000, vec![0, 0, 255, 255]).unwrap();
        recorder.stop_at(650_000);
        let LiveRecordingOutcome::Saved(project) = outcome(&mut recorder).unwrap() else {
            panic!("expected saved project");
        };
        assert_eq!(
            project.manifest().source_provenance,
            [SourceProvenance::Board]
        );
        assert_eq!(
            project
                .manifest()
                .timeline
                .frames
                .iter()
                .map(|frame| frame.duration.get())
                .collect::<Vec<_>>(),
            [250_000, 300_000]
        );
        drop(project);
        let reopened = ActiveProject::open(&path, LockPolicy::FailIfPresent).unwrap();
        let output = directory.path().join("board.gif");
        crate::export_project_snapshot_to_gif(
            &crate::ProjectExportSnapshot::from_active(&reopened.project),
            &output,
            &crate::ProjectGifExportOptions::default(),
            &gif_from_screen_gif::NeverCancel,
            &mut crate::NoopProjectExportProgress,
        )
        .unwrap();
        let decoded = gif_from_screen_media::decode_gif(
            fs::File::open(output).unwrap(),
            &gif_from_screen_media::GifDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!(decoded.frames().len(), 2);
        assert_eq!(decoded.frames()[0].duration_us(), 250_000);
        assert_eq!(decoded.frames()[1].rgba(), [0, 0, 255, 255]);
    }

    #[test]
    fn stop_and_discard_before_any_frame_do_not_create_a_directory() {
        for discard in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let request = options(directory.path());
            let path = request.project_path.clone();
            let mut recorder = LiveRgbaRecorder::start(request).unwrap();
            if discard {
                recorder.discard();
            } else {
                recorder.stop_at(1_000);
            }
            assert!(matches!(
                outcome(&mut recorder).unwrap(),
                LiveRecordingOutcome::Discarded
            ));
            assert!(!path.exists());
        }
    }

    #[test]
    fn discard_removes_only_new_recording_and_existing_targets_are_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let request = options(directory.path());
        let path = request.project_path.clone();
        let mut recorder = LiveRgbaRecorder::start(request.clone()).unwrap();
        recorder.try_frame(0, vec![1, 2, 3, 255]).unwrap();
        wait_frames(&recorder, 1);
        recorder.discard();
        assert!(matches!(
            outcome(&mut recorder).unwrap(),
            LiveRecordingOutcome::Discarded
        ));
        assert!(!path.exists());
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep.txt"), "original").unwrap();
        let mut other = LiveRgbaRecorder::start(request).unwrap();
        other.try_frame(0, vec![1, 2, 3, 255]).unwrap();
        assert!(outcome(&mut other).is_err());
        assert_eq!(
            fs::read_to_string(path.join("keep.txt")).unwrap(),
            "original"
        );
    }

    #[test]
    fn two_slot_queue_returns_pixels_under_backpressure_and_stop_never_waits() {
        let (sender, _receiver) = mpsc::sync_channel(2);
        let (_result_sender, result) = mpsc::sync_channel(1);
        let recorder = LiveRgbaRecorder {
            sender,
            terminal: Arc::new(Mutex::new(Terminal::Running)),
            progress: Arc::new(Mutex::new(LiveRecordingProgress::default())),
            result: Some(result),
            frame_bytes: 4,
            claim: Arc::new(Mutex::new(None)),
        };
        assert_eq!(
            recorder.try_frame(0, vec![0; 4]).unwrap(),
            LiveFrameSubmission::Accepted
        );
        assert_eq!(
            recorder.try_frame(1, vec![0; 4]).unwrap(),
            LiveFrameSubmission::Accepted
        );
        assert_eq!(
            recorder.try_frame_retaining(2, vec![7; 4]).unwrap(),
            (LiveFrameSubmission::Backpressure, vec![7; 4])
        );
        recorder.stop_at(3);
        assert_eq!(
            recorder.try_frame(4, vec![0; 4]).unwrap(),
            LiveFrameSubmission::Finishing
        );
    }

    fn completed_unpolled(recorder: &mut LiveRgbaRecorder) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            match recorder.result.as_ref().unwrap().try_recv() {
                Ok(result) => break result,
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline);
                    thread::yield_now();
                }
                Err(error) => panic!("{error}"),
            }
        };
        assert!(matches!(result, Ok(LiveRecordingOutcome::Saved(_))));
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(result).unwrap();
        recorder.result = Some(receiver);
    }

    #[test]
    fn automatic_limit_and_late_discard_are_both_completed_asynchronously() {
        let directory = tempfile::tempdir().unwrap();
        let mut request = options(directory.path());
        request.max_frames = 1;
        let path = request.project_path.clone();
        let mut recorder = LiveRgbaRecorder::start(request).unwrap();
        recorder.try_frame(0, vec![0, 0, 0, 255]).unwrap();
        completed_unpolled(&mut recorder);
        assert!(recorder.progress().limit_reached);
        recorder.discard();
        assert!(matches!(
            outcome(&mut recorder).unwrap(),
            LiveRecordingOutcome::Discarded
        ));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn discard_refuses_directory_replaced_after_recording_finished() {
        let directory = tempfile::tempdir().unwrap();
        let mut request = options(directory.path());
        request.max_frames = 1;
        let path = request.project_path.clone();
        let mut recorder = LiveRgbaRecorder::start(request).unwrap();
        recorder.try_frame(0, vec![0, 0, 0, 255]).unwrap();
        completed_unpolled(&mut recorder);
        let moved = directory.path().join("moved.gfsproj");
        fs::rename(&path, &moved).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep.txt"), "replacement").unwrap();
        recorder.discard();
        assert!(
            outcome(&mut recorder)
                .unwrap_err()
                .contains("changed externally")
        );
        assert_eq!(
            fs::read_to_string(path.join("keep.txt")).unwrap(),
            "replacement"
        );
        assert!(moved.join("manifest.json").exists());
    }

    #[test]
    fn failed_nonmonotonic_input_keeps_already_saved_frames_recoverable() {
        let directory = tempfile::tempdir().unwrap();
        let request = options(directory.path());
        let path = request.project_path.clone();
        let mut recorder = LiveRgbaRecorder::start(request).unwrap();
        recorder.try_frame(5, vec![0, 0, 0, 255]).unwrap();
        wait_frames(&recorder, 1);
        recorder.try_frame(5, vec![1, 0, 0, 255]).unwrap();
        assert!(
            outcome(&mut recorder)
                .unwrap_err()
                .contains("strictly increase")
        );
        let reopened = ActiveProject::open(path, LockPolicy::FailIfPresent).unwrap();
        assert_eq!(reopened.project.manifest().timeline.frames.len(), 1);
    }
}
