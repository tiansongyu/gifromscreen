//! Frozen production-V1 bytes, NOT expected WPF pixels.
//!
//! Inputs are byte-identical to source 4e28a35. Expected hashes are the actual
//! V1 outputs retained by its hosted run 34309842487, before the V2 dispatcher.
//! Never regenerate these values from V2 or from the Windows golden images.

use super::*;

const OLD: &[u8] = include_bytes!("vector_v1_compatibility.json");
const HASHES: [(&str, &str); 14] = [
    (
        "vector-square-fill",
        "8bb4e1a3402287f5b22128fd18ee38b6b72c91686adef0a435b8963078419434",
    ),
    (
        "vector-rounded-fraction",
        "dcf22199e765aab36b433ce1554c8d83e34c6bafbc6097a27ebfea65fa355d26",
    ),
    (
        "vector-rounded-axis-clamp",
        "b079870ae6393635c2ef1dfbb65d073453d02d66be504c16882f0f17a3e56b98",
    ),
    (
        "vector-triangle-rotated",
        "e65797fa706dabecec61589d271ba6904b57b802a850fc5088c213effd1cf40f",
    ),
    (
        "vector-block-arrow",
        "b48393a79d3653caccfdfd35ba615f65865451d11be7ed9bf17d3bab6b8e6373",
    ),
    (
        "vector-ellipse-fraction",
        "283da3b0bce545e069ca6ba4809906d8c52594bf71b01873b71e4d83175708a3",
    ),
    (
        "coverage-triangle",
        "44e39ffadbdbeef6c4aa355ef23798aea40a6a7d6a90b5f8b4dbcb8bd72d2eae",
    ),
    (
        "coverage-fractional-triangle",
        "d1840fd9e163d5882f56467029b45d4e216305d08b0a0441085f4c27a23e26fc",
    ),
    (
        "coverage-ellipse",
        "c869ebb135467a81fceff9a19d20d97101b0a8052208c59d3ad0d074d4bcf7d2",
    ),
    (
        "coverage-triangle-stroke",
        "4945efa0c3e8ef0be141472046e6729bf8e8773489e5c02649cf4f6aeab01a5e",
    ),
    (
        "layout-thin-triangle",
        "846bb860344d655f0c26c69b871aff06f6e4e178d3ad44714b687e1b73d32401",
    ),
    (
        "layout-inverted-triangle",
        "be83be5f5fedaab3d81c4c182fa59a1318cd2a97e33f06985101cf3930ac5fff",
    ),
    (
        "layout-inverted-arrow",
        "5a7246c75d3465798860f94f4d55a915d59613a45220d7d4cefe3987e2d3bbcb",
    ),
    (
        "layout-narrow-arrow",
        "8af8e58818db34b8605482d32baf1791fb53ebb7634e9a52f88b8fd96b8da08a",
    ),
];

#[test]
fn version_one_keeps_all_fourteen_archived_actual_outputs() {
    assert_eq!(
        sha256(OLD),
        "64689c02996e5bbce21efafbfd7b4f81e54682defc972854ed128cfb000dfd40"
    );
    let definition = parse_definition(OLD).unwrap();
    for (id, expected) in HASHES {
        let fixture = definition.fixtures.iter().find(|f| f.id == id).unwrap();
        let mut graph = Graph::new(&fixture.source).unwrap();
        for operation in &fixture.operations {
            graph.append(operation).unwrap();
        }
        assert_eq!(
            sha256(graph.render().unwrap().pixels()),
            expected,
            "legacy fixture {id}"
        );
    }
}

#[test]
fn wpf_inputs_change_only_the_explicit_renderer_version_not_any_geometry_color_or_case() {
    let mut expected: serde_json::Value = serde_json::from_slice(OLD).unwrap();
    let mut count = 0;
    for fixture in expected["fixtures"].as_array_mut().unwrap() {
        for operation in fixture["operations"].as_array_mut().unwrap() {
            if operation["kind"] == "vector_shapes" {
                for shape in operation["shapes"].as_array_mut().unwrap() {
                    assert_eq!(shape["version"], 1);
                    shape["version"] = 2.into();
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 15);
    let actual: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../../scripts/qa/wpf_reference/fixtures.json"
    ))
    .unwrap();
    assert_eq!(actual, expected);
}
