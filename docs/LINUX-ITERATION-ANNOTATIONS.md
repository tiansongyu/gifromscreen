# Linux iteration: recorded input, annotations and automatic tasks

Reference: ScreenToGif 2.43.2, commit `a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`. Implementation baseline: `7c13458` (capture/domain/editor) and `0b3fc5e` (desktop integration), followed by the focused fixes recorded in Git. This is an acceptance update to the earlier [parity audit](PARITY-AUDIT.md), not a claim that all rows or release gates are complete.

## New end-to-end paths

- X11 XI2 captures physical key/button events only when the user explicitly enables input recording. The independent event reader is stopped on pause/stop/drop; the recorder does not display **Paused** before backend acknowledgement. Server-generated key repeats and full layout/IME interpretation are not covered by the raw-event implementation.
- XFixes can save straight-alpha cursor pixels and hotspots separately from the frame. Both batch and incremental project writers preserve the original active capture timestamp, signed capture origin, cursor state/assets, input events and drop counts. Capture-only-changes retains metadata changes.
- New project directories are created with mode `0700`, and new manifests, journals, locks and assets with `0600`. Existing opened files are not retroactively chmodded. The event queue is bounded; dropped events are counted, not silently represented as complete capture.
- Progress, manual/recorded keys and clicks, and manual/recorded cursor annotations have authoring, re-editing, persistence, preview and export paths. See [annotation timing, fidelity and limits](ANNOTATIONS.md).
- Reusable automatic-task presets execute the six task classes actually handled by upstream: mouse events, key strokes, delay, progress, border and shadow. The preset is application-local data, not executable code. New imports/recordings are eligible; existing project opening is not. The entire task chain and its completion record are one undoable edit. See [settings and failures](AUTOMATIC-TASKS.md).

Wayland still uses the permitted Portal/PipeWire path. Embedded and hidden cursor choices are handed through preparation without reopening the chooser; editable cursor metadata and passive global input are not supplied by the current reader. Manual annotation is available independently of capture permissions.

## Smooth Loop correction

The previous implementation appended a fade toward the first frame. That remains useful, but its correct name is **Loop crossfade**. It is no longer presented as upstream Smooth Loop.

The new **Smooth loop search** follows upstream [`SmoothLoopAsync`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs) and [`CalculateDifference`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/ImageUtil/ImageMethods.cs): compare the first rendered frame with candidates starting after a skip threshold, scan forward or backward, keep the first candidate meeting the requested fraction of **exactly equal RGBA pixels**, and delete frames after it. No match or a match at the final frame leaves the project revision/history unchanged. Similarity is not average color distance. Existing overlays and transitions use the normal deletion/retiming command and restore exactly on undo.

The worker holds only the reference and current candidate surfaces, with cancellation and limits of 100,000 frames and 64 MiB per surface. Time lookup uses one linear prefix pass rather than repeatedly scanning the timeline. Tests cover both search directions, an exact 75% boundary, nearly-but-not-equal pixels, cancellation and no-op history.

## Native-window acceptance

Executed on an isolated Xvfb X11 display, with a separate `XDG_STATE_HOME` and temporary project directory. This is real native-window/renderer verification on a virtual X server, not GNOME/KDE, mixed-DPI or physical-device certification.

1. Create and save an automatically enabled preset containing a 100 ms delay task in the GUI; restart the application with `--import-gif` and a generated 36-frame, 50 ms/frame input.
2. Verify all imported filmstrip frames show 100 ms. One **Undo** restores 50 ms; **Redo** restores 100 ms. Journal inspection confirms `task_runs` is added, removed and restored in the same compound edits as timing.
3. Select all frames, author a progress bar and `{frame} / {frames}` label. The native preview and thumbnails show the first-frame partial bar and labels.
4. Close normally; export the journal-recovered project through the CLI. `ffprobe` reports a 160×96 GIF, 36 frames, 3.600 seconds.
5. Reopen the existing project. It remains at revision 5 with four journal entries: automatic apply, undo, redo, annotation. Tasks do not rerun. Navigate to the final frame and verify `36 / 36` and a full bar.
6. Verify the new project and asset directories are `0700`; manifest, journal and project lock are `0600`.
7. Resize the native window to 740×520 and open the automatic-task page. Its form is expanded by default, wraps descriptions and scrolls to the save/reload controls.
8. After the exact-fraction rendering correction, export the same legacy progress project again. Both GIFs are byte-identical (`74,276` bytes; SHA-256 `8e2bc7b469ed1937acc6e804a5f222c101129bdf504b1e44d55ff59bd255d7f6`).

Local run artifacts are under `/tmp/gfs-input-qa-p4Yvsc` and are not required project inputs or committed test fixtures. Automated regression tests provide the durable repeatable evidence.

## Boundary fixes found during this iteration

- Empty recorded-event tasks skip in a small canvas before validating an unused label box; subsequent delay/border tasks still execute. Genuine events and explicit out-of-canvas progress settings still report validation errors rather than silently resizing saved parameters.
- Manual/periodic sampling may first deliver an event long after its native timestamp. Its label now gets its hold interval from the first visible sampled frame; already displayed labels still expire, future timestamps do not render early, and selection/clock discontinuities remain boundaries.
- Closing the app after **Save presets** waits for the settings worker to finish, rather than abandoning an in-flight save.
- Newly added annotation tasks initialize sensible bounds from the open canvas, without modifying previously saved task parameters.
- Text-only progress retains its chosen background with a single alpha composition. Newly authored progress uses exact fractions and ties-to-even pixel/percentage rounding; old progress records retain their original pixels. Cursor modes no longer validate or display unrelated text styling fields.
- Recorded shortcut labels collapse modifier prefixes (Ctrl followed by C displays Ctrl+C), without merging distinct keys or genuine release/re-press events.

## Automated acceptance

The final workspace regression run passed **850 tests**, with four opt-in tests excluded from that default run. Rust 1.98 strict workspace/all-targets/all-features Clippy, formatting checks, and Rust 1.88 all-targets/all-features checking also passed.

All four opt-in cases were run separately: native isolated-X11 input lifecycle, 10,000-frame incremental durability, 1,000 distinct 720p frame persistence/recovery, and installed-font Chinese/Arabic shaping. Native input passed ten repeated CI-shaped runs; the camera preview/record/pause/resume test passed twenty consecutive runs after correctly accounting for bounded writer backpressure. See [performance measurements](LINUX-BENCHMARKS.md). These tests do not substitute for physical hardware or complete user-interface parity.

The new X11 lifecycle check now runs in Linux CI on its own Xvfb server. Both remote Linux CI and portable-package jobs passed at `dae7ba3`; Linux CI also passed for the final source baseline [`7f1b5c7`](https://github.com/tiansongyu/gifromscreen/actions/runs/34038953495). The portable checks below were run locally; they do not claim success for a later remote workflow that is still running.

## Portable artifact verification

Built from clean source `7f1b5c7d9a4f21f17f23124b1e51873dad02288d` with Rust 1.88.0:

- Local archive: `target/package-annotations-20260906/gifromscreen-0.1.0-linux-x86_64.tar.gz`.
- Size: `11,757,616` bytes; SHA-256: `7a04f7cb4eafdbe8bf1c23688f15d2bb2d0879a3351e19cd42c7cb45a6612e5d`.
- Build receipt: source and package trees clean; 316 dependency inventory entries; desktop ELF requires at most GLIBC 2.35 (CLI 2.34).
- All 12 archive/install/uninstall tests passed. Independent repackaging was byte-identical.
- Packaged desktop launched successfully on a fresh Xvfb server, then passed five more fresh-server runs. The original reused display also passed a retry.

The first packaged launch on the reused long-lived display failed once with `XOpenDisplayFailed`. The display was subsequently reachable; the failure did not reproduce in the tests above. Its cause is not established, and no application change or automatic retry was used to label it fixed. CI uses a dedicated fresh display. The root-owned long-lived Xvfb was stopped after verification; the synthetic QA images/projects and the two package archives remain available locally.

This is a development preview artifact with the explicit remaining issues below, not a stable release or complete parity declaration.

## Remaining gates

These paths remove several missing-feature categories from the baseline audit, but parameter and platform parity remain partial: richer key-layout/repeat handling, progress alignment/date-format/precision variants, advanced text styles, freeform Cinemagraph masks, global shortcuts and application services, localization/accessibility, full-resolution long-recording benchmarks, physical webcam and GNOME/KDE acceptance, and distribution/release gates. See [Linux status](LINUX-STATUS.md). A passing unit suite does not establish zero bugs or complete Linux delivery.

Specific follow-up acceptance cases retained from the source audit:

- Re-editing an event group currently retains its existing actual marker coverage, not the original full authoring selection. Increasing hold time therefore cannot extend into previously unmarked frames. Preserve the original authoring selection separately before claiming full hold-time editing parity.
- Mouse annotations still need held-button/drag tracking, independent extra-button colors and continuous pointer highlighting; a sampled click marker is not equivalent to those modes.
- Progress still needs centered growth, numbering offsets and the upstream date/time-format options. A manually written literal token format is not a full format editor.
- Cinemagraph baking resets frame transforms but currently retains original capture metadata. Re-authoring recorded spatial annotations after a transformed/baked frame can therefore use stale coordinates or duplicate a baked pointer. The next correction must preserve original metadata for recovery while explicitly retiring or mapping coordinates for the baked output; do not treat current bake-plus-reannotation behavior as validated.
- The same bake path removes selected spans from hidden/zero-opacity overlay tracks even though they did not contribute pixels. Preserve those editable tracks when fixing the bake boundary.
