//! Strict comparison of center samples from actual Windows WPF Ink diagnostics.
//! This is NOT a `GetGeometry` outline, eraser, coverage or pixel-parity assertion.
//! Real corpus: probe run 34085077295, source revision 5aa2e60. Later runs are
//! accepted only with matching current probe source inventory and complete IDs.

#[path = "ink_reference/comparison.rs"]
mod comparison;
#[path = "ink_reference/corpus.rs"]
mod corpus;
#[cfg(test)]
#[path = "ink_reference/synthetic.rs"]
mod synthetic;

use serde::Serialize;
use std::path::Path;

type Result<T> = std::result::Result<T, String>;

#[derive(Serialize)]
struct Report {
    claim: &'static str,
    provenance: serde_json::Value,
    index_sha256: String,
    diagnostic_sha256: String,
    verified_index_source_and_diagnostic_bytes: usize,
    cases: usize,
    routes: Vec<comparison::RouteSummary>,
    mismatches: Vec<comparison::Comparison>,
}

fn run(directory: &Path, repository: &Path) -> Result<Report> {
    let loaded = corpus::load(directory, repository)?;
    let comparisons = comparison::compare_corpus(&loaded.corpus)?;
    let routes = comparison::summarize(&comparisons);
    Ok(Report {
        claim: "Only raw, forced Bezier and effective center samples: exact counts, f64 XY bits and f32 pressure bits. No tolerance, synthetic fallback, outline/eraser/coverage or pixel-parity claim.",
        provenance: loaded.provenance,
        index_sha256: loaded.index_sha256,
        diagnostic_sha256: loaded.diagnostic_sha256,
        verified_index_source_and_diagnostic_bytes: loaded.bytes_read,
        cases: loaded.corpus.outline_cases.len(),
        routes,
        mismatches: comparisons
            .into_iter()
            .filter(|item| !item.bitwise_equal)
            .collect(),
    })
}

#[test]
#[ignore = "requires real Windows InkGeometryProbe artifacts in GFS_CINEMAGRAPH_PROBE_DIR"]
fn compare_windows_ink_samples() -> Result<()> {
    let directory = std::env::var_os("GFS_CINEMAGRAPH_PROBE_DIR")
        .filter(|value| !value.is_empty())
        .ok_or("GFS_CINEMAGRAPH_PROBE_DIR must identify actual indexed Windows probe artifacts; no fallback exists.")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report = run(Path::new(&directory), &repository)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    if !report.mismatches.is_empty() {
        return Err(format!(
            "{} strict Ink sample comparisons differ; all cases were checked. References and tolerances were not changed.",
            report.mismatches.len()
        ));
    }
    Ok(())
}
