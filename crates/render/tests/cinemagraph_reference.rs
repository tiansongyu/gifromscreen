//! External Windows evidence for already-clipped PM snapshot storage/composition.
//!
//! This does NOT validate Rust freehand/Ink geometry generation. The supplied
//! `clip-direct.pbgra` is an actual Windows snapshot, never a Rust-generated mask.
//! Ordinary tests below use synthetic data solely to test this comparator.
//! Initial real evidence: Windows Actions run 34078589472, probe commit a631aab.
//! Later runs may use another commit only when the complete probe source hashes
//! and fixed definition identity still match; actual provenance stays in the report.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

use gif_from_screen_domain::{
    AssetId, CaptureBinding, CaptureMetadata, ClipTransform, DurationUs, FrameClip, FrameId,
    FrameRenderStep, PhysicalSize, TimeUs,
};
use gif_from_screen_render::{
    AssetProviderError, CpuRenderer, FrameAssetProvider, NeverCancel, OverlayRenderPlan,
    PremultipliedRgbaSurface, RenderLimits, RgbaSurface,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_INDEX_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_SURFACE_BYTES: usize = 32 * 32 * 4;
const MAX_ARTIFACT_BYTES: usize = 64 * 1024;
const DEFINITION_SHA256: &str = "9ed2b610efa32f966f2c1be0a3dad18a4016972907f56b4ad97f7b629b4452b4";
type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Deserialize)]
struct Index {
    format_version: u16,
    fixture_count: usize,
    definition_sha256: String,
    provenance: serde_json::Value,
    source_files: Vec<SourceFile>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Case {
    fixture: Fixture,
    artifacts: Vec<Artifact>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    id: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    file: String,
    format: String,
    bytes: usize,
    sha256: String,
}

#[derive(Debug, Default, Serialize)]
struct Difference {
    pixels: usize,
    channels: usize,
    max_channel_error: u8,
    first_pixel: Option<usize>,
}

#[derive(Debug, Serialize)]
struct CaseComparison {
    id: String,
    render_clip: Difference,
    direct_overlays: Difference,
    detached_plan: Difference,
}

#[derive(Debug, Serialize)]
struct Report {
    claim: &'static str,
    generator_commit: String,
    provenance: serde_json::Value,
    definition_sha256: String,
    index_sha256: String,
    cases: usize,
    render_routes: usize,
    differing_cases: usize,
    different_channels: usize,
    verified_index_and_artifact_bytes: usize,
    mismatches: Vec<CaseComparison>,
}

fn safe_relative(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 256
        || value.contains(['\\', ':'])
        || value.split('/').any(|part| matches!(part, "" | "." | ".."))
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("Unsafe artifact path: {value:?}"));
    }
    Ok(path)
}

fn safe_file(root: &Path, name: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in safe_relative(name)?.components() {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Err(
                "Artifact paths must contain only real directories and regular files.".into(),
            );
        }
    }
    let resolved = path.canonicalize().map_err(|error| error.to_string())?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err("Artifact escapes its root or is not a regular file.".into());
    }
    Ok(resolved)
}

fn read_bounded(path: &Path, cap: usize, budget: &mut usize) -> Result<Vec<u8>> {
    let cap = cap.min(*budget);
    let file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > u64::try_from(cap).unwrap() {
        return Err("Artifact exceeds its per-file or cumulative byte budget.".into());
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(cap).unwrap() + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > cap {
        return Err("Artifact grew beyond its byte budget while reading.".into());
    }
    *budget -= bytes.len();
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_matches(bytes: &[u8], expected: &str) -> Result<()> {
    if !valid_hash(expected) || sha256(bytes) != expected {
        return Err("SHA-256 does not match the indexed bytes.".into());
    }
    Ok(())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn surface_bytes(fixture: &Fixture) -> Result<usize> {
    if !(1..=32).contains(&fixture.width) || !(1..=32).contains(&fixture.height) {
        return Err("Every fixture must be between 1 and 32 pixels on both axes.".into());
    }
    Ok(usize::try_from(fixture.width * fixture.height * 4).unwrap())
}

fn parse_index(bytes: &[u8]) -> Result<Index> {
    if bytes.len() > MAX_INDEX_BYTES {
        return Err("Probe index exceeds 2 MiB.".into());
    }
    let index: Index = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if index.format_version != 1
        || index.fixture_count != index.cases.len()
        || !(1..=128).contains(&index.cases.len())
        || !(1..=64).contains(&index.source_files.len())
    {
        return Err("Unsupported probe version or bounded fixture/source count.".into());
    }
    let mut ids = BTreeSet::new();
    let mut files = BTreeSet::new();
    for case in &index.cases {
        surface_bytes(&case.fixture)?;
        let id = &case.fixture.id;
        if id.is_empty()
            || id.len() > 96
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            || !ids.insert(id)
            || !(3..=64).contains(&case.artifacts.len())
        {
            return Err("Case IDs must be unique safe names with 3..=64 artifacts.".into());
        }
        for artifact in &case.artifacts {
            safe_relative(&artifact.file)?;
            if !artifact.file.starts_with(&format!("{id}/"))
                || !files.insert(&artifact.file)
                || artifact.bytes > MAX_ARTIFACT_BYTES
                || !valid_hash(&artifact.sha256)
                || artifact.format.is_empty()
                || artifact.format.len() > 128
            {
                return Err("Invalid, repeated or out-of-case artifact descriptor.".into());
            }
        }
    }
    Ok(index)
}

fn validate_provenance(index: &Index) -> Result<()> {
    verify_definition_cases(index)?;
    let p = &index.provenance;
    if !p["generator_commit"].as_str().is_some_and(|value| {
        value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) || index.definition_sha256 != DEFINITION_SHA256
        || p["upstream_commit"] != "a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd"
        || p["apartment_state"] != "STA"
        || !p["runtime"]
            .as_str()
            .is_some_and(|value| value.starts_with(".NET 9."))
        || !p["sdk_version"]
            .as_str()
            .is_some_and(|value| value.starts_with("9.0."))
        || !p["os_description"]
            .as_str()
            .is_some_and(|value| value.contains("Windows"))
    {
        return Err("Probe is not the explicitly pinned Windows .NET9 STA evidence set.".into());
    }
    for binary in [
        "presentation_core",
        "presentation_framework",
        "wpfgfx_cor3",
        "windows_codecs",
        "generator_assembly",
    ] {
        if !p[binary]["sha256"].as_str().is_some_and(valid_hash)
            || p[binary]["file_version"].as_str().is_none_or(str::is_empty)
        {
            return Err(format!("Missing measured binary provenance: {binary}"));
        }
    }
    Ok(())
}

// A copied definition-hash string is not proof that the full corpus is present.
fn verify_definition_cases(index: &Index) -> Result<()> {
    let mut expected = BTreeSet::new();
    for geometry in [
        "empty",
        "whole",
        "rectangle",
        "rectangle_fractional",
        "ellipse",
        "ellipse_fractional",
        "ink_dot_ellipse",
        "ink_dot_rectangle",
        "ink_line_ellipse",
        "ink_line_rectangle",
    ] {
        for first in ["opaque", "zero", "partial"] {
            for current in ["opaque", "zero", "partial"] {
                expected.insert(format!("{geometry}-{first}-{current}"));
            }
        }
    }
    if index.cases.len() != expected.len()
        || index.cases.iter().any(|case| {
            case.fixture.width != 12
                || case.fixture.height != 10
                || !expected.remove(&case.fixture.id)
        })
        || !expected.is_empty()
    {
        return Err("The fixed definition requires all 90 distinct 12x10 cases; partial evidence is not a full run.".into());
    }
    Ok(())
}

fn verify_source_files(index: &Index, repository: &Path) -> Result<()> {
    let relative = "scripts/qa/cinemagraph_probe";
    let mut expected = BTreeSet::new();
    for entry in fs::read_dir(repository.join(relative)).map_err(|error| error.to_string())? {
        let name = entry
            .map_err(|error| error.to_string())?
            .file_name()
            .into_string()
            .map_err(|_| "Non-UTF8 probe source name.")?;
        if Path::new(&name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cs"))
            || matches!(
                name.as_str(),
                "CinemagraphProbe.csproj" | "global.json" | "run-probe.ps1" | "README.md"
            )
        {
            expected.insert(format!("{relative}/{name}"));
        }
    }
    if expected.len() != index.source_files.len() || expected.len() > 64 {
        return Err("Probe source inventory differs from the current checkout.".into());
    }
    let mut seen = BTreeSet::new();
    let mut budget = 2 * 1024 * 1024;
    for source in &index.source_files {
        if !expected.contains(&source.path) || !seen.insert(&source.path) {
            return Err("Probe source inventory is incomplete, repeated or unexpected.".into());
        }
        let bytes = read_bounded(
            &safe_file(repository, &source.path)?,
            256 * 1024,
            &mut budget,
        )?;
        hash_matches(&bytes, &source.sha256)?;
    }
    Ok(())
}

fn load_case(root: &Path, case: &Case, budget: &mut usize) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut loaded = BTreeMap::new();
    for artifact in &case.artifacts {
        let bytes = read_bounded(&safe_file(root, &artifact.file)?, artifact.bytes, budget)?;
        if bytes.len() != artifact.bytes {
            return Err(format!("Indexed length does not match {}", artifact.file));
        }
        hash_matches(&bytes, &artifact.sha256)?;
        loaded.insert(artifact.file.clone(), bytes);
    }
    Ok(loaded)
}

fn required_image(
    case: &Case,
    loaded: &BTreeMap<String, Vec<u8>>,
    name: &str,
    format: &str,
) -> Result<Vec<u8>> {
    let path = format!("{}/{name}", case.fixture.id);
    let artifact = case
        .artifacts
        .iter()
        .find(|artifact| artifact.file == path)
        .ok_or_else(|| format!("Missing required indexed image {path}"))?;
    if artifact.format != format || artifact.bytes != surface_bytes(&case.fixture)? {
        return Err(format!(
            "Required image has incorrect format or dimensions: {path}"
        ));
    }
    loaded
        .get(&path)
        .cloned()
        .ok_or_else(|| format!("Image was not verified: {path}"))
}

fn swap_red_blue(bytes: &mut [u8]) {
    for pixel in bytes.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
}

struct Assets {
    current: RgbaSurface,
    snapshot: PremultipliedRgbaSurface,
}

impl FrameAssetProvider for Assets {
    fn load_rgba8(&self, id: AssetId) -> std::result::Result<RgbaSurface, AssetProviderError> {
        if id != AssetId::from_digest([1; 32]) {
            return Err("Typed PM snapshot must not pass through the straight-alpha loader".into());
        }
        Ok(self.current.clone())
    }

    fn load_premultiplied_rgba8(
        &self,
        id: AssetId,
    ) -> std::result::Result<PremultipliedRgbaSurface, AssetProviderError> {
        if id != AssetId::from_digest([2; 32]) {
            return Err("Unexpected PM snapshot identity".into());
        }
        Ok(self.snapshot.clone())
    }
}

fn difference(expected: &[u8], actual: &[u8]) -> Result<Difference> {
    if expected.len() != actual.len() || !expected.len().is_multiple_of(4) {
        return Err("Comparison requires equal packed RGBA lengths.".into());
    }
    let mut report = Difference::default();
    for (index, (left, right)) in expected
        .as_chunks::<4>()
        .0
        .iter()
        .zip(actual.as_chunks::<4>().0)
        .enumerate()
    {
        if left == right {
            continue;
        }
        report.pixels += 1;
        report.first_pixel.get_or_insert(index);
        for (left, right) in left.iter().zip(right) {
            let error = left.abs_diff(*right);
            report.channels += usize::from(error != 0);
            report.max_channel_error = report.max_channel_error.max(error);
        }
    }
    Ok(report)
}

fn compare_case(case: &Case, loaded: &BTreeMap<String, Vec<u8>>) -> Result<CaseComparison> {
    let mut current = required_image(case, loaded, "current-initial.bgra", "straight-bgra8")?;
    let mut snapshot = required_image(case, loaded, "clip-direct.pbgra", "premultiplied-bgra8")?;
    let expected = required_image(case, loaded, "direct-final.rgba", "straight-rgba8")?;
    swap_red_blue(&mut current);
    swap_red_blue(&mut snapshot);
    let size = PhysicalSize::new(case.fixture.width, case.fixture.height)
        .map_err(|error| error.to_string())?;
    let snapshot =
        PremultipliedRgbaSurface::new(size, snapshot).map_err(|error| error.to_string())?;
    let encoded = snapshot
        .encode(MAX_SURFACE_BYTES + 17)
        .map_err(|error| error.to_string())?;
    let decoded = PremultipliedRgbaSurface::decode(encoded, size, MAX_SURFACE_BYTES)
        .map_err(|error| error.to_string())?;
    if decoded != snapshot {
        return Err("Typed snapshot encode/decode changed PM pixels.".into());
    }
    let assets = Assets {
        current: RgbaSurface::new(size, current).map_err(|error| error.to_string())?,
        snapshot: decoded,
    };
    let frame = FrameClip {
        id: FrameId::from_u128(1),
        asset_id: AssetId::from_digest([1; 32]),
        duration: DurationUs::new(100_000).unwrap(),
        transform: ClipTransform::default(),
        capture_metadata: CaptureMetadata::default(),
        capture_binding: CaptureBinding::NotRecorded,
        capture_clock: None,
        effects: Vec::new(),
        render_steps: vec![
            FrameRenderStep::composite(1),
            FrameRenderStep::CinemagraphOverlay {
                snapshot_asset: AssetId::from_digest([2; 32]),
                snapshot_size: size,
            },
        ],
    };
    let renderer = CpuRenderer::with_limits(RenderLimits {
        max_surface_bytes: MAX_SURFACE_BYTES * 2,
    });
    let clip = renderer
        .render_clip(&frame, &assets, &NeverCancel)
        .map_err(|error| error.to_string())?;
    let overlays = renderer
        .render_clip_with_overlays(&frame, &[], TimeUs::ZERO, &assets, &NeverCancel)
        .map_err(|error| error.to_string())?;
    let plan = OverlayRenderPlan::for_frame(&[], frame.id, TimeUs::ZERO, &NeverCancel)
        .map_err(|error| error.to_string())?;
    let detached = renderer
        .render_clip_with_overlay_plan(&frame, &plan, &assets, &NeverCancel)
        .map_err(|error| error.to_string())?;
    if [clip.size(), overlays.size(), detached.size()]
        .iter()
        .any(|actual| *actual != size)
    {
        return Err("A render route changed the reference dimensions.".into());
    }
    Ok(CaseComparison {
        id: case.fixture.id.clone(),
        render_clip: difference(&expected, clip.pixels())?,
        direct_overlays: difference(&expected, overlays.pixels())?,
        detached_plan: difference(&expected, detached.pixels())?,
    })
}

fn compare_reference(root: &Path, repository: &Path) -> Result<Report> {
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    let repository = repository
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let mut budget = MAX_TOTAL_BYTES;
    let bytes = read_bounded(
        &safe_file(&root, "index.json")?,
        MAX_INDEX_BYTES,
        &mut budget,
    )?;
    let index = parse_index(&bytes)?;
    validate_provenance(&index)?;
    verify_source_files(&index, &repository)?;
    let mut report = Report {
        claim: "Already-clipped Windows PM snapshot storage+composition only; NOT Rust freehand/Ink geometry generation.",
        generator_commit: index.provenance["generator_commit"]
            .as_str()
            .unwrap()
            .to_owned(),
        provenance: index.provenance.clone(),
        definition_sha256: index.definition_sha256.clone(),
        index_sha256: sha256(&bytes),
        cases: index.cases.len(),
        render_routes: 3,
        differing_cases: 0,
        different_channels: 0,
        verified_index_and_artifact_bytes: 0,
        mismatches: Vec::new(),
    };
    for case in &index.cases {
        let loaded = load_case(&root, case, &mut budget)?;
        let comparison =
            compare_case(case, &loaded).map_err(|error| format!("{}: {error}", case.fixture.id))?;
        let channels = comparison.render_clip.channels
            + comparison.direct_overlays.channels
            + comparison.detached_plan.channels;
        if channels != 0 {
            report.differing_cases += 1;
            report.different_channels += channels;
            if report.mismatches.len() < 16 {
                report.mismatches.push(comparison);
            }
        }
    }
    report.verified_index_and_artifact_bytes = MAX_TOTAL_BYTES - budget;
    Ok(report)
}

#[test]
#[ignore = "requires actual Windows Cinemagraph probe artifacts in GFS_CINEMAGRAPH_PROBE_DIR"]
fn real_windows_clipped_snapshots_match_all_compositor_routes_exactly() {
    let directory = std::env::var_os("GFS_CINEMAGRAPH_PROBE_DIR")
        .expect("Set GFS_CINEMAGRAPH_PROBE_DIR to the real, indexed Windows probe artifacts; no synthetic fallback exists");
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report = compare_reference(Path::new(&directory), &repository)
        .expect("Valid external probe evidence");
    println!(
        "CINEMAGRAPH_REFERENCE {}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    assert_eq!(
        report.differing_cases, 0,
        "Every RGBA channel, including transparent RGB, must match exactly"
    );
}

// Synthetic comparator plumbing tests below are NOT Windows rendering evidence.
#[test]
fn synthetic_byte_comparison_does_not_ignore_transparent_rgb_or_small_errors() {
    let diff = difference(
        &[1, 2, 3, 0, 10, 20, 30, 255],
        &[2, 2, 3, 0, 10, 18, 30, 255],
    )
    .unwrap();
    assert_eq!(
        (diff.pixels, diff.channels, diff.max_channel_error),
        (2, 2, 2)
    );
    assert_eq!(diff.first_pixel, Some(0));
    assert!(difference(&[0; 4], &[0; 8]).is_err());
}

#[test]
fn a_copied_definition_hash_cannot_certify_a_partial_or_renamed_corpus() {
    let mut index = parse_index(&serde_json::to_vec(&synthetic_index()).unwrap()).unwrap();
    assert_eq!(index.definition_sha256, DEFINITION_SHA256);
    assert!(verify_definition_cases(&index).is_err());
    index.cases.clear();
    for geometry in [
        "empty",
        "whole",
        "rectangle",
        "rectangle_fractional",
        "ellipse",
        "ellipse_fractional",
        "ink_dot_ellipse",
        "ink_dot_rectangle",
        "ink_line_ellipse",
        "ink_line_rectangle",
    ] {
        for first in ["opaque", "zero", "partial"] {
            for current in ["opaque", "zero", "partial"] {
                index.cases.push(Case {
                    fixture: Fixture {
                        id: format!("{geometry}-{first}-{current}"),
                        width: 12,
                        height: 10,
                    },
                    artifacts: Vec::new(),
                });
            }
        }
    }
    assert!(verify_definition_cases(&index).is_ok());
    index.cases[0].fixture.width = 10;
    assert!(verify_definition_cases(&index).is_err());
    index.cases[0].fixture.width = 12;
    index.cases[0].fixture.id = "unrelated-opaque-opaque".to_owned();
    assert!(verify_definition_cases(&index).is_err());
}

#[test]
fn synthetic_paths_hashes_and_limits_are_strict() {
    for name in [
        "../escape",
        "/absolute",
        "C:/windows",
        "a\\b",
        "a//b",
        "a/./b",
        "",
    ] {
        assert!(safe_relative(name).is_err(), "{name}");
    }
    assert!(hash_matches(b"tiny", &sha256(b"tiny")).is_ok());
    assert!(hash_matches(b"Tiny", &sha256(b"tiny")).is_err());
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("small"), b"1234").unwrap();
    let path = safe_file(directory.path(), "small").unwrap();
    let mut budget = 3;
    assert!(read_bounded(&path, 4, &mut budget).is_err());
    assert_eq!(budget, 3);
    assert!(safe_file(directory.path(), ".").is_err());
    assert!(parse_index(&vec![b' '; MAX_INDEX_BYTES + 1]).is_err());
}

fn synthetic_index() -> serde_json::Value {
    let artifacts = [
        ("current-initial.bgra", "straight-bgra8"),
        ("clip-direct.pbgra", "premultiplied-bgra8"),
        ("direct-final.rgba", "straight-rgba8"),
    ]
    .map(|(name, format)| {
        serde_json::json!({
            "file": format!("synthetic/{name}"), "format": format,
            "bytes": 4, "sha256": sha256(&[0; 4]),
        })
    });
    serde_json::json!({
        "format_version": 1, "fixture_count": 1,
        "definition_sha256": DEFINITION_SHA256, "provenance": {},
        "source_files": [{"path": "synthetic.cs", "sha256": sha256(b"synthetic")}],
        "cases": [{"fixture": {"id": "synthetic", "width": 1, "height": 1}, "artifacts": artifacts}],
    })
}

#[test]
fn synthetic_index_rejects_repeated_paths_cases_dimensions_and_unindexed_inputs() {
    let original = synthetic_index();
    assert!(parse_index(&serde_json::to_vec(&original).unwrap()).is_ok());
    for mutation in [0, 1, 2, 3, 4] {
        let mut value = original.clone();
        match mutation {
            0 => value["cases"][0]["fixture"]["width"] = 33.into(),
            1 => value["fixture_count"] = 2.into(),
            2 => value["cases"][0]["artifacts"][1] = value["cases"][0]["artifacts"][0].clone(),
            3 => {
                value["cases"] =
                    serde_json::json!([value["cases"][0].clone(), value["cases"][0].clone()]);
                value["fixture_count"] = 2.into();
            }
            _ => value["cases"][0]["artifacts"][0]["file"] = "synthetic/../escape".into(),
        }
        assert!(parse_index(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value = original;
    value["cases"][0]["artifacts"][0]["file"] = "synthetic/unrelated.bgra".into();
    let index = parse_index(&serde_json::to_vec(&value).unwrap()).unwrap();
    let unindexed = BTreeMap::from([("synthetic/current-initial.bgra".to_owned(), vec![0; 4])]);
    assert!(
        required_image(
            &index.cases[0],
            &unindexed,
            "current-initial.bgra",
            "straight-bgra8"
        )
        .is_err()
    );
}

#[test]
fn synthetic_artifacts_require_exact_indexed_bytes_and_hashes() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("synthetic")).unwrap();
    let index = parse_index(&serde_json::to_vec(&synthetic_index()).unwrap()).unwrap();
    for artifact in &index.cases[0].artifacts {
        fs::write(root.path().join(&artifact.file), [0; 4]).unwrap();
    }
    // Unindexed files are neither loaded nor accepted as a missing required input.
    fs::write(
        root.path().join("unindexed-data"),
        b"not reference evidence",
    )
    .unwrap();
    let mut budget = MAX_TOTAL_BYTES;
    assert_eq!(
        load_case(root.path(), &index.cases[0], &mut budget)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(MAX_TOTAL_BYTES - budget, 12);
    fs::write(root.path().join(&index.cases[0].artifacts[0].file), [1; 4]).unwrap();
    budget = MAX_TOTAL_BYTES;
    assert!(load_case(root.path(), &index.cases[0], &mut budget).is_err());
}

#[cfg(unix)]
#[test]
fn synthetic_symlink_artifacts_are_rejected_even_when_target_is_inside_root() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("real"), b"data").unwrap();
    std::os::unix::fs::symlink("real", directory.path().join("alias")).unwrap();
    assert!(safe_file(directory.path(), "alias").is_err());
}
