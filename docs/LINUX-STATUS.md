# Linux implementation status

This file is the delivery ledger for the Linux-first implementation. Checked implementation items describe the stated scope, not complete ScreenToGif parity or release certification. See [the evidence-based parity audit](PARITY-AUDIT.md) for GUI/API distinctions and platform validation gaps.

## Current iteration

- [x] Freeze ScreenToGif 2.43.2 as the behavioral reference.
- [x] Select the Rust desktop architecture and define the Linux capability boundary.
- [x] Initialize a Rust workspace, desktop shell, diagnostics CLI, and recorder state machine.
- [x] Complete the domain model and crash-recoverable project store.
- [x] Complete a built-in GIF encoder vertical slice and round-trip tests.
- [x] Complete capture contracts, a synthetic source, and Linux runtime detection.
- [x] Connect the CLI vertical slice: synthetic capture -> project -> GIF.
- [x] Connect the first desktop X11 record-to-GIF vertical slice.
- [x] Hide the main UI while a standalone recorder frame is active.
- [x] Move and resize the recorder frame before capture; keep its border and controls outside the GIF.
- [x] Move the fixed-size X11 capture area during countdown, recording, or pause without restarting the session.
- [x] Start, pause, resume, stop-and-save, and discard from the recorder frame.
- [x] Persist stopped recordings as recoverable editable projects instead of flattening them immediately.
- [x] Edit frame selection, ordering, deletion, and variable delays in a virtualized timeline.
- [x] Render the current frame preview and export all or selected frames through a cancellable background GIF job.
- [x] Reopen existing projects with journal-recovery and asset-integrity reporting.
- [x] Import animated GIF files into editable projects with bounded decoding.
- [x] Import PNG, JPEG, BMP, and WebP files into editable projects with bounded decoding.
- [x] Crop, resize, rotate, and flip selected frames with journal-backed undo/redo.
- [x] Select, keep, or delete an explicit half-open time range with variable-frame timing.
- [x] Reduce frames, build Yoyo loops, scale delays, and remove rendered duplicates.
- [x] Negotiate a real Wayland ScreenCast Portal session through the PipeWire FD handoff.
- [x] Stream X11 recording frames into the recoverable project journal while capture is active.
- [x] Consume mapped Wayland PipeWire frames through a bounded native capture session.
- [x] Continue a Portal-prepared Wayland session into recording without reopening the chooser.
- [x] Hide the main UI and use a standalone Wayland frozen-preview crop controller with countdown, live fixed-size movement, pause/resume, stop, and discard.
- [x] Drop setup frames at the Wayland recording boundary and translate cropped damage, cursor, and input metadata.
- [x] Cut, copy, and paste bounded frame selections with immutable-asset reuse.
- [x] Show overflow-safe timing, selection, canvas, and asset statistics.
- [x] Route dropped project, GIF, PNG, JPEG, BMP, and WebP paths into background jobs.
- [x] Create, replace, remove, and GIF-export Fade/Slide transitions.
- [x] Play the export-expanded Fade/Slide sequence in the editor preview, with matching step timing, endpoint overlays, and pause/resume.
- [x] Create transparent or solid blank animations from a bounded desktop form.
- [x] Import ordered static-image sequences with timing, loop, reordering, and multi-file drop controls.
- [x] Retain, select, remove, and clear a bounded multi-entry frame clipboard history.
- [x] Preview and export timed raster, shape, and pressure-drawing overlays through one compositor.
- [x] Expose bounded Wu/custom palettes and deterministic Dotted, Blue Noise, and Interleaved Noise dithering in desktop export controls.
- [x] Trigger bounded manual snapshots and configure continuous or second/minute/hour periodic capture from the standalone recorder.
- [x] Replace quadratic per-frame manifest cloning with indexed recording journal paths and a 512-frame checkpoint cadence; pass the 10,000-frame durability preflight.
- [x] Author and remove timed line, arrow, rectangle, and ellipse overlay tracks with journal-backed undo/redo.
- [x] Draw a bounded freehand stroke directly on the frame preview and commit it as a timed, undoable overlay track.
- [x] Decode and author bounded raster-watermark tracks with atomic asset registration and undo/redo.
- [x] Keep manual capture controls responsive while a source stalls; return unused PipeWire buffers.
- [x] Retime overlays through frame insertion/deletion/duration changes with exact undo/redo.
- [x] Apply overlays only to the actual contiguous selected spans, leaving selection gaps untouched.
- [x] Anchor asynchronous watermark/text authoring and drawing drafts to their original project and selection.
- [x] Author shaped multilingual text with persistent source attributes and immutable raster pixels.
- [x] Render asynchronous thumbnails only for the visible timeline range with bounded queues and cache.
- [x] Loop the frame preview on an accumulated clock and skip late frames without timing drift.
- [x] Group editing controls by task and preserve the OS light/dark theme.
- [x] Keep the preview beside a bounded tool inspector on wide windows; put preview first on narrow windows.
- [x] Re-edit saved text while preserving track/item identities, timing, z order, and blend settings.
- [x] Insert undoable title frames at the beginning or after a chosen frame, excluding existing overlays from the title interval.
- [x] Reuse validated raster assets across frame, overlay, and mask roles without changing their persisted descriptors.
- [x] Import local video intervals through supervised FFmpeg/ffprobe with start, duration, FPS, size, progress, cancellation, and partial-project recovery.
- [x] Insert a same-canvas recorded/imported project with remapped identities, overlays, transitions, and journal-backed undo/redo.
- [x] Browse for projects, GIFs, images, and videos using native Linux file dialogs while retaining manual-path fallback.
- [x] Stream local-palette GIF exports through a bounded frame/transition working set instead of retaining the whole rendered animation.
- [x] Stream global-palette project exports through replayable analysis/encoding passes while retaining the original palette and sampling behavior.
- [x] Save/load/update/rename/delete complete project-local GIF export presets with undo/redo, without restoring file paths or overwrite authorization.
- [x] Return to an already-open editor from the launcher; pause preview when leaving the editor.
- [x] Record a drawing board using pen/highlighter/eraser, automatic or completed-stroke sampling, pause/resume, stop/save, and explicit discard.
- [x] Enumerate Linux V4L2 cameras and provide opt-in preview, recording, pause/resume, stop/save, and discard (simulated-source acceptance; physical cameras still need verification).
- [x] Bake rectangular Cinemagraph motion regions and append Smooth-loop crossfades in an exclusive background edit with exact undo/redo.
- [x] Save an independently identified project copy without switching the current editor, and retain a bounded recent-project list.
- [x] Build and verify an Ubuntu-22-compatible x86_64 portable tarball with exact-file installation, checksums, license notices, and CI artifacts.

## Delivery gates

### S0 — architecture validation

- [x] X11 real screen/region capture through GetImage.
- [ ] GNOME Wayland Portal + PipeWire capture.
- [ ] KDE Wayland Portal + PipeWire capture.
- [x] Bounded queue and dropped-frame timing compensation.
- [ ] Long-recording frame-store benchmark.
- [x] Initial GIF timing, loop, transparency, delta and disposal round-trip corpus.
- [x] 50,000-frame virtualized timeline prototype.

### M1 — usable alpha

- [x] X11 monitor, window, and region recorder.
- [x] Countdown; record, pause, resume, stop, and discard are complete.
- [x] Project autosave and crash recovery for active X11/Wayland recordings and editor changes.
- [x] Filmstrip selection, delete, reorder, reverse, delays, undo/redo.
- [x] Crop and resize.
- [x] GIF colors, loop, duration, duplicate merge, progress, cancellation.
- [ ] Flatpak and AppImage preview packages.

### M2 — editor parity

- [x] Window capture and capture-only-changes.
- [x] Manual and periodic snapshots.
- [x] Duplicate removal, frame reduction, Yoyo, and delay scaling.
- [x] Text-caption authoring, shaping, durable raster assets, preview, export, and undo/redo.
- [x] Existing-text editing and title-frame insertion.
- [x] Raster-watermark authoring with bounded background decoding.
- [x] Bounded free-drawing authoring on the rendered preview.
- [x] Shape overlay authoring with bounds, stroke/fill, opacity, blend mode, and z-order.
- [x] Border, shadow, blur, pixelate, darken, and lighten frame effects.
- [x] Raster watermark, shape, and pressure-drawing preview/export rendering.
- [x] Fade and slide transitions.
- [x] Palette, dithering, transparency, and delta-frame controls.
- [x] PNG/JPEG/BMP/WebP/GIF import.
- [x] Bounded multi-entry frame clipboard history and project statistics.
- [ ] Reusable editing/export presets.

Export presets are implemented; reusable editing-action presets/automatic tasks remain pending.

### M3 — complete content sources

- [x] Webcam recorder implementation and simulated-source control tests.
- [ ] Physical webcam device/mode/permission acceptance.
- [x] Drawing-board recorder, including native pointer-drawing acceptance.
- [x] Insert recording/media into an existing project via its saved .gfsproj (same canvas, 1,000 source frames, 512 MiB referenced raster budget).
- [x] Video import (local whitelisted formats through system FFmpeg; bounded duration and disk usage).
- [ ] Cursor, keyboard, and mouse-event metadata with capability fallbacks.
- [x] Rectangular baked Cinemagraph and baked Smooth Loop, with limits and undo/redo.
- [ ] Freeform Cinemagraph masks, progress overlays, and automatic tasks.

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
