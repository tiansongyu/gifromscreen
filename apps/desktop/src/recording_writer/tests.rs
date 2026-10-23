use std::{sync::mpsc, time::Duration};

use gif_from_screen_capture::CaptureTimestamp;
use gif_from_screen_domain::{PhysicalSize, ProjectId, UnixTimeMs};

use super::*;

type Events = Arc<Mutex<Vec<(u64, u64)>>>;

struct TestSink {
    events: Events,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    fail: Option<u64>,
    panic: bool,
}

impl RecordingFrameSink for TestSink {
    fn append_provisional_frame(
        &mut self,
        index: u64,
        frame: &RgbaFrame,
    ) -> Result<(), RecordingFrameSinkError> {
        if let Some((started, release)) = self.gate.take() {
            started.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(!self.panic, "injected writer panic");
        if self.fail == Some(index) {
            return Err(io::Error::other("injected storage failure").into());
        }
        self.events
            .lock()
            .unwrap()
            .push((index, frame.duration_us()));
        Ok(())
    }

    fn update_frame_duration(
        &mut self,
        index: u64,
        duration_us: u64,
    ) -> Result<(), RecordingFrameSinkError> {
        let mut events = self.events.lock().unwrap();
        let frame = events
            .iter_mut()
            .find(|frame| frame.0 == index)
            .ok_or_else(|| io::Error::other("duration preceded frame"))?;
        frame.1 = duration_us;
        Ok(())
    }
}

fn metadata() -> RecordingMetadata {
    RecordingMetadata {
        captured_at: CaptureTimestamp::from_micros(1),
        capture_origin: None,
        cursor: None,
        cursor_image: None,
        cursor_embedded: false,
        input_events: Vec::new(),
        dropped_input_events: 0,
    }
}

fn frame() -> RgbaFrame {
    RgbaFrame::from_shared_pixels(1, 1, vec![0, 1, 2, 255].into(), 10_000).unwrap()
}

fn writer(sink: TestSink, bytes: usize) -> BufferedRecordingSink {
    BufferedRecordingSink::spawn(
        sink,
        |_| Err("test sink cannot finalize an editor project".into()),
        PathBuf::from("unused-test-project"),
        bytes,
    )
    .unwrap()
}

fn wait_for_barrier(shared: &Shared) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if shared
            .state
            .lock()
            .unwrap()
            .pending
            .iter()
            .any(|pending| matches!(pending.command, Command::Barrier(_)))
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "flush never reached reserved control slot"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn admission_never_waits_for_disk_and_counts_in_flight_bytes() {
    let events = Events::default();
    let (started, received) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let bytes = frame().pixels().len() + metadata_bytes(&metadata());
    let mut sink = writer(
        TestSink {
            events: events.clone(),
            gate: Some((started, gate)),
            fail: None,
            panic: false,
        },
        bytes * 2,
    );
    sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
        .unwrap();
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    sink.enqueue_frame_duration(0, 16_667).unwrap();
    sink.enqueue_provisional_frame_with_metadata(1, &frame(), &metadata())
        .unwrap();
    assert_eq!(sink.shared.state.lock().unwrap().bytes, bytes * 2);
    let error = sink
        .enqueue_provisional_frame_with_metadata(2, &frame(), &metadata())
        .unwrap_err();
    assert!(error.to_string().contains("storage could not keep up"));
    assert!(events.lock().unwrap().is_empty());
    // A full byte budget still allows the ordered flush control slot.
    let (done, result) = mpsc::channel();
    let shared = sink.shared.clone();
    let worker = thread::spawn(move || {
        let flushed = sink.flush_pending().map_err(|error| error.to_string());
        done.send(flushed).unwrap();
        sink.preserve().unwrap();
    });
    wait_for_barrier(&shared);
    assert!(result.try_recv().is_err());
    release.send(()).unwrap();
    result
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert_eq!(*events.lock().unwrap(), vec![(0, 16_667), (1, 10_000)]);
}

#[test]
fn preservation_drains_accepted_frames_and_duration_corrections() {
    let events = Events::default();
    let mut sink = writer(
        TestSink {
            events: events.clone(),
            gate: None,
            fail: None,
            panic: false,
        },
        4096,
    );
    sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
        .unwrap();
    sink.enqueue_frame_duration(0, 33_333).unwrap();
    sink.preserve().unwrap();
    assert_eq!(*events.lock().unwrap(), vec![(0, 33_333)]);
}

#[test]
fn write_failure_is_reported_by_barrier_and_preservation() {
    let events = Events::default();
    let mut sink = writer(
        TestSink {
            events: events.clone(),
            gate: None,
            fail: Some(0),
            panic: false,
        },
        4096,
    );
    sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
        .unwrap();
    assert!(sink.flush_pending().is_err());
    let error = sink.preserve().unwrap_err();
    assert!(error.contains("injected storage failure"));
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn worker_panic_wakes_flush_instead_of_hanging() {
    let events = Events::default();
    let (started, received) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mut sink = writer(
        TestSink {
            events,
            gate: Some((started, gate)),
            fail: None,
            panic: true,
        },
        4096,
    );
    sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
        .unwrap();
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    let (done, result) = mpsc::channel();
    let shared = sink.shared.clone();
    let worker = thread::spawn(move || {
        done.send(sink.flush_pending().is_err()).unwrap();
        assert!(sink.preserve().unwrap_err().contains("panicked"));
    });
    wait_for_barrier(&shared);
    assert!(result.try_recv().is_err());
    release.send(()).unwrap();
    assert!(result.recv_timeout(Duration::from_secs(5)).unwrap());
    worker.join().unwrap();
}

#[test]
fn explicit_discard_removes_queued_work_before_releasing_project() {
    let events = Events::default();
    let (started, received) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mut sink = writer(
        TestSink {
            events: events.clone(),
            gate: Some((started, gate)),
            fail: None,
            panic: false,
        },
        4096,
    );
    sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
        .unwrap();
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    sink.enqueue_provisional_frame_with_metadata(1, &frame(), &metadata())
        .unwrap();
    sink.close(Close::Discard);
    assert!(sink.shared.state.lock().unwrap().pending.is_empty());
    release.send(()).unwrap();
    sink.discard().unwrap();
    assert_eq!(*events.lock().unwrap(), vec![(0, 10_000)]);
}

#[test]
fn duration_only_backlog_has_a_command_bound_and_a_reserved_flush_slot() {
    let events = Events::default();
    let (started, received) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mut sink = writer(
        TestSink {
            events,
            gate: Some((started, gate)),
            fail: None,
            panic: false,
        },
        4096,
    );
    sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
        .unwrap();
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    for duration in 1..MAX_PENDING_COMMANDS {
        sink.enqueue_frame_duration(0, duration as u64).unwrap();
    }
    assert_eq!(
        sink.shared.state.lock().unwrap().commands,
        MAX_PENDING_COMMANDS
    );
    assert!(sink.enqueue_frame_duration(0, 999).is_err());
    let shared = sink.shared.clone();
    let flush = thread::spawn(move || {
        sink.flush_pending().unwrap();
        sink.preserve().unwrap();
    });
    wait_for_barrier(&shared);
    assert_eq!(
        shared.state.lock().unwrap().commands,
        MAX_PENDING_COMMANDS + 1
    );
    release.send(()).unwrap();
    flush.join().unwrap();
}

#[test]
fn metadata_is_charged_before_admitting_a_frame() {
    let events = Events::default();
    let bytes = frame().pixels().len() + metadata_bytes(&metadata());
    let mut sink = writer(
        TestSink {
            events: events.clone(),
            gate: None,
            fail: None,
            panic: false,
        },
        bytes - 1,
    );
    assert!(
        sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
            .is_err()
    );
    assert_eq!(sink.shared.state.lock().unwrap().bytes, 0);
    sink.preserve().unwrap();
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn preserve_releases_lock_and_replays_all_accepted_journal_corrections() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recovery.gfsproj");
    let project = super::super::IncrementalRecordingProject::create(
        &root,
        PhysicalSize::new(1, 1).unwrap(),
        super::super::IncrementalRecordingProjectOptions {
            project_id: ProjectId::from_u128(101),
            app_version: "test".into(),
            created_at: UnixTimeMs::new(0),
            source_label: None,
        },
    )
    .unwrap();
    let mut sink = BufferedRecordingSink::new(project).unwrap();
    sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
        .unwrap();
    sink.enqueue_frame_duration(0, 44_444).unwrap();
    sink.preserve().unwrap();
    // No final checkpoint: this exercises journal recovery, not just reopening
    // the successful finish() snapshot.
    let recovered =
        ActiveProject::open(&root, gif_from_screen_project::LockPolicy::FailIfPresent).unwrap();
    let frames = &recovered.project.manifest().timeline.frames;
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].duration.get(), 44_444);
}

#[test]
fn final_project_includes_all_frames_and_final_journal_durations() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recording.gfsproj");
    let project = super::super::IncrementalRecordingProject::create(
        &root,
        PhysicalSize::new(1, 1).unwrap(),
        super::super::IncrementalRecordingProjectOptions {
            project_id: ProjectId::from_u128(100),
            app_version: "test".into(),
            created_at: UnixTimeMs::new(0),
            source_label: None,
        },
    )
    .unwrap();
    let mut sink = BufferedRecordingSink::new(project).unwrap();
    sink.enqueue_provisional_frame_with_metadata(0, &frame(), &metadata())
        .unwrap();
    sink.enqueue_frame_duration(0, 16_667).unwrap();
    let mut second = metadata();
    second.captured_at = CaptureTimestamp::from_micros(16_668);
    sink.enqueue_provisional_frame_with_metadata(1, &frame(), &second)
        .unwrap();
    sink.enqueue_frame_duration(1, 50_000).unwrap();
    sink.flush_pending().unwrap();
    let project = sink.finish().unwrap();
    let frames = &project.manifest().timeline.frames;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].duration.get(), 16_667);
    assert_eq!(frames[1].duration.get(), 50_000);
    drop(project);
    let recovered =
        ActiveProject::open(&root, gif_from_screen_project::LockPolicy::FailIfPresent).unwrap();
    assert_eq!(recovered.project.manifest().timeline.frames.len(), 2);
}
