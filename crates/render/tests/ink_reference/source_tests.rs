//! Source/integrity mechanics only, with explicitly synthetic files and no WPF claim.
use super::*;

fn inventory(root: &Path) -> Index {
    let source = root.join("scripts/qa/cinemagraph_probe");
    fs::create_dir_all(&source).unwrap();
    let source_files = [
        "Program.cs",
        "InkGeometryProbe.cs",
        "CinemagraphProbe.csproj",
        "global.json",
        "run-probe.ps1",
        "README.md",
    ]
    .into_iter()
    .map(|name| {
        fs::write(source.join(name), b"synthetic inventory only").unwrap();
        SourceFile {
            path: format!("scripts/qa/cinemagraph_probe/{name}"),
            sha256: sha256(b"synthetic inventory only"),
        }
    })
    .collect();
    Index {
        format_version: 1,
        fixture_count: 0,
        definition_sha256: String::new(),
        provenance: serde_json::Value::Null,
        source_files,
        cases: Vec::new(),
        additional_diagnostics: Vec::new(),
    }
}

#[test]
fn synthetic_source_inventory_requires_every_file_and_its_current_digest() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let mut index = inventory(&root);
    let mut budget = MAX_TOTAL;
    verify_sources(&index, &root, &mut budget).unwrap();
    index.source_files[0].sha256 = sha256(b"changed");
    assert!(verify_sources(&index, &root, &mut budget).is_err());
    index.source_files[0].sha256 = sha256(b"synthetic inventory only");
    index.source_files.pop();
    assert!(verify_sources(&index, &root, &mut budget).is_err());
}

#[test]
fn a_partial_index_or_missing_additional_diagnostic_is_not_a_reference() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let index = inventory(&root);
    assert!(validate_index(&index).is_err());
    let bytes=br#"{"format_version":1,"fixture_count":90,"definition_sha256":"copied hash","provenance":{},"source_files":[],"cases":[]}"#;
    assert!(serde_json::from_slice::<Index>(bytes).is_err());
}

#[cfg(unix)]
#[test]
fn source_and_artifact_symlinks_cannot_escape_or_alias_the_verified_tree() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let index = inventory(&root);
    let mut budget = MAX_TOTAL;
    let source = root.join("scripts/qa/cinemagraph_probe/Program.cs");
    let original = root.join("preserved-source");
    fs::rename(&source, &original).unwrap();
    std::os::unix::fs::symlink(&original, &source).unwrap();
    assert!(verify_sources(&index, &root, &mut budget).is_err());
    assert!(safe_file(&root, "scripts/qa/cinemagraph_probe/Program.cs").is_err());
    assert_eq!(fs::read(original).unwrap(), b"synthetic inventory only");
}
