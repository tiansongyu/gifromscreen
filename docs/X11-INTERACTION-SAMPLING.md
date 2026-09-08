# X11 desktop interaction snapshots

## Scope and fidelity

The recording form now exposes **Desktop interaction snapshots (X11)**.
XI2 raw key presses and button presses (including legacy wheel buttons) request
a source sample. Pointer motion and releases do not. Sampling and playback are
independent: the UI defaults to 500 ms per retained frame, as in the
[pinned playback audit](CAPTURE-PLAYBACK-TIMING.md), with measured timing as an
explicit alternative. Changed-only filtering remains optional; in fixed mode
omitted samples add no playback time. No optional pre-capture trigger delay is
implemented yet.

This mode covers the desktop, **including recorder controls**, not only the
selected capture rectangle. The [XI2 RawEvent protocol](https://xorg.freedesktop.org/archive/current/doc/inputproto/XI2proto.txt)
contains no destination window or root pointer coordinates. A later pointer/focus
query cannot establish the original event's recipient. Precise controller-event
exclusion needs separate ordered event evidence; it is not approximated here or
claimed as full ScreenToGif parity. The existing listener negotiates XI 2.0;
delivery during other clients' grabs is limited by that version's contract.
Physical key repeat, smooth-scroll device behavior, multiple seats and wider
compositor/hardware acceptance remain open. Wayland rejects this mode before
opening its permission chooser and keeps continuous/periodic/manual alternatives.

## Ownership, privacy and timing

One XI2 connection/thread is owned by each active X11 recording session. The
same feed optionally retains annotations; enabling triggers does not enable
input-event storage. Trigger-only mode does not read key labels/modifier maps,
query pointer positions or construct persistent key/click events. Dispatch
retains just one pending bit. Bursts coalesce while sampling is busy, with no
unbounded per-event screenshot backlog or artificial fixed FPS cap.

Metadata mode retains its existing 512-event bounded queue and loss accounting;
trigger and optional metadata become visible under the same short queue lock.
No native request is performed under that lock. Pause drops the listener and
pending input; Resume creates a fresh subscription. Stop/Discard/failure removes
the listener. Its private socket is shut down before joining, so a stalled
QueryPointer cannot hold teardown indefinitely. Trigger polling waits at most
100 ms per call, usually less under the collector's configured poll interval.

`CaptureSession::active_elapsed()` defaults to unknown for compatibility. X11
reports its pause-excluded monotonic session clock. Interaction duration limits
arm before the first sample and deduct elapsed time for a prestarted session;
attaching a collector does not restart that clock. Input-free recordings thus
expire. Pauses shift the collector deadline, and a sample returned after the
deadline is not admitted. Existing non-interaction cadence timing is unchanged;
in particular manual first-snapshot deadline semantics are a separate open audit.

No input before timeout produces the existing explicit empty-capture error, no
GIF and a retained empty project. It is not represented as a successful GIF.
Recorder notices now sit above the settings scroll area in their own bounded
status area, so errors and retained project paths remain discoverable.

## Automated evidence

- Two private-Xvfb integration tests own their display: keyboard/button/wheel
  samples, real pixels, trigger-only empty metadata, pause/drop/restart, target
  movement, active-clock pause exclusion and metadata-enabled delivery. Both
  pass on Rust 1.98.0 and 1.88.0 and match CI's `private_xvfb_ -- --ignored` filter.
- Two listener unit tests exercise 100,000 coalesced triggers and atomic optional
  metadata delivery without a second subscription.
- Four workflow regressions cover buffered, buffered+sink, sink-only and
  prestarted routes, fixed playback, unchanged filtering, raw clocks, cancellation,
  idle timed stop, initial pause and already-spent native active time.
- Desktop tests cover independent 500 ms defaults, invalid bounds, Wayland
  rejection before permission work, and visible recorder status at 640×480.
- Rust 1.98 workspace/all-targets/all-features: 1,531 pass, 30 explicitly ignored;
  strict Clippy passes. Rust 1.88 full suite passed before the final status test,
  followed by all 627 desktop tests passing with that final change.

## Native GNOME acceptance, 2026-09-08

Private lab `/tmp/gfs-wayland-qa.l17i2y7r`: Xvfb 1440×1000, GNOME/Mutter 42.9,
software rendering, private bus/config and a 1,200-second lifetime bound. Input
comes from XTest via the lab's `xdotool`, not a physical keyboard. App SHA-256:
`df61a646f259e5e4e018864cd44f88d1fefcd4a755c69a3017c7c82ffe25009c`.

`03-interaction-settings.png` shows the explicit scope/privacy warning and
500 ms default. A 3-second countdown plus 3-second active limit, without further
input, returned to the recorder page and retained an empty revision-0 64×64
project (`05-idle-expired.png`, `06-idle-message.png`). A subsequent 20-second
attempt also expired before the external driver sent input; it is **not** evidence
of interaction sampling. Both empty fixtures are retained.

The successful `events2.gfsproj` run used manual Stop, 64×64 at (100,300), all
samples retained and input metadata disabled:

1. Click in the fixture, A, B and wheel: four frames (`10-active.png`).
2. Click Pause: five frames (`11-paused.png`). This extra frame is the documented
   controller-input limitation, not a hidden accepted parity difference.
3. While paused, another fixture click, C, wheel and focus-pad Right/Shift+Right
   leave the count at five. The recording rectangle moves to (111,300)
   (`12-paused-move.png`), proving the focus pad's native 1+10 px movement here.
4. Resume, then no input: still five frames (`13-resumed-idle.png`), no replay of
   paused input. Fixture click/D, then Stop, produces eight total (`14-stopped.png`).
   Stop's own press is also included before the stop command is acknowledged.

The project has revision 8, eight 500,000 us delays, one capture clock
`4bde5de1278341bcaf105273232d72fe`, and sample timestamps
`1594768,1694863,2326794,2339000,3963107,5796748,5896838,6726083` us.
Origins are (100,300) for frames 1–5 and (111,300) for 6–8. Key/click event lists
and dropped-input counters are all empty/zero. The eight raw frames exactly
match their respective 64×64 screenshot crops: pre-move `10-active.png`,
post-move `13-resumed-idle.png`, with zero differing channels and no exclusions.
Using the old screenshot for the new position would include the old guide strip
in the reference and is not a valid pixel comparison.

After normal app close, CLI reopened/exported all eight frames. `events2.gif`
decodes to 64×64, two merged images with delays 2,500/1,500 ms, total **4,000 ms**,
675 bytes, SHA-256
`c9f00902235c647789d93b961ff0271f64a276bf7d078df062dff1b72ee61c54`.

The status-layout fix was separately checked with supervised `extra-app-3482005`,
SHA-256 `40978f4616383c64fcd4380939b75609e188d437ca01e6c6a32fafaa4b70a344`.
Opening an existing output project fails safely and displays its path directly
under Open recorder frame (`16-notice-visible.png`), without scrolling. This app
reported exit 0; the lab was explicitly stopped. No host services were touched.
