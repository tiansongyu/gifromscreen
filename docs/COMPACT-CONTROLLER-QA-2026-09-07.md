# Compact Linux recorder acceptance — 2026-09-07

This records the compact-controller change and the bounded evidence for the same source cohort's frame-owned core. It does not close the full Linux release checklist.

## Window behavior

Wayland reuses one dedicated root surface while hiding launcher/editor pages. It now requests a 720×480 native-logical controller, bounded by the prior local window extent, instead of retaining a maximized editor-sized surface. The compositor must acknowledge unmaximizing before resize is requested: winit ignores size requests while its last Wayland configuration is maximized. The geometry transition has a two-second deadline with a visible, non-blocking fallback notice. It never requests global window position or hide/unhide behavior.

App zoom and monitor DPI are separate. Size targets and acknowledgements use stable native-logical units; viewport commands convert through the current app zoom. Stopping or cancelling restores the captured local extent and known maximized state, even if zoom changed during recording. An exit replaces any pending compact request so a late acknowledgement cannot shrink the returned editor.

Primary controls come first in an automatically measured, wrapping toolbar. Tall content scrolls instead of being cut off by the old fixed 92-point height. X11 retains its previous position, decorations and size-restoration command sequence.

## Native nested-GNOME run

The existing private harness created `/tmp/gfs-wayland-qa.k95f0grd`, supervisor 905521, start ticks 152912647, owned Xvfb `:99`, with a 2400-second bound. It used its own GNOME/PipeWire/Portal environment and the normal system window-sharing chooser. Frozen binary SHA-256: `3d2e33e3036a925bc7a550cf4cf712ad601a5a57e2c50ee941103e08286d6df2`. No host desktop or audio services were replaced.

After GUI closure and native X11 checks, the lab was explicitly stopped. Its live execution handle returned `LAB_EXIT ... status=0`; final status is `stopped`, `lab stop requested`, `cleanup_complete: true`. Test projects, GIFs and screenshots are retained; private desktop services are no longer running.

The ordinary full-size preparation page selected a 200×100 crop at (100,150) from the 692×509 fixture. Opening the controller visibly reduced it to the requested 720×480 client area. The source remained partially visible and continued updating. Manual Start/countdown saved no frame until a snapshot was requested.

During recording, three zoom-in shortcuts enlarged the UI while retaining the same window extent. Snapshot, Pause, Stop and Discard remained visible and operable. One red snapshot was saved; after acknowledged pause, dragging the preview moved the fixed crop to (395,150). Resume followed by a second explicit snapshot saved green. Stop restored the original full-size window while retaining the enlarged UI, without a geometry-timeout notice.

`output/compact-manual.gfsproj`, schema 2/revision 2, contains:

| Frame | Source sample (µs) | Crop origin | Playback (µs) |
| --- | ---: | --- | ---: |
| 1 | 71,127,076 | (100,150) | 1,000,000 |
| 2 | 71,919,530 | (395,150) | 1,000,000 |

Both frames retain clock identity `e2150e9b100745efa1dd9495f2d7f483`. After normal application close, CLI reopen/export produced a 503-byte GIF with two 200×100 images, each 100 GIF ticks: exactly two seconds total. The small white moving-fixture details in the crops are expected source pixels, not the controller.

A second prepared session started with the UI already enlarged. Its controller again used the same compact window extent. Closing its native recorder window before Start returned to the original full-size recording settings page; it did not exit the app or create `compact-cancel.gfsproj`. The previous two-frame project remained available.

Screenshots in `logs/` include `compact-ready.png`, `compact-zoom-recording.png`, `compact-paused.png`, `compact-moved.png`, `compact-two-frames.png`, `compact-stopped-restored.png`, `compact-entered-zoomed.png` and `compact-closed-restored.png`.

## Automated coverage and limits

The integrated source passed 1019 default workspace tests on Rust 1.98, with six opt-in tests excluded from that default run. Strict workspace/all-target/all-feature Clippy, formatting, whitespace checks and Rust 1.88 all-target/all-feature checking passed. The controller has 21 regressions, including 66 font/narrow-window click combinations, 16 zoomed click combinations, acknowledgement order, timeouts, interrupted compaction, and exact X11 restoration commands. Frame-owned core regressions cover schema recovery, old journal checksums, reverse/retime, selected copying, hidden assets, Save As, preview/GIF/transition agreement, and non-duplicated baking.

After closing the GUI, the isolated X11 input-lifecycle test was explicitly run on the lab's authenticated Xvfb display and passed. The real X11 capture/movement/window smoke test also passed without a skip. These are additional native checks, not six blanket opt-in passes.

New frame-owned authoring and input-aware re-authoring are still not connected; see [the implementation ledger](FRAME-OWNED-OVERLAYS-PLAN.md). Neither the headless GUI tests nor nested software-rendered GNOME establish KDE, physical mixed-DPI/multi-monitor or hardware rendering acceptance. Compact sizing reduces obstruction but does not guarantee that a compositor can exclude the controller from a monitor recording or force a hidden application to redraw.
