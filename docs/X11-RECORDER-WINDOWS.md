# X11 recorder window and input contract

Native Mutter X11 checks exposed defects not covered by component tests:

- A maximized root window ignored the former 1×1/off-screen geometry requests,
  leaving the settings page visible under the recorder.
- eframe 0.32.3 initializes one shared Wgpu painter from the **root** transparency
  option. Setting only the recorder child's transparency did not enable an
  alpha-capable shared backbuffer.
- A visually transparent recording center still received clicks and took focus
  from the underlying application. Visual transparency is not input transparency.

## Corrected lifecycle

The root enables transparent backbuffers. During an X11 recorder session it stops
painting all ordinary pages, clears to transparent, removes decorations and enables
mouse passthrough. It remains mapped, at its existing geometry, so the recorder's
immediate viewport can continue being driven. Closing the recorder restores normal
input, decorations and saved geometry. Closing the parent during active recording
now requests **Stop and save** and awaits completion instead of reaching the
abandonment/Discard destructor path.

The recorder has a fresh UUID-bearing exact title. A bounded worker finds only a
window with this process's PID and that exact identity, verifies its current client
dimensions, and atomically installs its INPUT region as client rectangle minus
capture hole. The hole passes mouse events to the underlying app; borders and the
bottom controls remain interactive. The helper never changes a WM-owned parent,
visual bounding shape or another application's window.

The acknowledgement is tied to the current physical geometry. Changes revoke
readiness immediately and cancel/coalesce older work. Start is disabled until the
current hole is installed and the capture rectangle is inside its source. A global
Start waits for that tick's viewport geometry before executing; a clicked Cancel
or Close takes precedence. Physical conversion and 10-pixel nudges use effective
pixels-per-point, including egui zoom, not just the monitor's native scale.

A Start already accepted by the controller now survives a transient input-shape
change during that same draw. It is consumed only after the current geometry is
acknowledged, with a five-second limit. An invalid rectangle, failed preparation,
closed controller, terminal stage or explicit Cancel revokes it. A shortcut pressed
before initial preparation finishes is rejected visibly and requires another press;
it does not silently schedule recording after a permission/selection step. Errors
are visible in the floating toolbar. Its rows are top-aligned so the status line
does not fall below the fixed-height panel during countdown, recording or pause.

X11 defaults to the GL UI backend; explicit `WGPU_BACKEND` choices are retained.
The isolated software-Vulkan configuration failed transparent presentation, while
GL rendered opaque normal pages and the transparent recorder correctly. Capture
and CPU GIF rendering are independent of that UI backend. This is a tested backend
choice, not a claim that all Vulkan drivers fail or a proven driver deadlock cause.

## Evidence boundary

Initial trials used private labs `6r1s5442` (default renderer), `2q4h637_` (GL,
old lifecycle), `voih_8el` (transparent root, default renderer) and `q8ubhobo`
(transparent root, GL), all under `/tmp/gfs-wayland-qa.*`. Each was supervised
and stopped with successful cleanup; none created a recording project. Screenshots
and logs retain the failed and successful visual comparisons. In `q8ubhobo`,
activating the fixture then clicking the visible recording center still focused
the recorder before the input-shape fix, proving the separate input issue.

The helper's private Xvfb tests verify actual center-click delivery, retained
border/toolbar clicks, unchanged bounding shape, identity/size checks, cancellation,
and shape persistence after its connection closes. Desktop tests cover geometry
scaling, stale acknowledgement, deferred Start, parent-close preservation and
renderer configuration. Full native recorder/GIF acceptance follows separately;
neither these component tests nor the initial visual trials complete the wider
hardware, mixed-DPI or desktop matrix.

## Native checkpoint: 2026-09-07

Private Mutter/Xvfb lab `c2t9e9um` verified the input fix: clicking the center
focused the fixture, not the recorder. A button-started recording stopped through
the global shortcut and opened 3,686 frames in the editor. Its Ready shortcut
attempts did not start; there was insufficient instrumentation to identify why.
The app was closed normally and the lab stopped with complete cleanup.

Lab `r18q9xfg` then used bounded, opt-in `GFS_RECORDER_TRACE=1` debug diagnostics
(action/state only, maximum 128 lines; disabled in release). Unlike the earlier
trial, Ready F7 reached `Controller -> Start` with valid geometry and an acknowledged
input hole. With fixture focus, it entered the three-second countdown and captured
ten seconds at 10 FPS. Thus the earlier symptom is **not** a proven consistently
reproduced backend failure. The same-frame intent-loss path is separately covered
by deterministic regression tests.

Two native projects were closed normally, reopened through the CLI export path,
and decoded independently:

| Recording | Project | GIF |
| --- | --- | --- |
| Continuous | 100 frames, 640×480, origin (4,36), total 10,000,000 µs | 99 frames, 10 s, 1,386,494 bytes; one identical adjacent frame merged |
| Manual snapshots | 3 frames, 640×420, origin (45,114), each 1,000,000 µs | 3 frames, 3 s, 21,039 bytes |

The manual run used global F7 to open/start, F9 to capture, F7 to pause,
F9 while paused, F7 to resume, two further F9 presses, then F8 to stop. Focus
remained on the fixture. The editor and reopened project contain exactly the
three accepted snapshots, with fixed playback timing; the paused request did
not add a frame. Screenshots `09.png` through `13-manual-editor.png` retain the
settings, active/paused states and resulting editor timeline in the lab.

GIF SHA-256:

- continuous: `bdf72e3edd7c950a6105a4780d14131f4cc29067300781e81728235a8aad69e4`
- manual: `6fe854b66d1a126421b62af7eec89dcc11e53bf968feca1c46889d438938ac86`

For each of the three raw manual frames, all 8,416 pixels in the outer four-pixel
ring exactly match the fixture's expected four quadrant colors and alpha. This
checks all four edges/corners against known source content, rather than merely
searching for the recorder's color. For all 100 continuous raw frames, the visible
599-pixel fixture top edge starts at the expected local (41,78) and matches the
fixture colors exactly. These pixel checks apply to these stationary selections.
Both app supervisors exited with status 0; the lab then stopped with
`cleanup_complete: true`. The projects, GIFs and screenshots were retained.

This validates the tested stationary rectangles and control sequence, not full
monitor layout, tiny selections, live movement, physical keyboard repeat,
mixed-DPI hardware or Portal shortcut acceptance. The next layout separates the
physical capture model, native border and independent control panel; those gates
remain open until integrated and tested.
