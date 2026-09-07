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
