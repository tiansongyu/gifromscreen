# Independent Windows ink evidence

This separates sample fitting, outline generation, Boolean geometry, scan
conversion and final composition. A passing later component does not certify the
earlier producer. No reference pixels or tolerances were changed to make Rust pass.

## Actual generator and unchanged pixel corpus

[Windows/Linux workflow 34085077295](https://github.com/tiansongyu/gifromscreen/actions/runs/34085077295)
passed at generator commit `5aa2e60cb31bb112688862d50a5975cb00f34014`, using
SDK 9.0.317, desktop runtime 9.0.19 and Windows 26100. Its separate
`ink-geometry.json` has 38 outline observations, 8 `Transform(matrix, false)`
observations and 8 actual hit/erase cases. Generation alone proves none of their
Rust equivalents.

- Index SHA-256: `671aba618b8cede785eb8e66030b301d29ed061d63b3013fdf8a7cff1babc5f1`.
- Ink diagnostic: 324,373 bytes, SHA-256
  `3ddfa0fabbf726e6fe0c776dc7dfdb84cada21d71a3fda9dc3f2af505df74da9`.
- Original 90-case definition remains
  `9ed2b610efa32f966f2c1be0a3dad18a4016972907f56b4ad97f7b629b4452b4`.
  All 2,160 original per-case artifact hashes remain identical to the earlier
  independent corpus. The new ink diagnostic is separate, not an edited golden.
- Actual source inventories/hashes were checked. The generator only compiles
  the explicitly inventoried top-level C# files.

Artifacts were downloaded to
`/tmp/gfs-ink-geometry-review.X6Wir79y/cinemagraph-probe-34085077295-1`.
The existing strict snapshot comparator also passed local Rust 1.88 replay:
90 native PM inputs × 3 render routes, zero channel differences. It verified
2,141,338 index/per-case bytes; it does not count the new diagnostic as pixel
producer evidence.

## Sample fitting

A temporary independent comparison using production `fitted_ink_samples`
checked all 38 cases through three routes: unchanged raw samples, forced Bezier
fitting and the stroke's effective FitToCurve choice. All **114 sequences** have
equal sample counts and bitwise-equal f64 coordinates and f32 pressures.
Maximum absolute and ULP error are zero. Cases include five/six/seven-point
curves, cusps, loops, duplicate nodes, variable pressure, the distinct three-point
branch, both tips, pressure extremes and IgnorePressure.

The JSON reader must use exact floating-point round-trip parsing. Its default
parsing originally introduced up to `1.776e-15` artificial coordinate differences;
enabling `serde_json/float_roundtrip` removed those without changing the fitter,
expected values or allowed error. Temporary source/report are under
`/tmp/gfs-ink-geometry-review.X6Wir79y/`.

The comparison is now a repository regression in
`crates/render/tests/ink_reference.rs`, with eight synthetic verifier tests that
do not masquerade as Windows evidence. It rejects incomplete/repeated cases,
wrong source inventories/hashes, unsafe paths, inconsistent effective samples,
unbounded metadata and single-ULP differences. The real test is opt-in with no
fallback. It verified 1,645,977 index/source/diagnostic bytes and compared
150 raw, 238 forced-fit and 194 effective samples across the 38 cases.
Real replay passes on Rust 1.98.0 and 1.88.0. The complete repository cohort
passes 1,357 workspace tests on both toolchains plus strict Clippy; opt-in
hardware/external-evidence tests are not counted as ordinary passes.

```sh
GFS_CINEMAGRAPH_PROBE_DIR=/path/to/unmodified/windows-artifact \
  cargo test --locked -p gif-from-screen-render --test ink_reference \
  compare_windows_ink_samples -- --ignored --exact --nocapture --test-threads=1
```

The manual Windows-producer/Linux-comparator workflow also runs this sample
gate separately from snapshot composition. It does not turn the known line
outline mismatches below into passing producer tests.

## Actual production outline → clipped PM result

A second bounded program passed original fixture samples and dimensions to
production `outline_ink_stroke` and `clip_ink_reference`. It used the first-frame
decoded straight BGRA input, converted only channel order; there was no extra
PNG boundary and no native geometry string as producer input.

| Original input | Cases | Different PM pixels / channels | Different white-mask pixels |
|---|---:|---:|---:|
| Ellipse dot | 9 | 0 / 0 | 0 |
| Rectangle dot | 9 | 0 / 0 | 0 |
| Ellipse two-point line | 9 | 84 / 282 | 153 |
| Rectangle two-point line | 9 | 66 / 222 | 126 |

These totals count all alpha combinations. Transparent source cases can hide
geometry errors: their PM output is all zero, but white-mask comparisons still
fail for the two line geometries. They must not be called geometry passes.

Representative opaque-source PM values, in RGBA order:

- Ellipse line at `(11,9)`: Windows `[28,24,34,247]`, Rust `[20,17,24,175]`.
- Rectangle line at `(0,1)`: Windows `[20,63,146,167]`, Rust `[21,66,153,175]`.

The strict producer report is `/tmp/gfs-ink-production-compare.muebxy/report.json`,
251,970 bytes, SHA-256
`0e075bbac8b66831147cef08c6c808b52d4909fd52d24620eacb9b09eabd91b9`.
Its source, fixture identity and compiled production-source hashes are recorded
alongside the differences.

## Isolating the remaining difference

A deliberately separate diagnostic fed the native **post-Boolean path** into
production `rasterize_ink_paths`, then applied the already-established coverage64
PM rule. All 36 tested cases matched native white masks and clipped PM exactly.
This supports the shared-path scan converter for these cases, not the ability
to generate those paths from original Linux ink.

The remaining measured difference is upstream of that shared-path rasterization:
outline construction and/or Boolean geometry. The pinned WPF identity-transform
renderer uses `RenderTwoStrokeNodes` with start/end segment construction and
ArcTo optimization for separated nodes. The current ideal tip/sweep union is not
that path. This is a concrete source-level distinction, not proof that replacing
just one arc will resolve Boolean clipping and quantization as well.
[Pinned WPF StrokeRenderer](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/PresentationCore/MS/internal/Ink/StrokeRenderer.cs).

The isolation report is `post-boolean-report.json` in the same temporary directory,
41,231 bytes, SHA-256
`8b5c8b64f88d84f0f1ce86c86534b61592836a32f026c98f01602b13de18fe28`.
The ellipse eraser approximation and degenerate whole-clip VisualBrush behavior
also remain separate open items. Native authoring usability is covered in
[its own acceptance report](NATIVE-CINEMAGRAPH-QA-2026-09-07.md).
