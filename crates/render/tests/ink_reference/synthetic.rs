//! Synthetic comparator plumbing only; none of these samples is a WPF golden.

use super::{comparison, corpus};
use gif_from_screen_render::{InkError, InkPoint, InkSample};
use serde_json::{Value, json};

fn envelope() -> Value {
    let bounds = json!({"empty":true,"x":0.0,"y":0.0,"width":0.0,"height":0.0});
    let cases=corpus::expected_ids().into_iter().map(|id| {
        let single=id.starts_with("single-");
        let count=if single{1}else if id.starts_with("curve-five-"){5}else if id.starts_with("cusp-six-")||id.starts_with("variable-pressure-six-"){6}else if id.starts_with("parabola-three-"){3}else{7};
        let pressure=if id.contains("-p0-ignore"){0.0}else if id.contains("-p1-ignore"){1.0}else{0.5};
        let sample=json!({"x":if single{8.125}else{0.0},"y":if single{6.375}else{0.0},"pressure":pressure});
        let samples=vec![sample;count];
        json!({"id":id,"stroke":{
        "attributes":{"width":4.25,"height":3.25,"tip":if id.contains("-Rectangle-"){"Rectangle"}else{"Ellipse"},"fit_to_curve":id.ends_with("-fit"),"ignore_pressure":id.ends_with("-ignoreTrue"),"is_highlighter":false,
            "stylus_tip_transform":{"m11":1.0,"m12":0.0,"m21":0.0,"m22":1.0,"offset_x":0.0,"offset_y":0.0}},
        "raw_samples":samples.clone(),"get_bezier_stylus_points":samples.clone(),"effective_samples":samples,
        "geometry":{"type":"SyntheticOnly","path":"M0,0","fill_rule":"Nonzero","bounds":bounds.clone(),"stroke_bounds":bounds.clone(),"figures":1,"segments":0,"control_points":1}
    }})}).collect::<Vec<_>>();
    json!({"format_version":1,"interpretation":"Synthetic parser test, NOT Windows evidence","source_contract":"synthetic",
        "limits":{"max_raw_samples":64,"max_fitted_samples":4096,"max_path_characters":65536,"max_coordinate":64.0,"max_json_bytes":corpus::MAX_JSON},
        "outline_cases":cases,"transform_cases":vec![json!({});8],"erase_cases":vec![json!({});8],"observed_strokes":74,"observed_samples_and_controls":4096})
}

fn parse(value: &Value) -> super::Result<corpus::Corpus> {
    corpus::parse_corpus(&serde_json::to_vec(value).unwrap())
}

#[test]
fn synthetic_envelope_rejects_partial_duplicate_unknown_ids_and_versions() {
    let valid = envelope();
    parse(&valid).unwrap();
    let mut value = valid.clone();
    value["outline_cases"].as_array_mut().unwrap().pop();
    assert!(parse(&value).is_err());
    value = valid.clone();
    value["outline_cases"][1]["id"] = value["outline_cases"][0]["id"].clone();
    assert!(parse(&value).is_err());
    value = valid.clone();
    value["outline_cases"][0]["id"] = json!("not-a-reference-case");
    assert!(parse(&value).is_err());
    value = valid.clone();
    value["format_version"] = json!(2);
    assert!(parse(&value).is_err());
    value = valid.clone();
    value.as_object_mut().unwrap().remove("transform_cases");
    assert!(parse(&value).is_err());
    value = valid;
    value["observed_samples_and_controls"] = json!(0);
    assert!(parse(&value).is_err());
}

#[test]
fn synthetic_envelope_rejects_wrong_types_unbounded_samples_and_inconsistent_effective_path() {
    let valid = envelope();
    for bad in [json!(null), json!("NaN"), json!(-0.1), json!(1.1)] {
        let mut value = valid.clone();
        value["outline_cases"][0]["stroke"]["raw_samples"][0]["pressure"] = bad;
        assert!(parse(&value).is_err());
    }
    let mut value = valid.clone();
    value["outline_cases"][0]["stroke"]["raw_samples"][0]["x"] = json!(65.0);
    assert!(parse(&value).is_err());
    value = valid.clone();
    value["outline_cases"][0]["stroke"]["effective_samples"][0]["x"] = json!(0.25);
    assert!(parse(&value).is_err());
    value = valid.clone();
    value["outline_cases"][0]["stroke"]["raw_samples"] = json!([]);
    assert!(parse(&value).is_err());
    value = valid.clone();
    let sample = value["outline_cases"][0]["stroke"]["raw_samples"][0].clone();
    value["outline_cases"][0]["stroke"]["raw_samples"] = json!(vec![sample; 65]);
    assert!(parse(&value).is_err());
    value = valid.clone();
    value["outline_cases"][0]["stroke"]["geometry"]["bounds"]["width"] = json!(-1.0);
    assert!(parse(&value).is_err());
    value = valid;
    value["limits"]["max_fitted_samples"] = json!(usize::MAX);
    assert!(parse(&value).is_err());
    assert!(corpus::parse_corpus(&vec![b' '; corpus::MAX_JSON + 1]).is_err());
}

#[test]
fn tiny_sample_differences_missing_points_and_errors_never_pass_as_equal() {
    let expected = [corpus::Sample {
        x: 1.0,
        y: 2.0,
        pressure: 0.5,
    }];
    let actual = InkSample {
        position: InkPoint {
            x: f64::from_bits(1.0_f64.to_bits() + 1),
            y: 2.0,
        },
        pressure: 0.5,
    };
    let result = comparison::difference("synthetic", "effective", &expected, Ok(vec![actual]));
    assert!(!result.bitwise_equal);
    assert_eq!(result.max_ulp_distance, [1, 0, 0]);
    assert_eq!(result.mismatching_samples, 1);
    assert!(
        !comparison::difference("synthetic", "effective", &expected, Ok(Vec::new())).bitwise_equal
    );
    assert!(
        comparison::difference(
            "synthetic",
            "effective",
            &expected,
            Err(InkError::Cancelled)
        )
        .error
        .is_some()
    );
    let zero = [corpus::Sample {
        x: 0.0,
        y: 0.0,
        pressure: 0.0,
    }];
    let result = comparison::difference(
        "synthetic",
        "effective",
        &zero,
        Ok(vec![InkSample {
            position: InkPoint { x: -0.0, y: 0.0 },
            pressure: 0.0,
        }]),
    );
    assert!(!result.bitwise_equal);
    assert!(result.numerically_equal);
}

#[test]
fn decimal_parser_keeps_roundtrip_bits_instead_of_creating_false_native_differences() {
    for value in [
        "3.141592653589793",
        "1.0000000000000002",
        "8.7432525467563895",
        "-1.7763568394002505e-15",
    ] {
        let parsed: f64 = serde_json::from_str(value).unwrap();
        assert_eq!(parsed.to_bits(), value.parse::<f64>().unwrap().to_bits());
    }
}

#[test]
fn paths_hashes_and_cumulative_reads_are_bounded_without_external_data() {
    for path in [
        "../outside",
        "/outside",
        "a/../b",
        "a\\b",
        "C:/outside",
        "a//b",
        "",
    ] {
        assert!(corpus::safe_relative(path).is_err());
    }
    corpus::safe_relative("ink-geometry.json").unwrap();
    assert_eq!(
        corpus::sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    corpus::verify_hash(b"abc", &corpus::sha256(b"abc")).unwrap();
    assert!(corpus::verify_hash(b"abd", &corpus::sha256(b"abc")).is_err());
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("synthetic");
    std::fs::write(&file, b"abc").unwrap();
    let mut budget = 3;
    assert_eq!(corpus::read_bounded(&file, 3, &mut budget).unwrap(), b"abc");
    assert_eq!(budget, 0);
    assert!(corpus::read_bounded(&file, 3, &mut budget).is_err());
}
