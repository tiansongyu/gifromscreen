#![forbid(unsafe_code)]

//! Diagnostics and headless tooling for `GifFromScreen` projects and encoders.

use std::{env, fs, io, path::Path};

use gif_from_screen_domain::{
    AssetDescriptor, AssetKind, Canvas, CanvasBackground, CaptureMetadata, ClipTransform,
    ColorSpace, DurationUs, EditCommand, FrameClip, FrameId, PhysicalSize, ProjectId,
    ProjectManifest, RasterEncoding, UnixTimeMs,
};
use gif_from_screen_gif::{BuiltinGifEncoder, EncodeOptions, RgbaFrame};
use gif_from_screen_project::{ActiveProject, LockPolicy};

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
        "GifFromScreen CLI\n\nUSAGE:\n  gif-from-screen-cli doctor\n  gif-from-screen-cli demo [OUTPUT.gif]\n  gif-from-screen-cli demo-project PROJECT_DIRECTORY OUTPUT.gif\n  gif-from-screen-cli export PROJECT_DIRECTORY OUTPUT.gif\n  gif-from-screen-cli version"
    );
}

fn write_demo(output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if output.exists() {
        return Err(format!("refusing to overwrite {}", output.display()).into());
    }
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid output filename"))?;
    let partial = output.with_file_name(format!(".{file_name}.partial"));
    let frames = (0..36).map(demo_frame).collect::<Result<Vec<_>, _>>()?;
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
    let report = result?;
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
            transform: ClipTransform::default(),
            capture_metadata: CaptureMetadata::default(),
            effects: Vec::new(),
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
    let frames = opened
        .project
        .manifest()
        .timeline
        .frames
        .iter()
        .map(|clip| {
            if clip.transform != ClipTransform::default() || !clip.effects.is_empty() {
                return Err(
                    "project requires the render pipeline, which is not connected yet".into(),
                );
            }
            let descriptor = opened
                .project
                .manifest()
                .assets
                .get(&clip.asset_id)
                .ok_or("frame asset descriptor is missing")?;
            let AssetKind::Frame { size, encoding } = &descriptor.kind else {
                return Err("timeline item does not reference a frame asset".into());
            };
            if *encoding != RasterEncoding::Rgba8 {
                return Err(
                    "only raw RGBA8 project assets are supported by this vertical slice".into(),
                );
            }
            let width = u16::try_from(size.width.get())?;
            let height = u16::try_from(size.height.get())?;
            let pixels = opened.project.assets().read(clip.asset_id)?;
            RgbaFrame::new(width, height, pixels, clip.duration.get()).map_err(Into::into)
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;

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
    let report = result?;
    println!(
        "exported revision {} as {} GIF frames to {}",
        opened.project.manifest().revision,
        report.encoded_frames,
        output.display()
    );
    Ok(())
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
