# Linux Preview 3 — published artifact acceptance

Published on 2026-09-09 (Asia/Shanghai), from
`974dfe8387cfe2d37a0ac02ddefc69d3d3bb939f`:
[v0.1.0-preview.3](https://github.com/tiansongyu/gifromscreen/releases/tag/v0.1.0-preview.3).
Release ID `384925720`, `draft=false`, `prerelease=true`; the remote tag resolves
to that exact source commit. Preview 1 and Preview 2 were not replaced or edited.

This adds English/Chinese preview, crop, GIF export and preset controls/results,
typed validation and ordinary drawing input ownership. All 29 language identities
remain selectable and saved, but only the stated English/Chinese slice is
translated; full application/language coverage is not claimed.

## Public assets

| Asset | Bytes | SHA-256 |
| --- | ---: | --- |
| `gifromscreen-0.1.0-linux-x86_64.tar.gz` | 26,750,616 | `b2b20597e9f8c7dfba783c852169c543864a1c4fd6e4998f068cb958ad127296` |
| `gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256` | 105 | `de2462b5e7e149bad3164540df94568f7bbc01ad2c2b3c3851045b21df76990a` |

Asset IDs are `550947761` and `550947763`. Both were uploaded to a draft,
inspected for the correct sizes/digests/source and then published. The public
unauthenticated download was fetched separately into
`target/preview3-download-check.YYmvzvpb`; both public files compare byte-for-byte
with the validated CI originals, and the downloaded sidecar verifies the tarball.
The public copy passes all 12 portable tests again plus an owned-Xvfb visible-window
smoke check. This is separate from merely inspecting an authenticated CI artifact.
The application/package version remains `0.1.0`; distinguish previews by tag,
source commit and checksum, not by that internal version alone.

## CI source and archive checks

Both [Linux CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34255120229)
and [portable CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34255120359)
passed for the exact source. The portable artifact is `10067665674`, with ZIP
size 26,751,093 and SHA-256
`e13a373b8b197485aefd5e04b25c026b54c1635f071f7f4ad96319422ac5cc53`.
The downloaded ZIP matched the API digest, CRC and exact two expected members.

The build receipt records clean source and packaging trees, explicit Rust/Cargo
1.88.0 and `x86_64-unknown-linux-gnu` release output on Ubuntu 22.04. Git-object
recomputation matched all recorded fingerprints:

- Source: 345 files, `f0090f2e76a32b6e41fcc90ad3dff3ad8c5f847cafad420e6cadb2ec490ec453`.
- Packaging: 76 files, `bd84eaa49640a2d1168c12fa5fdfae65c4fa9abb1a3b3633687f8ed88dc1ce76`.
- Cargo.lock: `504f6ce0e3b7e115d2c9aef259be7adceca3823d688768c632506481c67f4112`.

The tarball has 894 members, 567 regular files and 49,996,650 unpacked bytes;
all 566 listed file checksums match. All 318 third-party entries resolve to 549
nonempty license files. The Noto CJK notice checks pass, and the exact 16,437,364-byte
font is present in the actual desktop executable, not only declared in metadata.
The portable test suite passes 12/12, including real CLI GIF output, installation,
removal, overwrite protection, archive validation and licensing checks.

| Executable | Bytes | SHA-256 | Highest required GLIBC |
| --- | ---: | --- | --- |
| Desktop | 43,662,080 | `b8b946930142f69fafd583d43990c614a12482a17968f2264eb42e38bc5f2c50` | 2.35 |
| CLI | 3,322,680 | `31c4e86266582d0daa81c36ee66434de5930fbacc864a43caa6afb5a649452b1` | 2.34 |

Actual ELF dependencies/version requirements match the build receipt. This is
not a static package, a signed artifact, a complete ABI/SBOM audit or an
independently reproduced compilation.

## Actual packaged desktop

The packaged desktop was extracted, frozen and launched as supervised
`extra-app-784878` in the owned private GNOME/X11 lab
`/tmp/gfs-wayland-qa.bsgn6rxp`. Its digest matches the release desktop above.
The saved System policy resolves to Chinese under `LC_ALL=zh_CN.UTF-8 LANGUAGE=zh`.
The app opens the revision-39 project from the
[source-build drawing/Undo/Redo acceptance](PREVIEW-EXPORT-QA-2026-09-09.md).

A quick Down/Up produces a one-point Ready draft, then explicit Cancel leaves
the committed artwork alone. Export through the Chinese GUI produces 56,981 bytes,
byte-identical to the debug-GUI and reopened-CLI drawing GIF:
`2114041a1788985f29d5f931ac5da4f2d67b5c485151e950c1c53ceb6015157a`.
Screenshots 29/30 record the real packaged controls and result. The app closes
with status 0; its supervisor and the whole lab confirm cleanup, with the original
launcher returning 0 after an explicit scoped stop.

This is a bounded software-rendered X11 exercise, not physical-device, Wayland,
mixed-DPI/multi-monitor, camera, full language/IME or zero-defect certification.
macOS, AppImage and Flatpak remain unshipped.
