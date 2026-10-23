# X11 recording-frame drag scheduling

The recording guide presents native X11 pointer movement directly on its worker,
without waiting for an egui frame or GPU/VSync callback. GIF capture FPS controls recorded samples independently of guide motion.
For a 60 Hz desktop the presentation budget is approximately 16.7 ms.

## Changes in 0.1.4

- An owned pointer gesture updates the border and handles directly on the X11
  worker. Native updates continue while the app is not polling the guide.
  Pending motion is coalesced to the latest pointer position before presenting.
  After geometry replies, the worker drains events buffered by x11rb before
  waiting for socket readiness, avoiding a housekeeping-timeout delay.
- Native preview permission is explicit: ready mode, or an acknowledged capture
  pause held by a retarget transaction. Live capture never receives that permission.
- Moving preview geometry invalidates the previous capture ACK. Release retains
  the final pointer/rectangle until the UI submits and receives a new geometry ACK.
  Resume still waits for capture retarget, exclusion and controller settling.
- The UI reconciles its geometry from the native gesture snapshot, including
  press/move/release sequences received together after a delayed UI frame.

## Changes in 0.1.3

- A private notification pipe wakes the X11 guide worker for geometry requests,
  cancellation and shutdown. Native pointer input wakes the same wait directly.
  The previous fixed 10 ms / 2 ms sleeps are removed. A 20 ms idle poll timeout
  remains for cancellation/watchdog and controller-observation housekeeping;
  input and commands interrupt it immediately.
- Native pointer events and geometry acknowledgements request a UI repaint
  outside the guide-state lock. During a drag, the UI requests the next VSync
  frame immediately rather than adding a 16 ms timer after rendering. VSync is
  explicitly enabled in the desktop options.
- Ready-mode position changes batch all visible border/grip/corner moves and
  reuse their shapes and artwork. They do not unmap and recreate the border.
- Recorder controls remain stationary during a drag and relocate after release.
  Active capture still pauses before changing regions, retains old-region
  protection, and waits for final geometry/placement confirmation before resuming.
  Holding an acknowledged paused gesture does not trigger the retarget timeout.

The guide uses native X11 windows. The compositor determines when their pixels
are presented; a server acknowledgement is not a hardware presentation receipt.
Slow drivers, compositor scheduling and pointer-device sampling can still affect
visible smoothness. No physical-display frame-rate guarantee is claimed.

## Verification

Run the regular desktop and capture tests, plus the explicitly isolated native
guide tests:

```sh
cargo test --locked -p gif-from-screen -p gif-from-screen-capture-linux --all-targets --all-features
cargo test --locked -p gif-from-screen-capture-linux --all-features recorder_guide::native_tests:: -- --ignored --nocapture
```

The latter starts its own supervised Xvfb servers and never uses the host display.
The notification regression checks that input/ACK callbacks run outside the state
lock. The native timing test measures 120 accepted geometry updates using callback
wakeups, without a UI polling timer, and prints median/p95 latency against the
16.7 ms budget. Existing native tests check capture exclusion, unchanged capture
pixels, gesture release, cancellation, cleanup and absence of unmap/shape churn
for ready-mode movement. Timings are native acceptance latency, not physical FPS.

The 0.1.4 pointer-to-window regression leaves the UI unpolled for 60 moves at
approximately 60 samples per second and reads the real X11 border positions.
On the local owned-Xvfb run, median acceptance was 0.189 ms and p95 0.232 ms.
These are server/window update measurements, not monitor presentation FPS.
A second native regression verifies missing permission and mid-gesture revocation;
the desktop coordinator checks that native preview requires an acknowledged held
pause and is revoked when the hold ends.
