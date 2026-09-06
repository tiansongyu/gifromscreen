# Native Linux playback timing acceptance — 2026-09-07

This is bounded acceptance evidence for capture/playback separation, not a complete ScreenToGif parity or zero-defect certificate. The behavior contract and pinned upstream audit are in [Capture cadence and GIF playback timing](CAPTURE-PLAYBACK-TIMING.md).

## Environment and isolation

The repository's `scripts/qa/wayland_nested.py` created `/tmp/gfs-wayland-qa.f4lssjb6`: owned Xvfb `:99`, nested GNOME, a private session bus, private PipeWire/media-session, the real desktop/ GNOME Portal, and the repository's animated GTK fixture. The supervisor was 791523 with start ticks 152564143. The lifetime was bounded to 2400 seconds. The normal system chooser was used to authorize the fixture window; no Portal permission responses were fabricated. Host desktop/audio/input services were not replaced.

After acceptance, both extra-app supervisors reported normal application exits. Explicit lab stop returned `LAB_EXIT ... status=0`; the final status is `stopped`, reason `lab stop requested`, with `cleanup_complete: true`. Test projects, GIFs and logs remain for inspection; the isolated desktop services are no longer running.

The initial frozen desktop binary had SHA-256 `8f22151deb954ce6db39b20b92e30f9e5ca418b963157b9783d381368d98b9cd`. It includes the new fixed-playback policy and recording clock identity, but predates the subsequent prepared-page layout repair and stricter invalid-clock append validation. Those later changes must not be inferred from this first run.

## Manual fixed playback, duplicate snapshots and paused crop movement

The form selected Manual snapshots with the default fixed 1000 ms playback delay. The normally authorized source was 692×509. The selected canvas was 200×100 at source-local (100,150), wholly within the red fixture quadrant.

1. Start/countdown produced zero saved frames without an explicit snapshot.
2. Two widely separated snapshot clicks retained two identical red frames. They share one immutable asset but have separate frame identities and durations.
3. Pause acknowledged before moving the fixed-size crop to (400,150), in the green quadrant.
4. Resume alone left the count at two; the third explicit snapshot captured green.
5. Stop restored the editor with three 1000 ms frames. A normal application close released the project lock.

`output/manual-fixed.gfsproj/manifest.json`, revision 3, records:

| Frame | Raw `captured_at` and clock `sampled_at` (µs) | Crop origin | Playback duration (µs) |
| --- | ---: | --- | ---: |
| 1 | 15,005,730 | (100,150) | 1,000,000 |
| 2 | 31,153,592 | (100,150) | 1,000,000 |
| 3 | 56,077,585 | (400,150) | 1,000,000 |

All three preserve clock identity `218c1660e0424d35a107eb83cce073a0` and the effective embedded-cursor policy. The successful incremental journal assigned each final 1,000,000 µs delay immediately, with no later duration correction. The raw sample instants remain unequal and are not replaced by a synthetic 0/1/2-second playback timeline.

The CLI reopened the saved project and exported `output/manual-fixed.gif`: 472 bytes, two GIF images after duplicate merging, delays 200 and 100 GIF ticks (red 2 seconds, green 1 second). Total project and GIF playback are both exactly 3 seconds.

Screenshots are retained under the lab's `logs/`: `playback-manual-selected.png`, `playback-controller-ready.png`, `playback-manual-zero.png`, `playback-paused-moved.png`, `playback-resumed-no-frame.png`, `playback-three.png` and `playback-manual-editor.png`. These contain only the isolated fixture and test application.

## Layout defect found during acceptance

At the normal 1280×720 nested surface size, the prepared source preview consumed the remaining vertical space and left exact-region, Open controller and Cancel controls below the visible content. Wheel input in the open space did not reach them. `playback-preview.png` and `playback-preview-controls.png` record that failure. Temporarily reducing application zoom twice made them reachable, allowing the first timing test above to proceed; that workaround was not a layout pass.

The repair gives preparation its own layout, with Open/Cancel above a bounded scrolling body and exact-coordinate controls before the aspect-preserving preview. A second frozen binary, SHA-256 `76792752e8d041151789d33699a39975259a900f222832ed65ab367f738d9503`, was launched in the same lab by supervised `run-app`, helper 813105. At default zoom and after three zoom-in shortcuts, all action/coordinate controls remained visible (`layout-default-fixed.png`, `layout-large-font-fixed.png`). After returning to default zoom, entering (100,150), 200×100 and clicking Apply changed the selection; Open transferred that exact crop to the controller (`coordinates-exact-test.png`, `changes-controller-ready.png`). Native pointer/keyboard interactions, not just text inspection, exercised these actions.

## Periodic fixed playback and changed-only filtering

The second binary selected a 2-second periodic sampling interval, fixed 66 ms playback delay and a 5000 ms capture limit. Start/countdown and automatic stop saved `output/periodic-fixed.gfsproj`, revision 3, with three 640×480 frames. Source sample instants were 31,077 / 2,042,500 / 4,042,110 µs, all sharing clock `afd49d0b75544bd28152f33465411b14`, distinct from the first manual recording. Each clip has duration 66,000 µs, total 198,000 µs. CLI reopen/export produced a 9034-byte GIF with one merged image and 20 ticks (200 ms), demonstrating cumulative rather than per-frame rounding.

The same app then created a separate recording with those timing settings, ChangesOnly enabled, and the exact 200×100 red crop at (100,150). Automatic stop saved `output/periodic-changes.gfsproj`, revision 1, with only one 66,000 µs clip and fresh clock `1ca57e81c29848abb3e3ce35f4760139`. Reopen/export produced a 240-byte GIF with 7 ticks (70 ms). Omitted periodic samples and the capture limit did not extend fixed playback. The previous three-frame project was preserved. The app was closed normally before CLI access; an earlier CLI attempt correctly refused its still-held project lock rather than overriding it.

The full-window periodic samples were also pixel-identical: that run alone did **not** prove ongoing animation rendering for a fully occluded GTK source. Timing/retention evidence is valid for delivered identical samples; freshness must not be inferred from increasing native sample timestamps. Screenshots `periodic-timed-stop.png` and `changes-timed-stop.png` retain the two editor results.

## Visible-versus-occluded source experiment

A third supervised app (helper 841080, the same second binary) used the normal Portal chooser, Continuous 10 FPS with measured playback and a 10-second capture limit. The maximized controller initially covered the fixture. Five seconds after clicking Start, including the configured 3-second countdown, a normal Alt+Tab brought the fixture in front while the same recording session continued. No source reinitialization or second authorization occurred.

`output/freshness-window.gfsproj` stores 100 clips and 72 distinct pixel assets. The first 22 samples, at 30,000 through 2,132,133 µs, share the covered window's frozen image. At 2,240,658 µs the image changes, and subsequent samples continue producing new assets through 9,943,094 µs. The experiment rules out always replaying the prepared frame in this session and establishes visibility-associated source freshness; it does not locate the precise GTK/GNOME frame-callback mechanism or guarantee hidden-window redraw for other applications.

Automatic stop restored the editor. After normal application close, CLI reopen/export produced revision 200, 72 GIF images, 650,243 bytes and exactly 1000 GIF ticks (10 seconds), matching the 10,000,000 µs project. `freshness-controller.png`, `freshness-visible-source.png` and `fixture-after-close.png` retain the UI observations. The controller now explains that selected windows should remain visible and that sample timestamps alone do not prove refreshed pixels; the source-specific advisory has an egui rendering regression. A compact-controller workflow remains usability work, not a claim to force another application to repaint.

## Limits

These runs do not validate physical display devices, KDE, mixed-DPI/multi-monitor behavior, long-duration capture, DMA-BUF, or globally observed Wayland keyboard/mouse events. Synthetic workflow tests separately cover measured timing, minute/hour sampling, missed samples and batch/incremental journal recovery. A native case proves only the paths actually exercised above.

## Source regression checks

The integrated source passed 963 workspace tests with six opt-in tests excluded from that default run on both Rust 1.97.1 and 1.98.0. The desktop contributes 387 tests, including small-window/enlarged-font hit testing; workflow contributes 51. Strict workspace/all-target/all-feature Clippy, formatting, whitespace checks and Rust 1.88 workspace/all-target/all-feature checking passed. The four new application timing integration tests also passed explicitly on Rust 1.88. The subsequent source-visibility advisory passed all ten controller regressions and strict desktop Clippy. No omitted physical-device or multi-gigabyte opt-in test is counted as a pass here.
