use super::*;
use gif_from_screen_gif::RgbaFrame;
use gif_from_screen_workflow::{CollectionSummary, PlaybackTiming, RecordingMetadata};

#[derive(Clone, Copy, Debug)]
enum Route {
    Buffered,
    BufferedSink,
    SinkOnly,
    PrestartedSink,
}

const ROUTES: [Route; 4] = [
    Route::Buffered,
    Route::BufferedSink,
    Route::SinkOnly,
    Route::PrestartedSink,
];

#[test]
fn fixed_interaction_timing_keeps_raw_clock_and_filters_changes_on_all_routes() {
    let mut interaction = request();
    interaction.cadence = CaptureCadence::OnInteraction;
    assert!(!interaction.input_events);
    let frames = samples();
    for retention in [FrameRetention::All, FrameRetention::ChangesOnly] {
        for route in ROUTES {
            let run = run_route(
                route,
                &frames,
                interaction.clone(),
                &CollectOptions {
                    frame_retention: retention,
                    ..fixed_options()
                },
            );
            let retained = if retention == FrameRetention::All {
                vec![0, 1, 2, 3]
            } else {
                vec![0, 2]
            };
            assert_eq!(run.summary.frames, retained.len() as u64);
            assert_eq!(run.summary.duration_us, retained.len() as u64 * 66_000);
            assert_eq!(run.summary.capture_duration_us, 7_200_000_000);
            for (metadata, index) in run.metadata.iter().zip(retained) {
                assert_eq!(metadata.captured_at, frames[index].captured_at());
                assert!(metadata.input_events.is_empty());
            }
            assert!(run.frames.iter().all(|frame| frame.duration_us() == 66_000));
        }
    }
}

struct PollBudget(AtomicUsize);
impl gif_from_screen_gif::CancellationToken for PollBudget {
    fn is_cancelled(&self) -> bool {
        self.0.fetch_add(1, Ordering::Relaxed) >= 500
    }
}

#[test]
fn interaction_prestarted_active_time_is_counted_before_any_frame_is_polled() {
    let backend = SyntheticCaptureBackend::new(Vec::new());
    let mut interaction = request();
    interaction.cadence = CaptureCadence::OnInteraction;
    let calls = Arc::new(SessionCallCounts::default());
    let mut session = TrackingSession {
        inner: backend.start_session(interaction).unwrap(),
        calls: Arc::clone(&calls),
        stalled: true,
        reported_active_elapsed: Some(Duration::from_secs(2)),
    };
    let (_controller, mut control) = RecordingController::channel();
    let mut sink = TestFrameSink::default();
    let error = collect_prestarted_controlled_to_sink(
        &mut session,
        &CollectOptions {
            limit: CollectionLimit::Duration(Duration::from_secs(1)),
            ..fixed_options()
        },
        &mut control,
        &mut sink,
        &PollBudget(AtomicUsize::new(0)),
        &mut NoopWorkflowProgress,
    )
    .unwrap_err();
    assert!(matches!(error, WorkflowError::EmptyCapture));
    assert_eq!(
        calls.polls.load(Ordering::Relaxed),
        0,
        "already-spent active time cannot be restarted by attaching the collector"
    );
    assert!(sink.events.is_empty());
}

#[test]
fn interaction_idle_cancellation_discards_and_other_cadence_deadlines_remain_unchanged() {
    for cadence in [CaptureCadence::OnInteraction, CaptureCadence::Manual] {
        for sink_only in [false, true] {
            let mut backend = CountingBackend::new(Vec::new());
            backend.stalled = true;
            let mut capture = request();
            capture.cadence = cadence;
            let (_controller, mut control) = RecordingController::channel();
            let mut sink = TestFrameSink::default();
            let options = CollectOptions {
                // Manual's pre-existing first-sample clock intentionally stays
                // unchanged in this cohort; interaction cancellation is idle.
                limit: if cadence == CaptureCadence::Manual {
                    CollectionLimit::Duration(Duration::from_micros(1))
                } else {
                    CollectionLimit::UntilStopped
                },
                poll_interval: Duration::from_millis(1),
                ..fixed_options()
            };
            let budget = PollBudget(AtomicUsize::new(490));
            let error = if sink_only {
                collect_controlled_to_sink(
                    &backend,
                    capture,
                    &options,
                    &mut control,
                    &mut sink,
                    &budget,
                    &mut NoopWorkflowProgress,
                )
                .unwrap_err()
            } else {
                collect_controlled(
                    &backend,
                    capture,
                    &options,
                    &mut control,
                    &budget,
                    &mut NoopWorkflowProgress,
                )
                .unwrap_err()
            };
            assert!(matches!(error, WorkflowError::Cancelled));
            assert_eq!(backend.session_calls.discards.load(Ordering::Relaxed), 1);
            assert!(sink.events.is_empty());
        }
    }
}

#[test]
fn interaction_duration_without_input_expires_and_excludes_initial_pause_on_all_routes() {
    for route in ROUTES {
        for initially_paused in [false, true] {
            let mut backend = CountingBackend::new(Vec::new());
            backend.stalled = true;
            let mut interaction = request();
            interaction.cadence = CaptureCadence::OnInteraction;
            let (controller, mut control) = RecordingController::channel();
            if initially_paused {
                assert!(controller.pause());
            }
            let mut resumed = false;
            let mut progress = |progress: WorkflowProgress| {
                if progress.phase == WorkflowPhase::Paused && !resumed {
                    std::thread::sleep(Duration::from_millis(30));
                    assert!(controller.resume());
                    resumed = true;
                }
            };
            let options = CollectOptions {
                limit: CollectionLimit::Duration(Duration::from_millis(20)),
                poll_interval: Duration::from_millis(1),
                ..fixed_options()
            };
            let budget = PollBudget(AtomicUsize::new(0));
            let started = std::time::Instant::now();
            let mut sink = TestFrameSink::default();
            let error = match route {
                Route::Buffered => collect_controlled(
                    &backend,
                    interaction,
                    &options,
                    &mut control,
                    &budget,
                    &mut progress,
                )
                .unwrap_err(),
                Route::BufferedSink => collect_controlled_with_sink(
                    &backend,
                    interaction,
                    &options,
                    &mut control,
                    &mut sink,
                    &budget,
                    &mut progress,
                )
                .unwrap_err(),
                Route::SinkOnly => collect_controlled_to_sink(
                    &backend,
                    interaction,
                    &options,
                    &mut control,
                    &mut sink,
                    &budget,
                    &mut progress,
                )
                .unwrap_err(),
                Route::PrestartedSink => {
                    let mut session = backend.start_session(interaction).unwrap();
                    collect_prestarted_controlled_to_sink(
                        session.as_mut(),
                        &options,
                        &mut control,
                        &mut sink,
                        &budget,
                        &mut progress,
                    )
                    .unwrap_err()
                }
            };
            assert!(
                matches!(error, WorkflowError::EmptyCapture),
                "{route:?}: {error}"
            );
            assert!(sink.events.is_empty());
            let minimum = if initially_paused { 50 } else { 20 };
            assert!(started.elapsed() >= Duration::from_millis(minimum));
            assert_eq!(resumed, initially_paused);
        }
    }
}

struct TestRun {
    summary: CollectionSummary,
    frames: Vec<RgbaFrame>,
    metadata: Vec<RecordingMetadata>,
    progress: Vec<WorkflowProgress>,
    sink_events: Vec<SinkEvent>,
}

fn samples() -> Vec<CapturedFrame> {
    [
        (1, 5_000_000, 10),
        (2, 65_000_000, 10),
        (3, 3_605_000_000, 20),
        (4, 7_205_000_000, 20),
    ]
    .map(|(sequence, at, red)| {
        frame(
            sequence,
            at,
            1,
            1,
            4,
            PixelFormat::Rgba8,
            vec![red, 0, 0, 255],
        )
    })
    .to_vec()
}

fn fixed_options() -> CollectOptions {
    CollectOptions {
        limit: CollectionLimit::UntilStopped,
        playback_timing: PlaybackTiming::Fixed(Duration::from_millis(66)),
        // Irrelevant measured-only settings must not invalidate fixed playback.
        tail_frame_duration: Duration::ZERO,
        ..CollectOptions::default()
    }
}

fn sink_frames(sink: &TestFrameSink) -> Vec<RgbaFrame> {
    let mut frames: Vec<RgbaFrame> = Vec::new();
    for event in &sink.events {
        match event {
            SinkEvent::Append {
                frame_index,
                duration_us,
                pixels,
            } => {
                assert_eq!(*frame_index, u64::try_from(frames.len()).unwrap());
                frames.push(RgbaFrame::new(1, 1, pixels.clone(), *duration_us).unwrap());
            }
            SinkEvent::Update {
                frame_index,
                duration_us,
            } => {
                let frame = &mut frames[usize::try_from(*frame_index).unwrap()];
                *frame = RgbaFrame::new(1, 1, frame.pixels().to_vec(), *duration_us).unwrap();
            }
        }
    }
    frames
}

fn run_route(
    route: Route,
    frames: &[CapturedFrame],
    request: CaptureRequest,
    options: &CollectOptions,
) -> TestRun {
    let backend = SyntheticCaptureBackend::new(frames.to_vec());
    let (controller, mut control) = RecordingController::channel();
    let mut snapshots = Vec::new();
    if request.cadence == CaptureCadence::Manual {
        snapshots = frames
            .iter()
            .map(|_| controller.trigger_snapshot())
            .collect();
    }
    let mut sink = TestFrameSink::default();
    let mut progress = Vec::new();
    let mut report = |value| progress.push(value);
    let recording = match route {
        Route::Buffered => Some(
            collect_controlled(
                &backend,
                request.clone(),
                options,
                &mut control,
                &NeverCancel,
                &mut report,
            )
            .unwrap(),
        ),
        Route::BufferedSink => Some(
            collect_controlled_with_sink(
                &backend,
                request.clone(),
                options,
                &mut control,
                &mut sink,
                &NeverCancel,
                &mut report,
            )
            .unwrap(),
        ),
        Route::SinkOnly | Route::PrestartedSink => None,
    };
    let summary = if let Some(recording) = &recording {
        recording.summary()
    } else if matches!(route, Route::PrestartedSink) {
        let mut session = backend.start_session(request).unwrap();
        collect_prestarted_controlled_to_sink(
            &mut *session,
            options,
            &mut control,
            &mut sink,
            &NeverCancel,
            &mut report,
        )
        .unwrap()
    } else {
        collect_controlled_to_sink(
            &backend,
            request,
            options,
            &mut control,
            &mut sink,
            &NeverCancel,
            &mut report,
        )
        .unwrap()
    };
    for (snapshot, frame) in snapshots.iter_mut().zip(frames) {
        let SnapshotTriggerStatus::Captured(receipt) = wait_for_snapshot(snapshot) else {
            panic!("snapshot must retain exactly one sample");
        };
        assert_eq!(receipt.captured_at(), frame.captured_at());
        assert_eq!(receipt.sequence(), frame.sequence());
        assert!(receipt.retained());
    }
    let (frames, metadata) = recording.map_or_else(
        || (sink_frames(&sink), sink.metadata.clone()),
        gif_from_screen_workflow::CollectedRecording::into_parts,
    );
    TestRun {
        summary,
        frames,
        metadata,
        progress,
        sink_events: sink.events,
    }
}

#[test]
fn fixed_and_measured_timings_agree_across_all_collection_routes() {
    assert_eq!(
        CollectOptions::default().playback_timing,
        PlaybackTiming::Measured
    );
    for playback_timing in [
        PlaybackTiming::Measured,
        PlaybackTiming::Fixed(Duration::from_millis(66)),
    ] {
        for frame_retention in [FrameRetention::All, FrameRetention::ChangesOnly] {
            let options = CollectOptions {
                limit: CollectionLimit::UntilStopped,
                playback_timing,
                frame_retention,
                tail_frame_duration: Duration::from_millis(100),
                ..CollectOptions::default()
            };
            let reference = run_route(Route::Buffered, &samples(), request(), &options);
            for route in ROUTES {
                let run = run_route(route, &samples(), request(), &options);
                assert_eq!(run.frames, reference.frames, "{route:?}");
                assert_eq!(run.metadata, reference.metadata, "{route:?}");
                assert_eq!(run.summary, reference.summary, "{route:?}");
                assert_eq!(run.progress, reference.progress, "{route:?}");
                assert_eq!(run.summary.capture_duration_us, 7_200_000_000);
                if matches!(playback_timing, PlaybackTiming::Fixed(_)) {
                    assert!(run.frames.iter().all(|frame| frame.duration_us() == 66_000));
                    assert_eq!(run.summary.duration_us, run.summary.frames * 66_000);
                    assert!(
                        run.sink_events
                            .iter()
                            .all(|event| matches!(event, SinkEvent::Append { .. }))
                    );
                } else {
                    assert_eq!(run.summary.duration_us, 7_200_100_000);
                }
            }
        }
    }
}

#[test]
fn fixed_manual_clicks_keep_identical_frames_without_timestamp_rewriting() {
    let frames = samples()[..3].to_vec();
    for route in ROUTES {
        let run = run_route(
            route,
            &frames,
            manual_request(),
            &CollectOptions {
                limit: CollectionLimit::MaxFrames(3),
                frame_retention: FrameRetention::ChangesOnly,
                playback_timing: PlaybackTiming::Fixed(Duration::from_secs(1)),
                ..fixed_options()
            },
        );
        assert_eq!(run.summary.frames, 3);
        assert_eq!(run.summary.duration_us, 3_000_000);
        assert_eq!(run.summary.capture_duration_us, 3_600_000_000);
        assert_eq!(run.frames[0].pixels(), run.frames[1].pixels());
        assert!(
            run.frames
                .iter()
                .all(|frame| frame.duration_us() == 1_000_000)
        );
        assert_eq!(
            run.metadata
                .iter()
                .map(|item| item.captured_at)
                .collect::<Vec<_>>(),
            frames
                .iter()
                .map(CapturedFrame::captured_at)
                .collect::<Vec<_>>()
        );
        let last = run.progress.last().unwrap();
        assert_eq!(last.capture_duration, Duration::from_secs(3600));
        assert_eq!(last.playback_duration, Duration::from_secs(3));
    }
}

#[test]
fn fixed_periodic_delay_ignores_sampling_gaps_and_exact_capture_limit_tail() {
    let mut periodic = request();
    periodic.cadence = CaptureCadence::interval(Duration::from_secs(60)).unwrap();
    for route in ROUTES {
        let run = run_route(
            route,
            &samples(),
            periodic.clone(),
            &CollectOptions {
                limit: CollectionLimit::Duration(Duration::from_secs(120)),
                ..fixed_options()
            },
        );
        assert_eq!(run.summary.frames, 2);
        assert_eq!(run.summary.duration_us, 132_000);
        assert_eq!(run.summary.capture_duration_us, 120_000_000);
        assert!(run.frames.iter().all(|frame| frame.duration_us() == 66_000));
    }
}

#[test]
fn fixed_playback_preserves_raw_input_even_when_identical_pixels_are_filtered() {
    use gif_from_screen_capture::{ButtonState, InputEvent, PhysicalPosition, PointerButton};
    let mut frames = samples();
    let event = InputEvent::PointerButton {
        at: CaptureTimestamp::from_micros(64_999_000),
        button: PointerButton::Primary,
        state: ButtonState::Pressed,
        position: Some(PhysicalPosition { x: 0, y: 0 }),
    };
    frames[1] = frames[1].clone().with_input_events(vec![event.clone()]);
    for route in ROUTES {
        let run = run_route(
            route,
            &frames,
            request(),
            &CollectOptions {
                frame_retention: FrameRetention::ChangesOnly,
                ..fixed_options()
            },
        );
        assert_eq!(
            run.summary.frames, 3,
            "the input-bearing unchanged sample must be kept"
        );
        assert_eq!(run.summary.duration_us, 198_000);
        assert_eq!(run.metadata[1].input_events, std::slice::from_ref(&event));
        assert_eq!(run.metadata[1].captured_at, frames[1].captured_at());
    }
}

#[test]
fn invalid_fixed_delay_is_rejected_before_starting_a_session() {
    for delay in [
        Duration::ZERO,
        Duration::from_nanos(999),
        Duration::from_secs(u64::MAX),
    ] {
        let backend = CountingBackend::new(samples());
        let error = collect(
            &backend,
            request(),
            &CollectOptions {
                playback_timing: PlaybackTiming::Fixed(delay),
                ..fixed_options()
            },
            &NeverCancel,
            &mut NoopWorkflowProgress,
        )
        .unwrap_err();
        assert!(matches!(error, WorkflowError::InvalidCollectionOption(_)));
        assert_eq!(backend.starts.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn fixed_playback_still_rejects_invalid_native_timestamp_order() {
    let repeated_time = vec![
        frame(1, 5_000, 1, 1, 4, PixelFormat::Rgba8, vec![10, 0, 0, 255]),
        frame(2, 5_000, 1, 1, 4, PixelFormat::Rgba8, vec![20, 0, 0, 255]),
    ];
    let backend = CountingBackend::new(repeated_time);
    assert!(matches!(
        collect(
            &backend,
            request(),
            &fixed_options(),
            &NeverCancel,
            &mut NoopWorkflowProgress
        ),
        Err(WorkflowError::NonMonotonicTimestamp { frame_index: 1, .. })
    ));
    assert_eq!(backend.session_calls.discards.load(Ordering::Relaxed), 1);
}

#[test]
fn fixed_stop_while_paused_keeps_the_retained_frame_delay() {
    for sink_only in [false, true] {
        let backend = CountingBackend::new(samples());
        let (controller, mut control) = RecordingController::channel();
        let mut sink = TestFrameSink::default();
        let mut progress = |value: WorkflowProgress| {
            if value.phase == WorkflowPhase::Capturing && value.frames_captured == 1 {
                assert!(controller.pause());
            } else if value.phase == WorkflowPhase::Paused {
                assert_eq!(value.playback_duration, Duration::from_millis(66));
                assert!(controller.stop());
            }
        };
        let options = CollectOptions {
            poll_interval: Duration::from_millis(1),
            ..fixed_options()
        };
        let summary = if sink_only {
            collect_controlled_to_sink(
                &backend,
                request(),
                &options,
                &mut control,
                &mut sink,
                &NeverCancel,
                &mut progress,
            )
            .unwrap()
        } else {
            collect_controlled_with_sink(
                &backend,
                request(),
                &options,
                &mut control,
                &mut sink,
                &NeverCancel,
                &mut progress,
            )
            .unwrap()
            .summary()
        };
        assert_eq!(summary.frames, 1);
        assert_eq!(summary.capture_duration_us, 0);
        assert_eq!(summary.duration_us, 66_000);
        assert_eq!(backend.session_calls.polls.load(Ordering::Relaxed), 1);
        assert_eq!(backend.session_calls.pauses.load(Ordering::Relaxed), 1);
        assert_eq!(backend.session_calls.discards.load(Ordering::Relaxed), 0);
        assert!(matches!(
            &sink.events[..],
            [SinkEvent::Append {
                duration_us: 66_000,
                ..
            }]
        ));
    }
}

#[test]
fn fixed_timing_handles_large_capture_clocks_without_rewriting_them() {
    let frames = [u64::MAX - 1, u64::MAX]
        .into_iter()
        .enumerate()
        .map(|(index, at)| {
            frame(
                u64::try_from(index).unwrap(),
                at,
                1,
                1,
                4,
                PixelFormat::Rgba8,
                vec![10, 0, 0, 255],
            )
        })
        .collect::<Vec<_>>();
    for route in ROUTES {
        let run = run_route(route, &frames, request(), &fixed_options());
        assert_eq!(run.summary.capture_duration_us, 1);
        assert_eq!(run.summary.duration_us, 132_000);
        assert_eq!(run.metadata[1].captured_at.as_micros(), u64::MAX);
    }
}

#[test]
fn fixed_duration_sum_overflow_is_rejected_before_persisting_the_next_frame() {
    let options = CollectOptions {
        playback_timing: PlaybackTiming::Fixed(Duration::from_micros(u64::MAX)),
        ..fixed_options()
    };
    for route in ROUTES {
        let run = run_route(route, &samples()[..1], request(), &options);
        assert_eq!(run.summary.duration_us, u64::MAX);
    }
    for sink_only in [false, true] {
        let backend = CountingBackend::new(samples());
        let (controller, mut control) = RecordingController::channel();
        drop(controller);
        let mut sink = TestFrameSink::default();
        let result = if sink_only {
            collect_controlled_to_sink(
                &backend,
                request(),
                &options,
                &mut control,
                &mut sink,
                &NeverCancel,
                &mut NoopWorkflowProgress,
            )
        } else {
            collect_controlled_with_sink(
                &backend,
                request(),
                &options,
                &mut control,
                &mut sink,
                &NeverCancel,
                &mut NoopWorkflowProgress,
            )
            .map(|recording| recording.summary())
        };
        assert!(
            matches!(result, Err(WorkflowError::InvalidCollectionOption(message)) if message.contains("overflow"))
        );
        assert!(matches!(
            &sink.events[..],
            [SinkEvent::Append {
                frame_index: 0,
                duration_us: u64::MAX,
                ..
            }]
        ));
        assert_eq!(sink.metadata.len(), 1);
        assert_eq!(backend.session_calls.discards.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn fixed_cancellation_and_discard_keep_only_the_durable_prefix_without_tail_updates() {
    for sink_only in [false, true] {
        for discard in [false, true] {
            check_fixed_cancellation(sink_only, discard);
        }
    }
}

fn check_fixed_cancellation(sink_only: bool, discard: bool) {
    let backend = CountingBackend::new(samples());
    let (controller, mut control) = RecordingController::channel();
    let cancellation = CancellationFlag::default();
    let mut sink = TestFrameSink::default();
    let mut progress = |value: WorkflowProgress| {
        if value.phase == WorkflowPhase::Capturing && value.frames_captured == 1 {
            if discard {
                assert!(controller.discard());
            } else {
                cancellation.cancel();
            }
        }
    };
    let result = if sink_only {
        collect_controlled_to_sink(
            &backend,
            request(),
            &fixed_options(),
            &mut control,
            &mut sink,
            &cancellation,
            &mut progress,
        )
    } else {
        collect_controlled_with_sink(
            &backend,
            request(),
            &fixed_options(),
            &mut control,
            &mut sink,
            &cancellation,
            &mut progress,
        )
        .map(|recording| recording.summary())
    };
    assert!(matches!(
        (&result, discard),
        (Err(WorkflowError::Discarded), true) | (Err(WorkflowError::Cancelled), false)
    ));
    assert!(matches!(
        &sink.events[..],
        [SinkEvent::Append {
            frame_index: 0,
            duration_us: 66_000,
            ..
        }]
    ));
    assert_eq!(sink.metadata.len(), 1);
    assert_eq!(backend.session_calls.polls.load(Ordering::Relaxed), 1);
    assert_eq!(backend.session_calls.discards.load(Ordering::Relaxed), 1);
}

#[test]
fn fixed_manual_pause_resume_does_not_add_frames_or_playback_waits() {
    for sink_only in [false, true] {
        check_fixed_manual_pause(sink_only);
    }
}

fn check_fixed_manual_pause(sink_only: bool) {
    let backend = CountingBackend::new(samples()[..3].to_vec());
    let (controller, mut control) = RecordingController::channel();
    let mut first = controller.trigger_snapshot();
    let mut paused_request = None;
    let mut next_requests = Vec::new();
    let mut pause_requested = false;
    let mut resumed = false;
    let mut third_requested = false;
    let mut sink = TestFrameSink::default();
    let options = CollectOptions {
        limit: CollectionLimit::MaxFrames(3),
        poll_interval: Duration::from_millis(1),
        ..fixed_options()
    };
    let mut progress = |value: WorkflowProgress| {
        if value.phase == WorkflowPhase::Capturing && value.frames_captured == 1 && !pause_requested
        {
            assert!(controller.pause());
            pause_requested = true;
        } else if value.phase == WorkflowPhase::Paused && !resumed {
            assert_eq!(value.frames_captured, 1);
            assert_eq!(value.playback_duration, Duration::from_millis(66));
            paused_request = Some(controller.trigger_snapshot());
            assert!(controller.resume());
            next_requests.push(controller.trigger_snapshot());
            resumed = true;
        } else if value.phase == WorkflowPhase::Capturing
            && value.frames_captured == 2
            && !third_requested
        {
            next_requests.push(controller.trigger_snapshot());
            third_requested = true;
        }
    };
    let summary = if sink_only {
        collect_controlled_to_sink(
            &backend,
            manual_request(),
            &options,
            &mut control,
            &mut sink,
            &NeverCancel,
            &mut progress,
        )
        .unwrap()
    } else {
        collect_controlled_with_sink(
            &backend,
            manual_request(),
            &options,
            &mut control,
            &mut sink,
            &NeverCancel,
            &mut progress,
        )
        .unwrap()
        .summary()
    };
    assert_eq!(summary.frames, 3);
    assert_eq!(summary.duration_us, 198_000);
    assert_eq!(summary.capture_duration_us, 3_600_000_000);
    assert_eq!(backend.session_calls.pauses.load(Ordering::Relaxed), 1);
    assert_eq!(backend.session_calls.resumes.load(Ordering::Relaxed), 1);
    assert_eq!(backend.session_calls.snapshots.load(Ordering::Relaxed), 3);
    assert_eq!(
        wait_for_snapshot(paused_request.as_mut().unwrap()),
        SnapshotTriggerStatus::Rejected(SnapshotTriggerRejection::Paused)
    );
    assert!(matches!(
        wait_for_snapshot(&mut first),
        SnapshotTriggerStatus::Captured(_)
    ));
    assert_eq!(next_requests.len(), 2);
    assert!(next_requests.iter_mut().all(|request| matches!(
        wait_for_snapshot(request),
        SnapshotTriggerStatus::Captured(_)
    )));
    assert_eq!(
        sink.metadata
            .iter()
            .map(|metadata| metadata.captured_at.as_micros())
            .collect::<Vec<_>>(),
        [5_000_000, 65_000_000, 3_605_000_000]
    );
    assert!(
        sink_frames(&sink)
            .iter()
            .all(|frame| frame.duration_us() == 66_000)
    );
}

#[test]
fn fixed_export_progress_distinguishes_capture_span_from_gif_duration() {
    let backend = SyntheticCaptureBackend::new(samples()[1..3].to_vec());
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("fixed.gif");
    let mut progress = Vec::new();
    let report = record_to_gif(
        &backend,
        request(),
        &target,
        &RecordToGifOptions {
            collection: CollectOptions {
                playback_timing: PlaybackTiming::Fixed(Duration::from_secs(1)),
                ..fixed_options()
            },
            ..RecordToGifOptions::default()
        },
        &NeverCancel,
        &mut |value| progress.push(value),
    )
    .unwrap();
    assert_eq!(report.collection.capture_duration_us, 3_540_000_000);
    assert_eq!(report.collection.duration_us, 2_000_000);
    for value in progress.iter().filter(|value| {
        matches!(
            value.phase,
            WorkflowPhase::StoppingCapture
                | WorkflowPhase::Encoding
                | WorkflowPhase::Committing
                | WorkflowPhase::Complete
        )
    }) {
        assert_eq!(value.capture_duration, Duration::from_secs(3540));
        assert_eq!(value.playback_duration, Duration::from_secs(2));
    }
    assert_eq!(progress.last().unwrap().phase, WorkflowPhase::Complete);
    let mut decoder = gif::DecodeOptions::new()
        .read_info(fs::File::open(target).unwrap())
        .unwrap();
    assert_eq!(decoder.read_next_frame().unwrap().unwrap().delay, 100);
    assert_eq!(decoder.read_next_frame().unwrap().unwrap().delay, 100);
    assert!(decoder.read_next_frame().unwrap().is_none());
}
