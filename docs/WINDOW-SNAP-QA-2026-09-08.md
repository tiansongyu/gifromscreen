# Pre-record X11 window snapping

## Reference and selected behavior

Pinned ScreenToGif 2.43.2 commit
`a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd` uses `WindowFromPoint` during its
crosshair gesture and calls `TrueWindowRectangle`. That helper prefers DWM
extended frame bounds and falls back to `GetWindowRect`:
[recorder lines 561–630](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Recorder.xaml.cs#L561),
[window helper lines 429–446](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif.Util/Native/WindowHelper.cs#L429).
Its recorder also adjusts selections against the virtual screen. The Linux
implementation below is not claimed to reproduce its crosshair/subwindow or
partial-window behavior yet.

“Fit region to a window…” appears on the X11 monitor-region controller **before
recording**. It offers discovered window titles and three deliberately different
physical coordinate policies:

| Mode | Coordinates used |
| --- | --- |
| Window frame (default) | Current client bounds expanded by checked WM frame extents, or inset by checked client-side shadow extents |
| Client area | Actual current X11 client contents, excluding its native border |
| Native bounds | Actual root-child ancestor, including its X11 border and any invisible native margins |

`_NET_FRAME_EXTENTS` describes borders added by the WM in left/right/top/bottom
order ([EWMH §5.17](https://specifications.freedesktop.org/wm/latest/ar01s05.html#id-1.6.18)).
GTK publishes scaled shadow widths under `_GTK_FRAME_EXTENTS`
([GTK 3.24 implementation](https://github.com/GNOME/gtk/blob/gtk-3-24/gdk/x11/gdkwindow-x11.c#L3515)).
Each extent property must contain exactly four 32-bit cardinals. Missing hints,
malformed/overflowing/empty geometry, contradictory combined nonzero WM and GTK
extents, or a hinted frame outside the actual native ancestor produce an explicit
error. Client/native modes remain available; no silent fallback is labeled as a
precise visible frame. Containment does not prove that every WM hint is visually
correct; hardware/compositor acceptance remains necessary.

The recorder's **own** controller placement continues to ignore extent hints and
uses real ancestry exclusively. Foreign-window snapping must not weaken this
established self-exclusion rule. Shared read-only rectangle/ancestry helpers avoid
duplicating coordinate arithmetic between the two paths.

## Ownership and safety

The overlay owns at most one asynchronous query and one bounded result. A private
X11 connection reuses the cancellable five-second transport; no UI-thread native
call, global grab, focus change, stacking change or target-window mutation occurs.
Window kind, title and optional PID are rechecked; own/helper/closed/hidden or
minimized windows are rejected. Root-child traversal has four-query/cycle bounds.
Two consecutive geometry observations, including the requested frame policy,
must agree within three attempts. This is not an atomic compositor snapshot,
cryptographic XID identity or continuous target tracking.

CaptureSource geometry and UI scale are not snap inputs. A successful rectangle
must fit completely within the currently selected monitor source. It replaces
the unfrozen `RecorderGeometry` atomically, then uses existing native guide and
control-placement acknowledgements before Start. Recording/paused canvas sizes
cannot be replaced. Pending snaps block Start's ready gate. Manual movement,
native border gestures, stage changes, cancellation and overlay destruction
invalidate pending work; moving away and back cannot revive a cancelled result.
Cancellation does not synchronously join native work on the UI thread.

The menu retains at most 256 discovered windows. **Refresh windows** now updates
it inside the controller, including after an initially empty list. Refresh does
not change the selected screen or capture rectangle. Selection follows the same
window ID when it remains present; long titles truncate in the bounded selector.
One cancellable five-second query reads at most 1,024 WM list entries/tree nodes,
filters hidden/helper/own windows and returns an explicit incomplete-list flag
when a limit is reached. Without EWMH lists, the bounded fallback descends unnamed
wrappers and stops at named/ICCCM clients instead of treating their widgets as
independent application windows. Late refreshes share the snap cancellation gate.
The dropdown remains available alongside the new [click-armed crosshair picker](X11-WINDOW-PICKER-QA-2026-09-08.md).
Upstream drag-from-button activation and subwindow behavior,
explicit clipping/partial-window policy, combined decoration/shadow cases and
mixed-DPI/multiple-monitor/physical GNOME/KDE acceptance stay open.

## Automated verification

- Three private-Xvfb snap tests on Rust 1.98.0 and 1.88.0: exact client/ancestor
  geometry including native borders, fresh movement despite stale catalog size
  and scale, ignored extent hints in native mode, preserved focus/ancestry,
  checked WM/GTK hints, missing/malformed/ambiguous/oversized hints, minimized/own/
  stale-title/closed targets and cancellation. These match CI's explicit
  `private_xvfb_ -- --ignored` invocation.
- Four existing private-Xvfb controller-observation tests pass on both versions
  after sharing native geometry helpers: reparenting, borders, stale hints,
  hidden movement, PID/lifetime failures and bounded ancestry.
- Three desktop snap tests: negative global coordinates, exact all-or-nothing
  changes, out-of-source and frozen-canvas rejection, delayed result after
  movement/stage/gesture changes, move-away-and-back, explicit cancel and drop.
- All 631 desktop tests pass on both versions. Rust 1.98 workspace/all-targets/
  all-features: 1,535 pass and 33 explicit ignores. Strict workspace Clippy and
  native-disabled capture-linux Clippy pass.

## Native GNOME recording evidence

Private lab `/tmp/gfs-wayland-qa.lmicch7t`, owned Xvfb 1440×1000, GNOME/Mutter
42.9, software rendering, private bus/config, 1,800-second lifetime. No host
desktop or service was used. The lab was explicitly stopped with cleanup complete.

Initial frozen app SHA-256:
`990d79cdab459c1dd6185b3d5f6378f74aa32359d2aad5cb55115fd4590ebec8`.
The fixture was discovered at (45,114), 640×420. After opening the controller it
was moved to client (730,114), still 640×420. `04-client-snapped.png` and fresh
`xwininfo` agree exactly; stale discovery geometry did not determine the snap.

The fixture's real native ancestor was (720,69), 660×475. Native-bounds snapping
matched it (`05-outer-snapped.png`). A 3-second countdown and 3-second recording
produced revision 58, 30 frames at 660×475 (`06-outer-recorded.png`). This retains
invisible margins and is **not** used as visible-frame parity evidence.
After normal close, `native-outer.gif` decodes to 30 frames, 3,000 ms, 450,103 bytes,
SHA-256 `da87a305732566f25d459ce230b1408ca3349291ae96bcc6c671de0f682a61d6`.

The follow-up default-window-frame build ran as supervised `extra-app-3559838`,
SHA-256 `4372b60088f3442af529aba1bc1f353851c776da80a0f48c2a04aba400262fd7`.
The actual foreign fixture reports `_NET_FRAME_EXTENTS = 0,0,37,0`, no GTK extents.
The checked default snap gives (730,77), **640×457**, containing the visible
titlebar and omitting the native ancestor's extra margins (`08-frame-snapped.png`).
It remains unchanged during recording; the snap controls are absent after Start
(`09-frame-recording.png`). Timed stop returns to the editor (`10-frame-editor.png`).

`window-frame.gfsproj` has revision 56, 30 frames, 3,000,000 us total and the same
(730,77) origin on every frame. Each raw frame was compared to the crop of the
active screenshot. Only the fixture's known animation regions were excluded:
local y=[107,135), and counter x=[12,628), y=[391,443). All **242,528 static pixels
per frame** match exactly in every frame, including the titlebar and captured
perimeter. The border/control panel is not present in those pixels. This bounded
test is not proof of all compositor presentation timing or physical hardware.

The app reported exit 0 after normal close. CLI reopened the project and exported
`window-frame.gif`: 640×457, 30 images, **3,000 ms**, 238,383 bytes, SHA-256
`8398f5e09098d2887e55f6c6b410ddfef4ab00502703bf5849f00c8fa60b681c`.

## In-controller refresh follow-up

Two additional explicit private-Xvfb tests pass on Rust 1.98.0 and 1.88.0:
tree fallback, new/renamed windows, duplicate/own/closed entries, malformed WM
lists, bounded-list reporting, cancellation and legacy-title fallback. A desktop
test verifies retained selection by ID, untouched capture geometry, recovery
from an empty list and an explicit truncation notice. The full desktop suite
now has 632 passing tests.

Private GNOME lab `/tmp/gfs-wayland-qa.qa54ggn5`, 1,200-second lifetime, reproduced
a title-type bug with its owned fixture: `xdotool set_window --name` wrote
`_NET_WM_NAME` as STRING rather than UTF8_STRING. XGetProperty reports bytes_after
on a type mismatch; the first implementation mistook this for an oversized title
and refreshed to an empty list (`02-refreshed.png`, `03-refreshed-actions.png`).
The fix ignores the wrong-typed modern property and uses the valid legacy
`WM_NAME`, matching ordinary source discovery. These failed observations remain
retained, not counted as successful acceptance. A separate two-instance driver
attempt did not reach the intended controller and is also not counted.

The final frozen app `extra-app-3605395`, SHA-256
`d36a818393c185a2616f9b9539baba80146cdd3e523b21fdec5c034731dc0448`,
ran after the earlier app instances had closed. While its recording frame stayed
open at (0,0), 640×480, the fixture was renamed to “Window refreshed in recorder”.
Refresh updated the selected label without changing that rectangle
(`09-refresh-success.png`). Snap then produced (45,77), 640×457 using the renamed
window's current frame hints (`10-snap-after-refresh.png`). Cancel restored the
main settings with those explicit snapped coordinates (`11-cancelled-controller.png`).
Refresh and Snap remain visible together above the fixed Start/Cancel controls.
No recording/project was created in this follow-up; the preceding recording/GIF
evidence remains separate. The final app reported exit 0 and the lab was explicitly
stopped with cleanup complete.
