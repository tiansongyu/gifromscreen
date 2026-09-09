# v0.1.0 — Linux release artifact acceptance

The release freezes the current Linux x86_64 feature scope, not complete
ScreenToGif parity. See [release notes](releases/v0.1.0.md) and
[completed/deferred work](WORK-STATUS.md). This record distinguishes the exact
shipped source from subsequent documentation-only commits.

## Frozen source and CI

Release source: **`b3cd1ff9a9ba3ebdaebe3e590d0f52dca9bed334`**.
The feature baseline is `67f11e7`; release preparation only changes the
preview subtitle to a product subtitle, a format-compatible local binding, a
test variable name, and repository documentation. It does not enable new
features or change saved rendering semantics. Projects remain schema 8 /
Vector v1.

- [Linux CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34322397716):
  passed, including strict lint, workspace tests and Ubuntu 22.04 native-backend
  checks with explicitly invoked private-Xvfb/input tests.
- [Portable CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34322397697):
  passed on Ubuntu 22.04 with pinned Rust **1.88.0**, a locked release build,
  build/source receipt checks, deterministic repackaging, safe installation
  tests, owned-display lifecycle tests and a packaged native-window smoke test.
- Local explicit Rust **1.98.0 and 1.88.0** each pass formatting, strict workspace
  Clippy and the full all-target/all-feature suite: **1,868 passed, 0 failed,
  56 ignored**. Ignored tests are not presented as passes.
- A separate Rust 1.88.0 native test executable passes **42/42** `private_xvfb_`
  tests and **1/1** isolated input-session test. They use owned displays, not
  the host desktop. Both process leaders were reaped and their owned groups
  were empty; no timeout or termination fallback was needed. Evidence:
  `/tmp/gfs-v010-native-qa.mRu9ax/result.json`, SHA-256
  `a42a61a081cbecf7a46817570c55b09b055573304e56a1c7db6476d440770d0c`.

The separate [WPF reference workflow](https://github.com/tiansongyu/gifromscreen/actions/runs/34322397712)
**still fails its full strict comparison** on the known default Vector v1
differences. Windows reference generation, filled-contour comparison and the
separate vector-candidate step pass. This is retained evidence of unfinished
pixel parity, not a green release certification or a newly enabled renderer.
No test tolerance or workflow gate was weakened for publication.

The current strict report contains 41 surfaces: 19 inputs and 22 outputs, with
no provenance/size errors. All inputs, eight original effect/chain outputs and
the square-fill output are exact. The remaining **13 Vector v1 outputs differ**
in 795 pixels in total. The older overlapping reference cases are unchanged;
the separate candidate is **14/14 RGBA-exact**, and filled-contour-only comparison
is **3/3 exact**. These separate results are not substituted for the default path.

## Exact archive and build identity

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `gifromscreen-0.1.0-linux-x86_64.tar.gz` | 27,143,602 | `9f904154af567973b476bd6449b3d4d6dd819c61f336e0bf9380999d05e5a26f` |
| `gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256` | 105 | `42020c5e4aa8756aa89c895c4203e98889a898da8b232e066b627a8727695979` |

CI artifact **10092485628** belongs to the exact source/run above. Its downloaded
ZIP is 27,144,079 bytes with SHA-256
`948bd3c055d6c9c372b4f03503ecf7a61c30381170f92d953f5f79055300aa3e`,
matching the GitHub artifact digest. ZIP CRC checks pass and there are exactly
the two intended release files.

`BUILD-INFO.json` has SHA-256
`91e652449b03089da2a9acd7397ca87da4d231564c659b720ad87fc327ea998e`.
It records `source_dirty=false`, `package_git_dirty=false`, Rust/Cargo 1.88.0,
the explicit `x86_64-unknown-linux-gnu` release target and glibc limit 2.35.
Independent recomputation from the frozen Git objects matches:

- Rust source tree: **377 files**,
  `d888e250afad7936c3247b203b30cf55fd82c4113c7010f0f88c70089b8b2e20`.
- Packaging source tree: **78 files**,
  `c792fed3a669c5274b899cac22c4e050e82439c650e200e968b96e638235e70e`.
- Cargo.lock:
  `516f8119016707a5310a02b5352e5587bf618ac0e10e9b2ff6664005374089f0`.

| Executable | Bytes | SHA-256 | Highest required GLIBC |
| --- | ---: | --- | --- |
| Desktop | 44,272,384 | `5880bb9b9acaf31d013589001985bec7bc3334d897dc6f0f7b15de88a63de898` | 2.35 |
| CLI | 3,609,416 | `4fce9926e206ba38146a75533d91dd0e7921d919437d78dc45e50aebc5c5abc8` | 2.35 |

The archive contains 902 members / 571 regular files / 50,901,714 unpacked
file bytes. All **570** listed checksums match. All **322** third-party inventory
entries resolve to **553** distinct nonempty license files. The exact
16,437,364-byte Noto CJK font is also present in the actual desktop executable.
ELF dependency/version inspection matches the receipt and finds no missing
runtime dependency on the checking host; this does not remove system-library
requirements on other desktops.

The actual CI archive passes all **12 portable tests** locally, including real
CLI GIF output, install/reinstall/uninstall, foreign-file and symlink protection,
corruption rejection and license checks. A separate owned-Xvfb run confirms a
visible packaged X11 window. That smoke test uses scoped process termination
for cleanup; it does not test normal close or the full recording workflow.

Machine-readable package audit:
`/tmp/gfs-release-v010.x5i8q4VC/package-verification.json`, SHA-256
`874f6ef957efaed600ce30c8bc991912e084bdbe6d1b6df5c3594139fdc344ed`.
The CI artifact ZIP, exact package, audit script and logs are retained in that
private local evidence directory; these `/tmp` paths are not public downloads.

## Scope and retained unfinished work

The bounded X11 checks do not qualify all physical GNOME/KDE/wlroots desktops,
Wayland sharing flows, mixed DPI, multiple monitors, physical cameras, long
recording workloads or all languages/IME/accessibility paths. The existing
[narrow-editor native record](NARROW-EDITOR-QA-2026-09-09.md) also preserves the
known home-card text overlap at 680 pixels / 150% UI zoom; Ctrl+0 is the documented
workaround. That issue was not silently marked fixed.

Packages are unsigned. Checksums establish integrity, not independent
authenticity. Byte-identical repackaging is not reproducible compilation or
a complete binary SBOM. AppImage, Flatpak and macOS are not shipped.

Unfinished schema 9 / Vector v2 integration is preserved separately in
[`archive/schema9-vector-v2-20260909`](https://github.com/tiansongyu/gifromscreen/tree/archive/schema9-vector-v2-20260909)
at `abe741b1ec6bd8fee83f24c21b23df919788e8c2`. It includes unfinished,
not-yet-buildable integration and is explicitly not part of this release.
No feature work was resumed during publication.
