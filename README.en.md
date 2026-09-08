<p align="center">
  <img src="packaging/linux/io.github.tiansongyu.gifromscreen.svg" width="88" height="88" alt="GifFromScreen icon">
</p>

# GifFromScreen

Record part of your Linux desktop, edit it frame by frame, and export a GIF.

Built in Rust, with recording, editing and export performed locally. Inspired by the ScreenToGif workflow and focused on GIF output. Linux comes first; macOS is not available yet.

[简体中文](README.md) · [Releases](https://github.com/tiansongyu/gifromscreen/releases) · [Installation](docs/PACKAGING.md) · [Report an issue](https://github.com/tiansongyu/gifromscreen/issues)

## Get and run

**[Download the Linux x86_64 preview](https://github.com/tiansongyu/gifromscreen/releases/download/v0.1.0-preview.2/gifromscreen-0.1.0-linux-x86_64.tar.gz)** · [SHA-256 file](https://github.com/tiansongyu/gifromscreen/releases/download/v0.1.0-preview.2/gifromscreen-0.1.0-linux-x86_64.tar.gz.sha256) · [Release notes](https://github.com/tiansongyu/gifromscreen/releases/tag/v0.1.0-preview.2)

Current version: `v0.1.0-preview.2` (about 26.7 MB), available without sign-in. This is an unsigned development preview, not a stable release. Newer main-branch test builds remain available in [CI Artifacts](https://github.com/tiansongyu/gifromscreen/actions/workflows/portable.yml), which require sign-in and expire after 30 days.

The portable package is built against Ubuntu 22.04 / glibc 2.35 and includes the desktop app and CLI. Follow the [download checksum and extraction instructions](docs/PACKAGING.md#download-and-run), then run from the extracted directory:

```sh
sha256sum --check SHA256SUMS
./bin/gif-from-screen
```

Installation and administrator privileges are not required. Optional `./install.sh` adds a per-user desktop launcher; see [installation and removal](docs/PACKAGING.md#installation-and-removal). AppImage remains under development validation; Flatpak has not been delivered. Packaging scripts are not published installers.

## Record → edit → GIF

![Actual X11 workflow: open the recording frame, pause and move the region, resume and stop into the editor](docs/assets/record-and-retarget.gif)

Recorded from the running X11 app, including a paused recording-frame drag; not a UI mock-up. [Demonstration provenance and checks](docs/assets/README.md).

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

See the [Linux feature ledger](docs/LINUX-STATUS.md) for current scope and the [development archive](docs/DEVELOPMENT-STATUS.md) for detailed history and evidence links.

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

The app follows the system locale by default and saves an explicit override from **Language**. English and Simplified Chinese cover the launcher, recorder controls, shortcut settings and core Frames/Timing/Transform editor controls. Migrated existing notices update with the language; advanced tools and some forms still need translation.

The 29 entries are translation targets, not 29 completed catalogs. Unavailable translations visibly fall back to English while preserving the selected preference. See the [localization plan](docs/LOCALIZATION-PLAN.md).

![Switching between English and Chinese in the real app with automatically saved language preferences](docs/assets/language-switch.gif)

## Requirements and limits

- Linux x86_64, an X11 or Wayland desktop, and an OpenGL ES / Vulkan-capable driver. The portable package is not static; it needs [system libraries](packaging/linux/README.txt).
- Wayland recording needs PipeWire and an appropriate xdg-desktop-portal backend. Sharing always follows the desktop's permission flow.
- Video import additionally needs system `ffmpeg` and `ffprobe`. Screen recording, image/GIF editing and built-in GIF export do not.
- GIF is the only export format; audio is not recorded. Camera support is implemented, but physical-device acceptance is still pending.
- Isolated GNOME/X11 workflows have been exercised end to end. Physical GNOME/KDE, mixed-DPI/multi-monitor setups, long high-resolution recordings and some effect-fidelity cases remain open. This is not a claim of complete ScreenToGif parity or zero defects.

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

[Report issues](https://github.com/tiansongyu/gifromscreen/issues) with reproduction steps, distribution, X11/Wayland, version and monitor setup. Remove personal information from logs and projects first. See the [architecture](docs/DESIGN.md), [feature comparison](docs/FEATURE_MATRIX.md) and [next iteration](docs/NEXT-LINUX-ITERATION.md).

Licensed under [MIT](packaging/licenses/LICENSE-MIT) or [Apache-2.0](packaging/licenses/LICENSE-APACHE). This independent implementation does not reuse ScreenToGif branding. Some numerical algorithms are ported from dotnet/WPF under MIT; see the third-party [NOTICE](packaging/licenses/NOTICE.txt).
