# Linux portable distribution

The supported package is a native **x86_64 Linux tar.gz**, built on Ubuntu 22.04 with Rust 1.88.0 and the checked-in `Cargo.lock`. It contains the desktop application and CLI. It is not a static binary: system graphics, PipeWire, Wayland/X11, and portal libraries remain system dependencies. FFmpeg and ffprobe are optional system dependencies for video import and are **not bundled**.

## Download and run

Download the [v0.1.0 Linux tarball](https://github.com/tiansongyu/gifromscreen/releases/download/v0.1.0/gifromscreen-0.1.0-linux-x86_64.tar.gz)
and its [SHA-256 file](https://github.com/tiansongyu/gifromscreen/releases/download/v0.1.0/gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256).
This is the first non-prerelease release, with a frozen feature scope. It remains
unsigned and does not claim full upstream parity or complete hardware coverage.
See the [release notes](releases/v0.1.0.md) and [completed/deferred summary](WORK-STATUS.md).

The `Linux portable package` GitHub Actions workflow also uploads a tarball and
its `.sha256` file for successful main-branch builds. These newer CI artifacts
require GitHub sign-in and expire after 30 days; that workflow itself does not
create Releases or tags. After downloading both Release files (or extracting a
CI artifact ZIP), run:

```sh
sha256sum --check gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256
tar -xzf gifromscreen-0.1.0-linux-x86_64.tar.gz
cd gifromscreen-0.1.0-linux-x86_64
sha256sum --check SHA256SUMS
./bin/gif-from-screen
./bin/gif-from-screen-cli --help
```

Checksums detect corruption, not authenticity independently of the download source. Use the artifact attached to the intended repository commit.

## Installation and removal

No installation is required. Optional per-user desktop integration requires Python 3.8 or later:

```sh
./install.sh
./install.sh --prefix "/absolute/path/user prefix"
```

The default prefix is `~/.local`; no `sudo` is used. The installer adds two relative executable links in `PREFIX/bin`, an application-owned payload in `PREFIX/lib/gifromscreen`, and the desktop launcher/icon below `PREFIX/share`. Launcher paths are escaped for the desktop-entry format, including spaces and reserved characters. For a custom prefix, add `PREFIX/bin` to your shell PATH if desired and expose `PREFIX/share` through your desktop's `XDG_DATA_DIRS` if it is not already searched. The default `~/.local/share` is normally searched automatically.

The installer verifies all package hashes before writing and refuses existing foreign files, destination symlinks, and symlinked parent directories. Reinstalling the identical package is a verified no-op. A different package requires explicit uninstall first; there is no in-place updater yet.

```sh
"$HOME/.local/lib/gifromscreen/uninstall.sh"
"/absolute/path/user prefix/lib/gifromscreen/uninstall.sh" --prefix "/absolute/path/user prefix"
```

Uninstall validates the complete recorded file inventory before removing any file, preserves modified files by refusing the operation, and only removes empty directories using `rmdir`. Unlisted projects and user data are never deleted. Installed source/binaries should not be edited in place; preserve any intentional changes elsewhere before uninstalling.

## Build and verify locally

Use a Linux x86_64 builder with the development libraries listed in `.github/workflows/portable.yml`, Python 3, `readelf`, and Rust 1.88.0 installed. `desktop-file-validate`, `xvfb-run`, and `xdotool` enable the complete packaging tests.

```sh
python3 scripts/build_portable.py --output-dir target/package-first
python3 scripts/test_build_portable.py
python3 scripts/build_portable.py --skip-build --output-dir target/package-second
cmp target/package-first/*.tar.gz target/package-second/*.tar.gz
python3 scripts/test_portable.py target/package-first/*.tar.gz
xvfb-run --auto-servernum python3 scripts/smoke_portable_desktop.py target/package-first/*.tar.gz
```

Existing archive outputs are never overwritten; choose a new output directory when testing a new package. `--target-dir` selects the build cache. `--max-glibc` defaults to 2.35: packaging inspects both ELF symbol-version requirements and refuses newer binaries or missing linked libraries. Changing this option changes the supported baseline; it does not make a newer binary compatible with older systems.

`BUILD-INFO.json` records the source revision, whether Rust sources were dirty, the actual Rust source-tree and Cargo.lock SHA-256 hashes, compiler versions, binary hashes, package-source hash, and ELF requirements. `--skip-build` only reuses binaries whose hashes and source-tree digest match a prior build receipt. A source edit during compilation fails the build rather than assigning an incorrect revision to the result.

Cargo receives an explicit `--target x86_64-unknown-linux-gnu`; the builder reads
only `TARGET_DIR/x86_64-unknown-linux-gnu/release`, including its versioned build
receipt. External `CARGO_BUILD_TARGET` or Cargo `build.target` settings cannot
redirect the new build while old host-directory binaries are packaged. Reuse
also requires matching receipt schema, toolchain/compiler identity, target,
profile and declared path remap. Old unversioned receipts require a fresh build;
they are not silently migrated. If `CARGO_ENCODED_RUSTFLAGS` is set, even empty,
packaging fails with an explicit unset instruction because that variable would
override the recorded `RUSTFLAGS` remap. These checks do not make arbitrary
system linkers or Cargo configuration hermetically reproducible.

The archive uses sorted entries, normalized owners/modes, `SOURCE_DATE_EPOCH` (defaulting to the source commit timestamp), and a zero gzip timestamp. CI proves **byte-identical repackaging of the same compiled binaries and package sources**. This is distinct from reproducible compilation across independent machines: system library versions, native build tools, and OS images are not all hermetically pinned, so that stronger claim is not made.

## Contents and licensing

Each package includes:

- Desktop and CLI ELF executables; no third-party packager or FFmpeg download.
- Freedesktop launcher, project-authored SVG icon, install/uninstall scripts, and runtime instructions.
- Complete project MIT/Apache-2.0 texts and third-party normal/build-dependency notices from the selected, locked Cargo graph.
- Original dependency license/notice files and embedded egui font license texts (including OFL and Ubuntu font licenses). When a monorepo crate archive omits its common license, the Apache-2.0 alternative is used where the crate explicitly offers it; additional `AND` obligations are retained. Other missing license texts cause packaging to fail.
- Pinned upstream cookie-factory MIT/copyright notices and the CC0 1.0 text, vendored with source URLs and SHA-256 checks. These are text notices, not downloaded executable tooling.
- Per-file `SHA256SUMS`, `BUILD-INFO.json`, and an adjacent archive SHA-256 file. The conservative dependency inventory includes build dependencies; it is not presented as a precise binary-linkage SPDX SBOM.

GitHub Actions dependencies in the packaging workflow are pinned to immutable commit hashes; the Rust toolchain version is fixed. The package builder itself needs no network beyond Cargo's locked dependency/toolchain retrieval.

The portable workflow restores a separate dependency cache for `target/portable-build`, after selecting Rust 1.88.0. Cache saves are limited to main-branch runs. A cache hit never bypasses the locked release build, source/binary receipt checks, deterministic repackaging, installation tests or native-window smoke test. This follows the cache action's [custom workspace/target configuration](https://github.com/Swatinem/rust-cache/tree/6323deb102c322ba6fcbdcafc7e3dddab59af2b6); it is a build-time optimization, not additional evidence of reproducible compilation or runtime correctness.

### Owned display readiness

The private-Xvfb harness waits for one newly created server to publish its
`-displayfd` readiness notification, then verifies a real connection before
launching the packaged app. Its default startup deadline is 30 seconds, with an
explicit finite `(0, 120]` override for tests. It never falls back to host DISPLAY
or starts a replacement server after an observation timeout. Timeout, early exit,
EOF and invalid/oversized protocol data are distinct errors; diagnostics include
the owned PID, elapsed time, budget, status and a bounded byte prefix. Cleanup
still terminates/reaps only that owned child.

This follows a [portable CI failure](https://github.com/tiansongyu/gifromscreen/actions/runs/34236118164)
where the previous five-second deadline expired while the child was alive and
stderr was empty. The specific runner delay was **not reproduced or diagnosed**.
Displayfd is emitted after server initialization, not immediately after choosing
a display number; see the pinned [Xserver initialization sequence](https://gitlab.freedesktop.org/xorg/xserver/-/blob/xorg-server-21.1.4/dix/main.c#L153)
and [readiness notification](https://gitlab.freedesktop.org/xorg/xserver/-/blob/xorg-server-21.1.4/os/connection.c#L198).
No fixed display-collision delay is claimed.

The 12 harness tests cover delayed/partial publication under a fake clock,
unchanged absolute deadlines, timeout/exit/EOF/invalid input, one-child cleanup,
real invalid-output subprocess pipes, and real overlapping private Xvfb servers.
The real portable-archive window smoke also passed locally with the revised
harness; these checks are separate from the full recording acceptance matrix.

Follow-up [portable CI 34237971373](https://github.com/tiansongyu/gifromscreen/actions/runs/34237971373)
and [Linux CI 34237971358](https://github.com/tiansongyu/gifromscreen/actions/runs/34237971358)
passed for `8ca42c0ec1952d74886924463ffa11ecefb78f74`. The portable run passed actual
archive construction, deterministic repackaging, safe installation tests, owned
display lifecycle, native packaged-window launch and artifact upload. The earlier
documentation-only `506df5b` run also passed with the old timeout; neither green
run diagnoses the earlier server delay, and the failed run remains a failure.

## Verification scope and remaining formats

The automated suite validates the real archive, CLI GIF export, installation under prefixes with spaces/reserved characters, idempotent reinstall, refusal to replace foreign files/symlinks, checksum corruption, modified-installed-file protection, traversal protection, and preservation of unlisted projects during uninstall. Xvfb verifies that the packaged desktop displays a native X11 window. These checks do not substitute for GNOME/KDE Wayland sharing permission and compositor testing.

AppImage and Flatpak are not yet claimed as distributable deliverables. The
[AppImage development builder](APPIMAGE-PLAN.md) now assembles an actual local
image with checked tooling, private native dependencies and tests, but explicitly
blocks publication while corresponding-source materials and runtime/platform
acceptance remain incomplete. Its initial smoke test exposed a runtime cleanup
failure; a source-built fix now passes extract-and-run and FUSE cleanup plus
same-input byte-identical repackaging. See the
[new measured record](APPIMAGE-RUNTIME-QA-2026-09-08.md) and
[original failure](APPIMAGE-DEVELOPMENT-QA-2026-09-08.md).
The actual AppImage now also passes bounded nested-GNOME window capture,
pause/crop movement and GUI/CLI GIF export; see
[Wayland package acceptance](APPIMAGE-WAYLAND-QA-2026-09-08.md). This does not
replace the remaining hardware, desktop-registration and source-material gates.
The [Flatpak plan](FLATPAK-PLAN.md) identifies application changes for durable
projects, folder-scoped exports, Camera Portal and X11 namespace-safe ownership;
it does not include an unbuilt placeholder manifest. Signed releases, an in-place
updater, and a machine-verified complete binary SBOM remain separate work.

References: [desktop-entry specification](https://specifications.freedesktop.org/desktop-entry/latest/), [GitHub artifact action](https://github.com/actions/upload-artifact), [Rust toolchain action](https://github.com/dtolnay/rust-toolchain), [Apache 2.0 text](https://www.apache.org/licenses/LICENSE-2.0.txt), [MIT license](https://opensource.org/license/mit), [CC0 1.0 legal code](https://creativecommons.org/publicdomain/zero/1.0/legalcode.txt).
