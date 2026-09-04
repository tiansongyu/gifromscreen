//! End-to-end tests spanning synthetic capture, workflow normalization, and GIF decoding.

use std::fs;
use std::io::Cursor;
use std::time::Duration;

use gif_from_screen_capture::{
    CaptureCadence, CaptureRequest, CaptureSourceId, CaptureTarget, CaptureTimestamp,
    CapturedFrame, PhysicalSize, PixelFormat, SyntheticCaptureBackend,
};
use gif_from_screen_gif::{CancellationFlag, NeverCancel};
use gif_from_screen_workflow::{
    CollectOptions, CollectionLimit, FrameRetention, NoopWorkflowProgress, RecordToGifOptions,
    RecordingController, WorkflowError, WorkflowPhase, WorkflowProgress, collect,
    partial_output_path, record_to_gif, record_to_gif_controlled,
};

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
