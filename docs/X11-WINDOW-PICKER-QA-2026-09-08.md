# X11 crosshair window selection

This is the click-armed baseline acceptance record. The subsequent
[drag-from-button implementation and separate acceptance](X11-DRAG-PICKER-QA-2026-09-08.md)
extends activation; the results below retain their original tested scope.

## Connected workflow and fidelity boundary

Before recording, expand **Fit region to a window…**, choose Window frame,
Client area or Native bounds, then activate **Pick window on screen…**. The
ordinary recorder controls and old selection outline hide temporarily. A
crosshair and passive outline identify the eligible window under the pointer.
Left-button release selects; right-button press or Escape cancels. Focus loss
also requests cancellation. A 30-second selection watchdog is followed by
bounded native cleanup. Cancel preserves the original selection.

This baseline uses **click to arm, then click/release the target**, not the pinned
reference recorder's press-on-button/drag-out/release gesture. Target lookup
selects managed/named top-level clients, not arbitrary child widgets. These are
fidelity differences at this baseline, alongside partial-window clipping policy,
multiple seats/screens, broader WM/CSD and hardware/mixed-DPI acceptance. The
overall Linux parity goal is not complete.

The existing [checked window-bounds policy](WINDOW-SNAP-QA-2026-09-08.md) remains
authoritative: a picked rectangle must fit the selected monitor, is applied
only in Ready state, and cannot replace a running recording's frozen canvas.
Picking is one-time positioning, not continued window tracking.

## Native ownership and ordering

`pick_snap_window` runs on an overlay-owned worker and a dedicated bounded X11
connection. It explicitly acquires the pointer on that root with a crosshair,
asynchronous pointer/keyboard processing, and no pointer confinement. It installs
**no keyboard grab or passive global input listener**. The
[XGrabPointer contract](https://xorg.freedesktop.org/archive/X11R7.5/doc/man/man3/XGrabPointer.3.html)
defines event redirection and grab-conflict behavior. A foreign grab is rejected,
never released by the picker.

Only server-generated button events at/after the successful grab request can
confirm selection. A left press must occur in this picker session. Confirmation
uses the **release event's** root child and physical coordinates, not a later
QueryPointer result. Target lookup uses the bounded client catalogue/ancestry
checks; an unrelated top-level popup does not cause selection of an occluded
window behind it. Current identity, geometry and frame hints are re-read through
the same snap observer. A disappearing target cannot reuse an old hover result.

Hover samples are bounded to approximately 25 Hz. A separate
`RecorderGuide::start_passive` owns the highlight strips: their bounding shapes
paint the outline, but their input shapes are always empty. They cannot become
the target of pointer lookup or begin recorder border gestures. Existing
interactive recorder guides retain their prior behavior.

The selection worker stops the passive guide and observes its cleanup before
returning, then acknowledges pointer ungrab and closes its connection. The UI
keeps Pick busy until this result arrives, including after Escape cancellation.
Close requests are queued through the same cleanup gate before restoring the
ordinary interactive window. Background failure/close/disposal also cancel
owned work. Per-operation transport deadlines are capped by the picker's
remaining selection lifetime; UI polling never waits on native APIs.

## Automated evidence

- Four explicit private-Xvfb picker tests pass on Rust 1.98.0 and 1.88.0:
  real pointer selection, release-then-move ordering, empty input shapes for the
  mapped highlights, unchanged keyboard focus and delivered key input,
  suppressed selection clicks, right/cancel-flag/watchdog cleanup, vanished
  targets, and preservation of another client's conflicting grab.
- The full private-Xvfb cohort passes all 30 tests on Rust 1.98.0, including
  the existing guide/retarget/shortcut/input regressions and 30-second border
  gesture watchdog. These tests own their displays, never host DISPLAY.
- New desktop tests prove one-time picked source/geometry application,
  out-of-source rejection, Escape cancellation remaining busy through cleanup,
  and deferred recorder close. All **635 desktop tests** pass on both Rust versions.
- Rust 1.98 workspace/all-targets/all-features: **1,539 pass, 39 explicit ignores**.
  Strict workspace Clippy, native-disabled capture-linux Clippy and formatting pass.

## GNOME native acceptance

Private lab `/tmp/gfs-wayland-qa.z8l5_vm2`: owned Xvfb 1440×1000, GNOME/Mutter
42.9, software rendering, private bus/config and an 1,800-second lifetime.
The accepted frozen app `extra-app-3644871` has SHA-256
`5c9111086f3b76f468716f6007250e0eab32926041c150828303eab26a2d7a46`.
Input was generated through the lab's XTest driver, not a physical device.

`03-hover.png` shows the fixture's 640×457 visible frame highlighted at (45,77),
with the recorder controls and old (0,0), 640×480 outline absent. During this
state GetInputFocus remains the recorder's verified window 0x600003/PID 3644879.
Right cancellation (`04-right-cancel.png`) and Escape cancellation
(`05-escape-cancel.png`) each restore the original region/control panel.

The subsequent target click/release selects (45,77), 640×457, even though the
driver immediately moves the pointer outside that target (`06-picked.png`).
Start, three-second countdown and a three-second capture produce 30 frames and
return to the editor (`07-recording.png`, `08-editor.png`). The resulting project
is revision 58 with 3,000,000 us total and the same (45,77) origin on every frame.

Every raw frame was compared against the active screenshot's 640×457 crop.
Only the fixture's known moving area y=[107,135), and counter x=[12,628),
y=[391,443), were excluded. All **242,528 remaining pixels in each frame match**,
including the titlebar and perimeter. This is scoped static-pixel evidence,
not a claim to compare changing counter pixels or certify all presentation timing.

After normal app close (reported exit 0), CLI reopened and exported `picker.gif`:
640×457, 30 images, **3,000 ms**, 238,829 bytes; SHA-256
`30d46c18d10843a069220a3a5a923193a42f5611f460b518692563d94980d169`.
The lab was explicitly stopped with cleanup complete.

### Closing during selection

A separate 600-second lab `/tmp/gfs-wayland-qa.v24ysnew` ran the close-gated build,
SHA-256 `9069445852d3f79ec4e5c9981381e43211073b082ccff346e8f017ece04831ce`.
Alt-F4 did not produce the intended window-close event while the pointer was
grabbed; that attempt is not counted as close-path acceptance, and later
ready-state restoration is not used to infer its cause.

After entering selection again, an isolated Rust QA helper at
`/tmp/gfs-picker-close.PVzxJ8` verified the lab config path, target XID 0x600003,
PID 3680646 and advertised WM_DELETE_WINDOW support, then sent that standard
close request. It did not destroy the window or kill the process.
`04-close-request-handled.png` shows the main recorder page restored with the
original (0,0), 640×480 selection and no hover outline. A normal titlebar Close
click then removed the app from the WM client list, showing pointer input was
usable again. The lab was explicitly stopped; no project was created by this
close-only check.
