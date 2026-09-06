use super::*;
use gif_from_screen_domain::{ProjectId, UnixTimeMs};
use gif_from_screen_project::LockPolicy;
use std::time::Instant;

fn options(root: &Path) -> CameraCaptureOptions {
    CameraCaptureOptions {
        device: CameraDevice {
            path: PathBuf::from("/dev/video0"),
            name: "Simulated camera".to_owned(),
        },
        fps: 20,
        recording: LiveRecordingOptions {
            project_path: root.join("camera.gfsproj"),
            canvas: PhysicalSize::new(16, 8).unwrap(),
            project_id: ProjectId::from_u128(1),
            app_version: "test".to_owned(),
            created_at: UnixTimeMs::new(0),
            provenance: SourceProvenance::Camera {
                device_label: Some("Simulated camera".to_owned()),
            },
            provisional_duration_us: 50_000,
            max_frames: 100,
            max_frame_bytes_total: 1024 * 1024,
        },
    }
}

fn ffmpeg_available() -> bool {
    let available = Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        eprintln!("SKIP camera simulation: FFmpeg is not installed");
    }
    available
}

fn simulated_camera(options: &CameraCaptureOptions, size: &str) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-re",
        "-f",
        "lavfi",
        "-i",
        // Unique pixels identify every decoded preview frame independently of
        // callback scheduling, queue pressure, or elapsed wall time.
        &format!("testsrc=size={size}:rate=20:duration=2,geq=r='N+1':g=0:b=0"),
    ]);
    output_options(&mut command, options);
    command
}

#[test]
fn device_paths_reject_network_files_aliases_and_non_video_nodes() {
    for path in [
        "/dev/null",
        "/dev/video",
        "/dev/video-1",
        "/dev/video0/extra",
        "/dev/../dev/video0",
        "video0",
        "http://camera/stream",
        "/tmp/video0",
    ] {
        assert!(validate_camera_device(Path::new(path)).is_err(), "{path}");
    }
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("video0");
    fs::write(&file, b"not a character device").unwrap();
    assert!(validate_camera_device(&file).is_err());
}

#[test]
fn v4l2_command_is_explicit_and_checks_driver_size_substitution() {
    let options = options(Path::new("unused"));
    let command = camera_command(&options);
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect();
    assert!(args.windows(2).any(|pair| pair == ["-f", "v4l2"]));
    assert!(args.windows(2).any(|pair| pair == ["-i", "/dev/video0"]));
    assert!(args.iter().any(|arg| arg.contains("eq(iw,16)*eq(ih,8)")));
    assert!(!args.iter().any(|arg| arg == "-y" || arg == "-f lavfi"));
}

#[test]
fn preview_record_pause_resume_stop_yields_camera_project_without_paused_time() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let options = options(directory.path());
    let path = options.recording.project_path.clone();
    let command = simulated_camera(&options, "16x8");
    let control = CameraControl::default();
    let mut previews = Vec::new();
    let mut dropped_frames = 0;
    let mut paused_clock = 0;
    let project = run_camera_command(
        options,
        command,
        &control,
        &AtomicBool::new(false),
        |progress| {
            let sequence = progress.preview.sequence;
            previews.push((
                sequence,
                gif_from_screen_project::AssetStore::id_for_bytes(&progress.preview.pixels),
            ));
            assert!(progress.dropped_frames >= dropped_frames);
            dropped_frames = progress.dropped_frames;
            // Only 2, 5 and 6 are recording frames. The first two always fit
            // the two-slot queue; its third submission may report backpressure.
            assert!(dropped_frames <= u64::from(sequence == 6));
            match sequence {
                1 => {
                    assert!(!path.exists());
                    assert_eq!(control.phase(), CameraPhase::Preview);
                    control.record().unwrap();
                }
                2 => {
                    control.pause().unwrap();
                    paused_clock = control.active_time_us();
                }
                3 | 4 => {
                    assert_eq!(control.phase(), CameraPhase::Paused);
                    assert_eq!(control.active_time_us(), paused_clock);
                    if sequence == 4 {
                        control.record().unwrap();
                    }
                }
                6 => control.stop(),
                _ => assert_eq!(control.phase(), CameraPhase::Recording),
            }
        },
    )
    .unwrap()
    .unwrap();
    let manifest = project.manifest();
    assert!(matches!(
        manifest.source_provenance[0],
        SourceProvenance::Camera { .. }
    ));
    let frame_count = assert_camera_frame_selection(manifest, &previews, dropped_frames);
    let duration: u64 = manifest
        .timeline
        .frames
        .iter()
        .map(|frame| frame.duration.get())
        .sum();
    assert!(
        duration <= control.active_time_us().saturating_add(1),
        "project duration {duration} exceeds the frozen active clock {}",
        control.active_time_us()
    );
    assert!(
        duration >= control.active_time_us().saturating_sub(paused_clock),
        "recording time after resume was lost"
    );
    manifest.validate().unwrap();
    drop(project);
    let reopened = ActiveProject::open(&path, LockPolicy::FailIfPresent).unwrap();
    assert_eq!(
        reopened.project.manifest().timeline.frames.len(),
        frame_count
    );
    assert!(reopened.asset_issues.is_empty());
}

fn assert_camera_frame_selection(
    manifest: &gif_from_screen_domain::ProjectManifest,
    previews: &[(u64, gif_from_screen_domain::AssetId)],
    dropped_frames: u64,
) -> usize {
    let frame_count = manifest.timeline.frames.len();
    assert!(frame_count > 0);
    assert_eq!(frame_count as u64 + dropped_frames, 3);
    assert_eq!(
        previews
            .iter()
            .map(|(_, asset)| *asset)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6,
        "the fixture must identify each preview independently"
    );
    let saved_sequences = manifest
        .timeline
        .frames
        .iter()
        .map(|frame| {
            previews
                .iter()
                .find(|(_, asset)| *asset == frame.asset_id)
                .expect("saved frame came from the preview")
                .0
        })
        .collect::<Vec<_>>();
    assert_eq!(saved_sequences[0], 2);
    assert!(
        saved_sequences
            .iter()
            .all(|sequence| [2, 5, 6].contains(sequence))
    );
    assert!(saved_sequences.windows(2).all(|pair| pair[0] < pair[1]));
    frame_count
}

#[test]
fn simulated_camera_size_substitution_is_rejected_without_partial_frames() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let options = options(directory.path());
    let path = options.recording.project_path.clone();
    let command = simulated_camera(&options, "32x16");
    let control = CameraControl::default();
    control.record().unwrap();
    let error = run_camera_command(options, command, &control, &AtomicBool::new(false), |_| {})
        .unwrap_err();
    assert!(error.contains("supported resolution"));
    assert!(!path.exists());
}

#[test]
fn explicit_camera_discard_removes_only_the_new_recording() {
    if !ffmpeg_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let options = options(directory.path());
    let path = options.recording.project_path.clone();
    let sibling = directory.path().join("keep-me");
    fs::write(&sibling, b"precious").unwrap();
    let command = simulated_camera(&options, "16x8");
    let control = CameraControl::default();
    control.record().unwrap();
    let project = run_camera_command(
        options,
        command,
        &control,
        &AtomicBool::new(false),
        |progress| {
            if progress.preview.sequence == 3 {
                control.discard();
            }
        },
    )
    .unwrap();
    assert!(project.is_none());
    assert!(!path.exists());
    assert_eq!(fs::read(&sibling).unwrap(), b"precious");
}

#[cfg(unix)]
#[test]
fn frozen_camera_can_be_stopped_without_a_delivered_frame() {
    let directory = tempfile::tempdir().unwrap();
    let options = options(directory.path());
    let path = options.recording.project_path.clone();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exec sleep 30"]);
    let control = CameraControl::default();
    control.record().unwrap();
    thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(80));
            control.stop();
        });
        let started = Instant::now();
        assert!(
            run_camera_command(options, command, &control, &AtomicBool::new(false), |_| {})
                .unwrap()
                .is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    });
    assert!(!path.exists());
}

#[cfg(unix)]
fn frozen_after_one_frame() -> Command {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        &format!("printf '{}'; exec sleep 30", "\\000".repeat(16 * 8 * 4)),
    ]);
    command
}

#[cfg(unix)]
#[test]
fn frozen_recording_saves_on_shutdown_and_only_deletes_on_explicit_discard() {
    for discard in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let options = options(directory.path());
        let path = options.recording.project_path.clone();
        let control = CameraControl::default();
        control.record().unwrap();
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(80));
                if discard {
                    control.discard();
                } else {
                    control.stop();
                }
            });
            let started = Instant::now();
            let saved = run_camera_command(
                options,
                frozen_after_one_frame(),
                &control,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            assert!(started.elapsed() < Duration::from_secs(2));
            if discard {
                assert!(saved.is_none());
                assert!(!path.exists());
            } else {
                let project = saved.unwrap();
                assert_eq!(project.manifest().timeline.frames.len(), 1);
                assert!(project.manifest().timeline.frames[0].duration.get() > 0);
            }
        });
    }
}

#[cfg(unix)]
#[test]
fn automatic_writer_limit_stops_even_if_camera_freezes_after_its_last_frame() {
    let directory = tempfile::tempdir().unwrap();
    let mut options = options(directory.path());
    options.recording.max_frames = 1;
    let control = CameraControl::default();
    control.record().unwrap();
    thread::scope(|scope| {
        // A watchdog makes a regression fail promptly instead of hanging tests.
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(700));
            control.stop();
        });
        let started = Instant::now();
        let project = run_camera_command(
            options,
            frozen_after_one_frame(),
            &control,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap()
        .unwrap();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(project.manifest().timeline.frames.len(), 1);
    });
}

#[cfg(unix)]
#[test]
fn writer_failure_preserves_existing_destination_and_stops_frozen_source() {
    let directory = tempfile::tempdir().unwrap();
    let options = options(directory.path());
    fs::create_dir(&options.recording.project_path).unwrap();
    let sentinel = options.recording.project_path.join("keep");
    fs::write(&sentinel, b"existing project").unwrap();
    let control = CameraControl::default();
    control.record().unwrap();
    let started = Instant::now();
    assert!(
        run_camera_command(
            options,
            frozen_after_one_frame(),
            &control,
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fs::read(&sentinel).unwrap(), b"existing project");
}
