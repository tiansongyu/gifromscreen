# Changelog

Release notes describe shipped behavior. Plans, experimental APIs and unfinished
branches are not completed features. [中文工作总结](docs/WORK-STATUS.md) ·
[All releases](https://github.com/tiansongyu/gifromscreen/releases)

## 0.1.1 — Unreleased graphics startup fix

- X11 now considers both OpenGL and Vulkan. It retains the verified GL
  preference but skips adapters incompatible with the actual window surface,
  required device limits or features, allowing a compatible Vulkan adapter.
- Explicit `WGPU_BACKEND` overrides remain strict; `WGPU_POWER_PREF` ranks
  compatible devices within the preferred backend. Existing software adapters
  may be selected, without downloading or installing drivers.
- The portable desktop smoke now exercises automatic selection and checks that
  the window stays alive, instead of forcing GL and accepting an initial window
  which may disappear on graphics initialization failure.
- [Cause, validation and scope](docs/GRAPHICS-BACKEND-STARTUP.md). The frozen
  v0.1.0 assets are unchanged; this fix is not retroactively in that download.

## 0.1.0 — 2026-09-09

First formal Linux x86_64 release; feature scope frozen for publication.

- Local screen recording, frame editing and GIF export in Rust, with X11 and
  Portal/PipeWire Wayland capture backends.
- Separate X11 recording border and controls; move a fixed-size region during
  recording or pause, with recording and playback timing handled separately.
- Import, annotation, effects, drawing-board recording, undo/redo, incremental
  project recovery and reusable export settings.
- Since Preview 3: multi-object Vector v1 canvas, expanded English/Chinese
  editor localization (848 messages), text/title and watermark workflows,
  idle color-picker drift fixes, and narrow-editor/header layout fixes.
- System locale by default; persistent System + 29 language choices. Only
  English and Simplified Chinese have application translations; the other
  27 currently fall back to English. Some UI remains untranslated.
- Ubuntu 22.04 / glibc 2.35 portable archive, desktop app and CLI, checksums,
  optional user-level installer and third-party license materials.
- Project schema 8 / Vector v1 remain the shipped format. Unfinished schema 9 /
  Vector v2 integration is preserved on a separate archive branch, not shipped.

Known limitations include narrow-launcher text overlap at 150% zoom, unfinished
WPF pixel parity, incomplete physical-desktop/camera/multi-monitor qualification,
unsigned packages, and no published AppImage, Flatpak or macOS version.

[Full release notes](docs/releases/v0.1.0.md) ·
[Completed and deferred work](docs/WORK-STATUS.md) ·
[Changes since Preview 3](https://github.com/tiansongyu/gifromscreen/compare/v0.1.0-preview.3...v0.1.0)

## Earlier previews

Development previews remain available as historical artifacts, not the current
download recommendation. The dates below use GitHub's UTC publication dates;
QA records may also show the following day in Asia/Shanghai:

- [Preview 3](https://github.com/tiansongyu/gifromscreen/releases/tag/v0.1.0-preview.3)
  — 2026-09-08.
- [Preview 2](https://github.com/tiansongyu/gifromscreen/releases/tag/v0.1.0-preview.2)
  — 2026-09-08.
- [Preview 1](https://github.com/tiansongyu/gifromscreen/releases/tag/v0.1.0-preview.1)
  — 2026-09-08.
