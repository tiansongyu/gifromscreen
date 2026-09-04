//! End-to-end tests spanning synthetic capture, workflow normalization, and GIF decoding.

use std::fs;
use std::io;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gif_from_screen_capture::{
    BackendDescriptor, BackendStatus, CaptureBackend, CaptureCadence, CaptureCapabilities,
    CaptureError, CaptureErrorKind, CaptureRequest, CaptureSession, CaptureSessionState,
    CaptureSource, CaptureSourceId, CaptureTarget, CaptureTimestamp, CapturedFrame, FramePoll,
    PhysicalSize, PixelFormat, SyntheticCaptureBackend,
};
use gif_from_screen_gif::{CancellationFlag, NeverCancel};
use gif_from_screen_workflow::{
    CollectOptions, CollectionLimit, FrameRetention, NoopWorkflowProgress, RecordToGifOptions,
    RecordingController, RecordingFrameSink, RecordingFrameSinkError, RecordingFrameSinkOperation,
    WorkflowError, WorkflowPhase, WorkflowProgress, collect, collect_controlled_to_sink,
    collect_controlled_with_sink, collect_prestarted_controlled_to_sink, partial_output_path,
    record_to_gif, record_to_gif_controlled,
};

#[derive(Default)]
struct SessionCallCounts {
    resumes: AtomicUsize,
    discards: AtomicUsize,
}

struct TrackingSession {
    inner: Box<dyn CaptureSession>,
    calls: Arc<SessionCallCounts>,
}

impl CaptureSession for TrackingSession {
    fn state(&self) -> CaptureSessionState {
        self.inner.state()
    }

    fn request(&self) -> &CaptureRequest {
        self.inner.request()
    }

    fn update_target(&mut self, target: CaptureTarget) -> Result<(), CaptureError> {
        self.inner.update_target(target)
    }

    fn pause(&mut self) -> Result<(), CaptureError> {
        self.inner.pause()
    }

    fn resume(&mut self) -> Result<(), CaptureError> {
        self.calls.resumes.fetch_add(1, Ordering::Relaxed);
        self.inner.resume()
    }

    fn stop(&mut self) -> Result<(), CaptureError> {
        self.inner.stop()
    }

    fn discard(&mut self) -> Result<(), CaptureError> {
        self.calls.discards.fetch_add(1, Ordering::Relaxed);
        self.inner.discard()
    }

    fn poll_frame(&mut self, timeout: Duration) -> Result<FramePoll, CaptureError> {
        self.inner.poll_frame(timeout)
    }
}

struct CountingBackend {
    inner: SyntheticCaptureBackend,
    starts: Arc<AtomicUsize>,
    session_calls: Arc<SessionCallCounts>,
}

impl CountingBackend {
    fn new(frames: Vec<CapturedFrame>) -> Self {
        Self {
            inner: SyntheticCaptureBackend::new(frames),
            starts: Arc::new(AtomicUsize::new(0)),
            session_calls: Arc::new(SessionCallCounts::default()),
        }
    }
}

impl CaptureBackend for CountingBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.inner.descriptor()
    }

    fn status(&self) -> BackendStatus {
        self.inner.status()
    }

    fn capabilities(&self) -> CaptureCapabilities {
        self.inner.capabilities()
    }

    fn list_sources(&self) -> Result<Vec<CaptureSource>, CaptureError> {
        self.inner.list_sources()
    }

    fn start_session(
        &self,
        request: CaptureRequest,
    ) -> Result<Box<dyn CaptureSession>, CaptureError> {
        self.starts.fetch_add(1, Ordering::Relaxed);
        let inner = self.inner.start_session(request)?;
        Ok(Box::new(TrackingSession {
            inner,
            calls: Arc::clone(&self.session_calls),
        }))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SinkEvent {
    Append {
        frame_index: u64,
        duration_us: u64,
        pixels: Vec<u8>,
    },
    Update {
        frame_index: u64,
        duration_us: u64,
    },
}

#[derive(Default)]
struct TestFrameSink {
    events: Vec<SinkEvent>,
    fail: Option<(RecordingFrameSinkOperation, u64)>,
}

impl RecordingFrameSink for TestFrameSink {
    fn append_provisional_frame(
        &mut self,
        frame_index: u64,
        frame: &gif_from_screen_gif::RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        if self.fail
            == Some((
                RecordingFrameSinkOperation::AppendProvisionalFrame,
                frame_index,
            ))
        {
            return Err(io::Error::other("injected append failure").into());
        }
        self.events.push(SinkEvent::Append {
            frame_index,
            duration_us: frame.duration_us(),
            pixels: frame.pixels().to_vec(),
        });
        Ok(())
    }

    fn update_frame_duration(
        &mut self,
        frame_index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError> {
        if self.fail
            == Some((
                RecordingFrameSinkOperation::UpdateFrameDuration,
                frame_index,
            ))
        {
            return Err(io::Error::other("injected duration failure").into());
        }
        self.events.push(SinkEvent::Update {
            frame_index,
            duration_us,
        });
        Ok(())
    }
}

fn request() -> CaptureRequest {
    CaptureRequest::new(
        CaptureTarget::Monitor(CaptureSourceId::new("synthetic:monitor:0").unwrap()),
        CaptureCadence::fixed_fps(50).unwrap(),
    )
}

fn frame(
    sequence: u64,
    timestamp_us: u64,
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    pixels: Vec<u8>,
) -> CapturedFrame {
    CapturedFrame::new(
        sequence,
        CaptureTimestamp::from_micros(timestamp_us),
        PhysicalSize::new(width, height).unwrap(),
        stride,
        format,
        pixels,
    )
    .unwrap()
}

fn max_frames_options(max_frames: u64, tail_us: u64) -> CollectOptions {
    CollectOptions {
        limit: CollectionLimit::MaxFrames(max_frames),
        tail_frame_duration: Duration::from_micros(tail_us),
        ..CollectOptions::default()
    }
}

#[test]
fn rgba_and_padded_bgra_frames_roundtrip_with_timestamp_durations() {
    let rgba = frame(
        1,
        0,
        2,
        1,
        12,
        PixelFormat::Rgba8,
        vec![255, 0, 0, 255, 0, 255, 0, 255, 9, 9, 9, 9],
    );
    let bgra = frame(
        2,
        20_000,
        2,
        1,
        12,
        PixelFormat::Bgra8,
        vec![255, 0, 0, 255, 0, 255, 255, 255, 7, 7, 7, 7],
    );
    let backend = SyntheticCaptureBackend::new(vec![rgba, bgra]);
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("capture.gif");
    let options = RecordToGifOptions {
        collection: max_frames_options(2, 30_000),
        ..RecordToGifOptions::default()
    };
    let mut progress = Vec::new();

    let report = record_to_gif(
        &backend,
        request(),
        &target,
        &options,
        &NeverCancel,
        &mut |event| progress.push(event),
    )
    .unwrap();

    assert_eq!(report.collection.frames, 2);
    assert_eq!(report.collection.duration_us, 50_000);
    assert!(report.bytes_written > 0);
    assert_eq!(
        progress.last().unwrap().phase,
        gif_from_screen_workflow::WorkflowPhase::Complete
    );
    assert!(!partial_output_path(&target).unwrap().exists());

    let bytes = fs::read(target).unwrap();
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options.read_info(Cursor::new(bytes)).unwrap();
    assert_eq!((decoder.width(), decoder.height()), (2, 1));
    let first = decoder.read_next_frame().unwrap().unwrap().clone();
    let second = decoder.read_next_frame().unwrap().unwrap().clone();
    assert_eq!(first.delay, 2);
    assert_eq!(second.delay, 3);
    assert_eq!(&first.buffer[..8], &[255, 0, 0, 255, 0, 255, 0, 255]);
    assert_eq!(&second.buffer[..8], &[0, 0, 255, 255, 255, 255, 0, 255]);
    assert!(decoder.read_next_frame().unwrap().is_none());
}

#[test]
fn duration_limit_uses_boundary_as_exact_tail_timestamp() {
    let frames = vec![
        frame(1, 10_000, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 30_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
        frame(3, 70_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 0, 255, 255]),
    ];
    let backend = SyntheticCaptureBackend::new(frames);
    let options = CollectOptions {
        limit: CollectionLimit::Duration(Duration::from_millis(50)),
        ..CollectOptions::default()
    };

    let recording = collect(
        &backend,
        request(),
        &options,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap();

    assert_eq!(recording.frames().len(), 2);
    assert_eq!(recording.frames()[0].duration_us(), 20_000);
    assert_eq!(recording.frames()[1].duration_us(), 30_000);
    assert_eq!(recording.summary().duration_us, 50_000);
}

#[test]
fn changes_only_merges_equal_pixels_without_losing_elapsed_time() {
    let red = vec![255, 0, 0, 255];
    let green = vec![0, 255, 0, 255];
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, red.clone()),
        frame(2, 10_000, 1, 1, 4, PixelFormat::Rgba8, red),
        frame(3, 30_000, 1, 1, 4, PixelFormat::Rgba8, green.clone()),
        frame(4, 50_000, 1, 1, 4, PixelFormat::Rgba8, green),
    ]);
    let recording = collect(
        &backend,
        request(),
        &CollectOptions {
            limit: CollectionLimit::UntilStopped,
            frame_retention: FrameRetention::ChangesOnly,
            tail_frame_duration: Duration::from_millis(20),
            ..CollectOptions::default()
        },
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap();

    assert_eq!(recording.frames().len(), 2);
    assert_eq!(recording.frames()[0].duration_us(), 30_000);
    assert_eq!(recording.frames()[1].duration_us(), 40_000);
    assert_eq!(recording.summary().duration_us, 70_000);
    assert_eq!(recording.summary().rgba_bytes, 8);
}

#[test]
fn changes_only_frame_limit_counts_retained_changes() {
    let red = vec![255, 0, 0, 255];
    let green = vec![0, 255, 0, 255];
    let blue = vec![0, 0, 255, 255];
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, red.clone()),
        frame(2, 10_000, 1, 1, 4, PixelFormat::Rgba8, red),
        frame(3, 20_000, 1, 1, 4, PixelFormat::Rgba8, green),
        frame(4, 30_000, 1, 1, 4, PixelFormat::Rgba8, blue),
    ]);
    let recording = collect(
        &backend,
        request(),
        &CollectOptions {
            frame_retention: FrameRetention::ChangesOnly,
            ..max_frames_options(2, 10_000)
        },
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap();

    assert_eq!(recording.frames().len(), 2);
    assert_eq!(recording.frames()[0].duration_us(), 20_000);
    assert_eq!(recording.frames()[1].duration_us(), 10_000);
    assert_eq!(recording.summary().duration_us, 30_000);
}

#[test]
fn incremental_sink_journals_provisional_frames_and_corrects_all_durations() {
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 10_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
        frame(3, 30_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 0, 255, 255]),
    ]);
    let (controller, mut control) = RecordingController::channel();
    drop(controller);
    let mut sink = TestFrameSink::default();

    let recording = collect_controlled_with_sink(
        &backend,
        request(),
        &CollectOptions {
            limit: CollectionLimit::UntilStopped,
            tail_frame_duration: Duration::from_millis(5),
            ..CollectOptions::default()
        },
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap();

    assert_eq!(
        sink.events,
        [
            SinkEvent::Append {
                frame_index: 0,
                duration_us: 5_000,
                pixels: vec![255, 0, 0, 255]
            },
            SinkEvent::Update {
                frame_index: 0,
                duration_us: 10_000
            },
            SinkEvent::Append {
                frame_index: 1,
                duration_us: 5_000,
                pixels: vec![0, 255, 0, 255]
            },
            SinkEvent::Update {
                frame_index: 1,
                duration_us: 20_000
            },
            SinkEvent::Append {
                frame_index: 2,
                duration_us: 5_000,
                pixels: vec![0, 0, 255, 255]
            },
            SinkEvent::Update {
                frame_index: 2,
                duration_us: 5_000
            },
        ]
    );
    assert_eq!(
        recording
            .frames()
            .iter()
            .map(gif_from_screen_gif::RgbaFrame::duration_us)
            .collect::<Vec<_>>(),
        [10_000, 20_000, 5_000]
    );
}

#[test]
fn changes_only_persists_first_static_frame_before_final_duration_is_known() {
    let red = vec![255, 0, 0, 255];
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, red.clone()),
        frame(2, 10_000, 1, 1, 4, PixelFormat::Rgba8, red.clone()),
        frame(3, 20_000, 1, 1, 4, PixelFormat::Rgba8, red),
    ]);
    let (controller, mut control) = RecordingController::channel();
    drop(controller);
    let mut sink = TestFrameSink {
        fail: Some((RecordingFrameSinkOperation::UpdateFrameDuration, 0)),
        ..TestFrameSink::default()
    };

    let error = collect_controlled_to_sink(
        &backend,
        request(),
        &CollectOptions {
            limit: CollectionLimit::UntilStopped,
            frame_retention: FrameRetention::ChangesOnly,
            tail_frame_duration: Duration::from_millis(5),
            ..CollectOptions::default()
        },
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowError::FrameSink {
            operation: RecordingFrameSinkOperation::UpdateFrameDuration,
            frame_index: 0,
            ..
        }
    ));
    assert_eq!(
        sink.events,
        [SinkEvent::Append {
            frame_index: 0,
            duration_us: 5_000,
            pixels: vec![255, 0, 0, 255]
        }]
    );
}

#[test]
fn sink_append_failure_is_typed_and_stops_before_later_frames() {
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 10_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
        frame(3, 20_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 0, 255, 255]),
    ]);
    let (controller, mut control) = RecordingController::channel();
    drop(controller);
    let mut sink = TestFrameSink {
        fail: Some((RecordingFrameSinkOperation::AppendProvisionalFrame, 1)),
        ..TestFrameSink::default()
    };

    let error = collect_controlled_to_sink(
        &backend,
        request(),
        &max_frames_options(3, 5_000),
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowError::FrameSink {
            operation: RecordingFrameSinkOperation::AppendProvisionalFrame,
            frame_index: 1,
            ..
        }
    ));
    assert_eq!(
        sink.events,
        [
            SinkEvent::Append {
                frame_index: 0,
                duration_us: 5_000,
                pixels: vec![255, 0, 0, 255]
            },
            SinkEvent::Update {
                frame_index: 0,
                duration_us: 10_000
            },
        ]
    );
}

#[test]
fn sink_only_collection_bounds_resident_frame_instead_of_total_recording() {
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 10_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
        frame(3, 20_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 0, 255, 255]),
    ]);
    let (controller, mut control) = RecordingController::channel();
    drop(controller);
    let mut sink = TestFrameSink::default();

    let summary = collect_controlled_to_sink(
        &backend,
        request(),
        &CollectOptions {
            frame_buffer_limit_bytes: 4,
            ..max_frames_options(3, 5_000)
        },
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap();

    assert_eq!(summary.frames, 3);
    assert_eq!(summary.rgba_bytes, 12);
    assert_eq!(summary.duration_us, 25_000);
}

#[test]
fn prestarted_sink_collection_reuses_the_session_and_persists_its_frames() {
    let backend = CountingBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 10_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
    ]);
    let mut session = backend.start_session(request()).unwrap();
    let (controller, mut control) = RecordingController::channel();
    drop(controller);
    let mut sink = TestFrameSink::default();

    let summary = collect_prestarted_controlled_to_sink(
        &mut *session,
        &max_frames_options(2, 5_000),
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap();

    assert_eq!(backend.starts.load(Ordering::Relaxed), 1);
    assert_eq!(summary.frames, 2);
    assert_eq!(summary.duration_us, 15_000);
    assert_eq!(
        sink.events,
        [
            SinkEvent::Append {
                frame_index: 0,
                duration_us: 5_000,
                pixels: vec![255, 0, 0, 255]
            },
            SinkEvent::Update {
                frame_index: 0,
                duration_us: 10_000
            },
            SinkEvent::Append {
                frame_index: 1,
                duration_us: 5_000,
                pixels: vec![0, 255, 0, 255]
            },
            SinkEvent::Update {
                frame_index: 1,
                duration_us: 5_000
            },
        ]
    );
}

#[test]
fn prestarted_paused_session_resumes_through_the_existing_control_path() {
    let backend = CountingBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 10_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
    ]);
    let mut session = backend.start_session(request()).unwrap();
    session.pause().unwrap();
    let (controller, mut control) = RecordingController::channel();
    assert!(controller.resume());
    let mut sink = TestFrameSink::default();

    let summary = collect_prestarted_controlled_to_sink(
        &mut *session,
        &max_frames_options(2, 5_000),
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap();

    assert_eq!(summary.frames, 2);
    assert_eq!(backend.starts.load(Ordering::Relaxed), 1);
    assert_eq!(backend.session_calls.resumes.load(Ordering::Relaxed), 1);
    assert_eq!(session.state(), CaptureSessionState::Stopped);
}

#[test]
fn prestarted_terminal_session_returns_a_typed_error() {
    let backend = CountingBackend::new(vec![frame(
        1,
        0,
        1,
        1,
        4,
        PixelFormat::Rgba8,
        vec![255, 0, 0, 255],
    )]);
    let mut session = backend.start_session(request()).unwrap();
    session.stop().unwrap();
    let (controller, mut control) = RecordingController::channel();
    drop(controller);
    let mut sink = TestFrameSink::default();

    let error = collect_prestarted_controlled_to_sink(
        &mut *session,
        &max_frames_options(1, 5_000),
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowError::Capture(error)
            if error.kind() == CaptureErrorKind::InvalidStateTransition
    ));
    assert_eq!(backend.session_calls.discards.load(Ordering::Relaxed), 1);
}

#[test]
fn prestarted_collection_discards_after_sink_failure() {
    let backend = CountingBackend::new(vec![frame(
        1,
        0,
        1,
        1,
        4,
        PixelFormat::Rgba8,
        vec![255, 0, 0, 255],
    )]);
    let mut session = backend.start_session(request()).unwrap();
    let (controller, mut control) = RecordingController::channel();
    drop(controller);
    let mut sink = TestFrameSink {
        fail: Some((RecordingFrameSinkOperation::AppendProvisionalFrame, 0)),
        ..TestFrameSink::default()
    };

    let error = collect_prestarted_controlled_to_sink(
        &mut *session,
        &max_frames_options(1, 5_000),
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        WorkflowError::FrameSink {
            operation: RecordingFrameSinkOperation::AppendProvisionalFrame,
            frame_index: 0,
            ..
        }
    ));
    assert_eq!(backend.session_calls.discards.load(Ordering::Relaxed), 1);
    assert_eq!(session.state(), CaptureSessionState::Discarded);
}

#[test]
fn empty_capture_is_typed() {
    let backend = SyntheticCaptureBackend::new(Vec::new());
    let error = collect(
        &backend,
        request(),
        &max_frames_options(1, 10_000),
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();
    assert!(matches!(error, WorkflowError::EmptyCapture));
}

#[test]
fn rejects_dimension_changes_and_gif_oversize() {
    let changed = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![0; 4]),
        frame(2, 10_000, 2, 1, 8, PixelFormat::Rgba8, vec![0; 8]),
    ]);
    let error = collect(
        &changed,
        request(),
        &max_frames_options(2, 10_000),
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::DimensionMismatch { frame_index: 1, .. }
    ));

    let oversize = SyntheticCaptureBackend::new(vec![frame(
        1,
        0,
        65_536,
        1,
        65_536 * 4,
        PixelFormat::Rgba8,
        vec![0; 65_536 * 4],
    )]);
    let error = collect(
        &oversize,
        request(),
        &max_frames_options(1, 10_000),
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::DimensionsOutOfRange { frame_index: 0, .. }
    ));
}

#[test]
fn rejects_equal_timestamps_and_buffer_limit() {
    let equal = SyntheticCaptureBackend::new(vec![
        frame(1, 42, 1, 1, 4, PixelFormat::Rgba8, vec![0; 4]),
        frame(2, 42, 1, 1, 4, PixelFormat::Rgba8, vec![1; 4]),
    ]);
    let error = collect(
        &equal,
        request(),
        &max_frames_options(2, 10_000),
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::NonMonotonicTimestamp { frame_index: 1, .. }
    ));

    let backend =
        SyntheticCaptureBackend::new(vec![frame(1, 0, 2, 1, 8, PixelFormat::Rgba8, vec![0; 8])]);
    let options = CollectOptions {
        frame_buffer_limit_bytes: 7,
        ..max_frames_options(1, 10_000)
    };
    let error = collect(
        &backend,
        request(),
        &options,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        WorkflowError::FrameBufferLimitExceeded {
            required_bytes: 8,
            limit_bytes: 7
        }
    ));
}

#[test]
fn cancellation_and_encode_failure_leave_no_partial_output() {
    let backend = SyntheticCaptureBackend::new(vec![frame(
        1,
        0,
        1,
        1,
        4,
        PixelFormat::Rgba8,
        vec![0, 0, 0, 255],
    )]);
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("existing.gif");
    fs::write(&target, b"original").unwrap();
    let partial = partial_output_path(&target).unwrap();

    let cancellation = CancellationFlag::default();
    cancellation.cancel();
    let error = record_to_gif(
        &backend,
        request(),
        &target,
        &RecordToGifOptions::default(),
        &cancellation,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();
    assert!(matches!(error, WorkflowError::Cancelled));
    assert!(!partial.exists());
    assert_eq!(fs::read(&target).unwrap(), b"original");

    let options = RecordToGifOptions {
        collection: max_frames_options(1, 10_000),
        encoding: gif_from_screen_gif::EncodeOptions {
            max_colors: 1,
            ..gif_from_screen_gif::EncodeOptions::default()
        },
    };
    let error = record_to_gif(
        &backend,
        request(),
        &target,
        &options,
        &NeverCancel,
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();
    assert!(matches!(error, WorkflowError::Encode(_)));
    assert!(!partial.exists());
    assert_eq!(fs::read(target).unwrap(), b"original");
}

#[test]
fn controlled_stop_encodes_frames_collected_so_far() {
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 20_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
        frame(3, 40_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 0, 255, 255]),
    ]);
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("stopped.gif");
    let (controller, mut control) = RecordingController::channel();
    let progress_controller = controller.clone();
    let options = RecordToGifOptions {
        collection: CollectOptions {
            limit: CollectionLimit::UntilStopped,
            tail_frame_duration: Duration::from_millis(15),
            ..CollectOptions::default()
        },
        ..RecordToGifOptions::default()
    };

    let report = record_to_gif_controlled(
        &backend,
        request(),
        &target,
        &options,
        &mut control,
        &NeverCancel,
        &mut move |progress: WorkflowProgress| {
            if progress.phase == WorkflowPhase::Capturing && progress.frames_captured == 2 {
                assert!(progress_controller.stop());
            }
        },
    )
    .unwrap();

    assert_eq!(report.collection.frames, 2);
    assert_eq!(report.collection.duration_us, 35_000);
    assert!(target.is_file());
}

#[test]
fn controlled_stop_finalizes_incremental_sink_tail() {
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 20_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
        frame(3, 40_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 0, 255, 255]),
    ]);
    let (controller, mut control) = RecordingController::channel();
    let stop_controller = controller.clone();
    let mut sink = TestFrameSink::default();

    let summary = collect_controlled_to_sink(
        &backend,
        request(),
        &CollectOptions {
            limit: CollectionLimit::UntilStopped,
            tail_frame_duration: Duration::from_millis(15),
            ..CollectOptions::default()
        },
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut move |progress: WorkflowProgress| {
            if progress.phase == WorkflowPhase::Capturing && progress.frames_captured == 2 {
                assert!(stop_controller.stop());
            }
        },
    )
    .unwrap();

    assert_eq!(summary.duration_us, 35_000);
    assert_eq!(
        sink.events.last(),
        Some(&SinkEvent::Update {
            frame_index: 1,
            duration_us: 15_000
        })
    );
}

#[test]
fn duration_limit_excludes_time_spent_paused() {
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 5_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
        frame(3, 10_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 0, 255, 255]),
    ]);
    let (controller, mut control) = RecordingController::channel();
    let pause_controller = controller.clone();
    let resume_controller = controller.clone();
    let resume_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        assert!(resume_controller.resume());
    });
    let mut requested_pause = false;
    let recording = gif_from_screen_workflow::collect_controlled(
        &backend,
        request(),
        &CollectOptions {
            limit: CollectionLimit::Duration(Duration::from_millis(10)),
            poll_interval: Duration::from_millis(1),
            ..CollectOptions::default()
        },
        &mut control,
        &NeverCancel,
        &mut move |progress: WorkflowProgress| {
            if !requested_pause
                && progress.phase == WorkflowPhase::Capturing
                && progress.frames_captured == 1
            {
                assert!(pause_controller.pause());
                requested_pause = true;
            }
        },
    )
    .unwrap();
    resume_thread.join().unwrap();

    assert_eq!(recording.frames().len(), 2);
    assert_eq!(recording.frames()[0].duration_us(), 5_000);
    assert_eq!(recording.frames()[1].duration_us(), 5_000);
    assert_eq!(recording.summary().duration_us, 10_000);
}

#[test]
fn controlled_discard_never_creates_an_output() {
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 20_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
    ]);
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("discarded.gif");
    let (controller, mut control) = RecordingController::channel();
    let progress_controller = controller.clone();

    let error = record_to_gif_controlled(
        &backend,
        request(),
        &target,
        &RecordToGifOptions {
            collection: max_frames_options(2, 10_000),
            ..RecordToGifOptions::default()
        },
        &mut control,
        &NeverCancel,
        &mut move |progress: WorkflowProgress| {
            if progress.phase == WorkflowPhase::Capturing && progress.frames_captured == 1 {
                assert!(progress_controller.discard());
            }
        },
    )
    .unwrap_err();

    assert!(matches!(error, WorkflowError::Discarded));
    assert!(!target.exists());
    assert!(!partial_output_path(&target).unwrap().exists());
}

#[test]
fn controlled_discard_does_not_finalize_incremental_sink() {
    let backend = SyntheticCaptureBackend::new(vec![
        frame(1, 0, 1, 1, 4, PixelFormat::Rgba8, vec![255, 0, 0, 255]),
        frame(2, 20_000, 1, 1, 4, PixelFormat::Rgba8, vec![0, 255, 0, 255]),
    ]);
    let (controller, mut control) = RecordingController::channel();
    let discard_controller = controller.clone();
    let mut sink = TestFrameSink::default();

    let error = collect_controlled_to_sink(
        &backend,
        request(),
        &max_frames_options(2, 10_000),
        &mut control,
        &mut sink,
        &NeverCancel,
        &mut move |progress: WorkflowProgress| {
            if progress.phase == WorkflowPhase::Capturing && progress.frames_captured == 1 {
                assert!(discard_controller.discard());
            }
        },
    )
    .unwrap_err();

    assert!(matches!(error, WorkflowError::Discarded));
    assert_eq!(
        sink.events,
        [SinkEvent::Append {
            frame_index: 0,
            duration_us: 10_000,
            pixels: vec![255, 0, 0, 255]
        }]
    );
}
