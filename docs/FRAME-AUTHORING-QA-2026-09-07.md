# Frame-owned authoring acceptance — 2026-09-07

This is evidence for the connected authoring, conversion and replay workflows, not a complete Linux release or zero-defect certificate. Geometric ownership after later transforms and the physical desktop gates remain separate.

## Controlled fixture and isolation

The repository's private GNOME harness created `/tmp/gfs-wayland-qa.2e265rsk`, supervisor 1036068 with start ticks 153353644, owned Xvfb `:99`, private bus/PipeWire/Portal services and a 2400-second lifetime. No host services were replaced. The application was controlled through ordinary visible native UI interactions.

The project fixture was produced through the public Rust domain/project APIs by `/tmp/gfs-authoring-native.JIshQQ`. A separate `source.gfsproj` remained untouched; its copy in the lab was edited. It contained three 320×160, 100 ms frames, known source-clock identity 456, deliberately synthetic `Ctrl+C` at source sample 0, an event-empty sample at 100,000 µs, and a later `B` at 200,000 µs. A legacy timed progress layer had width 10 and no frozen progress style. These events were **not captured from a physical keyboard**; the run verifies native editor interaction with controlled recorded metadata.

Initial frozen desktop SHA-256: `a5b3798e39fb91222676b546e798810a6f42009ed24d21e8ce5bf5432f543c3a`. A later same-lab supervised app used `a54c8b686cd9f10724032067de25f24b090b6700f0f8803abca876e5e5e286f7`, adding disambiguated group selection. Helper processes 1040886 and 1062936 reported normal exits. The main lab was explicitly stopped; its live handle returned `LAB_EXIT ... status=0`, and final status reported `stopped` and `cleanup_complete: true`. Projects, GIFs and screenshots remain for inspection.

## Explicit legacy conversion

With only the first frame selected, the Layers page displayed the legacy progress layer as one time-anchored item. Clicking **Attach to frames** converted the complete layer into three frame-owned marks, preserving its TrackId, opacity 230 and layer position. Stored progress values became 0 / 333333 / 666666 millionths with no exact fraction field, retaining the legacy rounding policy.

Native Undo restored the timed layer; Redo restored frame ownership. After normal app close, the CLI recovered revision 4 and exported `converted.gif`. It was byte-identical to `before.gif`: 1118 bytes, three images, 300 ms total. Thus conversion was not inferred from metadata alone. Relevant screenshots: `authoring-legacy-layer.png`, `authoring-converted.png`.

## New input authoring, copying and regeneration

The reopened project selected all three frames and used **Recorded keys**, 500 ms hold and 20 px font. The actual Add annotations button saved a new frame-owned group at revision 5, with labels `Ctrl+C`, `Ctrl+C`, and `Ctrl+C  B`. Its three cells shared one 435-byte input pool, with owner prefix counts 1 / 1 / 2. The event-empty middle owner retained source sample 100,000 µs and prefix 1.

The native frame UI then copied the middle frame, deleted the original first and third frames, pasted the snapshot, and deleted the remaining original middle frame. Only copied owner `5d34b2f17fcd4da7acf61f1625ab2fcb` remained. Its raw key event array was empty; its source clock/sample and replay prefix stayed unchanged, and `Ctrl+C` remained visible.

The copied group's real re-edit controls exercised the source-time boundary:

| Revision | Requested edit | Result |
| --- | --- | --- |
| 9 | Hold 1 ms | No visible key mark; the owner, scope and prefix reference remain |
| 10 | Hold 500 ms, font 24 px | `Ctrl+C` is restored at the larger font; later `B` is absent |
| 11/12 | Native Undo/Redo | Empty/visible states restore correctly |
| 13 | Reopen and update saved group again | Same visible `Ctrl+C`, with no need for deleted source frames |

After revision 12, CLI reopen/export produced `held-copy.gif`: one 320×160 image, 1220 bytes, 10 GIF ticks (100 ms). After reopening the GUI and updating again, `held-copy-reopened.gif` was byte-identical. The application recovered the journal over the older snapshot; an explicit native **Save checkpoint** then wrote revision 12 with the single copied owner and empty raw key array. A stale snapshot alone was not treated as the current project state.

Screenshots include `authoring-keys-preview.png`, `authoring-source-removed.png`, `authoring-only-copy.png`, `authoring-short-hold.png`, `authoring-held-copy-restored.png`, and `authoring-recovered-project.png`.

## Same-name selection fix

The run exposed an actual usability ambiguity: original and copied groups both appeared as `Recorded keys`. The corrected menu shows `Layer 2 · Recorded keys · 0 frames` and `Layer 4 · Recorded keys · 1 frame`. The selected label uses the same description, while the actual identity remains TrackId and project names are unchanged.

The later native app selected Layer 4 and loaded its saved 500 ms / 24 px settings without changing the journal, then successfully regenerated it. `authoring-duplicate-group-names.png` records the initial ambiguity; `authoring-distinct-groups.png` and `authoring-distinct-selected.png` record the repair. The text selector uses the same label helper and has separate actual-egui click regressions.

## Replay data and privacy

The pool `f750e4ff132495848f711b92944db3f789c1d0fcfccb2fcc4134f3f941055f73` intentionally still contains the later synthetic `B`, but the retained owner's prefix never consumes it. Shared pools avoid a full history copy per frame; they are not a data-sanitization mechanism. An editable project can retain unconsumed events and historical assets after frames are removed. Use GIF export to exclude replay metadata, and review visible captured pixels/labels before sharing. The app's authoring form states this boundary.

## Source checks

The final source passed 1066 default workspace tests on Rust 1.98, with six opt-in cases excluded from that run. Strict workspace/all-target/all-feature Clippy, formatting and Rust 1.88 all-target/all-feature checking passed. Focused minimum-version checks also covered editor/text workflows and project copies. Coverage includes ordinary authors, legacy behavior, exact conversion pixels and budgets, cancellation, pool integrity/prefixes, source deletion, copied labels, hidden assets, non-raster project insertion, Save As/GIF separation, and same-name selector identity.

Native keyboard capture, mixed DPI, KDE, hardware rendering and device interruption were not newly established by this synthetic-input editor run. Previous capture evidence remains separately documented.
