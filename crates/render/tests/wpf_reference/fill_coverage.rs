//! A separate diagnostic gate for layout + filled contours, never project parity.

use gif_from_screen_domain::Rgba;
use gif_from_screen_render::{InkLimits, rasterize_ink_paths, wpf_vector_shape_geometry};

use super::*;

const PROBES: [&str; 3] = [
    "coverage-triangle",
    "coverage-fractional-triangle",
    "coverage-ellipse",
];

#[test]
#[ignore = "requires real Windows WPF fill-coverage probe artifacts; not full project parity"]
fn compare_windows_wpf_fill_coverage() -> Result<()> {
    let reference = std::env::var_os("GFS_WPF_REFERENCE_DIR")
        .filter(|v| !v.is_empty())
        .ok_or("GFS_WPF_REFERENCE_DIR is required")?;
    let output = std::env::var_os("GFS_WPF_COMPARE_DIR")
        .filter(|v| !v.is_empty())
        .ok_or("GFS_WPF_COMPARE_DIR is required")?;
    let root = independent_output(Path::new(&reference), Path::new(&output))?;
    let output = claim_output(Path::new(&output))?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut budget = MAX_TOTAL_BYTES;
    let bytes = read_bounded(
        &repository.join("scripts/qa/wpf_reference/fixtures.json"),
        MAX_SPEC_BYTES,
        &mut budget,
    )?;
    let definition = parse_definition(&bytes)?;
    if definition.fixtures.len() != FIXTURE_COUNT {
        return Err("The complete reference definition is required, not a selected rewrite".into());
    }
    let index_bytes = read_bounded(
        &safe_file(&root, "index.json")?,
        MAX_INDEX_BYTES,
        &mut budget,
    )?;
    let index: ReferenceIndex = serde_json::from_slice(&index_bytes).map_err(|e| e.to_string())?;
    validate_index(&index, &definition, &bytes)?;
    validate_generator_files(&index, &repository, &mut budget)?;
    let mut reports = Vec::new();
    for id in PROBES {
        let fixture = definition
            .fixtures
            .iter()
            .find(|f| f.id == id)
            .ok_or("Missing fill probe")?;
        let reference = index
            .fixtures
            .iter()
            .find(|f| f.id == id)
            .ok_or("Missing fill artifact")?;
        let input = load_reference(&root, &reference.input, &mut budget)?;
        if input != fixture.source.surface()? || input.pixels().iter().any(|v| *v != 0) {
            return Err("Fill probes require matching transparent input bytes".into());
        }
        let expected = reference
            .stages
            .first()
            .ok_or("Missing fill output stage")?;
        load_reference(&root, expected, &mut budget)?; // validate all PNG/RGBA/PM digests and DPI
        let pm_path = expected
            .premultiplied_file
            .as_ref()
            .ok_or("Missing real pre-PNG bytes")?;
        let expected = RgbaSurface::new(
            input.size(),
            read_bounded(&safe_file(&root, pm_path)?, MAX_SURFACE_BYTES, &mut budget)?,
        )
        .map_err(|e| e.to_string())?;
        let actual = fill_probe(fixture)?;
        let (diff, pixels) = difference(&expected, &actual);
        if diff.mismatch_pixels != 0 {
            write_png(&output.join(format!("{id}-actual-pm.png")), &actual)?;
            write_png(&output.join(format!("{id}-diff.png")), &pixels)?;
        }
        reports.push(serde_json::json!({"fixture":id,"difference":diff}));
    }
    let passed = reports
        .iter()
        .all(|report| report["difference"]["mismatch_pixels"] == 0);
    let report = serde_json::json!({
        "scope":"layout and opaque filled-contour coverage only; NOT stroke or project parity",
        "definition_sha256":sha256(&bytes),"provenance":index.provenance,
        "generator_files":index.generator_files,"probes":reports,"passed":passed,
    });
    write_new(
        &output.join("fill-coverage.json"),
        &serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )?;
    if !passed {
        return Err(format!(
            "Fill coverage differs; inspect {}",
            output.display()
        ));
    }
    Ok(())
}

fn fill_probe(fixture: &Fixture) -> Result<RgbaSurface> {
    let [Operation::VectorShapes { shapes }] = fixture.operations.as_slice() else {
        return Err("Fill probe requires one shape operation".into());
    };
    let [shape] = shapes.as_slice() else {
        return Err("Fill probe requires exactly one shape".into());
    };
    if shape.stroke_width_hundredths != 0
        || shape.fill
            != Some(Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            })
    {
        return Err("Fill probe requires opaque white fill and no stroke".into());
    }
    let geometry = wpf_vector_shape_geometry(shape).map_err(|e| e.to_string())?;
    let size = PhysicalSize::new(fixture.source.width, fixture.source.height)
        .map_err(|e| e.to_string())?;
    let coverage = rasterize_ink_paths(
        &[geometry.outline().clone()],
        size,
        false,
        &InkLimits {
            max_bytes: MAX_RENDER_WORKING_BYTES,
            ..InkLimits::default()
        },
        &NeverCancel,
    )
    .map_err(|e| e.to_string())?;
    // Compare original PM coverage bytes. White makes BGRA/RGBA identical.
    // WPF scales by count/64 directly, not by a rounded intermediate A8 alpha.
    let bytes = coverage
        .into_iter()
        .flat_map(|count| {
            let alpha = u8::try_from((255 * u16::from(count) * 4 + 128) >> 8).unwrap();
            [alpha; 4]
        })
        .collect();
    RgbaSurface::new(size, bytes).map_err(|e| e.to_string())
}

#[test]
fn probe_rejects_strokes_and_does_not_alias_the_persisted_vector_one_route() {
    let definition = parse_definition(include_bytes!(
        "../../../../scripts/qa/wpf_reference/fixtures.json"
    ))
    .unwrap();
    for id in PROBES {
        let fixture = definition.fixtures.iter().find(|f| f.id == id).unwrap();
        assert_eq!(
            fill_probe(fixture).unwrap().size(),
            fixture.source.surface().unwrap().size()
        );
    }
    let stroked = definition
        .fixtures
        .iter()
        .find(|f| f.id == "coverage-triangle-stroke")
        .unwrap();
    assert!(fill_probe(stroked).is_err());
}
