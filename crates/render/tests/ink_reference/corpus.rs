use super::Result;
use serde::{Deserialize, Serialize, de::IgnoredAny};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

pub(super) const MAX_JSON: usize = 2 * 1024 * 1024;
const MAX_TOTAL: usize = 16 * 1024 * 1024;
const DEFINITION: &str = "9ed2b610efa32f966f2c1be0a3dad18a4016972907f56b4ad97f7b629b4452b4";
const UPSTREAM: &str = "a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd";

#[derive(Deserialize)]
struct Index {
    format_version: u16,
    fixture_count: usize,
    definition_sha256: String,
    provenance: serde_json::Value,
    source_files: Vec<SourceFile>,
    cases: Vec<OriginalCase>,
    additional_diagnostics: Vec<Artifact>,
}
#[derive(Deserialize)]
struct OriginalCase {
    fixture: OriginalFixture,
}
#[derive(Deserialize)]
struct OriginalFixture {
    id: String,
    width: u32,
    height: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    path: String,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    file: String,
    format: String,
    bytes: usize,
    sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Sample {
    pub x: f64,
    pub y: f64,
    pub pressure: f32,
}
impl Sample {
    pub fn bitwise_equal(self, other: Self) -> bool {
        self.x.to_bits() == other.x.to_bits()
            && self.y.to_bits() == other.y.to_bits()
            && self.pressure.to_bits() == other.pressure.to_bits()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Matrix {
    m11: f64,
    m12: f64,
    m21: f64,
    m22: f64,
    offset_x: f64,
    offset_y: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Attributes {
    pub width: f64,
    pub height: f64,
    pub tip: String,
    pub fit_to_curve: bool,
    pub ignore_pressure: bool,
    is_highlighter: bool,
    stylus_tip_transform: Matrix,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bounds {
    empty: bool,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Geometry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    fill_rule: String,
    bounds: Bounds,
    stroke_bounds: Bounds,
    figures: usize,
    segments: usize,
    control_points: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stroke {
    pub attributes: Attributes,
    pub raw_samples: Vec<Sample>,
    pub get_bezier_stylus_points: Vec<Sample>,
    pub effective_samples: Vec<Sample>,
    geometry: Geometry,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub id: String,
    pub stroke: Stroke,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    #[serde(rename = "max_raw_samples")]
    raw_samples: usize,
    #[serde(rename = "max_fitted_samples")]
    fitted_samples: usize,
    #[serde(rename = "max_path_characters")]
    path_characters: usize,
    #[serde(rename = "max_coordinate")]
    coordinate: f64,
    #[serde(rename = "max_json_bytes")]
    json_bytes: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Corpus {
    format_version: u16,
    interpretation: String,
    source_contract: String,
    limits: Limits,
    pub outline_cases: Vec<Case>,
    // These sections must be present, but this test does not compare their semantics.
    transform_cases: Vec<IgnoredAny>,
    erase_cases: Vec<IgnoredAny>,
    observed_strokes: usize,
    observed_samples_and_controls: usize,
}

pub(super) struct Loaded {
    pub corpus: Corpus,
    pub provenance: serde_json::Value,
    pub index_sha256: String,
    pub diagnostic_sha256: String,
    pub bytes_read: usize,
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn verify_hash(bytes: &[u8], expected: &str) -> Result<()> {
    if !valid_hash(expected) || sha256(bytes) != expected {
        return Err("SHA-256 does not match indexed bytes.".into());
    }
    Ok(())
}

pub(super) fn safe_relative(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 256
        || value.contains(['\\', ':'])
        || value.split('/').any(|part| matches!(part, "" | "." | ".."))
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Unsafe reference path.".into());
    }
    Ok(path)
}
fn safe_file(root: &Path, value: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in safe_relative(value)?.components() {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Err("Reference contains a symlink or non-regular component.".into());
        }
    }
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    if !path.starts_with(root) || !path.is_file() {
        return Err("Reference file escapes its root.".into());
    }
    Ok(path)
}
pub(super) fn read_bounded(path: &Path, cap: usize, budget: &mut usize) -> Result<Vec<u8>> {
    let cap = cap.min(*budget);
    let file = File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > u64::try_from(cap).unwrap() {
        return Err("Reference exceeds its file/cumulative byte budget.".into());
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(cap).unwrap() + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > cap {
        return Err("Reference grew beyond its byte budget.".into());
    }
    *budget -= bytes.len();
    Ok(bytes)
}

pub(super) fn load(directory: &Path, repository: &Path) -> Result<Loaded> {
    if fs::symlink_metadata(directory)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Reference root cannot be a symbolic link.".into());
    }
    let root = directory
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let repository = repository
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let mut budget = MAX_TOTAL;
    let index_bytes = read_bounded(&safe_file(&root, "index.json")?, MAX_JSON, &mut budget)?;
    let index: Index = serde_json::from_slice(&index_bytes).map_err(|error| error.to_string())?;
    validate_index(&index)?;
    verify_sources(&index, &repository, &mut budget)?;
    let artifact = &index.additional_diagnostics[0];
    let bytes = read_bounded(
        &safe_file(&root, &artifact.file)?,
        artifact.bytes,
        &mut budget,
    )?;
    if bytes.len() != artifact.bytes {
        return Err("Ink diagnostic length differs from index.".into());
    }
    verify_hash(&bytes, &artifact.sha256)?;
    let corpus = parse_corpus(&bytes)?;
    Ok(Loaded {
        corpus,
        provenance: index.provenance,
        index_sha256: sha256(&index_bytes),
        diagnostic_sha256: sha256(&bytes),
        bytes_read: MAX_TOTAL - budget,
    })
}

fn validate_index(index: &Index) -> Result<()> {
    if index.format_version != 1
        || index.fixture_count != 90
        || index.cases.len() != 90
        || index.definition_sha256 != DEFINITION
        || index.additional_diagnostics.len() != 1
        || !(1..=32).contains(&index.source_files.len())
    {
        return Err("Incomplete or unsupported Windows probe index.".into());
    }
    let artifact = &index.additional_diagnostics[0];
    if artifact.file != "ink-geometry.json"
        || artifact.format != "json"
        || !(1..=MAX_JSON).contains(&artifact.bytes)
        || !valid_hash(&artifact.sha256)
    {
        return Err("Missing/invalid indexed ink-geometry.json.".into());
    }
    let mut original_ids = BTreeSet::new();
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
                original_ids.insert(format!("{geometry}-{first}-{current}"));
            }
        }
    }
    for case in &index.cases {
        if case.fixture.width != 12
            || case.fixture.height != 10
            || !original_ids.remove(&case.fixture.id)
        {
            return Err("Original 90-case definition is incomplete or altered.".into());
        }
    }
    let p = &index.provenance;
    if p["upstream_commit"] != UPSTREAM
        || p["apartment_state"] != "STA"
        || !p["runtime"]
            .as_str()
            .is_some_and(|s| s.starts_with(".NET 9."))
        || !p["sdk_version"]
            .as_str()
            .is_some_and(|s| s.starts_with("9.0."))
        || !p["os_description"]
            .as_str()
            .is_some_and(|s| s.contains("Windows"))
        || !p["generator_commit"]
            .as_str()
            .is_some_and(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("Probe lacks real Windows .NET9 STA provenance.".into());
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
            return Err(format!("Missing binary provenance: {binary}"));
        }
    }
    Ok(())
}

fn verify_sources(index: &Index, repository: &Path, budget: &mut usize) -> Result<()> {
    let relative = "scripts/qa/cinemagraph_probe";
    let mut expected = BTreeSet::new();
    for entry in fs::read_dir(repository.join(relative)).map_err(|error| error.to_string())? {
        let name = entry
            .map_err(|error| error.to_string())?
            .file_name()
            .into_string()
            .map_err(|_| "Non-UTF8 source name.")?;
        if Path::new(&name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cs"))
            || matches!(
                name.as_str(),
                "CinemagraphProbe.csproj" | "global.json" | "run-probe.ps1" | "README.md"
            )
        {
            expected.insert(format!("{relative}/{name}"));
        }
        if expected.len() > 32 {
            return Err("Probe source inventory exceeds its bound.".into());
        }
    }
    if expected.len() != index.source_files.len()
        || !expected.contains(&format!("{relative}/InkGeometryProbe.cs"))
    {
        return Err("Probe source inventory does not match this checkout.".into());
    }
    let mut seen = BTreeSet::new();
    for source in &index.source_files {
        if !expected.contains(&source.path) || !seen.insert(&source.path) {
            return Err("Probe source inventory is repeated or incomplete.".into());
        }
        let bytes = read_bounded(&safe_file(repository, &source.path)?, 256 * 1024, budget)?;
        verify_hash(&bytes, &source.sha256)?;
    }
    Ok(())
}

pub(super) fn expected_ids() -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for name in [
        "curve-five",
        "cusp-six",
        "loop-seven",
        "duplicate-seven",
        "variable-pressure-six",
        "parabola-three",
    ] {
        for tip in ["Ellipse", "Rectangle"] {
            for fitting in ["fit", "raw"] {
                ids.insert(format!("{name}-{tip}-{fitting}"));
            }
        }
    }
    for tip in ["Ellipse", "Rectangle"] {
        for pressure in ["0", "0.5", "1"] {
            for ignore in ["False", "True"] {
                ids.insert(format!("single-{tip}-p{pressure}-ignore{ignore}"));
            }
        }
        ids.insert(format!("single-{tip}-implicit-pressure"));
    }
    ids
}

pub(super) fn parse_corpus(bytes: &[u8]) -> Result<Corpus> {
    if bytes.len() > MAX_JSON {
        return Err("Ink diagnostic exceeds 2 MiB.".into());
    }
    let corpus: Corpus = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if corpus.format_version != 1
        || corpus.outline_cases.len() != 38
        || corpus.transform_cases.len() != 8
        || corpus.erase_cases.len() != 8
        || !(38..=128).contains(&corpus.observed_strokes)
        || corpus.observed_samples_and_controls > 65536
        || corpus.interpretation.is_empty()
        || corpus.interpretation.len() > 4096
        || corpus.source_contract.is_empty()
        || corpus.source_contract.len() > 4096
    {
        return Err("Incomplete or unsupported Ink diagnostic envelope.".into());
    }
    let limits = &corpus.limits;
    if limits.raw_samples != 64
        || limits.fitted_samples != 4096
        || limits.path_characters != 65536
        || limits.coordinate.to_bits() != 64.0_f64.to_bits()
        || limits.json_bytes != MAX_JSON
    {
        return Err("Ink diagnostic limit contract changed.".into());
    }
    let mut ids = expected_ids();
    let mut values = 0_usize;
    for case in &corpus.outline_cases {
        if !ids.remove(&case.id) {
            return Err("Ink case IDs are missing, repeated or unexpected.".into());
        }
        validate_stroke(&case.stroke)?;
        validate_case_attributes(case)?;
        values = values
            .checked_add(
                case.stroke.raw_samples.len()
                    + case.stroke.get_bezier_stylus_points.len()
                    + case.stroke.geometry.control_points,
            )
            .filter(|count| *count <= 65536)
            .ok_or("Ink sample/control budget exceeded.")?;
    }
    if !ids.is_empty() || values > corpus.observed_samples_and_controls {
        return Err("Partial Ink cases or inconsistent observed counters.".into());
    }
    Ok(corpus)
}

fn validate_stroke(stroke: &Stroke) -> Result<()> {
    let a = &stroke.attributes;
    let matrix = &a.stylus_tip_transform;
    if !matches!(a.tip.as_str(), "Ellipse" | "Rectangle")
        || a.width.to_bits() != 4.25_f64.to_bits()
        || a.height.to_bits() != 3.25_f64.to_bits()
        || a.is_highlighter
    {
        return Err("Unexpected Ink attributes.".into());
    }
    if [
        matrix.m11,
        matrix.m12,
        matrix.m21,
        matrix.m22,
        matrix.offset_x,
        matrix.offset_y,
    ]
    .map(f64::to_bits)
        != [1.0_f64, 0.0, 0.0, 1.0, 0.0, 0.0].map(f64::to_bits)
    {
        return Err("This centerline contract requires identity tip transforms.".into());
    }
    for (samples, maximum) in [
        (&stroke.raw_samples, 64),
        (&stroke.get_bezier_stylus_points, 4096),
        (&stroke.effective_samples, 4096),
    ] {
        if !(1..=maximum).contains(&samples.len())
            || samples.iter().any(|s| {
                !s.x.is_finite()
                    || !s.y.is_finite()
                    || s.x.abs() > 64.0
                    || s.y.abs() > 64.0
                    || !s.pressure.is_finite()
                    || !(0.0..=1.0).contains(&s.pressure)
            })
        {
            return Err("Non-finite, unbounded or empty Ink samples.".into());
        }
    }
    let effective = if a.fit_to_curve {
        &stroke.get_bezier_stylus_points
    } else {
        &stroke.raw_samples
    };
    if effective.len() != stroke.effective_samples.len()
        || !effective
            .iter()
            .zip(&stroke.effective_samples)
            .all(|(a, b)| a.bitwise_equal(*b))
    {
        return Err("Effective samples disagree with the FitToCurve flag.".into());
    }
    let g = &stroke.geometry;
    if g.kind.is_empty()
        || g.kind.len() > 128
        || g.path.is_empty()
        || g.path.len() > 65536
        || g.fill_rule != "Nonzero"
        || g.figures > 4096
        || g.segments > 32768
        || g.control_points > 32768
    {
        return Err("Invalid/budget-exceeding geometry metadata.".into());
    }
    for b in [&g.bounds, &g.stroke_bounds] {
        let fields = [b.x, b.y, b.width, b.height];
        if fields.iter().any(|v| !v.is_finite() || v.abs() > 64.0)
            || b.width < 0.0
            || b.height < 0.0
            || b.empty && fields.iter().any(|v| v.abs() > 0.0)
        {
            return Err("Invalid geometry bounds.".into());
        }
    }
    Ok(())
}

fn validate_case_attributes(case: &Case) -> Result<()> {
    let a = &case.stroke.attributes;
    let tip = if case.id.contains("-Rectangle-") {
        "Rectangle"
    } else {
        "Ellipse"
    };
    if a.tip != tip
        || a.fit_to_curve != case.id.ends_with("-fit")
        || a.ignore_pressure != case.id.ends_with("-ignoreTrue")
    {
        return Err("Case identity and authored attributes disagree.".into());
    }
    if case.id.starts_with("single-") {
        let expected_pressure = if case.id.contains("-p0-ignore") {
            0.0_f32
        } else if case.id.contains("-p1-ignore") {
            1.0_f32
        } else {
            0.5_f32
        };
        let raw = &case.stroke.raw_samples;
        if raw.len() != 1
            || !raw[0].bitwise_equal(Sample {
                x: 8.125,
                y: 6.375,
                pressure: expected_pressure,
            })
        {
            return Err("Single-point input differs from the fixed probe definition.".into());
        }
    } else {
        let count = if case.id.starts_with("curve-five-") {
            5
        } else if case.id.starts_with("cusp-six-") || case.id.starts_with("variable-pressure-six-")
        {
            6
        } else if case.id.starts_with("parabola-three-") {
            3
        } else {
            7
        };
        if case.stroke.raw_samples.len() != count {
            return Err("Curve raw sample count differs from its fixed case.".into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod source_tests;
