//! Explicit candidate-vector gate, never the persisted V1/full-project gate.

use std::time::{Duration, Instant};

use gif_from_screen_render::{CancellationToken, render_wpf_vector_shapes};

use super::*;

const VECTOR_IDS: [&str; 14] = [
    "vector-square-fill",
    "vector-rounded-fraction",
    "vector-rounded-axis-clamp",
    "vector-triangle-rotated",
    "vector-block-arrow",
    "vector-ellipse-fraction",
    "coverage-triangle",
    "coverage-fractional-triangle",
    "coverage-ellipse",
    "coverage-triangle-stroke",
    "layout-thin-triangle",
    "layout-inverted-triangle",
    "layout-inverted-arrow",
    "layout-narrow-arrow",
];
const MAX_OBJECTS: usize = VECTOR_IDS.len() * MAX_VECTOR_SHAPES;
const MAX_REPORT_BYTES: usize = 512 * 1024;
const DEADLINE_SECONDS: u64 = 60;
const SCOPE: &str = "explicit candidate WPF layout/brush/surface and PM canvas grouping; NOT saved vector V1 or full-project parity";

#[derive(Serialize)]
struct CandidateReport {
    scope: &'static str,
    maximum_objects: usize,
    deadline_seconds: u64,
    surface_reserved_work_per_batch: u64,
    reference_only_stages_validated: usize,
    vectors_compared: usize,
    comparison: ComparisonReport,
}

#[test]
#[ignore = "requires all real Windows WPF artifacts; explicit candidate, not saved V1/project parity"]
fn compare_windows_wpf_vector_candidate() -> Result<()> {
    let reference = std::env::var_os("GFS_WPF_REFERENCE_DIR")
        .filter(|value| !value.is_empty())
        .ok_or("GFS_WPF_REFERENCE_DIR is required")?;
    let output = std::env::var_os("GFS_WPF_COMPARE_DIR")
        .filter(|value| !value.is_empty())
        .ok_or("GFS_WPF_COMPARE_DIR is required")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut input_budget = MAX_SPEC_BYTES;
    let bytes = read_bounded(
        &repository.join("scripts/qa/wpf_reference/fixtures.json"),
        MAX_SPEC_BYTES,
        &mut input_budget,
    )?;
    compare_candidate(
        &bytes,
        Path::new(&reference),
        Path::new(&output),
        &repository,
    )
}

fn compare_candidate(
    bytes: &[u8],
    reference: &Path,
    output: &Path,
    repository: &Path,
) -> Result<()> {
    let root = independent_output(reference, output)?;
    let output = claim_output(output)?;
    let mut report = CandidateReport {
        scope: SCOPE,
        maximum_objects: MAX_OBJECTS,
        deadline_seconds: DEADLINE_SECONDS,
        surface_reserved_work_per_batch: 100_000_000,
        reference_only_stages_validated: 0,
        vectors_compared: 0,
        comparison: ComparisonReport {
            format_version: 1,
            definition_sha256: sha256(bytes),
            provenance: serde_json::Value::Null,
            generator_files: Vec::new(),
            strict_rgba: true,
            stages: Vec::new(),
            errors: Vec::new(),
            passed: false,
        },
    };
    if let Err(error) = run_comparison(bytes, &root, &output, repository, &mut report) {
        report.comparison.errors.push(error);
    }
    report.comparison.passed = report.comparison.errors.is_empty()
        && report.vectors_compared == VECTOR_IDS.len()
        && report.comparison.stages.len() == FIXTURE_COUNT + VECTOR_IDS.len()
        && report.comparison.stages.iter().all(|stage| {
            stage.error.is_none()
                && stage
                    .difference
                    .as_ref()
                    .is_some_and(|diff| diff.mismatch_pixels == 0)
        });
    let encoded = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_REPORT_BYTES {
        return Err("Candidate report exceeds 512 KiB".into());
    }
    let report_path = output.join("vector-candidate.json");
    write_new(&report_path, &encoded)?;
    if !report.comparison.passed {
        return Err(format!(
            "Strict candidate vector comparison failed; inspect {}. No reference or V1 gate was changed.",
            report_path.display()
        ));
    }
    Ok(())
}

fn run_comparison(
    bytes: &[u8],
    root: &Path,
    output: &Path,
    repository: &Path,
    report: &mut CandidateReport,
) -> Result<()> {
    let definition = parse_definition(bytes)?;
    validate_selection(&definition)?;
    let mut budget = IoBudget {
        input: MAX_TOTAL_BYTES
            .checked_sub(bytes.len())
            .ok_or("Definition budget exceeded")?,
        output: MAX_TOTAL_BYTES - MAX_REPORT_BYTES,
    };
    let index_bytes = read_bounded(
        &safe_file(root, "index.json")?,
        MAX_INDEX_BYTES,
        &mut budget.input,
    )?;
    let index: ReferenceIndex =
        serde_json::from_slice(&index_bytes).map_err(|error| error.to_string())?;
    report.comparison.provenance = index.provenance.clone();
    report
        .comparison
        .generator_files
        .clone_from(&index.generator_files);
    validate_index(&index, &definition, bytes)?;
    validate_generator_files(&index, repository, &mut budget.input)?;
    let deadline = Deadline(Instant::now());
    let mut objects = MAX_OBJECTS;
    for fixture in &definition.fixtures {
        let reference = index
            .fixtures
            .iter()
            .find(|item| item.id == fixture.id)
            .ok_or("Missing validated fixture")?;
        // Compare every definition input, including the five non-vector cases.
        // Actual rendering below reads only Fixture, never these reference bytes.
        report.comparison.stages.push(compare_surface(
            root,
            output,
            &fixture.id,
            0,
            &reference.input,
            fixture.source.surface(),
            &mut budget,
        ));
        if matches!(
            fixture.operations.as_slice(),
            [Operation::VectorShapes { .. }]
        ) {
            let actual = candidate_surface(fixture, &deadline, &mut objects);
            report.comparison.stages.push(compare_surface(
                root,
                output,
                &fixture.id,
                1,
                &reference.stages[0],
                actual,
                &mut budget,
            ));
            report.vectors_compared += 1;
        } else {
            // These stages are still fully loaded/hash/DPI/path checked. They
            // are not rendered here or counted as candidate/full-project parity.
            for (position, image) in reference.stages.iter().enumerate() {
                match load_reference(root, image, &mut budget.input) {
                    Ok(_) => report.reference_only_stages_validated += 1,
                    Err(error) => report.comparison.errors.push(format!(
                        "{} stage {} artifact: {error}",
                        fixture.id,
                        position + 1
                    )),
                }
            }
        }
    }
    deadline.check()
}

fn validate_selection(definition: &Definition) -> Result<()> {
    if definition.fixtures.len() != FIXTURE_COUNT {
        return Err("Candidate requires the complete 19-fixture definition".into());
    }
    let mut ids = BTreeSet::new();
    for fixture in &definition.fixtures {
        if fixture
            .operations
            .iter()
            .any(|op| matches!(op, Operation::VectorShapes { .. }))
        {
            let [Operation::VectorShapes { shapes }] = fixture.operations.as_slice() else {
                return Err(
                    "Candidate only measures complete, single VectorShapes operations".into(),
                );
            };
            validate_vector_shapes(shapes)?;
            ids.insert(fixture.id.as_str());
        }
    }
    if ids != VECTOR_IDS.into_iter().collect() {
        return Err("Candidate vector fixture ID set is incomplete or changed".into());
    }
    Ok(())
}

struct IoBudget {
    input: usize,
    output: usize,
}

fn compare_surface(
    root: &Path,
    output: &Path,
    fixture: &str,
    stage: usize,
    reference: &ReferenceImage,
    actual: Result<RgbaSurface>,
    budget: &mut IoBudget,
) -> StageReport {
    let mut report = StageReport {
        fixture_id: fixture.into(),
        stage,
        reference: reference.clone(),
        actual_sha256: None,
        difference: None,
        error: None,
    };
    report.error = (|| {
        let expected = load_reference(root, reference, &mut budget.input)?;
        let actual = actual?;
        report.actual_sha256 = Some(sha256(actual.pixels()));
        let (diff, pixels) = difference(&expected, &actual);
        let mismatched = diff.mismatch_pixels != 0;
        report.difference = Some(diff);
        if mismatched {
            // Reserve the maximum encoded sizes before writing, not after an
            // unbounded sequence of failed fixture images. Report has its own
            // reservation; a depleted diagnostic budget is an explicit failure.
            budget.output = budget
                .output
                .checked_sub(2 * MAX_PNG_BYTES)
                .ok_or("Candidate diagnostic output exceeds its 16 MiB budget")?;
            write_png(
                &output.join(format!("{fixture}-stage-{stage:02}-actual.png")),
                &actual,
            )?;
            write_png(
                &output.join(format!("{fixture}-stage-{stage:02}-diff.png")),
                &pixels,
            )?;
        }
        Ok::<_, String>(())
    })()
    .err();
    report
}

struct Deadline(Instant);
impl Deadline {
    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err("Candidate comparison exceeded its 60-second render deadline".into())
        } else {
            Ok(())
        }
    }
}
impl CancellationToken for Deadline {
    fn is_cancelled(&self) -> bool {
        self.0.elapsed() >= Duration::from_secs(DEADLINE_SECONDS)
    }
}

fn candidate_surface(
    fixture: &Fixture,
    deadline: &Deadline,
    objects: &mut usize,
) -> Result<RgbaSurface> {
    let [Operation::VectorShapes { shapes }] = fixture.operations.as_slice() else {
        return Err("Candidate requires one complete VectorShapes operation".into());
    };
    validate_vector_shapes(shapes)?;
    *objects = objects
        .checked_sub(shapes.len())
        .ok_or("Candidate object budget exceeded")?;
    let byte_len = surface_bytes(fixture.source.width, fixture.source.height)?;
    if fixture.source.pixels.len() != byte_len / 4 {
        return Err("Candidate input dimensions mismatch".into());
    }
    let size = PhysicalSize::new(fixture.source.width, fixture.source.height)
        .map_err(|error| error.to_string())?;
    let mut canvas = Vec::new();
    canvas
        .try_reserve_exact(byte_len)
        .map_err(|error| error.to_string())?;
    canvas.resize(byte_len, 0_u8);
    let max_surface_bytes = MAX_RENDER_WORKING_BYTES
        .checked_sub(canvas.capacity())
        .ok_or("Candidate canvas memory budget exceeded")?;
    deadline.check()?;
    let surface =
        render_wpf_vector_shapes(shapes, size, RenderLimits { max_surface_bytes }, deadline)
            .map_err(|error| error.to_string())?;
    // The whole ordered shape canvas is placed over the input exactly once,
    // then the PNG/WIC reciprocal rule is applied once. Never feed expected
    // pixels back into this canvas, and never round-trip each shape separately.
    for (position, ((pixel, group), input)) in canvas
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(surface.pixels().as_chunks::<4>().0)
        .zip(&fixture.source.pixels)
        .enumerate()
    {
        if position.is_multiple_of(1024) {
            deadline.check()?;
        }
        *pixel = unpremultiply(over(*group, premultiply(*input)));
    }
    deadline.check()?;
    RgbaSurface::new(size, canvas).map_err(|error| error.to_string())
}

// Actual-side arithmetic, intentionally equivalent to the crate-private
// wpf_pixels helpers. These functions are not Windows-generated expectations.
fn mul_byte(channel: u8, alpha: u8) -> u8 {
    let product = u32::from(channel) * u32::from(alpha) + 128;
    u8::try_from((product + (product >> 8)) >> 8).unwrap()
}
fn premultiply(pixel: [u8; 4]) -> [u8; 4] {
    [
        mul_byte(pixel[0], pixel[3]),
        mul_byte(pixel[1], pixel[3]),
        mul_byte(pixel[2], pixel[3]),
        pixel[3],
    ]
}
fn over(source: [u8; 4], destination: [u8; 4]) -> [u8; 4] {
    std::array::from_fn(|channel| {
        source[channel].saturating_add(mul_byte(destination[channel], 255 - source[3]))
    })
}
fn unpremultiply(pixel: [u8; 4]) -> [u8; 4] {
    if pixel[3] == 0 {
        return [0; 4];
    }
    if pixel[3] == 255 {
        return pixel;
    }
    let reciprocal = (255 << 16) / u32::from(pixel[3]);
    let channel =
        |value: u8| u8::try_from(((u32::from(value) * reciprocal) >> 16).min(255)).unwrap();
    [
        channel(pixel[0]),
        channel(pixel[1]),
        channel(pixel[2]),
        pixel[3],
    ]
}

#[test]
fn candidate_selection_requires_all_fourteen_full_vector_cases() {
    let definition = parse_definition(include_bytes!(
        "../../../../scripts/qa/wpf_reference/fixtures.json"
    ))
    .unwrap();
    validate_selection(&definition).unwrap();
    let mut incomplete = definition.clone();
    incomplete.fixtures.pop();
    assert!(validate_selection(&incomplete).is_err());
    let mut mixed = definition;
    mixed.fixtures[5].operations.push(Operation::ImageBorder {
        style: ImageBorderStyle::default(),
    });
    assert!(validate_selection(&mixed).is_err());
}

#[test]
fn candidate_actual_math_keeps_pm_group_order_and_one_codec_boundary() {
    assert_eq!(unpremultiply([0, 0, 253, 253]), [0, 0, 254, 253]);
    assert_eq!(unpremultiply([0, 7, 9, 0]), [0; 4]);
    let red = premultiply([255, 0, 0, 128]);
    let blue = premultiply([0, 0, 255, 128]);
    assert_eq!(over(blue, red), [64, 0, 128, 192]);
    assert_ne!(over(blue, red), over(red, blue));
}

#[test]
fn candidate_shape_count_and_deadline_are_not_silent_skips() {
    let definition = parse_definition(include_bytes!(
        "../../../../scripts/qa/wpf_reference/fixtures.json"
    ))
    .unwrap();
    let fixture = &definition.fixtures[5];
    assert!(candidate_surface(fixture, &Deadline(Instant::now()), &mut 0).is_err());
    let expired = Deadline(
        Instant::now()
            .checked_sub(Duration::from_secs(DEADLINE_SECONDS))
            .unwrap(),
    );
    let mut objects = MAX_OBJECTS;
    assert!(candidate_surface(fixture, &expired, &mut objects).is_err());
    assert!(
        candidate_surface(
            &definition.fixtures[0],
            &Deadline(Instant::now()),
            &mut objects
        )
        .is_err()
    );
}

fn synthetic_image(root: &Path, id: &str, image: &RgbaSurface) -> ReferenceImage {
    // Comparator transport mechanics only. This is not a Windows golden.
    fs::create_dir(root.join(id)).unwrap();
    let rgba_file = format!("{id}/stage-01.rgba");
    let png_file = format!("{id}/stage-01.png");
    let pm_file = format!("{id}/stage-01.pbgra");
    write_new(&root.join(&rgba_file), image.pixels()).unwrap();
    write_png(&root.join(&png_file), image).unwrap();
    let pm: Vec<_> = image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| {
            let value = premultiply(*pixel);
            [value[2], value[1], value[0], value[3]]
        })
        .collect();
    write_new(&root.join(&pm_file), &pm).unwrap();
    ReferenceImage {
        width: image.size().width.get(),
        height: image.size().height.get(),
        decoded_dpi_x: 96.0,
        decoded_dpi_y: 96.0,
        working_dpi_x: 96.0,
        working_dpi_y: 96.0,
        rgba_sha256: sha256(image.pixels()),
        png_sha256: sha256(&fs::read(root.join(&png_file)).unwrap()),
        premultiplied_sha256: Some(sha256(&pm)),
        rgba_file,
        png_file,
        premultiplied_file: Some(pm_file),
    }
}

#[test]
fn candidate_mismatch_keeps_actual_diff_and_rejects_tampered_reference() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("reference");
    let output = directory.path().join("comparison");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&output).unwrap();
    let size = PhysicalSize::new(1, 1).unwrap();
    let expected = RgbaSurface::new(size, vec![1, 2, 3, 255]).unwrap();
    let reference = synthetic_image(&root, "synthetic-not-wpf", &expected);
    let original = fs::read(root.join(&reference.rgba_file)).unwrap();
    let mut budget = IoBudget {
        input: MAX_TOTAL_BYTES,
        output: MAX_TOTAL_BYTES - MAX_REPORT_BYTES,
    };
    let report = compare_surface(
        &root,
        &output,
        "synthetic-not-wpf",
        1,
        &reference,
        RgbaSurface::new(size, vec![2, 2, 3, 255]).map_err(|e| e.to_string()),
        &mut budget,
    );
    assert!(report.error.is_none());
    assert_eq!(report.difference.unwrap().mismatch_pixels, 1);
    assert!(
        output
            .join("synthetic-not-wpf-stage-01-actual.png")
            .is_file()
    );
    assert!(output.join("synthetic-not-wpf-stage-01-diff.png").is_file());
    assert_eq!(fs::read(root.join(&reference.rgba_file)).unwrap(), original);
    fs::write(root.join(&reference.rgba_file), [9, 2, 3, 255]).unwrap();
    let report = compare_surface(
        &root,
        &output,
        "synthetic-not-wpf",
        1,
        &reference,
        Ok(expected),
        &mut budget,
    );
    assert!(report.error.unwrap().contains("SHA-256"));
    assert!(report.difference.is_none());
}

#[test]
fn candidate_partial_reference_fails_with_explicit_non_v1_report() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("reference");
    let output = directory.path().join("comparison");
    fs::create_dir(&root).unwrap();
    let bytes = include_bytes!("../../../../scripts/qa/wpf_reference/fixtures.json");
    let index = serde_json::json!({
        "format_version":1,"definition_sha256":sha256(bytes),
        "provenance":{"runtime":"synthetic","sdk_version":"synthetic","os_description":"not Windows evidence","os_version":"test"},
        "generator_files":[],"fixtures":[],
    });
    write_new(
        &root.join("index.json"),
        &serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    let original = fs::read(root.join("index.json")).unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(compare_candidate(bytes, &root, &output, &repository).is_err());
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("vector-candidate.json")).unwrap()).unwrap();
    assert_eq!(report["scope"], SCOPE);
    assert_eq!(report["comparison"]["passed"], false);
    assert_eq!(report["vectors_compared"], 0);
    assert!(
        !report["comparison"]["errors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read(root.join("index.json")).unwrap(), original);
}

#[test]
fn candidate_bad_png_raw_pair_and_diagnostic_budget_never_become_passes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("reference");
    let output = directory.path().join("comparison");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&output).unwrap();
    let size = PhysicalSize::new(1, 1).unwrap();
    let expected = RgbaSurface::new(size, vec![1, 2, 3, 255]).unwrap();
    let mut reference = synthetic_image(&root, "synthetic-not-wpf", &expected);
    let mut budget = IoBudget {
        input: MAX_TOTAL_BYTES,
        output: 0,
    };
    let report = compare_surface(
        &root,
        &output,
        "synthetic-not-wpf",
        1,
        &reference,
        RgbaSurface::new(size, vec![2, 2, 3, 255]).map_err(|e| e.to_string()),
        &mut budget,
    );
    assert!(report.error.unwrap().contains("16 MiB"));
    assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    // Both artifacts' own hashes are valid, but their RGBA pixels disagree.
    let changed = [9, 2, 3, 255];
    fs::write(root.join(&reference.rgba_file), changed).unwrap();
    reference.rgba_sha256 = sha256(&changed);
    let report = compare_surface(
        &root,
        &output,
        "synthetic-not-wpf",
        1,
        &reference,
        Ok(expected),
        &mut budget,
    );
    assert!(report.error.is_some());
    assert!(report.difference.is_none());
}
