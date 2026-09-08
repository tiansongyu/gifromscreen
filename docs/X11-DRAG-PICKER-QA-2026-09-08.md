# X11 drag-from-button window picker

## Status and reference

The native drag handle and desktop integration are implemented. Twelve private
Xvfb regression tests and all 650 desktop tests pass on Rust 1.98.0 and 1.88.0.
Native GNOME acceptance for this activation path is recorded separately below;
earlier click-picker acceptance is not evidence that dragging from this button
works on a composited desktop.

Pinned ScreenToGif 2.43.2 commit
`a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd` starts its crosshair interaction from
the recorder button's preview mouse-down handler, observes `WindowFromPoint`,
draws the target frame and applies `TrueWindowRectangle` after release:
[activation](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Recorder.xaml.cs#L409),
[target and release](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Recorder.xaml.cs#L561).
That is the interaction reference, not an assertion that Win32 targeting and
Linux window geometry are identical.

## Native mechanism

Implementation: `crates/capture-linux/src/drag_picker.rs`,
`drag_picker/native.rs` and `window_picker.rs`.

- A dedicated worker/connection creates one **InputOnly child of the recorder's
  GUI client**, not a desktop-sized input window. The supplied PID must be this
  process's PID and match the parent's checked `_NET_WM_PID`; root and non-GUI
  parents are rejected. This property check is an application ownership guard,
  not authentication against a hostile X11 client.
- Only Button1 on this child's hit area has a passive grab. Pointer processing
  is synchronous; keyboard processing remains asynchronous. Right-clicks and
  clicks outside the hit area remain ordinary input while idle. No keyboard
  grab is installed.
- A valid initiating press atomically claims its generation under the same
  mutex used by layout requests. `claimed` blocks new layout/Start actions but
  does **not** permit hiding the origin window yet.
- On the **same X connection**, `GrabPointer(root, ASYNC, press.time)` replaces
  the activated passive grab. There is no intervening ungrab/regrab gap. Only
  its successful reply publishes `active`, allowing the GUI to hide controls.
  The child remains viewable through the handoff; clearing its input shape is
  not an unmap.
- Fast movement/release queued while pointer processing was frozen is handled
  after the replacement grab. Selection uses the release event's routed root
  child, not a later pointer query or potentially rewritten event coordinates.
  The originating GUI root-child identity is checked before the handoff permits
  hiding it. Fresh ancestry, identity, visibility and geometry checks still run
  on the selected application window. Hover keeps its point containment check.
- Releasing over another eligible window completes the drag. Releasing over the
  originating GUI branch switches to click-to-pick; this includes a stationary
  button click and deliberately uses window identity, not a tiny button-rectangle
  boundary. Right-click
  cancels active selection; GUI Escape/focus-loss cancellation is connected
  without installing a global keyboard grab.

This handoff follows Xlib's same-client grab replacement and frozen-event rules;
`ReplayPointer` is only applicable before replacing the passive grab with an
explicit root grab. See [Xlib §§12.1–12.3](https://xorg.freedesktop.org/archive/X11R7.5/doc/libX11/libX11.html).
An InputOnly child may have an empty input region, distinct from restoring its
default rectangular shape: [SHAPE specification](https://xorg.freedesktop.org/archive/X11R7.7/doc/xextproto/shape.html).

## Stale layout, cancellation and cleanup

The request contains a nonzero monotonically increasing generation, client-local
physical hit rectangle, exact parent size and selected window-bounds policy.
The worker checks these against the initiating event and current parent state.
Parent configuration/map/state changes invalidate readiness and clear the input
shape. Event-sequence barriers prevent old input from starting a new layout's
gesture.

There are three distinct press outcomes:

- **Accept:** claim exactly the current generation, then perform the root-grab
  handoff.
- **Replay stale layout:** empty the child's input shape, recheck cancellation
  and the parent's visibility/ownership/containment, then call
  `AllowEvents(REPLAY_POINTER, press.time)`. A checked request orders replay
  before reinstalling the newest layout. No synthetic parent click is sent.
- **Consume/cancel lost scope:** cancelled, hidden, destroyed or otherwise
  invalid parents do not intentionally replay their initiating press into
  another window. Return through native cleanup and preserve the old capture
  region.

These are checked X11 operations, not an atomic compositor snapshot. Another
client can change a window between requests; the implementation does not claim
to prevent arbitrary hostile reconfiguration. The regression tests distinguish
hidden-but-still-viewable state from an actual unmap.

During root selection, parent/child destruction and PID-change events are
checked **before** the input sequence filter. Closing the origin therefore
terminates the root grab; intentionally hiding it after a successful handoff
does not. Idle handles have no selection deadline. An accepted gesture has a
30-second lifetime; native I/O uses the existing cancellable five-second
operation deadline. The transport checks cancellation with bounded polling.
Drop requests cancellation; cleanup attempts ungrab/destruction with a
100-millisecond transport allowance and closes the dedicated connection. Active
selection also waits for its native highlight cleanup before publishing a
terminal result. The result is single-use; readiness, claim and active state
are cleared when consumed. No UI-thread native join is needed.

## Desktop integration and fallback

`apps/desktop/src/window_snap.rs` publishes the actual enabled button rectangle
after layout. It intersects the UI clip rectangle, converts client-local points
to physical pixels and rounds inward. It does not derive this hit area from the
capture region's global origin. Missing/clipped controls, popups and disabled
states do not leave an invisible idle hit target. Parent identity, physical
size, UI scale, bounds policy and recording geometry are part of the binding.

`apps/desktop/src/x11_recorder.rs` supplies the observed parent and controls the
handoff. Only Ready/unfrozen monitor-region selection can change the capture
rectangle. Claims and active/closing selections block Start; active picking
hides the recorder controls. Close waits for native cleanup before restoring
the ordinary main interface. Late results must still match their original
generation, layout, geometry and stage. They cannot retarget an already-started
recording or revive a cancelled operation.

The same button retains click-to-pick. If that fallback is requested while an
idle native child exists, it waits for the child service's terminal cleanup
before starting the separate picker. A native-handle error is visible and
offers **Retry drag handle**; the window dropdown, refresh and explicit snap
remain available. Selection remains a one-time position, not continuous target
tracking. Current geometry policies and target validation are described in
[window snapping](WINDOW-SNAP-QA-2026-09-08.md) and the existing
[click picker](X11-WINDOW-PICKER-QA-2026-09-08.md).

## Automated evidence

All twelve tests in `crates/capture-linux/src/x11_drag_picker_tests.rs` passed
on both Rust 1.98.0 and 1.88.0. Native and portable strict Clippy checks also
passed in this cohort. Each native test allocates its own supervised Xvfb;
none reads host input or uses the host `DISPLAY`.

| Test case | Verified boundary |
| --- | --- |
| Held and queued fast release | Both routes select the intended target once, including release followed immediately by motion elsewhere |
| Stationary click | Button click arms subsequent click-to-pick |
| Queued click then move away | Release on the originating GUI still arms click-to-pick even if later coordinates leave it |
| Hide after handoff | Actual root pointer processing continues after origin unmap |
| Outside/nonprimary clicks | Parent receives normal input; picker stays idle |
| Stale parent size | Original click reaches parent exactly once without selection |
| Disabled pending layout | Old hit cannot start the disabled/new generation; click is replayed once |
| Explicit stop | Active cancellation cleans owned native resources |
| Active parent close | Destroy after confirmed handoff terminates promptly, unlike permitted unmap |
| Short watchdog | Idle does not expire; accepted selection does |
| Cancel/unmap/hidden parent | Initiating press is not replayed to parent/underlay; no successful selection is accepted |
| Foreign parent/Drop | Invalid identity is rejected and Drop releases the active service |

The fast-release and stale-layout tests use a server grab **only inside the
private fixture** to queue input before the worker can issue replacement
requests. `QueryPointer` confirms pointer thaw or queued Button1 release;
these checks do not rely on an arbitrary sleep to declare input processed.
Replay tests count both press and release, and drain again after service cleanup
to detect delayed duplicates. Negative tests reject a successful selection
whether its result was already available on the first poll or arrived later.
Cleanup checks verify child/highlight removal and acquisition from a different
connection, rather than merely trusting a cancellation flag.

Reproduce the native subset with Xvfb installed:

```sh
cargo +1.98.0 test --locked -p gif-from-screen-capture-linux --lib --no-default-features --features native-x11 private_xvfb_drag_handle_ -- --ignored
cargo +1.88.0 test --locked -p gif-from-screen-capture-linux --lib --no-default-features --features native-x11 private_xvfb_drag_handle_ -- --ignored
```

The `private_xvfb_` prefix also includes these tests in the explicit native CI
step. Ordinary headless test runs keep them ignored, not silently passed.
The final complete private-Xvfb cohort passes **42/42** on Rust 1.98.0; the
12 drag plus four existing click-picker tests pass **16/16** on both Rust
versions. The final workspace/all-targets/all-features run passes **1,554 tests**
with **51 explicit ignores**; workspace strict Clippy and formatting pass.
Native-disabled backend tests pass 48/48 on both versions, separately from
the feature-unified native desktop build.

### GUI regression boundaries

The final desktop cohort includes 22 `window_snap`, 12 `x11_controller_ui` and
12 `x11_recorder` tests on each Rust version. They cover clipped/physical hit
coordinates, scale and parent changes, generation rejection, claimed versus
active visibility, idle Start, immediate stop intent with deferred close
restoration, fallback cleanup and sticky failure/retry behavior. The actual
controller draw test preserves the same measured waiting-label space through
Ready → claimed → active: disabling Start must not move the initiating button
and cancel its own gesture. The one-line readiness hint likewise leaves the
button caption and hit area stable.

## Native acceptance and remaining parity gaps

### Native investigation record

Private lab `/tmp/gfs-wayland-qa.6e5wsqxn` runs owned Xvfb 1440×1000 and
GNOME/Mutter 42.9 with private config/bus and software rendering. The first
frozen executable has SHA-256
`8af1fa0f2c0615b547a0e1761037d446a56cd2c8d88c6cebf1d3f928de725e21`.
This initial build is not the final fast-release acceptance binary.

- `05-held-drag.png` → `06-drag-selected.png`: press the button at (220,684),
  move while held to (500,250), then release. Controls/old guide hide while
  selecting, then the visible fixture frame becomes (45,77), 640×457.
- `20-client-held.png` → `21-client-held-selected.png` verifies Client area
  selects (45,114), 640×420. `23-active-right-start.png` →
  `24-active-right-keeps-client.png` cancels an already-active Window frame
  gesture and retains that Client rectangle. `13-active-cancel-start.png` →
  `14-active-cancel-keeps-client.png` records Escape restoration; its filename
  is historical, and that earlier selection was still the Window frame.
- `25-click-frame-armed.png` → `26-click-frame-selected.png`: stationary click
  keeps the picker armed; the subsequent target click selects the Window frame.
- `29-active-close-before.png` → `30-active-close-restored.png`: while Button1
  is held in active selection, the isolated helper
  `/tmp/gfs-picker-close-6e5wsqxn.dmLtZ5` validates the lab, XID 0x600003,
  PID 3819548/process lifetime and supported protocol, then sends one standard
  WM_DELETE_WINDOW. The main recorder page returns with (45,77), 640×457;
  a normal titlebar Close removes the app from the WM client list. No XDestroy
  or forced app termination is used.

The initial fast test returned `Picked(None)` when release was immediately
followed by movement outside the target. Repeating with five queued pure-XTest
requests reproduced it (`27-xtest-before.png`, `28-xtest-fast-after.png`), so this
is **not** attributed to xdotool's pointer-warp behavior. Adding the same trailing
motion to the private-Xvfb regression reproduced failure on both Rust versions.
The release still named the intended root child, but its coordinates had become
the later pointer position; preceding motion coordinates had changed too. The
old containment check incorrectly rejected the valid target. Recovering a
coordinate from those motion events would have repeated the same error.
The original fast test without that trailing motion was insufficient. The fix
uses the release's target identity with fresh ancestry/window validation, not a
late pointer query. Fresh frozen-binary acceptance is recorded below.

This trace matches upstream Xserver 21.1.4: frozen queues advance `hotPhys`,
while replay hit testing uses historical `hot`;
[`ProcessDeviceEvent`](https://gitlab.freedesktop.org/xorg/xserver/-/blob/6bf62381d0a1fb54226a10f9d0e6b03aff12f3aa/Xi/exevents.c#L1849-1862)
then copies the `hotPhys` position through
[`GetSpritePosition`](https://gitlab.freedesktop.org/xorg/xserver/-/blob/6bf62381d0a1fb54226a10f9d0e6b03aff12f3aa/dix/events.c#L1042-1047).
The source/trace agreement is not an audit of every distribution patch or server.

### Final frozen-binary acceptance

The old app was normally closed before launching supervised
`extra-app-3858536` in the same lab. The final executable SHA-256 is
`612427e210c018df74422a5933d9caad87c276668b6bf24bc229d764db7b2f9d`.
The lab-scoped helper at `/tmp/gfs-xtest-fast-drag.vUkrZh` checks its exact
environment and queues five pure-XTest requests: move to button, press,
move to target, release, move outside. It uses no server grab, pointer warp,
sleep, focus change or synthetic SendEvent.

The previously failing burst now selects correctly. `37-fixed-stable.png`
shows Client area (45,114), 640×420; `38-final-frame-ready.png` →
`39-final-fast-frame.png` changes it to Window frame (45,77), 640×457,
despite the final pointer being at (1000,800). Early screenshots 34/36 were
taken during cleanup, not used as assertions that control restoration had
finished. Cancelling the idle armed controller then restores the main page
(`40-final-recording-form.png`) without losing the selected frame rectangle.

After setting a 3,000 ms limit, reopening the controller and Start/countdown,
the editor contains **30 frames** (`43-final-editor.png`). The saved project
is revision **59**, 640×457, total **3,000,000 us**, with (45,77) stored on every
frame and one capture-clock identity. Raw sampling timestamps remain strictly
increasing, spanning 2,900,048 us; that span is not the total GIF playback delay.

The fixed-mask comparator at `/tmp/gfs-drag-pixel-compare.WRM3Cu3U/compare.py`
uses `41-final-ready-to-record.png`, the **pre-recording** screenshot, as its
static reference. Screenshot 42 was taken too late and already shows the editor;
it is not labeled active-recording evidence. From the 640×457 crop, only local
y=[107,135) and counter x=[12,628), y=[391,443) are excluded. Every one of the
**242,528 remaining pixels per frame** matches exactly in RGBA: **7,275,840
comparisons, zero differences**. Changing counter/white-box pixels are outside
this assertion. The result is `drag-picker-static-comparison.json`, SHA-256
`7ffa4ed67178338ed409c3e5ecda3b36e60eef462567f00038019e258c739006`.

Normal titlebar Close reports app exit **0**. CLI reopening/export produces
`output/drag-picker.gif`: **640×457, 30 GIF images, 3,000 ms, 238,305 bytes**;
SHA-256 `7a23cc2bf0df8d767e9afa61ad51b2989986c6e567b6700f1d2572779a628a21`.
The lab was explicitly stopped. These are owned virtual-display/XTest results,
not physical mouse, hardware-rendered or mixed-display acceptance.

### Remaining acceptance

- Child-widget/subwindow targeting is not complete upstream parity. Current
  target discovery resolves supported application windows; this is not a
  promise to select every nested Win32-like child region.
- Partial/out-of-source windows still use the existing all-or-nothing fit
  policy. Explicit clipping and multi-monitor-spanning selection remain open.
- Hardware GNOME/KDE, mixed-DPI/multiple-monitor layouts, fractional scaling,
  combined decoration/shadow cases and rapid WM reconfiguration remain release
  gates. Logical-to-physical unit tests do not replace those checks.
- This X11 mechanism does not bypass Wayland's portal consent or provide a
  portable Wayland global pointer/window hierarchy. It also does not change
  the fixed-canvas rule during recording or constitute continuous window
  tracking.

The new activation path advances the crosshair interaction; neither these
twelve tests nor this document claim complete ScreenToGif parity or that all
Linux bugs have been resolved.
