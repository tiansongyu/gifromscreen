# Linux implementation status

This file is the delivery ledger for the Linux-first implementation. A feature is marked complete only after its acceptance tests pass on the relevant backend.

## Current iteration

- [x] Freeze ScreenToGif 2.43.2 as the behavioral reference.
- [x] Select the Rust desktop architecture and define the Linux capability boundary.
- [x] Initialize a Rust workspace, desktop shell, diagnostics CLI, and recorder state machine.
- [x] Complete the domain model and crash-recoverable project store.
- [x] Complete a built-in GIF encoder vertical slice and round-trip tests.
- [x] Complete capture contracts, a synthetic source, and Linux runtime detection.
- [x] Connect the CLI vertical slice: synthetic capture -> project -> GIF.
- [x] Connect the first desktop X11 record-to-GIF vertical slice.

## Delivery gates

### S0 — architecture validation

- [x] X11 real screen/region capture through GetImage.
- [ ] GNOME Wayland Portal + PipeWire capture.
- [ ] KDE Wayland Portal + PipeWire capture.
- [ ] Bounded queue and dropped-frame timing compensation.
- [ ] Long-recording frame-store benchmark.
- [x] Initial GIF timing, loop, transparency, delta and disposal round-trip corpus.
- [ ] 50,000-frame virtualized timeline prototype.

### M1 — usable alpha

- [ ] Monitor and region recorder.
- [ ] Countdown, record, pause, resume, stop, discard.
- [ ] Project autosave and crash recovery.
- [ ] Filmstrip selection, delete, reorder, reverse, delays, undo/redo.
- [ ] Crop and resize.
- [ ] GIF colors, loop, duration, duplicate merge, progress, cancellation.
- [ ] Flatpak and AppImage preview packages.

### M2 — editor parity

- [ ] Window capture and capture-only-changes.
- [ ] Manual and periodic snapshots.
- [ ] Duplicate removal, frame reduction, Yoyo, delay scaling.
- [ ] Text, title, drawing, shapes, watermark, border, shadow, privacy effects.
- [ ] Fade and slide transitions.
- [ ] Palette, dithering, transparency, and delta-frame controls.
- [ ] Image/GIF import, clipboard history, presets, and statistics.

### M3 — complete content sources

- [ ] Webcam recorder.
- [ ] Drawing-board recorder.
- [ ] Insert recording/media into an existing project.
- [ ] Video import.
- [ ] Cursor, keyboard, and mouse-event metadata with capability fallbacks.
- [ ] Cinemagraph, progress overlays, Smooth Loop, and automatic tasks.

### M4 — Linux release

- [ ] Chinese and English localization.
- [ ] Keyboard navigation and accessibility review.
- [ ] Global shortcuts, tray fallback, CLI automation, updates, diagnostics.
- [ ] GNOME/KDE/wlroots/X11 real-machine matrix.
- [ ] Signed artifacts, checksums, SBOM, notices, and stable release channel.

## Definition of done

The Linux version is complete only when every non-macOS item in `FEATURE_MATRIX.md` is either:

1. implemented and linked to an automated/manual acceptance result, or
2. explicitly documented as unavailable because of a verified Wayland capability restriction, with a usable fallback.
