//! Explicit desktop-worker recording benchmark, invoked only by the owned-Xvfb harness.
//! It exercises the same incremental project writer as the GUI, without claiming
//! to measure GUI rendering, compositor presentation or physical monitor latency.

use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Instant,
};

use gif_from_screen_capture::{CaptureBackend, CaptureSourceKind};
use gif_from_screen_gif::CancellationFlag;
use gif_from_screen_project::{ActiveProject, AssetCheck, LockPolicy};
use gif_from_screen_workflow::{RecordingController, WorkflowPhase, WorkflowProgress};
use serde::Deserialize;
use serde_json::json;

use crate::{
    RecordingCadenceChoice, RecordingCompletion, RecordingCursor, RecordingSettings,
    RecordingWorkerRequest, X11CaptureBackend, run_incremental_x11_recording,
};

#[derive(Deserialize)]
struct Configuration {
    display: String,
    xvfb_pid: u32,
    width: u32,
    height: u32,
    fps: u32,
    duration_ms: u64,
}

fn owns_display_listeners(table: &str, socket_inodes: &BTreeSet<String>, number: u16) -> bool {
    let path = format!("/tmp/.X11-unix/X{number}");
    let abstract_path = format!("@{path}");
    let mut found = false;
    for line in table.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 8 || (fields[7] != path && fields[7] != abstract_path) {
            continue;
        }
        let flags = u32::from_str_radix(fields[3], 16).unwrap_or(0);
        if fields[4] != "0001" || flags & 0x0001_0000 == 0 {
            continue; // Connected Unix streams do not determine a new client's destination.
        }
        if !socket_inodes.contains(fields[6]) {
            return false; // Both address forms must belong to this owned Xvfb.
        }
        found = true;
    }
    found
}

fn verify_display_owner(config: &Configuration, uid: u32, number: u16) {
    let process = PathBuf::from(format!("/proc/{}", config.xvfb_pid));
    assert_eq!(fs::metadata(&process).unwrap().uid(), uid);
    let command = fs::read(process.join("cmdline")).unwrap();
    let executable = command.split(|byte| *byte == 0).next().unwrap();
    assert!(executable.ends_with(b"/Xvfb") || executable == b"Xvfb");
    let socket_inodes: BTreeSet<_> = fs::read_dir(process.join("fd"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter_map(|target| {
            target
                .to_str()?
                .strip_prefix("socket:[")?
                .strip_suffix(']')
                .map(str::to_owned)
        })
        .collect();
    assert!(
        owns_display_listeners(
            &fs::read_to_string("/proc/net/unix").unwrap(),
            &socket_inodes,
            number
        ),
        "the supplied Xvfb PID must own this DISPLAY's listening Unix sockets"
    );
    // Xorg -displayfd may legitimately run without creating a lock file. When
    // present, the lock remains an additional check, never the sole proof.
    match fs::read_to_string(format!("/tmp/.X{number}-lock")) {
        Ok(pid) => assert_eq!(pid.trim(), config.xvfb_pid.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("could not verify the optional Xvfb lock: {error}"),
    }
}

fn checked_configuration() -> (PathBuf, Configuration) {
    let directory = PathBuf::from(
        std::env::var_os("GFS_RECORDING_PERF_DIR")
            .expect("run scripts/qa/recording_perf.py; never capture the host DISPLAY"),
    );
    let metadata = fs::symlink_metadata(&directory).unwrap();
    assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
    assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    assert_eq!(metadata.mode() & 0o077, 0);
    let config: Configuration =
        serde_json::from_slice(&fs::read(directory.join("configuration.json")).unwrap()).unwrap();
    assert_eq!(std::env::var("DISPLAY").unwrap(), config.display);
    let number = config
        .display
        .strip_prefix(':')
        .unwrap()
        .parse::<u16>()
        .unwrap();
    verify_display_owner(&config, metadata.uid(), number);
    assert!((1..=60).contains(&config.fps));
    assert!((1..=600_000).contains(&config.duration_ms));
    (directory, config)
}

fn project_bytes(path: &Path) -> u64 {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                project_bytes(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}

fn percentile(values: &[u64], percent: usize) -> Option<u64> {
    let index = values
        .len()
        .checked_mul(percent)?
        .div_ceil(100)
        .checked_sub(1)?;
    values.get(index).copied()
}

#[test]
fn display_ownership_requires_listening_socket_in_the_supplied_process() {
    let table = "Num RefCount Protocol Flags Type St Inode Path\n0: 2 0 00010000 0001 01 41 /tmp/.X11-unix/X7\n0: 2 0 00010000 0001 01 42 @/tmp/.X11-unix/X7\n";
    let owned = BTreeSet::from(["41".into(), "42".into()]);
    assert!(owns_display_listeners(table, &owned, 7));
    assert!(!owns_display_listeners(table, &owned, 1));
    assert!(!owns_display_listeners(
        table,
        &BTreeSet::from(["41".into()]),
        7
    ));
    assert!(!owns_display_listeners(table, &BTreeSet::new(), 7));
}

#[test]
fn connected_streams_and_unrelated_displays_are_not_ownership_proof() {
    let table = "Num RefCount Protocol Flags Type St Inode Path\n0: 2 0 00000000 0001 03 41 /tmp/.X11-unix/X7\n0: 2 0 00010000 0001 01 42 @/tmp/.X11-unix/X1\n";
    assert!(!owns_display_listeners(
        table,
        &BTreeSet::from(["41".into(), "42".into()]),
        7
    ));
}

#[test]
#[ignore = "use scripts/qa/recording_perf.py to allocate and verify an owned Xvfb"]
#[allow(
    clippy::too_many_lines,
    reason = "one explicit benchmark keeps capture, flush and separate recovery measurements together"
)]
fn owned_xvfb_desktop_incremental_recording() {
    let (directory, config) = checked_configuration();
    let backend = X11CaptureBackend::connect(None).unwrap();
    let source = backend
        .list_sources()
        .unwrap()
        .into_iter()
        .find(|source| source.kind() == CaptureSourceKind::Monitor)
        .unwrap();
    let geometry = source.geometry().unwrap();
    assert_eq!(
        (geometry.size().width(), geometry.size().height()),
        (config.width, config.height)
    );
    let worker = RecordingWorkerRequest {
        settings: RecordingSettings {
            output: directory.join("unused.gif").to_string_lossy().into_owned(),
            duration_ms: config.duration_ms,
            cadence: RecordingCadenceChoice::FixedFps,
            fps: config.fps,
            countdown_seconds: 0,
            cursor: RecordingCursor::Hidden,
            changes_only: false,
            region_enabled: true,
            region_x: 0,
            region_y: 0,
            region_width: config.width,
            region_height: config.height,
            ..RecordingSettings::default()
        },
        project_path: directory.join("recording.gfsproj"),
        canvas: gif_from_screen_domain::PhysicalSize::new(config.width, config.height).unwrap(),
        source_id: source.id().clone(),
        source_kind: source.kind(),
        source_label: "Owned Xvfb dynamic numbered source".into(),
    };
    let (_controller, mut control) = RecordingController::channel();
    let cancellation = CancellationFlag::default();
    let mut latest: Option<WorkflowProgress> = None;
    let mut capture_stopped = None;
    let mut collection_and_barrier = None;
    let started = Instant::now();
    fs::write(directory.join("recording-started"), b"1").unwrap();
    let completion = run_incremental_x11_recording(
        &worker,
        &mut control,
        &cancellation,
        &mut |snapshot: WorkflowProgress| {
            if snapshot.phase == WorkflowPhase::StoppingCapture && capture_stopped.is_none() {
                capture_stopped = Some(started.elapsed());
            }
            latest = Some(snapshot);
        },
        || collection_and_barrier = Some(started.elapsed()),
    );
    let elapsed = started.elapsed();
    fs::write(directory.join("recording-finished"), b"1").unwrap();
    let (status, error, expected) = match completion {
        RecordingCompletion::Completed(project) => {
            let manifest = project.manifest().clone();
            drop(project);
            ("completed", None, Some(manifest))
        }
        RecordingCompletion::Failed { error, .. } => ("failed", Some(error), None),
        RecordingCompletion::Discarded { cleanup_error } => ("discarded", cleanup_error, None),
    };
    let verification_started = Instant::now();
    let reopened = ActiveProject::open(&worker.project_path, LockPolicy::FailIfPresent).unwrap();
    assert!(reopened.asset_issues.is_empty());
    let digest_issues = reopened
        .project
        .validate_assets(AssetCheck::FullDigest)
        .unwrap();
    assert!(digest_issues.is_empty());
    if let Some(expected) = expected {
        assert_eq!(
            reopened.project.manifest(),
            &expected,
            "reopened project must retain the saved timeline"
        );
    }
    let manifest = reopened.project.manifest();
    let timestamps: Vec<_> = manifest
        .timeline
        .frames
        .iter()
        .map(|frame| frame.capture_metadata.captured_at.unwrap().get())
        .collect();
    assert!(timestamps.windows(2).all(|pair| pair[0] < pair[1]));
    let mut intervals: Vec<_> = timestamps
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    intervals.sort_unstable();
    let span_us = timestamps
        .first()
        .zip(timestamps.last())
        .map_or(0, |(first, last)| last - first);
    let intervals_count = u32::try_from(intervals.len()).unwrap();
    let actual_fps = (span_us > 0).then(|| {
        f64::from(intervals_count) / std::time::Duration::from_micros(span_us).as_secs_f64()
    });
    let report = json!({
        "schema": 1,
        "scope": "desktop incremental X11 capture and project writer; no GUI/physical-display FPS claim",
        "status": status, "error": error,
        "width": config.width, "height": config.height, "requested_fps": config.fps,
        "requested_duration_ms": config.duration_ms,
        "recording_and_flush_wall_seconds": elapsed.as_secs_f64(),
        "capture_stopped_wall_seconds": capture_stopped.map(|value| value.as_secs_f64()),
        "collection_and_barrier_wall_seconds": collection_and_barrier.map(|value| value.as_secs_f64()),
        "stopping_and_pending_writer_drain_seconds": collection_and_barrier.zip(capture_stopped).map(|(barrier, stop)| barrier.saturating_sub(stop).as_secs_f64()),
        "final_checkpoint_seconds": collection_and_barrier.map(|value| elapsed.saturating_sub(value).as_secs_f64()),
        "frames": timestamps.len(), "unique_pixel_assets": manifest.assets.len(),
        "capture_pts_span_us": span_us, "capture_pts_us": timestamps,
        "capture_fps_from_pts": actual_fps,
        "capture_interval_us": {"p50": percentile(&intervals, 50), "p95": percentile(&intervals, 95), "p99": percentile(&intervals, 99), "max": intervals.last()},
        "progress_frames": latest.map(|snapshot| snapshot.frames_captured),
        "playback_duration_us": manifest.timeline.total_duration().unwrap().get(),
        "project_bytes": project_bytes(&worker.project_path),
        "recovery": {"reopen": true, "full_digest": true, "monotonic_pts": true, "verification_seconds": verification_started.elapsed().as_secs_f64()}
    });
    fs::write(
        directory.join("recording.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("{report}");
    assert_eq!(
        status, "completed",
        "recording failure was reported in recording.json"
    );
    assert!(!manifest.timeline.frames.is_empty());
}
