//! Strict external WPF conformance runner. Ordinary tests below use synthetic
//! files only to test this comparator; they are not Windows reference evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};

use gif_from_screen_domain::{
    AssetId, BlendMode, CaptureBinding, CaptureMetadata, ClipTransform, DurationUs, FrameClip,
    FrameId, FrameOverlayCell, FrameOverlayMark, FrameRenderStep, ImageBorderStyle,
    ImageShadowStyle, OverlayContent, OverlayId, OverlayTrack, PhysicalPoint, PhysicalPx,
    PhysicalSize, TimeUs, TrackId,
};
use gif_from_screen_render::{
    AssetProviderError, CpuRenderer, NeverCancel, RenderLimits, RgbaSurface,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_SPEC_BYTES: usize = 64 * 1024;
const MAX_INDEX_BYTES: usize = 256 * 1024;
const MAX_SURFACE_BYTES: usize = 256 * 256 * 4;
const MAX_PNG_BYTES: usize = MAX_SURFACE_BYTES + 64 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    format_version: u16,
    fixtures: Vec<Fixture>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    id: String,
    source: Image,
    operations: Vec<Operation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    ImageShadow { style: ImageShadowStyle },
    ImageBorder { style: ImageBorderStyle },
    Overlay { x: u32, y: u32, image: Image },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceIndex {
    format_version: u16,
    definition_sha256: String,
    provenance: serde_json::Value,
    generator_files: Vec<GeneratorFile>,
    fixtures: Vec<ReferenceFixture>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratorFile {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceFixture {
    id: String,
    input: ReferenceImage,
    stages: Vec<ReferenceImage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceImage {
    width: u32,
    height: u32,
    decoded_dpi_x: f64,
    decoded_dpi_y: f64,
    working_dpi_x: f64,
    working_dpi_y: f64,
    rgba_file: String,
    rgba_sha256: String,
    png_file: String,
    png_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    premultiplied_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    premultiplied_sha256: Option<String>,
}

impl ReferenceImage {
    fn validate_dpi(&self) -> Result<()> {
        // Pixel coordinates are the protocol's exact physical 96-DPI space.
        // WIC's PNG integer pixels-per-metre conversion is metadata only;
        // these accepted readback values never introduce a pixel tolerance.
        if self.working_dpi_x.to_bits() != 96.0_f64.to_bits()
            || self.working_dpi_y.to_bits() != 96.0_f64.to_bits()
        {
            return Err("Reference working DPI must be exactly 96 on both axes.".into());
        }
        for decoded in [self.decoded_dpi_x, self.decoded_dpi_y] {
            if !decoded.is_finite()
                || ![
                    96.0,
                    3779.0 * 0.0254,
                    3780.0 * 0.0254,
                    f64::from(95.9866_f32),
                    f64::from(96.012_f32),
                ]
                .iter()
                .any(|allowed| (decoded - allowed).abs() <= 1e-9)
            {
                return Err(
                    "Reference decoded DPI is not an approved 96-DPI PNG conversion.".into(),
                );
            }
        }
        Ok(())
    }
}

impl Image {
    fn surface(&self) -> Result<RgbaSurface> {
        let bytes = surface_bytes(self.width, self.height)?;
        if self.pixels.len() != bytes / 4 {
            return Err("Image pixel count does not match its dimensions.".into());
        }
        RgbaSurface::new(
            PhysicalSize::new(self.width, self.height).map_err(|error| error.to_string())?,
            self.pixels.iter().flatten().copied().collect(),
        )
        .map_err(|error| error.to_string())
    }
}

fn surface_bytes(width: u32, height: u32) -> Result<usize> {
    if !(1..=256).contains(&width) || !(1..=256).contains(&height) {
        return Err("Reference dimensions must be between 1 and 256 pixels.".into());
    }
    Ok(usize::try_from(width * height * 4).expect("bounded image fits usize"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

fn parse_definition(bytes: &[u8]) -> Result<Definition> {
    if bytes.len() > MAX_SPEC_BYTES {
        return Err("Definition exceeds 64 KiB.".into());
    }
    let definition: Definition =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if definition.format_version != 1
        || definition.fixtures.is_empty()
        || definition.fixtures.len() > 5
    {
        return Err("Unsupported definition version or fixture count (1..=5).".into());
    }
    let mut ids = BTreeSet::new();
    for fixture in &definition.fixtures {
        if !valid_id(&fixture.id)
            || !ids.insert(&fixture.id)
            || !(1..=8).contains(&fixture.operations.len())
        {
            return Err("Fixture IDs must be unique safe names, with 1..=8 operations.".into());
        }
        fixture.source.surface()?;
        for operation in &fixture.operations {
            match operation {
                Operation::ImageShadow { style } => style.validate()?,
                Operation::ImageBorder { style } => style.validate()?,
                Operation::Overlay { x, y, image } => {
                    image.surface()?;
                    if *x > 256 || *y > 256 {
                        return Err("Overlay position exceeds the bounded fixture canvas.".into());
                    }
                }
            }
        }
    }
    Ok(definition)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn verify_hash(bytes: &[u8], expected: &str) -> Result<()> {
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || sha256(bytes) != expected
    {
        return Err("SHA-256 binding does not match the supplied bytes.".into());
    }
    Ok(())
}

fn relative_path(value: &str) -> Result<&Path> {
    // Reject Windows separators/prefixes on Unix too; the same artifact must
    // have the same safe meaning on both platforms.
    let path = Path::new(value);
    if value.is_empty()
        || value.contains(['\\', ':'])
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("Unsafe reference path: {value:?}"));
    }
    Ok(path)
}

fn safe_file(root: &Path, value: &str) -> Result<PathBuf> {
    let relative = relative_path(value)?;
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("Reference path contains a symbolic link: {value}"));
        }
    }
    let resolved = path.canonicalize().map_err(|error| error.to_string())?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err("Reference path escapes its root or is not a regular file.".into());
    }
    Ok(resolved)
}

fn read_bounded(path: &Path, cap: usize, budget: &mut usize) -> Result<Vec<u8>> {
    let cap = cap.min(*budget);
    let file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > u64::try_from(cap).unwrap() {
        return Err("Reference file exceeds its per-file or cumulative byte budget.".into());
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(cap).unwrap() + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > cap {
        return Err("Reference file grew beyond its byte budget while reading.".into());
    }
    *budget -= bytes.len();
    Ok(bytes)
}

fn validate_index(
    index: &ReferenceIndex,
    definition: &Definition,
    definition_bytes: &[u8],
) -> Result<()> {
    if index.format_version != 1
        || index.fixtures.len() != definition.fixtures.len()
        || index
            .provenance
            .as_object()
            .is_none_or(serde_json::Map::is_empty)
    {
        return Err("Reference index version/count/provenance is invalid.".into());
    }
    verify_hash(definition_bytes, &index.definition_sha256)?;
    for field in ["runtime", "sdk_version", "os_description", "os_version"] {
        if index
            .provenance
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(format!("Reference provenance is missing {field}."));
        }
    }
    let mut ids = BTreeSet::new();
    let mut files = BTreeSet::new();
    for reference in &index.fixtures {
        let fixture = definition
            .fixtures
            .iter()
            .find(|fixture| fixture.id == reference.id)
            .ok_or("Reference has an unknown fixture ID.")?;
        if !ids.insert(&reference.id) || reference.stages.len() != fixture.operations.len() {
            return Err("Reference fixture IDs/counts differ from the definition.".into());
        }
        for (position, image) in std::iter::once(&reference.input)
            .chain(&reference.stages)
            .enumerate()
        {
            surface_bytes(image.width, image.height)?;
            let stem = if position == 0 {
                "input".to_owned()
            } else {
                format!("stage-{position:02}")
            };
            if image.rgba_file != format!("{}/{stem}.rgba", reference.id)
                || image.png_file != format!("{}/{stem}.png", reference.id)
            {
                return Err("Reference file names do not match fixture/stage identities.".into());
            }
            if image.premultiplied_file.is_some() != image.premultiplied_sha256.is_some() {
                return Err("Premultiplied file and digest must be present together.".into());
            }
            for path in [&image.rgba_file, &image.png_file]
                .into_iter()
                .chain(image.premultiplied_file.as_ref())
            {
                relative_path(path)?;
                if !files.insert(path) {
                    return Err("Reference image paths must not alias another stage.".into());
                }
            }
        }
    }
    Ok(())
}

fn validate_generator_files(
    index: &ReferenceIndex,
    repository: &Path,
    budget: &mut usize,
) -> Result<()> {
    let repository = repository
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let relative = "scripts/qa/wpf_reference";
    let directory = repository.join(relative);
    let mut expected = BTreeSet::new();
    for entry in fs::read_dir(&directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Non-UTF8 generator file name.")?;
        if Path::new(&name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cs"))
            || matches!(name.as_str(), "WpfReference.csproj" | "global.json")
        {
            expected.insert(format!("{relative}/{name}"));
        }
    }
    if expected.len() > 64
        || expected.len() != index.generator_files.len()
        || !expected.contains(&format!("{relative}/WpfReference.csproj"))
        || !expected.contains(&format!("{relative}/global.json"))
        || !expected.iter().any(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("cs"))
        })
    {
        return Err("Generator source/configuration file set is invalid or incomplete.".into());
    }
    let mut supplied = BTreeSet::new();
    for binding in &index.generator_files {
        if !expected.contains(&binding.path) || !supplied.insert(&binding.path) {
            return Err("Generator source file set differs from this checkout.".into());
        }
        let bytes = read_bounded(&safe_file(&repository, &binding.path)?, 512 * 1024, budget)?;
        verify_hash(&bytes, &binding.sha256)?;
    }
    Ok(())
}

fn load_reference(
    root: &Path,
    reference: &ReferenceImage,
    budget: &mut usize,
) -> Result<RgbaSurface> {
    reference.validate_dpi()?;
    let expected = surface_bytes(reference.width, reference.height)?;
    let bytes = read_bounded(&safe_file(root, &reference.rgba_file)?, expected, budget)?;
    verify_hash(&bytes, &reference.rgba_sha256)?;
    if bytes.len() != expected {
        return Err("Reference RGBA length must exactly match its dimensions.".into());
    }
    let png = read_bounded(
        &safe_file(root, &reference.png_file)?,
        MAX_PNG_BYTES,
        budget,
    )?;
    verify_hash(&png, &reference.png_sha256)?;
    let mut reader = image::ImageReader::with_format(Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(256);
    limits.max_image_height = Some(256);
    limits.max_alloc = Some(1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|error| error.to_string())?;
    if (decoded.width(), decoded.height()) != (reference.width, reference.height) {
        return Err("Reference PNG dimensions disagree with its raw image.".into());
    }
    if let (Some(path), Some(hash)) = (
        &reference.premultiplied_file,
        &reference.premultiplied_sha256,
    ) {
        let bytes = read_bounded(&safe_file(root, path)?, expected, budget)?;
        verify_hash(&bytes, hash)?;
        if bytes.len() != expected {
            return Err("Premultiplied reference byte count is invalid.".into());
        }
    }
    RgbaSurface::new(
        PhysicalSize::new(reference.width, reference.height).map_err(|error| error.to_string())?,
        bytes,
    )
    .map_err(|error| error.to_string())
}

struct Graph {
    frame: FrameClip,
    tracks: Vec<OverlayTrack>,
    assets: BTreeMap<AssetId, RgbaSurface>,
    next_identity: u32,
}

impl Graph {
    fn new(source: &Image) -> Result<Self> {
        let asset_id = AssetId::from_digest([1; 32]);
        Ok(Self {
            frame: FrameClip {
                id: FrameId::from_u128(1),
                asset_id,
                duration: DurationUs::new(100_000).unwrap(),
                transform: ClipTransform::default(),
                effects: Vec::new(),
                capture_metadata: CaptureMetadata::default(),
                capture_binding: CaptureBinding::NotRecorded,
                capture_clock: None,
                render_steps: Vec::new(),
            },
            tracks: Vec::new(),
            assets: BTreeMap::from([(asset_id, source.surface()?)]),
            next_identity: 2,
        })
    }

    fn append(&mut self, operation: &Operation) -> Result<()> {
        let identity = self.next_identity;
        self.next_identity += 1;
        match operation {
            Operation::ImageShadow { .. } | Operation::ImageBorder { .. } => {
                for cell in self
                    .tracks
                    .iter_mut()
                    .flat_map(|track| track.frame_cells.iter_mut().flatten())
                {
                    if cell.stage.is_none() {
                        cell.stage = Some(identity);
                    }
                }
                self.frame
                    .render_steps
                    .push(FrameRenderStep::Composite { stage_id: identity });
                self.frame.render_steps.push(match operation {
                    Operation::ImageShadow { style } => {
                        FrameRenderStep::ImageShadow { style: *style }
                    }
                    Operation::ImageBorder { style } => {
                        FrameRenderStep::ImageBorder { style: *style }
                    }
                    Operation::Overlay { .. } => unreachable!(),
                });
            }
            Operation::Overlay { x, y, image } => {
                let mut digest = [0; 32];
                digest[..4].copy_from_slice(&identity.to_le_bytes());
                let asset_id = AssetId::from_digest(digest);
                let surface = image.surface()?;
                let size = surface.size();
                self.assets.insert(asset_id, surface);
                let cell = FrameOverlayCell::whole(
                    self.frame.id,
                    1,
                    vec![FrameOverlayMark {
                        id: OverlayId::from_u128(u128::from(identity)),
                        z_index: 0,
                        content: OverlayContent::Raster {
                            asset_id,
                            position: PhysicalPoint {
                                x: PhysicalPx::new(*x),
                                y: PhysicalPx::new(*y),
                            },
                            size,
                            opacity: 255,
                        },
                    }],
                );
                self.tracks.push(OverlayTrack {
                    id: TrackId::from_u128(u128::from(identity)),
                    name: format!("Fixture overlay {identity}"),
                    visible: true,
                    opacity: 255,
                    blend_mode: BlendMode::Normal,
                    items: Vec::new(),
                    annotation: None,
                    annotation_scope: None,
                    frame_cells: Some(vec![cell]),
                });
            }
        }
        Ok(())
    }

    fn render(&self) -> Result<RgbaSurface> {
        let provider = |id| -> std::result::Result<RgbaSurface, AssetProviderError> {
            self.assets
                .get(&id)
                .cloned()
                .ok_or_else(|| std::io::Error::other("Unknown fixture asset.").into())
        };
        let image = CpuRenderer::with_limits(RenderLimits {
            max_surface_bytes: MAX_SURFACE_BYTES,
        })
        .render_clip_with_overlays(
            &self.frame,
            &self.tracks,
            TimeUs::ZERO,
            &provider,
            &NeverCancel,
        )
        .map_err(|error| error.to_string())?;
        surface_bytes(image.size().width.get(), image.size().height.get())?;
        Ok(image)
    }
}

#[derive(Debug, Serialize)]
struct PixelDifference {
    expected_size: [u32; 2],
    actual_size: [u32; 2],
    mismatch_pixels: usize,
    mismatch_channels: usize,
    channel_max_delta: [u8; 4],
    first_mismatch_coordinates: Vec<[u32; 2]>,
}

fn difference(expected: &RgbaSurface, actual: &RgbaSurface) -> (PixelDifference, RgbaSurface) {
    let expected_size = [expected.size().width.get(), expected.size().height.get()];
    let actual_size = [actual.size().width.get(), actual.size().height.get()];
    let width = expected_size[0].max(actual_size[0]);
    let height = expected_size[1].max(actual_size[1]);
    let mut report = PixelDifference {
        expected_size,
        actual_size,
        mismatch_pixels: 0,
        mismatch_channels: 0,
        channel_max_delta: [0; 4],
        first_mismatch_coordinates: Vec::new(),
    };
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let delta = match (pixel_at(expected, x, y), pixel_at(actual, x, y)) {
                (Some(first), Some(second)) => {
                    std::array::from_fn(|channel| first[channel].abs_diff(second[channel]))
                }
                _ => [255; 4],
            };
            if delta != [0; 4] {
                report.mismatch_pixels += 1;
                if report.first_mismatch_coordinates.len() < 64 {
                    report.first_mismatch_coordinates.push([x, y]);
                }
            }
            for (channel, change) in delta.iter().enumerate() {
                report.mismatch_channels += usize::from(*change != 0);
                report.channel_max_delta[channel] = report.channel_max_delta[channel].max(*change);
            }
            // Also make alpha-only differences visible without hiding RGB
            // differences underneath transparent pixels in the numeric report.
            pixels.extend_from_slice(&[
                delta[0].max(delta[3]),
                delta[1].max(delta[3]),
                delta[2].max(delta[3]),
                255,
            ]);
        }
    }
    (
        report,
        RgbaSurface::new(PhysicalSize::new(width, height).unwrap(), pixels).unwrap(),
    )
}

fn pixel_at(surface: &RgbaSurface, x: u32, y: u32) -> Option<&[u8]> {
    if x >= surface.size().width.get() || y >= surface.size().height.get() {
        return None;
    }
    let offset = usize::try_from((y * surface.size().width.get() + x) * 4).unwrap();
    Some(&surface.pixels()[offset..offset + 4])
}

#[derive(Serialize)]
struct StageReport {
    fixture_id: String,
    stage: usize,
    reference: ReferenceImage,
    actual_sha256: Option<String>,
    difference: Option<PixelDifference>,
    error: Option<String>,
}

#[derive(Serialize)]
struct ComparisonReport {
    format_version: u16,
    definition_sha256: String,
    provenance: serde_json::Value,
    generator_files: Vec<GeneratorFile>,
    strict_rgba: bool,
    stages: Vec<StageReport>,
    errors: Vec<String>,
    passed: bool,
}

fn claim_output(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || fs::read_dir(path)
                    .map_err(|error| error.to_string())?
                    .next()
                    .is_some()
            {
                return Err("Comparison output must be a new or empty real directory.".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    path.canonicalize().map_err(|error| error.to_string())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())
}

fn write_png(path: &Path, surface: &RgbaSurface) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(file),
        surface.pixels(),
        surface.size().width.get(),
        surface.size().height.get(),
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|error| error.to_string())
}

fn independent_output(reference: &Path, output: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(reference)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Reference root must not be a symbolic link.".into());
    }
    let root = reference
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let prospective = if output.exists() {
        output.canonicalize().map_err(|error| error.to_string())?
    } else {
        output
            .parent()
            .ok_or("Comparison output needs a parent directory.")?
            .canonicalize()
            .map_err(|error| error.to_string())?
            .join(
                output
                    .file_name()
                    .ok_or("Comparison output needs a directory name.")?,
            )
    };
    if prospective.starts_with(&root) || root.starts_with(&prospective) {
        return Err("Comparison output must be independent of the reference directory.".into());
    }
    Ok(root)
}

fn compare_stage(
    root: &Path,
    output: &Path,
    fixture: &str,
    stage: usize,
    reference: &ReferenceImage,
    actual: Result<RgbaSurface>,
    budget: &mut usize,
) -> StageReport {
    let mut report = StageReport {
        fixture_id: fixture.to_owned(),
        stage,
        reference: reference.clone(),
        actual_sha256: None,
        difference: None,
        error: None,
    };
    let result = (|| {
        let expected = load_reference(root, reference, budget)?;
        let actual = actual?;
        report.actual_sha256 = Some(sha256(actual.pixels()));
        let (difference, diff) = difference(&expected, &actual);
        let mismatch = difference.mismatch_pixels != 0;
        report.difference = Some(difference);
        if mismatch {
            write_png(
                &output.join(format!("{fixture}-stage-{stage:02}-actual.png")),
                &actual,
            )?;
            write_png(
                &output.join(format!("{fixture}-stage-{stage:02}-diff.png")),
                &diff,
            )?;
        }
        Ok::<_, String>(())
    })();
    report.error = result.err();
    report
}

fn compare(
    definition_bytes: &[u8],
    reference_root: &Path,
    output: &Path,
    repository: &Path,
) -> Result<()> {
    // Reject overlap before creating even a failure report in the reference.
    let root = independent_output(reference_root, output)?;
    let output = claim_output(output)?;
    let mut report = ComparisonReport {
        format_version: 1,
        definition_sha256: sha256(definition_bytes),
        provenance: serde_json::Value::Null,
        generator_files: Vec::new(),
        strict_rgba: true,
        stages: Vec::new(),
        errors: Vec::new(),
        passed: false,
    };
    let result = (|| {
        let definition = parse_definition(definition_bytes)?;
        if definition.fixtures.len() != 5 {
            return Err("External conformance definition must contain all five fixtures.".into());
        }
        let mut budget = MAX_TOTAL_BYTES;
        let index_bytes = read_bounded(
            &safe_file(&root, "index.json")?,
            MAX_INDEX_BYTES,
            &mut budget,
        )?;
        let index: ReferenceIndex =
            serde_json::from_slice(&index_bytes).map_err(|error| error.to_string())?;
        report.provenance = index.provenance.clone();
        report.generator_files.clone_from(&index.generator_files);
        validate_index(&index, &definition, definition_bytes)?;
        validate_generator_files(&index, repository, &mut budget)?;
        for fixture in &definition.fixtures {
            let reference = index
                .fixtures
                .iter()
                .find(|reference| reference.id == fixture.id)
                .expect("validated identities");
            report.stages.push(compare_stage(
                &root,
                &output,
                &fixture.id,
                0,
                &reference.input,
                fixture.source.surface(),
                &mut budget,
            ));
            let mut graph = Graph::new(&fixture.source)?;
            for (position, operation) in fixture.operations.iter().enumerate() {
                let rendered = graph.append(operation).and_then(|()| graph.render());
                report.stages.push(compare_stage(
                    &root,
                    &output,
                    &fixture.id,
                    position + 1,
                    &reference.stages[position],
                    rendered,
                    &mut budget,
                ));
            }
        }
        Ok::<_, String>(())
    })();
    if let Err(error) = result {
        report.errors.push(error);
    }
    report.passed = report.errors.is_empty()
        && report.stages.iter().all(|stage| {
            stage.error.is_none()
                && stage
                    .difference
                    .as_ref()
                    .is_some_and(|difference| difference.mismatch_pixels == 0)
        });
    write_new(
        &output.join("report.json"),
        &serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
    )?;
    if !report.passed {
        return Err(format!(
            "Strict WPF comparison failed; inspect {}. No reference was accepted or changed.",
            output.join("report.json").display()
        ));
    }
    Ok(())
}

#[test]
#[ignore = "requires independently generated Windows WPF reference artifacts"]
fn compare_windows_wpf_reference() -> Result<()> {
    let reference = std::env::var_os("GFS_WPF_REFERENCE_DIR")
        .filter(|value| !value.is_empty())
        .ok_or("GFS_WPF_REFERENCE_DIR is required.")?;
    let output = std::env::var_os("GFS_WPF_COMPARE_DIR")
        .filter(|value| !value.is_empty())
        .ok_or("GFS_WPF_COMPARE_DIR is required.")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let definition = repository.join("scripts/qa/wpf_reference/fixtures.json");
    let mut budget = MAX_SPEC_BYTES;
    let bytes = read_bounded(&definition, MAX_SPEC_BYTES, &mut budget)?;
    compare(
        &bytes,
        Path::new(&reference),
        Path::new(&output),
        &repository,
    )
}

#[cfg(test)]
mod mechanical_tests {
    use super::*;

    fn definition() -> Definition {
        Definition {
            format_version: 1,
            fixtures: vec![Fixture {
                id: "synthetic-not-wpf".into(),
                source: Image {
                    width: 1,
                    height: 1,
                    pixels: vec![[255, 0, 0, 255]],
                },
                operations: vec![Operation::ImageShadow {
                    style: ImageShadowStyle::default(),
                }],
            }],
        }
    }

    #[test]
    fn parser_rejects_unknown_payloads_duplicate_ids_counts_and_dimensions() {
        let original = definition();
        parse_definition(&serde_json::to_vec(&original).unwrap()).unwrap();
        for invalid in [
            Definition {
                format_version: 2,
                ..original.clone()
            },
            Definition {
                fixtures: vec![original.fixtures[0].clone(); 2],
                ..original.clone()
            },
            Definition {
                fixtures: Vec::new(),
                ..original.clone()
            },
        ] {
            assert!(parse_definition(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        let mut invalid = original.clone();
        invalid.fixtures[0].source.width = 257;
        assert!(parse_definition(&serde_json::to_vec(&invalid).unwrap()).is_err());
        invalid = original;
        invalid.fixtures[0].operations = vec![invalid.fixtures[0].operations[0].clone(); 9];
        assert!(parse_definition(&serde_json::to_vec(&invalid).unwrap()).is_err());
        assert!(parse_definition(&vec![b' '; MAX_SPEC_BYTES + 1]).is_err());
        assert!(parse_definition(br#"{"format_version":1,"fixtures":[],"execute":"no"}"#).is_err());
    }

    #[test]
    fn sha256_is_standard_and_binds_exact_bytes_and_transparent_channels() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        verify_hash(b"abc", &sha256(b"abc")).unwrap();
        assert!(verify_hash(b"abd", &sha256(b"abc")).is_err());
        assert!(verify_hash(b"abc", "bad").is_err());
        let first = Image {
            width: 1,
            height: 1,
            pixels: vec![[0, 0, 0, 0]],
        }
        .surface()
        .unwrap();
        let second = Image {
            width: 1,
            height: 1,
            pixels: vec![[1, 0, 0, 0]],
        }
        .surface()
        .unwrap();
        let (difference, _) = difference(&first, &second);
        assert_eq!(difference.mismatch_pixels, 1);
        assert_eq!(difference.mismatch_channels, 1);
        assert_eq!(difference.channel_max_delta, [1, 0, 0, 0]);
        assert_eq!(difference.first_mismatch_coordinates, [[0, 0]]);
    }

    #[test]
    fn portable_paths_reject_traversal_absolute_and_windows_aliases() {
        for bad in [
            "",
            "/etc/passwd",
            "../outside",
            "a/../outside",
            "a/./b",
            "a//b",
            "a/",
            "C:/outside",
            "a\\b",
            "\\\\host\\share",
        ] {
            assert!(relative_path(bad).is_err(), "{bad}");
        }
        assert_eq!(
            relative_path("fixture/stage-01.rgba").unwrap(),
            Path::new("fixture/stage-01.rgba")
        );
    }

    #[test]
    fn reads_and_output_claims_are_bounded_and_do_not_overwrite_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bytes");
        write_new(&path, &[1, 2, 3]).unwrap();
        let mut budget = 4;
        assert_eq!(read_bounded(&path, 3, &mut budget).unwrap(), [1, 2, 3]);
        assert_eq!(budget, 1);
        assert!(read_bounded(&path, 3, &mut budget).is_err());
        assert!(read_bounded(&path, 2, &mut 100).is_err());
        assert!(claim_output(directory.path()).is_err());
        assert!(write_new(&path, &[9]).is_err());
        assert_eq!(fs::read(&path).unwrap(), [1, 2, 3]);
        let empty = directory.path().join("output");
        claim_output(&empty).unwrap();
        claim_output(&empty).unwrap();
        assert!(independent_output(&empty, &empty.join("nested")).is_err());
        assert!(!empty.join("nested").exists());
    }

    #[test]
    fn mismatch_dimensions_and_alpha_are_visible_without_tolerance() {
        let first = Image {
            width: 1,
            height: 1,
            pixels: vec![[2, 3, 4, 255]],
        }
        .surface()
        .unwrap();
        let second = Image {
            width: 2,
            height: 1,
            pixels: vec![[2, 3, 4, 254], [0; 4]],
        }
        .surface()
        .unwrap();
        let (report, diff) = difference(&first, &second);
        assert_eq!(report.mismatch_pixels, 2);
        assert_eq!(report.mismatch_channels, 5);
        assert_eq!(report.channel_max_delta, [255; 4]);
        assert_eq!(&diff.pixels()[..4], &[1, 1, 1, 255]);
    }

    #[test]
    fn prefixes_keep_original_source_and_prior_overlays_at_their_paint_stage() {
        let mut graph = Graph::new(&Image {
            width: 2,
            height: 1,
            pixels: vec![[255, 0, 0, 255], [0; 4]],
        })
        .unwrap();
        let overlay = |x, rgba| Operation::Overlay {
            x,
            y: 0,
            image: Image {
                width: 1,
                height: 1,
                pixels: vec![rgba],
            },
        };
        graph.append(&overlay(1, [0, 255, 0, 255])).unwrap();
        graph
            .append(&Operation::ImageShadow {
                style: ImageShadowStyle {
                    blur_radius_hundredths: 0,
                    depth_hundredths: 100,
                    direction_hundredths: 0,
                    opacity_basis_points: 10_000,
                    ..ImageShadowStyle::default()
                },
            })
            .unwrap();
        assert!(
            graph.tracks[0].frame_cells.as_ref().unwrap()[0]
                .stage
                .is_some()
        );
        assert_eq!(
            graph.render().unwrap().pixels(),
            &[255, 0, 0, 255, 0, 255, 0, 255, 2, 2, 2, 255]
        );
        graph.append(&overlay(2, [0, 0, 255, 255])).unwrap();
        assert_eq!(graph.tracks[1].frame_cells.as_ref().unwrap()[0].stage, None);
        assert_eq!(
            graph.render().unwrap().pixels(),
            &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]
        );
        assert_eq!(
            graph.assets[&graph.frame.asset_id].pixels(),
            &[255, 0, 0, 255, 0, 0, 0, 0]
        );
    }

    fn synthetic_reference(
        directory: &Path,
        id: &str,
        stem: &str,
        image: &RgbaSurface,
    ) -> ReferenceImage {
        let rgba_file = format!("{id}/{stem}.rgba");
        let png_file = format!("{id}/{stem}.png");
        write_new(&directory.join(&rgba_file), image.pixels()).unwrap();
        write_png(&directory.join(&png_file), image).unwrap();
        ReferenceImage {
            width: image.size().width.get(),
            height: image.size().height.get(),
            decoded_dpi_x: 96.0,
            decoded_dpi_y: 96.0,
            working_dpi_x: 96.0,
            working_dpi_y: 96.0,
            rgba_sha256: sha256(image.pixels()),
            png_sha256: sha256(&fs::read(directory.join(&png_file)).unwrap()),
            rgba_file,
            png_file,
            premultiplied_file: None,
            premultiplied_sha256: None,
        }
    }

    fn synthetic_generator(repository: &Path) -> Vec<GeneratorFile> {
        let generator = repository.join("scripts/qa/wpf_reference");
        fs::create_dir_all(&generator).unwrap();
        ["Program.cs", "WpfReference.csproj", "global.json"]
            .into_iter()
            .map(|name| {
                write_new(&generator.join(name), b"synthetic comparator test only").unwrap();
                GeneratorFile {
                    path: format!("scripts/qa/wpf_reference/{name}"),
                    sha256: sha256(b"synthetic comparator test only"),
                }
            })
            .collect()
    }

    #[test]
    fn dpi_envelope_allows_only_documented_png_metadata_conversions() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("synthetic")).unwrap();
        let source = Image {
            width: 1,
            height: 1,
            pixels: vec![[1, 2, 3, 255]],
        }
        .surface()
        .unwrap();
        let reference = synthetic_reference(directory.path(), "synthetic", "input", &source);
        let root = directory.path().canonicalize().unwrap();
        for decoded in [
            96.0,
            95.9866,
            96.012,
            f64::from(95.9866_f32),
            f64::from(96.012_f32),
        ] {
            let mut changed = reference.clone();
            changed.decoded_dpi_x = decoded;
            changed.decoded_dpi_y = decoded;
            changed.validate_dpi().unwrap();
            let mut budget = MAX_TOTAL_BYTES;
            assert_eq!(
                load_reference(&root, &changed, &mut budget).unwrap(),
                source
            );
        }
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            72.0,
            120.0,
            95.99,
            96.000_002,
            95.986_602,
            96.012_002,
        ] {
            let mut changed = reference.clone();
            changed.decoded_dpi_x = invalid;
            assert!(changed.validate_dpi().is_err());
            changed.decoded_dpi_x = 96.0;
            changed.decoded_dpi_y = invalid;
            assert!(changed.validate_dpi().is_err());
        }
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            95.9866,
            96.012,
            96.0 + f64::EPSILON * 128.0,
        ] {
            let mut changed = reference.clone();
            changed.working_dpi_x = invalid;
            assert!(changed.validate_dpi().is_err());
            changed.working_dpi_x = 96.0;
            changed.working_dpi_y = invalid;
            assert!(changed.validate_dpi().is_err());
        }
        for field in [
            "decoded_dpi_x",
            "decoded_dpi_y",
            "working_dpi_x",
            "working_dpi_y",
        ] {
            let mut json = serde_json::to_value(&reference).unwrap();
            json.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<ReferenceImage>(json).is_err());
        }
        let output = directory.path().join("report-output");
        fs::create_dir(&output).unwrap();
        let mut budget = MAX_TOTAL_BYTES;
        let report = compare_stage(
            &root,
            &output,
            "synthetic",
            0,
            &reference,
            Ok(source),
            &mut budget,
        );
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["reference"]["decoded_dpi_x"], 96.0);
        assert_eq!(json["reference"]["decoded_dpi_y"], 96.0);
        assert_eq!(json["reference"]["working_dpi_x"], 96.0);
        assert_eq!(json["reference"]["working_dpi_y"], 96.0);
    }

    #[test]
    fn synthetic_comparator_integrity_checks_bind_sources_and_collect_all_stage_failures() {
        // These bytes are explicitly synthetic plumbing fixtures, never a WPF
        // oracle or evidence that the renderer agrees with Windows.
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("synthetic-repository");
        let generator_files = synthetic_generator(&repository);
        let reference = directory.path().join("synthetic-reference");
        fs::create_dir(&reference).unwrap();
        let source = Image {
            width: 1,
            height: 1,
            pixels: vec![[255, 0, 0, 255]],
        };
        let definition = Definition {
            format_version: 1,
            fixtures: (0..5)
                .map(|number| Fixture {
                    id: format!("synthetic-{number}"),
                    source: source.clone(),
                    operations: vec![Operation::Overlay {
                        x: 0,
                        y: 0,
                        image: source.clone(),
                    }],
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&definition).unwrap();
        let surface = source.surface().unwrap();
        let mut index = ReferenceIndex {
            format_version: 1,
            definition_sha256: sha256(&bytes),
            provenance: serde_json::json!({"runtime":"synthetic, not WPF", "sdk_version":"synthetic", "os_description":"synthetic", "os_version":"synthetic"}),
            generator_files,
            fixtures: definition
                .fixtures
                .iter()
                .map(|fixture| {
                    fs::create_dir(reference.join(&fixture.id)).unwrap();
                    ReferenceFixture {
                        id: fixture.id.clone(),
                        input: synthetic_reference(&reference, &fixture.id, "input", &surface),
                        stages: vec![synthetic_reference(
                            &reference,
                            &fixture.id,
                            "stage-01",
                            &surface,
                        )],
                    }
                })
                .collect(),
        };
        validate_index(&index, &definition, &bytes).unwrap();
        let mut budget = MAX_TOTAL_BYTES;
        validate_generator_files(&index, &repository, &mut budget).unwrap();
        let duplicate_stage = index.fixtures[0].stages[0].clone();
        index.fixtures[0].stages.push(duplicate_stage);
        assert!(validate_index(&index, &definition, &bytes).is_err());
        index.fixtures[0].stages.pop();
        index.fixtures[0].input.premultiplied_file = Some("synthetic-0/input.pbgra".into());
        assert!(validate_index(&index, &definition, &bytes).is_err());
        index.fixtures[0].input.premultiplied_file = None;
        let mut wrong_hash = index.clone();
        wrong_hash.generator_files[0].sha256 = sha256(b"different generator");
        assert!(validate_generator_files(&wrong_hash, &repository, &mut budget).is_err());
        wrong_hash.generator_files.pop();
        assert!(validate_generator_files(&wrong_hash, &repository, &mut budget).is_err());
        // An RGB difference must be reported as a pixel mismatch,
        // while an unrelated corrupt digest must not stop remaining stages.
        let changed = [255, 1, 0, 255];
        fs::write(reference.join(&index.fixtures[0].input.rgba_file), changed).unwrap();
        index.fixtures[0].input.rgba_sha256 = sha256(&changed);
        index.fixtures[0].stages[0].rgba_sha256 = sha256(b"corrupted binding");
        write_new(
            &reference.join("index.json"),
            &serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        let output = directory.path().join("comparison");
        assert!(compare(&bytes, &reference, &output, &repository).is_err());
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
        assert_eq!(report["passed"], false);
        assert_eq!(report["stages"].as_array().unwrap().len(), 10);
        assert_eq!(report["stages"][0]["difference"]["mismatch_channels"], 1);
        assert!(
            report["stages"][1]["error"]
                .as_str()
                .unwrap()
                .contains("SHA-256")
        );
        assert_eq!(report["stages"][9]["difference"]["mismatch_pixels"], 0);
        assert!(output.join("synthetic-0-stage-00-actual.png").is_file());
        assert!(output.join("synthetic-0-stage-00-diff.png").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn reference_symlinks_are_rejected_even_when_their_targets_are_inside() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        write_new(&root.join("real"), &[1]).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        assert!(safe_file(&root, "alias").is_err());
        std::os::unix::fs::symlink(&root, root.join("nested")).unwrap();
        assert!(safe_file(&root, "nested/real").is_err());
        assert!(claim_output(&root.join("nested")).is_err());
    }
}
