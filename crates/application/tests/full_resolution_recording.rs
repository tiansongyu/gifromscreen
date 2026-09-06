//! Opt-in full-resolution sink-only persistence benchmark, not a native FPS test.

use std::{
    fs,
    time::{Duration, Instant},
};

use gif_from_screen_application::{
    INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES, IncrementalRecordingProject,
    IncrementalRecordingProjectOptions,
};
use gif_from_screen_capture::{
    CaptureCadence, CaptureError, CaptureRequest, CaptureSession, CaptureSessionState,
    CaptureSourceId, CaptureTarget, CaptureTimestamp, CapturedFrame, FramePoll,
    PhysicalSize as CaptureSize, PixelFormat,
};
use gif_from_screen_domain::{DurationUs, FrameId, PhysicalSize, ProjectId, UnixTimeMs};
use gif_from_screen_gif::{NeverCancel, RgbaFrame};
use gif_from_screen_project::{ActiveProject, LockPolicy};
use gif_from_screen_workflow::{
    CollectOptions, CollectionLimit, FrameRetention, NoopWorkflowProgress, RecordingController,
    RecordingFrameSink, RecordingFrameSinkError, collect_prestarted_controlled_to_sink,
};

const WIDTH: u32 = 1_280;
const HEIGHT: u32 = 720;
const FRAME_COUNT: u64 = 1_000;
const FRAME_BYTES: u64 = WIDTH as u64 * HEIGHT as u64 * 4;
const FRAME_DURATION_US: u64 = 33_333;
const TAIL_DURATION_US: u64 = 100_000;
const ASSET_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const _: () = assert!(FRAME_COUNT * FRAME_BYTES < ASSET_BUDGET_BYTES);

struct GeneratedSession {
    request: CaptureRequest,
    next: u64,
    state: CaptureSessionState,
}

fn frame_color(sequence: u64) -> [u8; 4] {
    let bytes = sequence.to_le_bytes();
    [bytes[0], bytes[1], bytes[2], 255]
}

impl CaptureSession for GeneratedSession {
    fn state(&self) -> CaptureSessionState {
        self.state
    }

    fn request(&self) -> &CaptureRequest {
        &self.request
    }

    fn update_target(&mut self, _target: CaptureTarget) -> Result<(), CaptureError> {
        Err(CaptureError::invalid_request(
            "benchmark source has a fixed target",
        ))
    }

    fn pause(&mut self) -> Result<(), CaptureError> {
        self.state = CaptureSessionState::Paused;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), CaptureError> {
        self.state = CaptureSessionState::Recording;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), CaptureError> {
        self.state = CaptureSessionState::Stopped;
        Ok(())
    }

    fn discard(&mut self) -> Result<(), CaptureError> {
        self.state = CaptureSessionState::Discarded;
        Ok(())
    }

    fn poll_frame(&mut self, _timeout: Duration) -> Result<FramePoll, CaptureError> {
        if self.state == CaptureSessionState::Paused {
            return Ok(FramePoll::Pending);
        }
        if self.state.is_terminal() || self.next == FRAME_COUNT {
            return Ok(FramePoll::EndOfStream);
        }
        // Unique solid-color frames minimize generator work while requiring
        // complete independent RGBA assets. No preceding source frame is kept.
        let pixels = frame_color(self.next).repeat(usize::try_from(FRAME_BYTES / 4).unwrap());
        let frame = CapturedFrame::new(
            self.next,
            CaptureTimestamp::from_micros(self.next * FRAME_DURATION_US),
            CaptureSize::new(WIDTH, HEIGHT)?,
            usize::try_from(WIDTH).unwrap() * 4,
            PixelFormat::Rgba8,
            pixels,
        )?;
        self.next += 1;
        Ok(FramePoll::Frame(frame))
    }
}

struct ProjectSink(IncrementalRecordingProject);

impl RecordingFrameSink for ProjectSink {
    fn append_provisional_frame(
        &mut self,
        frame_index: u64,
        frame: &RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        self.0
            .append_frame(FrameId::from_u128(u128::from(frame_index) + 1), frame)?;
        Ok(())
    }

    fn update_frame_duration(
        &mut self,
        frame_index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError> {
        self.0.set_frame_duration(
            FrameId::from_u128(u128::from(frame_index) + 1),
            DurationUs::new(duration_us).expect("workflow durations are positive"),
        )?;
        Ok(())
    }
}

fn rss_high_water_kib() -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn verify_recorded_assets(project: &ActiveProject, expected_frames: usize) -> u64 {
    let mut verified_bytes = 0_u64;
    for (index, frame) in project.manifest().timeline.frames.iter().enumerate() {
        assert_eq!(frame.id, FrameId::from_u128(index as u128 + 1));
        let expected_duration = if index + 1 == expected_frames {
            TAIL_DURATION_US
        } else {
            FRAME_DURATION_US
        };
        assert_eq!(frame.duration.get(), expected_duration);
        let pixels = project.assets().read(frame.asset_id).unwrap();
        assert_eq!(u64::try_from(pixels.len()).unwrap(), FRAME_BYTES);
        assert_eq!(&pixels[..4], &frame_color(u64::try_from(index).unwrap()));
        verified_bytes += u64::try_from(pixels.len()).unwrap();
    }
    let disk_entries: Vec<_> = fs::read_dir(project.assets().directory())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .collect();
    assert_eq!(disk_entries.len(), expected_frames);
    assert_eq!(disk_entries.iter().sum::<u64>(), verified_bytes);
    assert!(verified_bytes < ASSET_BUDGET_BYTES);
    verified_bytes
}

#[test]
#[ignore = "writes 3.43 GiB of unique temporary assets; run explicitly as a persistence benchmark"]
fn full_resolution_sink_only_recording_recovers_all_unique_assets() {
    let directory = tempfile::tempdir().unwrap();
    let temporary_path = directory.path().to_path_buf();
    let mut sink = ProjectSink(
        IncrementalRecordingProject::create(
            directory.path(),
            PhysicalSize::new(WIDTH, HEIGHT).unwrap(),
            IncrementalRecordingProjectOptions {
                project_id: ProjectId::from_u128(720_1000),
                app_version: "full-resolution-persistence-benchmark".to_owned(),
                created_at: UnixTimeMs::new(0),
                source_label: Some("Unpaced synthetic 720p RGBA frames".to_owned()),
            },
        )
        .unwrap(),
    );
    let mut session = GeneratedSession {
        request: CaptureRequest::new(
            CaptureTarget::Monitor(CaptureSourceId::new("benchmark:generated").unwrap()),
            CaptureCadence::fixed_fps(30).unwrap(),
        ),
        next: 0,
        state: CaptureSessionState::Recording,
    };
    let (_controller, mut control) = RecordingController::channel();
    let baseline_rss_kib = rss_high_water_kib();
    let started = Instant::now();
    let summary = collect_prestarted_controlled_to_sink(
        &mut session,
        &CollectOptions {
            limit: CollectionLimit::MaxFrames(FRAME_COUNT),
            frame_retention: FrameRetention::All,
            tail_frame_duration: Duration::from_micros(TAIL_DURATION_US),
            // Sink-only accounting admits one resident normalized frame even
            // though the aggregate recording is a thousand times larger.
            frame_buffer_limit_bytes: FRAME_BYTES,
            ..CollectOptions::default()
        },
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap();
    let persist_elapsed = started.elapsed();
    let capture_rss_kib = rss_high_water_kib();
    let retained_frames = usize::try_from(FRAME_COUNT).unwrap();
    assert_eq!(summary.frames, FRAME_COUNT);
    assert_eq!(summary.rgba_bytes, FRAME_COUNT * FRAME_BYTES);
    assert_eq!(
        summary.duration_us,
        (FRAME_COUNT - 1) * FRAME_DURATION_US + TAIL_DURATION_US
    );
    assert_eq!(sink.0.summary().frames, retained_frames);
    assert_eq!(sink.0.summary().duration_us, summary.duration_us);
    assert_eq!(session.state(), CaptureSessionState::Stopped);
    // Skip finish() deliberately: the next open must recover the real tail
    // left by automatic checkpoints plus synchronous incremental journaling.
    drop(sink);
    let reopen_started = Instant::now();
    let opened = ActiveProject::open(directory.path(), LockPolicy::FailIfPresent).unwrap();
    let reopen_elapsed = reopen_started.elapsed();
    assert!(opened.asset_issues.is_empty());
    assert!(opened.journal_recovery.is_clean());
    assert_eq!(
        opened.project.manifest().timeline.frames.len(),
        retained_frames
    );
    assert_eq!(opened.project.manifest().assets.len(), retained_frames);
    assert!(opened.journal_recovery.replayed_records > 0);
    assert!(
        opened.journal_recovery.replayed_records
            <= u64::try_from(2 * INCREMENTAL_RECORDING_CHECKPOINT_INTERVAL_FRAMES + 1,).unwrap()
    );
    let verification_started = Instant::now();
    let verified_bytes = verify_recorded_assets(&opened.project, retained_frames);
    let verification_elapsed = verification_started.elapsed();
    let final_rss_kib = rss_high_water_kib();
    if let (Some(baseline), Some(peak)) = (baseline_rss_kib, final_rss_kib) {
        assert!(
            peak.saturating_sub(baseline) < 256 * 1024,
            "resident growth must stay far below the 3.43 GiB recording"
        );
    }
    eprintln!(
        "720p SinkOnly: frames={}, asset_bytes={verified_bytes}, persist={persist_elapsed:?}, reopen={reopen_elapsed:?}, verify_all_digests={verification_elapsed:?}, replayed_records={}, rss_kib_baseline={baseline_rss_kib:?}, rss_kib_after_capture={capture_rss_kib:?}, rss_kib_final={final_rss_kib:?}",
        summary.frames, opened.journal_recovery.replayed_records,
    );
    drop(opened);
    directory.close().unwrap();
    assert!(!temporary_path.exists());
    eprintln!("All benchmark assets and the temporary project were removed.");
}
