#![forbid(unsafe_code)]

//! Diagnostics and headless tooling for `GifFromScreen` projects and encoders.

use std::{env, fs, io, path::Path, time::Duration};

use gif_from_screen_capture::{
    CaptureBackend, CaptureCadence, CaptureRequest, CaptureTarget, CursorCaptureMode, FramePoll,
    PhysicalRect as CaptureRect, PixelFormat,
};
use gif_from_screen_capture_linux::X11CaptureBackend;
use gif_from_screen_domain::{
    AssetDescriptor, AssetId, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
    ColorSpace, DurationUs, EdgeWidths, EditCommand, Effect, FrameClip, FrameId,
    PhysicalRect as DomainRect, PhysicalSize, ProjectId, ProjectManifest, RasterEncoding, Rgba,
    UnixTimeMs,
};
use gif_from_screen_gif::{BuiltinGifEncoder, EncodeOptions, RgbaFrame};
use gif_from_screen_project::{ActiveProject, LockPolicy};
use gif_from_screen_render::{
    AssetProviderError, CpuRenderer, FrameAssetProvider, NeverCancel, RgbaSurface,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "help".to_owned());

    match command.as_str() {
        "doctor" => doctor(),
        "version" | "--version" | "-V" => println!("{}", env!("CARGO_PKG_VERSION")),
        "demo" => {
            let output = arguments.next().unwrap_or_else(|| "demo.gif".to_owned());
            write_demo(Path::new(&output))?;
        }
        "demo-project" => {
            let project = arguments.next().ok_or("missing PROJECT_DIRECTORY")?;
            let output = arguments.next().ok_or("missing OUTPUT.gif")?;
            write_demo_project(Path::new(&project), Path::new(&output))?;
        }
        "export" => {
            let project = arguments.next().ok_or("missing PROJECT_DIRECTORY")?;
            let output = arguments.next().ok_or("missing OUTPUT.gif")?;
            export_project(Path::new(&project), Path::new(&output))?;
        }
        "sources-x11" => list_x11_sources()?,
        "record-x11" => {
            let output = arguments.next().ok_or("missing OUTPUT.gif")?;
            let duration_ms = parse_optional(&mut arguments, 3_000_u64, "duration milliseconds")?;
            let fps = parse_optional(&mut arguments, 10_u32, "FPS")?;
            let region = parse_optional_region(&mut arguments)?;
            record_x11(Path::new(&output), duration_ms, fps, region)?;
        }
        "help" | "--help" | "-h" => help(),
        other => {
            eprintln!("unknown command: {other}");
            help();
            return Err("unknown command".into());
        }
    }
    Ok(())
}

fn help() {
    println!(
        "GifFromScreen CLI\n\nUSAGE:\n  gif-from-screen-cli doctor\n  gif-from-screen-cli demo [OUTPUT.gif]\n  gif-from-screen-cli demo-project PROJECT_DIRECTORY OUTPUT.gif\n  gif-from-screen-cli export PROJECT_DIRECTORY OUTPUT.gif\n  gif-from-screen-cli sources-x11\n  gif-from-screen-cli record-x11 OUTPUT.gif [DURATION_MS] [FPS] [X Y WIDTH HEIGHT]\n  gif-from-screen-cli version"
    );
}

fn write_demo(output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if output.exists() {
        return Err(format!("refusing to overwrite {}", output.display()).into());
    }
    let frames = (0..36).map(demo_frame).collect::<Result<Vec<_>, _>>()?;
    let report = write_gif_frames(output, frames)?;
    println!(
        "wrote {} frames ({} GIF frames, {} ticks) to {}",
        report.input_frames,
        report.encoded_frames,
        report.encoded_duration_ticks,
        output.display()
    );
    Ok(())
}

fn write_demo_project(
    project_root: &Path,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    const WIDTH: u16 = 160;
    const HEIGHT: u16 = 96;
    let manifest = ProjectManifest::new(
        ProjectId::from_u128(unique_u128()),
        env!("CARGO_PKG_VERSION"),
        UnixTimeMs::new(unix_time_ms()),
        Canvas {
            size: PhysicalSize::new(u32::from(WIDTH), u32::from(HEIGHT))?,
            color_space: ColorSpace::Srgb,
            background: CanvasBackground::Transparent,
        },
    )?;
    let mut project = ActiveProject::create(project_root, manifest)?;

    for index in 0..36_u16 {
        let frame = demo_frame(index)?;
        let asset_id = project.assets().put(frame.pixels())?;
        let descriptor = AssetDescriptor {
            id: asset_id,
            byte_len: u64::try_from(frame.pixels().len())?,
            kind: AssetKind::Frame {
                size: PhysicalSize::new(u32::from(WIDTH), u32::from(HEIGHT))?,
                encoding: RasterEncoding::Rgba8,
            },
        };
        let clip = FrameClip {
            id: FrameId::from_u128(unique_u128()),
            asset_id,
            duration: DurationUs::new(frame.duration_us()).ok_or("zero frame duration")?,
            transform: ClipTransform {
                crop: Some(DomainRect::new(4, 4, 152, 88)?),
                output_size: Some(PhysicalSize::new(u32::from(WIDTH), u32::from(HEIGHT))?),
                ..ClipTransform::default()
            },
            capture_metadata: CaptureMetadata::default(),
            effects: vec![Effect::Border {
                widths: EdgeWidths {
                    top: 2,
                    right: 2,
                    bottom: 2,
                    left: 2,
                },
                color: Rgba {
                    red: 242,
                    green: 153,
                    blue: 74,
                    alpha: 255,
                },
            }],
        };
        let mut commands = Vec::with_capacity(2);
        if !project.manifest().assets.contains_key(&asset_id) {
            commands.push(EditCommand::RegisterAsset { asset: descriptor });
        }
        commands.push(EditCommand::InsertFrames {
            index: project.manifest().timeline.frames.len(),
            frames: vec![clip],
        });
        project.commit(EditCommand::Compound { commands })?;
    }
    project.checkpoint_and_compact()?;
    drop(project);
    export_project(project_root, output)
}

fn export_project(project_root: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if output.exists() {
        return Err(format!("refusing to overwrite {}", output.display()).into());
    }
    let opened = ActiveProject::open(project_root, LockPolicy::FailIfPresent)?;
    if !opened.asset_issues.is_empty() {
        return Err(format!("project has asset problems: {:?}", opened.asset_issues).into());
    }
    let renderer = CpuRenderer::new();
    let provider = ProjectAssetProvider {
        project: &opened.project,
    };
    let frames = opened
        .project
        .manifest()
        .timeline
        .frames
        .iter()
        .map(|clip| {
            let surface = renderer.render_clip(clip, &provider, &NeverCancel)?;
            RgbaFrame::new(
                u16::try_from(surface.width())?,
                u16::try_from(surface.height())?,
                surface.into_pixels(),
                clip.duration.get(),
            )
            .map_err(Into::into)
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;

    let report = write_gif_frames(output, frames)?;
    println!(
        "exported revision {} as {} GIF frames to {}",
        opened.project.manifest().revision,
        report.encoded_frames,
        output.display()
    );
    Ok(())
}

struct ProjectAssetProvider<'a> {
    project: &'a ActiveProject,
}

impl FrameAssetProvider for ProjectAssetProvider<'_> {
    fn load_rgba8(&self, asset_id: AssetId) -> Result<RgbaSurface, AssetProviderError> {
        let descriptor = self
            .project
            .manifest()
            .assets
            .get(&asset_id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("asset descriptor {asset_id} is missing"),
                )
            })?;
        let AssetKind::Frame { size, encoding } = &descriptor.kind else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("asset {asset_id} is not a frame"),
            )
            .into());
        };
        if *encoding != RasterEncoding::Rgba8 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("asset {asset_id} is not stored as raw RGBA8"),
            )
            .into());
        }
        let pixels = self.project.assets().read(asset_id)?;
        RgbaSurface::new(*size, pixels).map_err(Into::into)
    }
}

fn write_gif_frames(
    output: &Path,
    frames: Vec<RgbaFrame>,
) -> Result<gif_from_screen_gif::EncodeReport, Box<dyn std::error::Error>> {
    if output.exists() {
        return Err(format!("refusing to overwrite {}", output.display()).into());
    }
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid output filename"))?;
    let partial = output.with_file_name(format!(".{file_name}.partial"));
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let mut file = fs::File::create(&partial)?;
        let report = BuiltinGifEncoder::default().encode_frames(
            frames,
            &mut file,
            &EncodeOptions::default(),
        )?;
        file.sync_all()?;
        drop(file);
        fs::rename(&partial, output)?;
        Ok(report)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

fn list_x11_sources() -> Result<(), Box<dyn std::error::Error>> {
    let backend = X11CaptureBackend::connect(None)?;
    for source in backend.list_sources()? {
        let geometry = source.geometry().map_or_else(
            || "unknown geometry".to_owned(),
            |rect| {
                format!(
                    "{}x{} at {},{}",
                    rect.size().width(),
                    rect.size().height(),
                    rect.origin().x,
                    rect.origin().y
                )
            },
        );
        println!("{}\t{}\t{geometry}", source.id(), source.name());
    }
    Ok(())
}

fn record_x11(
    output: &Path,
    duration_ms: u64,
    fps: u32,
    region: Option<CaptureRect>,
) -> Result<(), Box<dyn std::error::Error>> {
    if duration_ms == 0 || duration_ms > 60_000 {
        return Err("duration must be between 1 and 60000 milliseconds".into());
    }
    if !(1..=60).contains(&fps) {
        return Err("FPS must be between 1 and 60".into());
    }
    let backend = X11CaptureBackend::connect(None)?;
    let source = backend
        .list_sources()?
        .into_iter()
        .next()
        .ok_or("X11 backend returned no capture sources")?;
    let target = region.map_or_else(
        || CaptureTarget::Monitor(source.id().clone()),
        |region| CaptureTarget::Region {
            source: source.id().clone(),
            region,
        },
    );
    let mut request = CaptureRequest::new(target, CaptureCadence::fixed_fps(fps)?);
    request.cursor = CursorCaptureMode::Hidden;
    let mut session = backend.start_session(request)?;
    let deadline = std::time::Instant::now()
        .checked_add(Duration::from_millis(duration_ms))
        .ok_or("capture deadline overflow")?;
    let mut captured = Vec::new();
    while std::time::Instant::now() < deadline {
        match session.poll_frame(Duration::from_millis(250))? {
            FramePoll::Frame(frame) => captured.push(frame),
            FramePoll::Pending => {}
            FramePoll::EndOfStream => break,
        }
    }
    session.stop()?;
    if captured.is_empty() {
        return Err("capture finished without any frames".into());
    }

    let fallback_duration = 1_000_000_u64 / u64::from(fps);
    let mut frames = Vec::with_capacity(captured.len());
    for (index, frame) in captured.iter().enumerate() {
        let frame_delay_us = captured
            .get(index + 1)
            .map(|next| {
                next.captured_at()
                    .as_micros()
                    .saturating_sub(frame.captured_at().as_micros())
            })
            .filter(|duration| *duration > 0)
            .unwrap_or(fallback_duration);
        frames.push(captured_frame_to_gif(frame, frame_delay_us)?);
    }
    let report = write_gif_frames(output, frames)?;
    println!(
        "captured {} X11 frames and wrote {} GIF frames to {}",
        captured.len(),
        report.encoded_frames,
        output.display()
    );
    Ok(())
}

fn captured_frame_to_gif(
    frame: &gif_from_screen_capture::CapturedFrame,
    duration_us: u64,
) -> Result<RgbaFrame, Box<dyn std::error::Error>> {
    let width = usize::try_from(frame.size().width())?;
    let height = usize::try_from(frame.size().height())?;
    let tight_stride = width.checked_mul(4).ok_or("RGBA row size overflow")?;
    let tight_len = tight_stride
        .checked_mul(height)
        .ok_or("RGBA frame size overflow")?;
    let mut pixels = Vec::with_capacity(tight_len);
    for row in frame.pixels().chunks(frame.stride()).take(height) {
        let row = row.get(..tight_stride).ok_or("captured row is truncated")?;
        match frame.format() {
            PixelFormat::Rgba8 => pixels.extend_from_slice(row),
            PixelFormat::Bgra8 => {
                for pixel in row.chunks_exact(4) {
                    pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
            }
            _ => return Err("unsupported captured pixel format".into()),
        }
    }
    if pixels.len() != tight_len {
        return Err("captured frame has too few rows".into());
    }
    RgbaFrame::new(
        u16::try_from(width)?,
        u16::try_from(height)?,
        pixels,
        duration_us,
    )
    .map_err(Into::into)
}

fn parse_optional<T: std::str::FromStr>(
    arguments: &mut impl Iterator<Item = String>,
    default: T,
    label: &str,
) -> Result<T, Box<dyn std::error::Error>> {
    arguments.next().map_or(Ok(default), |value| {
        value
            .parse()
            .map_err(|_| format!("invalid {label}: {value}").into())
    })
}

fn parse_optional_region(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<Option<CaptureRect>, Box<dyn std::error::Error>> {
    let Some(x) = arguments.next() else {
        return Ok(None);
    };
    let y = arguments.next().ok_or("region requires Y WIDTH HEIGHT")?;
    let width = arguments.next().ok_or("region requires WIDTH HEIGHT")?;
    let height = arguments.next().ok_or("region requires HEIGHT")?;
    Ok(Some(CaptureRect::new(
        x.parse().map_err(|_| "invalid region X")?,
        y.parse().map_err(|_| "invalid region Y")?,
        width.parse().map_err(|_| "invalid region WIDTH")?,
        height.parse().map_err(|_| "invalid region HEIGHT")?,
    )?))
}

fn unique_u128() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let high = u128::from(u64::try_from(unix_time_ms()).unwrap_or_default());
    (high << 64) | u128::from(SEQUENCE.fetch_add(1, Ordering::Relaxed))
}

fn unix_time_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

fn demo_frame(index: u16) -> Result<RgbaFrame, gif_from_screen_gif::FrameError> {
    const WIDTH: u16 = 160;
    const HEIGHT: u16 = 96;
    let mut pixels = vec![0_u8; usize::from(WIDTH) * usize::from(HEIGHT) * 4];
    let square_x = 8 + (index * 4) % (WIDTH - 32);
    let square_y = 28 + ((index / 9) % 2) * 12;

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let offset = (usize::from(y) * usize::from(WIDTH) + usize::from(x)) * 4;
            let in_square =
                x >= square_x && x < square_x + 24 && y >= square_y && y < square_y + 24;
            let color = if in_square {
                [242, 153, 74, 255]
            } else {
                [
                    24,
                    36 + u8::try_from(x / 4).expect("x gradient fits u8"),
                    58 + u8::try_from(y / 3).expect("y gradient fits u8"),
                    255,
                ]
            };
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    RgbaFrame::new(WIDTH, HEIGHT, pixels, 50_000)
}

fn doctor() {
    let session = env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".to_owned());
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = env::var_os("DISPLAY").is_some();
    let portal = Path::new("/usr/share/dbus-1/interfaces/org.freedesktop.portal.ScreenCast.xml")
        .exists()
        || Path::new("/usr/share/dbus-1/services/org.freedesktop.portal.Desktop.service").exists();

    println!("GifFromScreen Linux diagnostics");
    println!("session type: {session}");
    println!("Wayland display: {}", yes_no(wayland));
    println!("X11 display: {}", yes_no(x11));
    println!("XDG desktop portal files: {}", yes_no(portal));

    let candidate = if wayland || session.eq_ignore_ascii_case("wayland") {
        "Wayland portal + PipeWire"
    } else if x11 || session.eq_ignore_ascii_case("x11") {
        "X11"
    } else {
        "none detected"
    };
    println!("candidate capture backend: {candidate}");
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
