<p align="center">
  <img src="packaging/linux/io.github.tiansongyu.gifromscreen.svg" width="88" height="88" alt="GifFromScreen icon">
</p>

# GifFromScreen

[![Linux CI](https://github.com/tiansongyu/gifromscreen/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/tiansongyu/gifromscreen/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/tiansongyu/gifromscreen)](https://github.com/tiansongyu/gifromscreen/releases/latest)
[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-93450a)](Cargo.toml)
[![MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#contribute-and-license)

Record part of your Linux desktop, edit it frame by frame, and export a GIF.

Built in Rust, with recording, editing and export performed locally. An independent project inspired by the [ScreenToGif](https://github.com/NickeManarin/ScreenToGif) workflow, focused on GIF output. macOS is not available yet.

[简体中文](README.md) · [Download](https://github.com/tiansongyu/gifromscreen/releases/latest) · [Installation](docs/PACKAGING.md) · [Completed & deferred](docs/WORK-STATUS.md) · [Report an issue](https://github.com/tiansongyu/gifromscreen/issues)

## Get and run

**[Download for Linux x86_64](https://github.com/tiansongyu/gifromscreen/releases/download/v0.1.0/gifromscreen-0.1.0-linux-x86_64.tar.gz)** · [SHA-256 file](https://github.com/tiansongyu/gifromscreen/releases/download/v0.1.0/gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256) · [Release notes](docs/releases/v0.1.0.md)

**v0.1.0** is the first non-prerelease release, freezing the currently verified feature set. Download without a GitHub account. It does not claim complete upstream parity or zero defects. Development builds remain separate in [CI Artifacts](https://github.com/tiansongyu/gifromscreen/actions/workflows/portable.yml), which require sign-in and expire after 30 days.

**X11 / NVIDIA startup:** if v0.1.0 reports `incompatible_surface_backends: Backends(GL)`, use `WGPU_BACKEND=vulkan ./bin/gif-from-screen` as a temporary workaround. The 0.1.1 fix on `main` selects a compatible backend automatically; it is not retroactively included in the v0.1.0 download. [Fix and validation](docs/GRAPHICS-BACKEND-STARTUP.md).

The portable package targets **Ubuntu 22.04 / glibc 2.35** and includes the desktop app and CLI. Download the archive and checksum into the same directory, then run:

```sh
sha256sum --check gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256
tar -xzf gifromscreen-0.1.0-linux-x86_64.tar.gz
cd gifromscreen-0.1.0-linux-x86_64
sha256sum --check SHA256SUMS
./bin/gif-from-screen
```

No installation or administrator privileges are required. Optional `./install.sh` adds a per-user desktop launcher; see [installation and removal](docs/PACKAGING.md#installation-and-removal). Packages are unsigned: checksums verify integrity, not independent authenticity. AppImage is excluded from this release; Flatpak has not been delivered.

## Record → edit → GIF

![Actual X11 workflow: open the recording frame, pause and move the region, resume and stop into the editor](docs/assets/record-and-retarget.gif)

Recorded from the real X11 app, including a paused recording-frame drag. These earlier-version demos have some older text and layout. [Provenance and checks](docs/assets/README.md).

1. **Choose and record.** Open Screen recorder and choose your source and region. X11 uses a separate border and control panel. Wayland first asks for sharing permission, then lets you crop inside the authorized source. Resize before recording; move the fixed-size region while recording or paused.
2. **Edit the frames.** Stop to open the editor. Remove unwanted frames, change playback timing, crop, or add text, arrows and click annotations. Edits support undo and redo.
3. **Export a GIF.** Export all or selected frames with color, looping, transparency and dithering controls. Keep the `.gfsproj` project to continue editing later.

### What it can do

- **Recording:** countdown, pause/resume, stop-and-save and explicit discard; continuous, periodic and manual snapshots, plus desktop-interaction snapshots on X11.
- **Position and timing:** X11 numeric coordinates, arrow-key nudging, window snapping and drag-to-pick; separate controls and capture region; playback delays independent of sampling intervals.
- **Frame editing:** selection, ordering, cut/copy/paste, delays, frame reduction, duplicate removal, Yoyo loops, crop/resize/rotate/flip and fade/slide transitions.
- **Annotations and effects:** captions, title frames, watermarks, shapes, drawing, borders, shadows, Cinemagraph tools, editable layers, progress, keys, clicks and cursors.
- **Sources:** GIF and PNG/JPEG/BMP/WebP import, image sequences, video import, blank animations, a drawing-board recorder and a camera-recording entry point.
- **Projects:** incremental recording saves, recovery of persisted work, Save As, recent projects and reusable editing/export presets.

This release includes the [multi-object shape canvas](docs/VECTOR-SHAPE-CANVAS.md): triangles, rounded rectangles, ellipses, block arrows, multi-selection and direct move/resize/rotate. It keeps Vector v1 / project schema 8; unfinished next-generation rendering integration is not included.

Use the [release work summary](docs/WORK-STATUS.md) as the current completion ledger. The [feature comparison](docs/FEATURE_MATRIX.md) records ScreenToGif goals and gaps; the [development archive](docs/DEVELOPMENT-STATUS.md) retains historical evidence.

## X11 and Wayland

| | X11 | Wayland |
| --- | --- | --- |
| Region selection | Desktop border, numeric positioning, window picking/snapping | System Portal source selection, then preview-based cropping |
| Movement during capture | Move a fixed-size desktop region | Move a fixed-size crop within the authorized source |
| Controls | Separate panel, preferably outside the region | Compact controller; move it outside a monitor capture yourself |
| Global shortcuts | Optional, configurable and saved | GlobalShortcuts Portal; desktop support and permission required |
| Input/cursor annotations | Optional key/button metadata and editable cursors | Embedded/hidden cursor; manual annotations available |

Shortcuts are off by default: Ctrl+Shift+F7 opens/prepares the recorder, then starts or toggles pause; Ctrl+Shift+F8 stops; Ctrl+Shift+F9 takes a manual snapshot. Buttons remain available if registration fails. Read the [shortcut contract](docs/GLOBAL-SHORTCUTS.md).

Wayland does not offer a universal transparent desktop frame, automatic controller exclusion, or guaranteed repainting of occluded applications. Prefer a window source, or keep the controller outside a monitor capture.

## Interface languages

The app follows the machine locale by default; change **Language** to save an override across restarts. This release integrates **848 English / Simplified Chinese messages** across navigation, recording, core editing, preview, crop, export, effects, layers, watermarks and text/title tools. Migrated notices update with the language; some forms, shared widgets and backend diagnostics remain untranslated.

The selector offers **System + 29 language choices**. The other **27 target languages have no translations yet and visibly fall back to English**, while preserving your preference. Selectable does not mean translated. See the [localization scope](docs/LOCALIZATION-PLAN.md).

![Switching between English and Chinese in the real app with automatically saved language preferences](docs/assets/language-switch.gif)

## Requirements and limits

- Linux x86_64, an X11 or Wayland desktop, and an OpenGL ES / Vulkan-capable driver. The portable package is not static; it needs [system libraries](packaging/linux/README.txt).
- Wayland recording needs PipeWire and an appropriate xdg-desktop-portal backend. Sharing always follows the desktop's permission flow.
- Video import additionally needs system `ffmpeg` and `ffprobe`. Screen recording, image/GIF editing and built-in GIF export do not.
- GIF is the only export format; audio is not recorded. Camera support is implemented, but physical-device acceptance is still pending.
- Isolated GNOME/X11 workflows have been exercised end to end. Physical GNOME/KDE, mixed-DPI/multi-monitor setups, long high-resolution recordings and some effect-fidelity cases remain open. This is not a claim of complete ScreenToGif parity or zero defects.
- At 150% zoom in a narrow window, launcher-card text may overlap; return to 100% or enlarge the window. The editor has the single-scroll and critical-header-action visibility fixes.

Input metadata collection is off by default. Enabling it may record sensitive input: review projects before sharing them. GIF files omit project input metadata, but rendered annotations remain visible.

## Build from source

Use Rust 1.88 or newer and the [Linux development libraries used by CI](.github/workflows/portable.yml). From the repository root:

```sh
cargo run --locked -p gif-from-screen
cargo run --locked -p gif-from-screen-cli -- doctor
```

Development checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
```

## Contribute and license

[Report issues](https://github.com/tiansongyu/gifromscreen/issues) with reproduction steps, distribution, X11/Wayland, version and monitor setup. Remove personal information from logs and projects first. See [contributing](CONTRIBUTING.md), the [changelog](CHANGELOG.md) and [architecture](docs/DESIGN.md). Feature expansion is frozen for this release; deferred work is collected in the [work summary](docs/WORK-STATUS.md).

Licensed under [MIT](packaging/licenses/LICENSE-MIT) or [Apache-2.0](packaging/licenses/LICENSE-APACHE). This independent implementation does not reuse ScreenToGif branding. Some numerical algorithms are ported from dotnet/WPF under MIT; see the third-party [NOTICE](packaging/licenses/NOTICE.txt).
