//! Durability barriers for accepted write-behind frames, timing, and manual acknowledgements.

use std::{
    io,
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

use gif_from_screen_capture::{
    CaptureBackend, CaptureCadence, CaptureRequest, CaptureSourceId, CaptureTarget,
    CaptureTimestamp, CapturedFrame, PhysicalSize, PixelFormat, SyntheticCaptureBackend,
};
use gif_from_screen_gif::{NeverCancel, RgbaFrame};
use gif_from_screen_workflow::{
    CollectOptions, CollectionLimit, CollectionSummary, NoopWorkflowProgress, PlaybackTiming,
    RecordingControl, RecordingController, RecordingFrameSink, RecordingFrameSinkError,
    RecordingFrameSinkOperation, RecordingMetadata, SnapshotTriggerRejection,
    SnapshotTriggerStatus, WorkflowError, collect_controlled_to_sink, collect_controlled_with_sink,
    collect_prestarted_controlled_to_sink,
};

#[derive(Clone, Copy, Debug)]
enum Route {
    Full,
    SinkOnly,
    Prestarted,
}

const ROUTES: [Route; 3] = [Route::Full, Route::SinkOnly, Route::Prestarted];

impl Route {
    fn collect(
        self,
        backend: &dyn CaptureBackend,
        request: CaptureRequest,
        options: &CollectOptions,
        control: &mut RecordingControl,
        sink: &mut dyn RecordingFrameSink,
    ) -> Result<CollectionSummary, WorkflowError> {
        match self {
            Self::Full => collect_controlled_with_sink(
                backend,
                request,
                options,
                control,
                sink,
                &NeverCancel,
                &mut NoopWorkflowProgress,
            )
            .map(|recording| recording.summary()),
            Self::SinkOnly => collect_controlled_to_sink(
                backend,
                request,
                options,
                control,
                sink,
                &NeverCancel,
                &mut NoopWorkflowProgress,
            ),
            Self::Prestarted => {
                let mut session = backend.start_session(request)?;
                collect_prestarted_controlled_to_sink(
                    &mut *session,
                    options,
                    control,
                    sink,
                    &NeverCancel,
                    &mut NoopWorkflowProgress,
                )
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Operation {
    Append { index: u64, duration_us: u64 },
    Duration { index: u64, duration_us: u64 },
}

struct FlushGate {
    entered: SyncSender<()>,
    release: Receiver<bool>,
}

#[derive(Default)]
struct DeferredSink {
    pending: Vec<Operation>,
    durable: Vec<Operation>,
    flushes: usize,
    fail_flush: bool,
    first_flush: Option<FlushGate>,
}

impl RecordingFrameSink for DeferredSink {
    fn enqueue_provisional_frame_with_metadata(
        &mut self,
        index: u64,
        frame: &RgbaFrame,
        metadata: &RecordingMetadata,
    ) -> Result<(), RecordingFrameSinkError> {
        assert_eq!(metadata.captured_at.as_micros(), index * 10_000);
        self.pending.push(Operation::Append {
            index,
            duration_us: frame.duration_us(),
        });
        Ok(())
    }

    fn enqueue_frame_duration(
        &mut self,
        index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError> {
        self.pending
            .push(Operation::Duration { index, duration_us });
        Ok(())
    }

    fn flush_pending(&mut self) -> Result<(), RecordingFrameSinkError> {
        self.flushes += 1;
        if let Some(gate) = self.first_flush.take() {
            gate.entered.send(())?;
            if !gate.release.recv()? {
                return Err(io::Error::other("injected deferred flush failure").into());
            }
        }
        if self.fail_flush {
            return Err(io::Error::other("injected deferred flush failure").into());
        }
        self.durable.append(&mut self.pending);
        Ok(())
    }

    fn append_provisional_frame(
        &mut self,
        _: u64,
        _: &RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        panic!("capture must use admission rather than synchronously append");
    }

    fn update_frame_duration(&mut self, _: u64, _: u64) -> Result<(), RecordingFrameSinkError> {
        panic!("capture must enqueue timing rather than synchronously update");
    }
}

fn backend() -> SyntheticCaptureBackend {
    SyntheticCaptureBackend::new(
        [0_u8, 1]
            .into_iter()
            .map(|index| {
                CapturedFrame::new(
                    u64::from(index) + 1,
                    CaptureTimestamp::from_micros(u64::from(index) * 10_000),
                    PhysicalSize::new(1, 1).unwrap(),
                    4,
                    PixelFormat::Rgba8,
                    vec![index, 0, 0, 255],
                )
                .unwrap()
            })
            .collect(),
    )
}

fn request(cadence: CaptureCadence) -> CaptureRequest {
    CaptureRequest::new(
        CaptureTarget::Monitor(CaptureSourceId::new("synthetic:monitor:0").unwrap()),
        cadence,
    )
}

fn options(frames: u64) -> CollectOptions {
    CollectOptions {
        limit: CollectionLimit::MaxFrames(frames),
        tail_frame_duration: Duration::from_micros(7_000),
        poll_interval: Duration::from_millis(1),
        ..CollectOptions::default()
    }
}

#[test]
fn all_collection_routes_flush_frames_and_final_timing_before_success() {
    for route in ROUTES {
        let (controller, mut control) = RecordingController::channel();
        drop(controller);
        let mut sink = DeferredSink::default();
        let summary = route
            .collect(
                &backend(),
                request(CaptureCadence::fixed_fps(60).unwrap()),
                &options(2),
                &mut control,
                &mut sink,
            )
            .unwrap();

        assert_eq!(summary.frames, 2, "{route:?}");
        assert_eq!(summary.duration_us, 17_000, "{route:?}");
        assert_eq!(sink.flushes, 1, "{route:?}");
        assert!(sink.pending.is_empty(), "{route:?}");
        assert_eq!(
            sink.durable,
            [
                Operation::Append {
                    index: 0,
                    duration_us: 7_000
                },
                Operation::Duration {
                    index: 0,
                    duration_us: 10_000
                },
                Operation::Append {
                    index: 1,
                    duration_us: 7_000
                },
                Operation::Duration {
                    index: 1,
                    duration_us: 7_000
                },
            ],
            "{route:?}"
        );
    }
}

#[test]
fn fixed_playback_flushes_even_without_duration_corrections() {
    for route in ROUTES {
        let (controller, mut control) = RecordingController::channel();
        drop(controller);
        let mut sink = DeferredSink::default();
        let summary = route
            .collect(
                &backend(),
                request(CaptureCadence::fixed_fps(60).unwrap()),
                &CollectOptions {
                    playback_timing: PlaybackTiming::Fixed(Duration::from_micros(7_000)),
                    ..options(2)
                },
                &mut control,
                &mut sink,
            )
            .unwrap();

        assert_eq!(summary.duration_us, 14_000, "{route:?}");
        assert_eq!(sink.flushes, 1, "{route:?}");
        assert!(sink.pending.is_empty(), "{route:?}");
        assert_eq!(sink.durable.len(), 2, "{route:?}");
        assert!(
            sink.durable
                .iter()
                .all(|operation| matches!(operation, Operation::Append { .. })),
            "{route:?}"
        );
    }
}

#[test]
fn deferred_flush_failure_cannot_report_success_on_any_collection_route() {
    for route in ROUTES {
        let (controller, mut control) = RecordingController::channel();
        drop(controller);
        let mut sink = DeferredSink {
            fail_flush: true,
            ..DeferredSink::default()
        };
        let error = route
            .collect(
                &backend(),
                request(CaptureCadence::fixed_fps(60).unwrap()),
                &options(2),
                &mut control,
                &mut sink,
            )
            .unwrap_err();

        assert!(
            matches!(
                error,
                WorkflowError::FrameSink {
                    operation: RecordingFrameSinkOperation::FlushPending,
                    frame_index: 1,
                    ..
                }
            ),
            "{route:?}: {error}"
        );
        assert_eq!(sink.flushes, 1, "{route:?}");
        assert!(sink.durable.is_empty(), "{route:?}");
        assert_eq!(sink.pending.len(), 4, "{route:?}");
    }
}

#[test]
fn manual_snapshot_waits_for_durability_and_rejects_deferred_failure_on_all_routes() {
    for route in ROUTES {
        for fail_flush in [false, true] {
            let (controller, mut control) = RecordingController::channel();
            let (entered_tx, entered_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let mut sink = DeferredSink {
                    first_flush: Some(FlushGate {
                        entered: entered_tx,
                        release: release_rx,
                    }),
                    ..DeferredSink::default()
                };
                let result = route.collect(
                    &backend(),
                    request(CaptureCadence::Manual),
                    &options(1),
                    &mut control,
                    &mut sink,
                );
                (result, sink)
            });
            let mut snapshot = controller.trigger_snapshot();
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                snapshot.status(),
                SnapshotTriggerStatus::Pending,
                "{route:?}"
            );

            release_tx.send(!fail_flush).unwrap();
            let (result, sink) = worker.join().unwrap();
            if fail_flush {
                assert!(
                    matches!(
                        result,
                        Err(WorkflowError::FrameSink {
                            operation: RecordingFrameSinkOperation::FlushPending,
                            frame_index: 0,
                            ..
                        })
                    ),
                    "{route:?}"
                );
                assert!(
                    matches!(
                        snapshot.status(),
                        SnapshotTriggerStatus::Rejected(
                            SnapshotTriggerRejection::CollectionFailed(_)
                        )
                    ),
                    "{route:?}"
                );
                assert!(sink.durable.is_empty(), "{route:?}");
            } else {
                assert_eq!(result.unwrap().frames, 1, "{route:?}");
                assert!(
                    matches!(snapshot.status(), SnapshotTriggerStatus::Captured(_)),
                    "{route:?}"
                );
                assert_eq!(sink.flushes, 2, "{route:?}");
                assert!(sink.pending.is_empty(), "{route:?}");
                assert_eq!(sink.durable.len(), 2, "{route:?}");
            }
        }
    }
}
