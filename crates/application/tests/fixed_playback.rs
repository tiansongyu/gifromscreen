//! Capture cadence, native input clocks, durable project timing, and GIF timing
//! must remain independent across both recording persistence routes.

use std::{
    fs::File,
    path::Path,
    time::{Duration, Instant},
};

use gif_from_screen_application::{
    IncrementalRecordingProject, IncrementalRecordingProjectOptions, NoopProjectExportProgress,
    ProjectExportSnapshot, ProjectGifExportOptions, RecordingProjectOptions,
    export_project_snapshot_to_gif, persist_collected_recording,
};
use gif_from_screen_capture::{
    ButtonState, CaptureCadence, CaptureRequest, CaptureSourceId, CaptureTarget, CaptureTimestamp,
    CapturedFrame, InputEvent, KeyState, PhysicalPosition, PhysicalSize as CaptureSize,
    PixelFormat, PointerButton, SyntheticCaptureBackend,
};
use gif_from_screen_domain::{
    CaptureBinding, CaptureClockId, DurationUs, FrameId, MouseButton, PhysicalSize, ProjectId,
    TimeUs, UnixTimeMs,
};
use gif_from_screen_gif::{CancellationToken, NeverCancel, RgbaFrame};
use gif_from_screen_project::{ActiveProject, LockPolicy};
use gif_from_screen_workflow::{
    CollectOptions, CollectionLimit, CollectionSummary, FrameRetention, PlaybackTiming,
    RecordingControl, RecordingController, RecordingFrameSink, RecordingFrameSinkError,
    RecordingMetadata, SnapshotTriggerStatus, WorkflowPhase, WorkflowProgress,
    WorkflowProgressSink, collect_controlled, collect_controlled_to_sink,
};

#[derive(Clone, Copy)]
enum Route {
    Batch,
    Incremental,
}

struct Case<'a> {
    native: &'a [CapturedFrame],
    retained: &'a [usize],
    cadence: CaptureCadence,
    timing: PlaybackTiming,
    durations: &'a [u64],
    pause: bool,
}

struct Deadline(Instant);

impl CancellationToken for Deadline {
    fn is_cancelled(&self) -> bool {
        self.0.elapsed() > Duration::from_secs(5)
    }
}

struct ProjectSink {
    writer: IncrementalRecordingProject,
    duration_updates: usize,
}

fn frame_id(index: u64) -> FrameId {
    FrameId::from_u128(u128::from(index) + 1)
}

impl RecordingFrameSink for ProjectSink {
    fn append_provisional_frame(
        &mut self,
        _index: u64,
        _frame: &RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        panic!("native metadata must not be silently lost at the sink boundary");
    }

    fn append_provisional_frame_with_metadata(
        &mut self,
        index: u64,
        frame: &RgbaFrame,
        metadata: &RecordingMetadata,
    ) -> Result<(), RecordingFrameSinkError> {
        self.writer
            .append_frame_with_metadata(frame_id(index), frame, Some(metadata))?;
        Ok(())
    }

    fn update_frame_duration(
        &mut self,
        index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError> {
        self.duration_updates += 1;
        self.writer
            .set_frame_duration(frame_id(index), DurationUs::new(duration_us).unwrap())?;
        Ok(())
    }
}

fn native_frame(index: u64, timestamp: u64, red: u8, events: bool) -> CapturedFrame {
    let frame = CapturedFrame::new(
        index,
        CaptureTimestamp::from_micros(timestamp),
        CaptureSize::new(1, 1).unwrap(),
        4,
        PixelFormat::Rgba8,
        vec![red, 0, 0, 255],
    )
    .unwrap()
    .with_capture_origin(PhysicalPosition { x: -20, y: 30 });
    if !events {
        return frame;
    }
    frame
        .with_input_events(vec![
            InputEvent::Key {
                at: CaptureTimestamp::from_micros(timestamp - 500),
                native_code: 38,
                text: Some("a".to_owned()),
                state: KeyState::Pressed,
                repeat: true,
                modifiers: 2,
            },
            InputEvent::PointerButton {
                at: CaptureTimestamp::from_micros(timestamp - 250),
                button: PointerButton::Primary,
                state: ButtonState::Released,
                position: Some(PhysicalPosition { x: 0, y: 0 }),
            },
        ])
        .with_dropped_input_events(2)
}

fn persist_route(
    root: &Path,
    route: Route,
    case: &Case<'_>,
    control: &mut RecordingControl,
    progress: &mut dyn WorkflowProgressSink,
) -> (ActiveProject, CollectionSummary) {
    let backend = SyntheticCaptureBackend::new(case.native.to_vec());
    let request = CaptureRequest::new(
        CaptureTarget::Monitor(CaptureSourceId::new("synthetic:monitor:0").unwrap()),
        case.cadence,
    );
    let options = CollectOptions {
        limit: if case.cadence == CaptureCadence::Manual {
            CollectionLimit::MaxFrames(u64::try_from(case.retained.len()).unwrap())
        } else {
            CollectionLimit::UntilStopped
        },
        frame_retention: FrameRetention::ChangesOnly,
        playback_timing: case.timing,
        poll_interval: Duration::from_millis(1),
        // Deliberately unrelated to every fixed playback delay in these tests.
        tail_frame_duration: Duration::from_millis(100),
        ..CollectOptions::default()
    };
    let cancellation = Deadline(Instant::now());
    match route {
        Route::Batch => {
            let recording = collect_controlled(
                &backend,
                request,
                &options,
                control,
                &cancellation,
                progress,
            )
            .unwrap();
            let summary = recording.summary();
            let project = persist_collected_recording(
                root,
                recording,
                RecordingProjectOptions {
                    project_id: ProjectId::from_u128(1),
                    frame_ids: (0..summary.frames).map(frame_id).collect(),
                    app_version: "timing-integration".to_owned(),
                    created_at: UnixTimeMs::new(1),
                    source_label: Some("same synthetic source label".to_owned()),
                },
            )
            .unwrap();
            (project, summary)
        }
        Route::Incremental => {
            let writer = IncrementalRecordingProject::create(
                root,
                PhysicalSize::new(1, 1).unwrap(),
                IncrementalRecordingProjectOptions {
                    project_id: ProjectId::from_u128(1),
                    app_version: "timing-integration".to_owned(),
                    created_at: UnixTimeMs::new(1),
                    source_label: Some("same synthetic source label".to_owned()),
                },
            )
            .unwrap();
            let mut sink = ProjectSink {
                writer,
                duration_updates: 0,
            };
            let summary = collect_controlled_to_sink(
                &backend,
                request,
                &options,
                control,
                &mut sink,
                &cancellation,
                progress,
            )
            .unwrap();
            assert_eq!(sink.writer.summary().duration_us, summary.duration_us);
            if matches!(case.timing, PlaybackTiming::Fixed(_)) {
                assert_eq!(
                    sink.duration_updates, 0,
                    "fixed delays must be final from their first journal append"
                );
            }
            // Exercise journal recovery without a final checkpoint, just as a
            // process exit after the last successful append would require.
            drop(sink.writer);
            let project = ActiveProject::open(root, LockPolicy::FailIfPresent)
                .unwrap()
                .project;
            (project, summary)
        }
    }
}

fn record(root: &Path, route: Route, case: &Case<'_>) -> (ActiveProject, CollectionSummary) {
    let (controller, mut control) = RecordingController::channel();
    let manual = case.cadence == CaptureCadence::Manual;
    let mut receipts = Vec::new();
    if manual {
        receipts.push(controller.trigger_snapshot());
    }
    let mut paused = 0;
    let mut pause_started = false;
    let mut last_count = 0;
    let mut updates = Vec::new();
    let mut progress = |update: WorkflowProgress| {
        updates.push(update);
        if update.phase == WorkflowPhase::Paused {
            paused += 1;
            if paused == 3 {
                controller.resume();
                if manual {
                    receipts.push(controller.trigger_snapshot());
                }
            }
        } else if update.phase == WorkflowPhase::Capturing && update.frames_captured > last_count {
            last_count = update.frames_captured;
            if case.pause && !pause_started && last_count == 1 {
                pause_started = true;
                controller.pause();
            } else if manual && last_count < u64::try_from(case.retained.len()).unwrap() {
                receipts.push(controller.trigger_snapshot());
            }
        }
    };
    let (project, summary) = persist_route(root, route, case, &mut control, &mut progress);
    if manual {
        assert_eq!(receipts.len(), case.native.len());
        for (receipt, original) in receipts.iter_mut().zip(case.native) {
            let SnapshotTriggerStatus::Captured(receipt) = receipt.status() else {
                panic!("every accepted manual request must acknowledge its retained native frame");
            };
            assert_eq!(receipt.captured_at(), original.captured_at());
            assert_eq!(receipt.sequence(), original.sequence());
            assert!(receipt.retained());
        }
    }
    if case.pause {
        assert_eq!(paused, 3);
        let paused_updates: Vec<_> = updates
            .iter()
            .filter(|update| update.phase == WorkflowPhase::Paused)
            .collect();
        assert!(
            paused_updates
                .windows(2)
                .all(|pair| pair[0].capture_duration == pair[1].capture_duration
                    && pair[0].playback_duration == pair[1].playback_duration)
        );
    }
    let stopped = updates.last().unwrap();
    assert_eq!(stopped.phase, WorkflowPhase::StoppingCapture);
    assert_eq!(
        stopped.capture_duration.as_micros(),
        u128::from(summary.capture_duration_us)
    );
    assert_eq!(
        stopped.playback_duration.as_micros(),
        u128::from(summary.duration_us)
    );
    (project, summary)
}

fn assert_native_metadata(project: &ActiveProject, case: &Case<'_>) -> CaptureClockId {
    let frames = &project.manifest().timeline.frames;
    assert_eq!(frames.len(), case.retained.len());
    let identity = frames[0].capture_clock.unwrap().id.unwrap();
    assert!(!identity.is_nil());
    for (frame, &original_index) in frames.iter().zip(case.retained) {
        let native = &case.native[original_index];
        let raw = &frame.capture_metadata;
        let sampled = TimeUs::new(native.captured_at().as_micros());
        assert_eq!(frame.capture_binding, CaptureBinding::Original);
        assert_eq!(raw.captured_at, Some(sampled));
        assert_eq!(frame.capture_clock.unwrap().sampled_at, sampled);
        assert_eq!(frame.capture_clock.unwrap().id, Some(identity));
        let origin = raw.capture_origin.unwrap();
        assert_eq!((origin.x, origin.y), (-20, 30));
        assert_eq!(raw.dropped_input_events, native.dropped_input_events());
        if native.input_events().is_empty() {
            assert!(raw.key_strokes.is_empty() && raw.mouse_events.is_empty());
        } else {
            assert_eq!(raw.key_strokes.len(), 1);
            let key = &raw.key_strokes[0];
            assert_eq!(key.at.get(), sampled.get() - 500);
            assert_eq!(key.physical_key, "x11:38");
            assert_eq!(key.display_text.as_deref(), Some("a"));
            assert!(key.pressed && key.repeat);
            assert_eq!(key.modifiers, 2);
            assert_eq!(raw.mouse_events.len(), 1);
            let click = &raw.mouse_events[0];
            assert_eq!(click.at.get(), sampled.get() - 250);
            assert_eq!(click.button, MouseButton::Left);
            assert!(!click.pressed);
            let point = click.position.unwrap();
            assert_eq!((point.x.get(), point.y.get()), (0, 0));
        }
        assert_eq!(
            project.assets().read(frame.asset_id).unwrap(),
            native.pixels()
        );
    }
    identity
}

fn exercise(case: &Case<'_>) {
    let directory = tempfile::tempdir().unwrap();
    let mut identities = Vec::new();
    // Independent projects deliberately reuse their project ID, source label,
    // timestamps and content, so none of these can masquerade as clock identity.
    for (index, route) in [Route::Batch, Route::Incremental, Route::Incremental]
        .into_iter()
        .enumerate()
    {
        let root = directory.path().join(format!("recording-{index}.gfsproj"));
        let (project, summary) = record(&root, route, case);
        assert_eq!(summary.duration_us, case.durations.iter().sum::<u64>());
        let first = &case.native[case.retained[0]];
        let last = case.native.last().unwrap();
        assert_eq!(
            summary.capture_duration_us,
            last.captured_at().as_micros() - first.captured_at().as_micros()
        );
        identities.push(assert_native_metadata(&project, case));
        let manifest = project.manifest().clone();
        drop(project);
        let reopened = ActiveProject::open(&root, LockPolicy::FailIfPresent)
            .unwrap()
            .project;
        assert_eq!(reopened.manifest(), &manifest);
        assert_eq!(
            assert_native_metadata(&reopened, case),
            *identities.last().unwrap()
        );
        let durations: Vec<_> = reopened
            .manifest()
            .timeline
            .frames
            .iter()
            .map(|frame| frame.duration.get())
            .collect();
        assert_eq!(durations, case.durations);
        let output = directory.path().join(format!("output-{index}.gif"));
        let report = export_project_snapshot_to_gif(
            &ProjectExportSnapshot::from_active(&reopened),
            &output,
            &ProjectGifExportOptions::default(),
            &NeverCancel,
            &mut NoopProjectExportProgress,
        )
        .unwrap();
        assert_eq!(report.encoding.input_duration_us, summary.duration_us);
        let expected_ticks = (summary.duration_us + 5_000) / 10_000;
        let mut decoder = gif::DecodeOptions::new()
            .read_info(File::open(output).unwrap())
            .unwrap();
        let mut ticks = 0;
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            ticks += u64::from(frame.delay);
        }
        assert_eq!(ticks, expected_ticks);
        assert_eq!(report.encoding.encoded_duration_ticks, expected_ticks);
    }
    for (index, identity) in identities.iter().enumerate() {
        assert!(
            !identities[..index].contains(identity),
            "independent recording clocks must never be inferred from shared labels, timestamps or project IDs"
        );
    }
}

#[test]
fn manual_snapshots_keep_duplicates_fixed_delays_and_native_clocks_across_pause() {
    let native = [
        native_frame(1, 10_000, 30, true),
        native_frame(2, 8_010_000, 30, false),
        native_frame(3, 60_010_000, 30, false),
    ];
    exercise(&Case {
        native: &native,
        retained: &[0, 1, 2],
        cadence: CaptureCadence::Manual,
        timing: PlaybackTiming::Fixed(Duration::from_secs(1)),
        durations: &[1_000_000; 3],
        pause: true,
    });
}

#[test]
fn minute_hour_and_fps_sampling_do_not_become_playback_duration() {
    for (cadence, timestamps) in [
        (
            CaptureCadence::interval(Duration::from_secs(4)).unwrap(),
            [10_000, 4_010_000, 8_010_000],
        ),
        (
            CaptureCadence::interval(Duration::from_secs(240)).unwrap(),
            [10_000, 240_010_000, 480_010_000],
        ),
        // At 15 FPS, missed delivery slots still contribute no extra fixed delay.
        (
            CaptureCadence::fixed_fps(15).unwrap(),
            [10_000, 110_000, 910_000],
        ),
    ] {
        let native: Vec<_> = timestamps
            .into_iter()
            .enumerate()
            .map(|(index, at)| {
                native_frame(
                    u64::try_from(index).unwrap(),
                    at,
                    u8::try_from(index * 60).unwrap(),
                    true,
                )
            })
            .collect();
        exercise(&Case {
            native: &native,
            retained: &[0, 1, 2],
            cadence,
            timing: PlaybackTiming::Fixed(Duration::from_millis(66)),
            durations: &[66_000; 3],
            pause: true,
        });
    }
}

#[test]
fn changed_only_fixed_playback_skips_pixels_but_keeps_input_only_samples() {
    let native = [
        native_frame(1, 10_000, 30, false),
        native_frame(2, 4_010_000, 30, false),
        native_frame(3, 8_010_000, 120, false),
        native_frame(4, 12_010_000, 120, false),
        native_frame(5, 16_010_000, 120, true),
        native_frame(6, 20_010_000, 120, false),
    ];
    exercise(&Case {
        native: &native,
        retained: &[0, 2, 4],
        cadence: CaptureCadence::interval(Duration::from_secs(4)).unwrap(),
        timing: PlaybackTiming::Fixed(Duration::from_millis(66)),
        durations: &[66_000; 3],
        pause: false,
    });
}

#[test]
fn measured_playback_still_extends_retained_frames_across_skipped_samples() {
    let native = [
        native_frame(1, 10_000, 30, false),
        native_frame(2, 4_010_000, 30, false),
        native_frame(3, 8_010_000, 120, false),
        native_frame(4, 12_010_000, 120, false),
        native_frame(5, 16_010_000, 120, true),
        native_frame(6, 20_010_000, 120, false),
    ];
    exercise(&Case {
        native: &native,
        retained: &[0, 2, 4],
        cadence: CaptureCadence::fixed_fps(15).unwrap(),
        timing: PlaybackTiming::Measured,
        durations: &[8_000_000, 8_000_000, 4_100_000],
        pause: true,
    });
}
