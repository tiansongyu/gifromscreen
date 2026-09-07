# Independent X11 selection, border and controls

The desktop now uses this split controller instead of the historical combined
viewport described in [X11 recorder windows](X11-RECORDER-WINDOWS.md). See
[bounded native acceptance and retained failures](NATIVE-X11-SPLIT-QA-2026-09-07.md).
The full feature/release scope in
[Linux status](LINUX-STATUS.md) remains unchanged.

## Requirements

The selected physical rectangle is authoritative. A full-monitor selection must
retain the monitor's exact width and height. A 1×1 selection must remain 1×1,
regardless of the control panel's minimum size. Font size, application zoom and
window-manager repositioning must never become an implicit crop or resize.

Before recording, position, dimensions and countdown can change. Once a recording
canvas is established, position can change but dimensions cannot. Moving the
source region must preserve the session, project and pause-excluded clock.
Stop-and-save and explicit Discard retain their existing different semantics.

## Implementation boundaries

- `RecorderGeometry`: checked global physical rectangle inside a source, clamped
  movement, Ready-only resize, canvas lock and checked source-local conversion.
- `RecorderGuide`: a dedicated X11 connection owns narrow override-redirect
  border strips. No full-canvas transparent GPU surface or unrelated window
  mutation is required. Both bounding and input shapes exclude the capture area.
- `x11_controller_ui`: independent responsive controls edit desired geometry and
  return existing recorder actions. Primary actions stay outside the settings
  scroll area. This view neither positions native windows nor acknowledges capture.

These desktop modules are connected to the real application and its existing
recording controller. Their tests run in the desktop crate; earlier standalone
harness results are not used as proof of this connection. The old combined-window
production path and its superseded UI-only tests have been removed, with history
retained in Git.

### Native border primitive

The `capture-linux::RecorderGuide` API is implemented independently of the UI.
One worker/connection owns four narrow visible windows plus one invisible,
empty-input `InputOnly` gesture keeper. Requests have strictly increasing
nonzero generations; pending updates coalesce, and stale acknowledgements cannot
make a newer request ready. Polling is nonblocking. The event queue is limited to
64 entries, motion coalesces, and overflow cancels the gesture. Debug output omits
capture/pointer coordinates.

A real primary-button border press begins a distinct, process-wide non-reused
`gesture_id`. An asynchronous pointer grab routes only that gesture through the
stable keeper without changing keyboard focus. Updates and temporary hide preserve
its motion/release events. Idle strips select no pointer motion, and the service
never subscribes to a root pointer feed. Explicit cancellation, release, stop,
drop, failure and a real 30-second watchdog end the grab. Cleanup releases only
a grab this connection owns; fast-click backlog that cannot acquire its old press
is cancelled rather than killing the service.

Each update unmaps the owned strips, installs both bounding/input shapes, and
maps only their visible portions. It can additionally exclude an old capture
rectangle. Explicit hide and whole-root capture unmap all strips. It never scans
or modifies another client's windows. The shared cancellable X11 transport bounds
setup and each update to five seconds, with a short cleanup grace. A detected root
dimension change fails with an instruction to reselect the source instead of
silently keeping stale clipping. Root structure notifications are not root mouse
or keyboard recording.

The initial implementation accepts 1–16 px borders and strips representable with
signed X11 coordinates and dimensions no larger than 32,767 px. Unsupported values
are rejected before queueing. This is an explicit native representation limit,
not a license to silently shrink the selected GIF canvas.

Eight tests run on their own supervised Xvfb servers verify actual visible border
pixels, unchanged captured source bytes, matching Bounding/Input exclusions,
center-click delivery, owned pointer events, signed positions, full-root/explicit
hide, continuous gestures, the watchdog, stale-identity rejection and
own-window-only cleanup. They pass on Rust
1.98 and 1.88. They are included by the CI `private_xvfb_ -- --ignored` step, not
silently skipped as hardware tests. These tests do not establish compositor shadow
behavior on every desktop. Two RGB trials proved that `_GTK_FRAME_EXTENTS=0`
does not fix override-redirect shadows in Mutter 42.9: its property hook skips
these windows. The four strips now use an XRender-verified 32-bit TrueColor alpha
visual, full-alpha colored pixels and a dedicated colormap, without declaring an
opaque region. No compatible visual is an explicit error, not an RGB fallback.
The subsequent native raw-pixel comparison passes the previously failing edges.

The service can also observe the process-owned controller identified by its real
window handle and PID. It reads client geometry and at most four real parent
links to find the outer boundary, instead of trusting stale `_NET_FRAME_EXTENTS`.
PID, source root and native border bounds are checked; hidden windows remain
queryable and loss of identity fails explicitly. No controller/parent property is
modified by this observer. Its results are not atomic/presentation snapshots.

## Placement and starting

The control panel should fit entirely below, above, left or right of the capture
rectangle, then on another available monitor. Include native decorations/shadows
in its occupied extent. Confirm actual native placement after the compositor/WM
responds; the requested coordinates alone are insufficient.

If no screen space remains, do not shrink the capture rectangle or paint the
controller over it. Full-monitor recording needs a hidden controller with an
explicit, recoverable control path: confirmed global bindings, timed stop, and
window-switcher restoration that pauses capture before opaque UI is drawn.
Restoring a transparent/minimized window must not discard captured frames.

In the implementation, the root stays blank before minimizing; Start waits for
the WM's minimized acknowledgement. Do not require X11 `MapState` to become
unviewable: native Mutter evidence shows a client can be `Iconic` and have
`_NET_WM_STATE_HIDDEN` while still `IsViewable` for redirected preview rendering.
Geometry and WM visibility are different contracts. Recovery first requests and
acknowledges pause, then paints the controls; Resume is an intent that hides them
again before native resumption. Timer completion restores the editor normally.

The existing action adapter must validate **after** the control view returns:
editing a numeric field can change desired geometry in the same input batch as
Start/Resume. A previously accepted Start may wait briefly for current geometry;
it must expire, remain visible as pending, and be revoked on close/failure/invalid
selection. Permission or selection preparation must never be silently skipped.

## Live movement

Keep desired, backend-applied and guide-acknowledged geometry distinct. Only the
latest generation may acknowledge a request. Do not display a requested position
as the actual recording position before the backend accepts it.

A new border may intersect the *old* capture rectangle. Movement therefore needs
an explicit sampling/presentation transition, including pause acknowledgement,
safe controller placement and capture-target acknowledgement. Subtracting both
old/new rectangles from a guide is an additional safeguard, not a complete
presentation protocol. An X server ACK or XSync does not prove that the compositor
has presented fresh pixels. No fixed sleep alone certifies absence of self-capture.

The native primitive now preserves a bounded, explicitly initiated gesture across
presentation updates. Its Xvfb tests move, update, hide, show and release with the
same gesture identity. Desktop drag/resize and sampling coordination still need
their own integration evidence. The tested desktop drag briefly pauses sampling,
coalesces desired positions and waits for actual control placement, capture-target
ACK and the final guide generation before restoring the requested pause state.
It does not save a frame for every intermediate pointer position. Global toggles
are fenced/deferred through this transition; Stop remains available.

## Required integration evidence

1. Real crate tests for the prepared modules and all callers, both supported Rust
   toolchains, strict Clippy, plus existing recorder/Wayland regression suites.
2. Private Xvfb: exact border shapes, center click-through, own-window-only updates,
   hide/show, generation handling and cleanup, including failed setup/update.
3. Private Mutter: full 1440×1000 monitor, 1×1/16×16 and ordinary regions; all
   four output edges/corners checked against known source pixels.
4. Native continuous movement and rapid reversal; delayed/stale target replies;
   inspect every retained frame for border, toolbar and shadow contamination.
5. UI zoom, large fonts and controller repositioning leave the physical recording
   canvas unchanged; primary controls remain reachable.
6. Full-monitor stop/recovery with unavailable or conflicting shortcuts; source
   disappearance and guide failure preserve the recoverable project.
7. Separate physical GNOME/KDE, mixed-DPI/multiple-display and keyboard/device
   acceptance. Software-rendered nested tests do not replace these release gates.

Ordinary, 1×1, full-monitor and one reversed live-drag sequence now have bounded
native evidence. Mixed-DPI/hardware/compositor, arbitrary WM animation and repeated
stress cases remain open; this contract is not full Linux release certification.
