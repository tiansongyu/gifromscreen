# Linux iteration: live sources, motion edits, and portable delivery

Code baseline: `c8fb5a0`. This records a completed implementation/verification batch, not complete ScreenToGif equivalence or proof that no bugs remain.

## New working flows

- Camera: enumerate V4L2 devices without opening them, explicitly preview/record, pause/resume, stop/save, and discard. Requested resolution/FPS must be supported by the device; failures are surfaced. No audio is captured. No physical camera is available on the verification host, so device acceptance remains pending.
- Board: fixed-size transparent or solid canvas, pen, highlighter, eraser, automatic-FPS or completed-stroke capture. Repeated samples in one highlighter stroke do not accumulate opacity; separate strokes may overlap. Erasing restores the chosen canvas background. Pause time is excluded.
- Camera/board share a two-frame bounded background writer. Normal shutdown saves captured frames. Only explicit discard removes the newly created project; directory device/inode ownership is checked before removal. Zero-frame stops create no project, and failures preserve recoverable data.
- Cinemagraph: bake the current frame as a static baseline, allowing motion inside a rectangle (or its inverse). Transparent pixels and discontinuous selections are handled correctly. This is explicitly a baked rectangular edit, not arbitrary painted-mask authoring.
- Smooth loop: append a bounded crossfade from the final rendered frame to the first; exact first-frame pixels are reached at the end. Original overlays do not get applied twice. Both motion operations run rendering, asset persistence, and journal commit off the UI thread, retain the editor workspace and undo history on failure/cancellation, and support undo/redo/reopening.
- Save As: capture a source snapshot, stream and verify its assets into a new independently identified project, retain the original project and lock, and do not automatically switch the editor. Existing, aliased, or nested destinations are rejected; cleanup only targets the invocation's own directory.
- Recent projects: bounded to 20 entries/64 KiB, with background I/O, an advisory lock, atomic writes, missing-file indicators, and explicit removal. Corrupt/unknown-version history is preserved rather than overwritten. Tests and package smoke runs isolate XDG state.

## GIF working memory

Local export renders only needed frames and active overlay assets. Global export now also uses a replayable source: bounded palette analysis, then encoding; NeuQuant uses an extra bounded sampling pass when required to preserve its original 65,536-sample behavior. Duplicate merging, dithering, transparency, timing, loop settings, and transition pixels match the prior buffered path in byte-for-byte tests.

The stronger Global regression uses 151 adjacent-distinct 1280×720 frames (556,646,400 logical RGBA bytes), a 12 MiB renderer budget, and an old one-shot global-buffer limit of one byte. It exports 151 real frames successfully, with exact decoded pixels/timing. Standalone peak RSS was 42,172 KiB; runtime was approximately 21.37 seconds. Source assets are reused, so this is not a high-entropy disk/encoder throughput benchmark. Metadata still scales with frame count; pixel, histogram, and encoder working memory do not.

## Verification

- Full workspace: 758 tests passed, zero failed; three opt-in long/environment tests excluded from the default run.
- Full-workspace Clippy with Rust 1.98.0: passed with `-D warnings`.
- Full-workspace locked check with Rust 1.88.0: passed.
- Video/FFmpeg process tests, frozen camera control tests, board sampling/brush tests, directory-replacement protection, motion transparency/gaps/undo tests, and project-copy/history corruption tests are included.
- Native Xvfb: a real pointer-drawn V stroke was recorded, paused, and stopped into a seven-frame editable project. Its accumulated duration was about 0.71 active seconds; time spent paused during verification was excluded. The project appeared in recent history. Camera controls correctly disabled acquisition and displayed the no-device explanation.
- Earlier native checks in this iteration also imported a real video, inserted a second project and undid it, and displayed a paused transition step.
- The known external-XDestroyWindow `winit` robustness issue and unverified compositor/hardware cases from the earlier QA record are not erased by these results.

## Current downloadable package

Built from clean Rust source `c8fb5a0` with Rust 1.88.0 on the glibc-2.35 baseline:

`target/package-current/gifromscreen-0.1.0-linux-x86_64.tar.gz`

- Size: 11,208,108 bytes.
- SHA-256: `ee75625ede46433c3616943ea5a3b02184547a136ba3fa67c90e64cc42a4d498`.
- Dependency inventory: 316 entries, including license/font notices.
- A second repackaging produced an identical archive.
- All 12 portable installation/integrity tests and the packaged native-window smoke test passed.
- FFmpeg remains an optional system dependency; it is not bundled. Checksums are supplied, but a signed stable release and precise SPDX SBOM are not claimed. AppImage/Flatpak delivery remains pending.

See [PACKAGING.md](PACKAGING.md) for installation and [LINUX-STATUS.md](LINUX-STATUS.md) for the remaining feature/release gates. Remaining work includes input-event/progress overlays, automatic editing tasks, more complete mask/typography tools, localization, global shortcuts/tray behavior, and real desktop/device acceptance.
