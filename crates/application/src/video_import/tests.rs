use super::*;
use gif_from_screen_project::LockPolicy;

fn options(root: &Path) -> VideoImportOptions {
    VideoImportOptions {
        input: root.join("input.mkv"),
        project_path: root.join("movie.gfsproj"),
        project_id: ProjectId::from_u128(100),
        app_version: "test".to_owned(),
        created_at: UnixTimeMs::new(0),
        start: Duration::ZERO,
        duration: Duration::from_secs(1),
        fps: 3,
        output_size: None,
        limits: VideoImportLimits::default(),
    }
}

#[test]
fn frame_boundaries_do_not_accumulate_fractional_fps_drift() {
    let durations: Vec<_> = (0..3)
        .map(|index| frame_boundary(index + 1, 3) - frame_boundary(index, 3))
        .collect();
    assert_eq!(durations, [333_333, 333_333, 333_334]);
    assert_eq!(frame_boundary(60, 60), 1_000_000);
}

#[test]
fn budget_validation_rejects_large_frames_and_excess_frame_count() {
    let mut request = options(Path::new("unused"));
    request.duration = Duration::from_secs(300);
    request.fps = 60;
    assert!(validate_options(&request).is_err());
    request.duration = Duration::from_secs(1);
    request.output_size = Some(PhysicalSize::new(5000, 5000).unwrap());
    assert!(validate_options(&request).is_err());
    request.output_size = Some(PhysicalSize::new(2, 2).unwrap());
    assert_eq!(validate_options(&request).unwrap(), 60);
    request.duration = Duration::from_micros(1);
    assert_eq!(validate_options(&request).unwrap(), 1);
    request.output_size = Some(PhysicalSize::new(u32::MAX, u32::MAX).unwrap());
    assert!(validate_options(&request).is_err());
}

#[test]
fn duration_metadata_is_parsed_without_nan_exponents_or_integer_overflow() {
    assert_eq!(
        parse_seconds("1.2345678"),
        Some(Duration::from_micros(1_234_567))
    );
    assert_eq!(parse_seconds("2.1"), Some(Duration::from_millis(2100)));
    for value in [
        "NaN",
        "inf",
        "-1",
        "1e9",
        "1.2.3",
        "18446744073709551615",
        "",
    ] {
        assert_eq!(parse_seconds(value), None);
    }
    let mut request = options(Path::new("unused"));
    request.start = Duration::from_millis(700);
    request.duration = Duration::from_secs(10);
    assert_eq!(
        trim_to_source(&mut request, Some(Duration::from_secs(1))).unwrap(),
        1
    );
    assert_eq!(request.duration, Duration::from_millis(300));
    request.start = Duration::from_secs(1);
    assert!(trim_to_source(&mut request, Some(Duration::from_secs(1))).is_err());
}

#[test]
fn malformed_metadata_and_playlist_formats_are_rejected() {
    for bytes in [
        &b"{}"[..],
        &br#"{"streams":[],"format":{"format_name":"mov"}}"#[..],
        &br#"{"streams":[{"width":2,"height":2}],"format":{"format_name":"hls"}}"#[..],
        &br#"{"streams":[{"width":2,"height":2,"disposition":{"attached_pic":1}}],"format":{"format_name":"mov"}}"#[..],
        &br#"{"streams":[{"width":8000,"height":8000}],"format":{"format_name":"mov"}}"#[..],
        &br#"{"streams":[{"width":2,"height":2,"side_data_list":[{"rotation":45}]}],"format":{"format_name":"mov"}}"#[..],
    ] {
        assert!(parse_probe(bytes).is_err());
    }
}

#[test]
fn rotation_metadata_changes_the_default_canvas() {
    let metadata = parse_probe(br#"{"streams":[{"width":16,"height":8,"side_data_list":[{"rotation":-90}]}],"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2"}}"#).unwrap();
    assert_eq!(metadata.size, PhysicalSize::new(8, 16).unwrap());
}

#[test]
fn source_path_is_one_literal_argument_and_decoder_is_local_only() {
    let request = options(Path::new("unused"));
    let path = Path::new("/tmp/weird video $(touch NEVER); 'quotes'.mp4");
    let command = decoder_command(path, &request, PhysicalSize::new(8, 4).unwrap(), 3);
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy())
        .collect();
    assert_eq!(command.get_program(), "ffmpeg");
    assert!(
        args.windows(2)
            .any(|pair| pair == ["-protocol_whitelist", "file"])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "-i" && pair[1] == path.to_string_lossy())
    );
    assert!(!args.iter().any(|arg| arg.contains("hls") || arg == "-y"));
}

#[test]
fn cancellation_before_start_creates_no_directory() {
    let directory = tempfile::tempdir().unwrap();
    let request = options(directory.path());
    let destination = request.project_path.clone();
    let error = import_video_project(request, &AtomicBool::new(true), |_| {}).unwrap_err();
    assert!(error.is_cancelled());
    assert!(error.partial_project_path().is_none());
    assert!(!destination.exists());
}

fn require_ffmpeg() -> bool {
    let available = Command::new("ffmpeg")
        .args(["-version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        eprintln!("SKIP real video fixture: system FFmpeg is not installed");
    }
    available
}

fn make_fixture(path: &Path) {
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=16x8:rate=6:duration=2",
            "-c:v",
            "ffv1",
            "-threads",
            "1",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn real_video_interval_is_streamed_to_a_durable_editable_project() {
    if !require_ffmpeg() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut request = options(directory.path());
    make_fixture(&request.input);
    request.start = Duration::from_millis(500);
    request.output_size = Some(PhysicalSize::new(8, 4).unwrap());
    let destination = request.project_path.clone();
    let mut progress = Vec::new();
    let project = import_video_project(request, &AtomicBool::new(false), |update| {
        progress.push(update);
    })
    .unwrap();
    assert_eq!(project.manifest().timeline.frames.len(), 3);
    assert_eq!(
        project.manifest().canvas.size,
        PhysicalSize::new(8, 4).unwrap()
    );
    assert_eq!(progress.last().unwrap().duration_us, 1_000_000);
    assert!(matches!(
        project.manifest().source_provenance[0],
        SourceProvenance::Imported { .. }
    ));
    project.manifest().validate().unwrap();
    drop(project);
    let reopened = ActiveProject::open(&destination, LockPolicy::FailIfPresent).unwrap();
    assert_eq!(reopened.project.manifest().timeline.frames.len(), 3);
    assert!(reopened.asset_issues.is_empty());
}

#[test]
fn real_video_partial_last_frame_and_end_of_source_preserve_interval_duration() {
    if !require_ffmpeg() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut request = options(directory.path());
    make_fixture(&request.input);
    request.start = Duration::from_millis(1500);
    request.duration = Duration::from_secs(10);
    let project = import_video_project(request, &AtomicBool::new(false), |_| {}).unwrap();
    let frames = &project.manifest().timeline.frames;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].duration.get(), 333_333);
    assert_eq!(frames[1].duration.get(), 166_667);
}

#[test]
fn real_video_seek_changes_the_imported_pixels() {
    if !require_ffmpeg() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut request = options(directory.path());
    make_fixture(&request.input);
    request.fps = 1;
    let first = import_video_project(request.clone(), &AtomicBool::new(false), |_| {}).unwrap();
    request.start = Duration::from_secs(1);
    request.project_path = directory.path().join("later.gfsproj");
    let later = import_video_project(request, &AtomicBool::new(false), |_| {}).unwrap();
    assert_ne!(
        first.manifest().timeline.frames[0].asset_id,
        later.manifest().timeline.frames[0].asset_id
    );
}

#[test]
fn real_video_cancellation_retains_only_the_new_partial_project() {
    if !require_ffmpeg() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let request = options(directory.path());
    make_fixture(&request.input);
    let destination = request.project_path.clone();
    let cancelled = AtomicBool::new(false);
    let error = import_video_project(request, &cancelled, |_| {
        cancelled.store(true, Ordering::Relaxed);
    })
    .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(error.partial_project_path(), Some(destination.as_path()));
    let reopened = ActiveProject::open(&destination, LockPolicy::FailIfPresent).unwrap();
    assert_eq!(reopened.project.manifest().timeline.frames.len(), 1);
    assert!(reopened.asset_issues.is_empty());
}

#[test]
fn existing_destination_and_disk_budget_fail_before_any_project_mutation() {
    if !require_ffmpeg() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut request = options(directory.path());
    make_fixture(&request.input);
    fs::create_dir(&request.project_path).unwrap();
    let sentinel = request.project_path.join("do-not-overwrite");
    fs::write(&sentinel, b"precious").unwrap();
    let error = import_video_project(request.clone(), &AtomicBool::new(false), |_| {}).unwrap_err();
    assert!(matches!(error, VideoImportError::InvalidOptions(_)));
    assert_eq!(fs::read(&sentinel).unwrap(), b"precious");
    request.project_path = directory.path().join("too-large.gfsproj");
    request.limits.max_total_bytes = 1;
    let destination = request.project_path.clone();
    let error = import_video_project(request, &AtomicBool::new(false), |_| {}).unwrap_err();
    assert!(matches!(error, VideoImportError::InvalidOptions(_)));
    assert!(!destination.exists());
}

#[test]
fn real_playlist_input_is_rejected_without_project_creation() {
    if !require_ffmpeg() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut request = options(directory.path());
    request.input = directory.path().join("unsafe.m3u8");
    fs::write(&request.input, "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nhttp://127.0.0.1:9/never-open.ts\n#EXT-X-ENDLIST\n").unwrap();
    let destination = request.project_path.clone();
    assert!(import_video_project(request, &AtomicBool::new(false), |_| {}).is_err());
    assert!(!destination.exists());
}
