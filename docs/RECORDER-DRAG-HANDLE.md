# Large recording-frame drag handle

The 0.1.2 maintenance build addresses the difficulty of grabbing a four-pixel
X11 border. An orange **144×36 physical-pixel grip** sits outside the selected
pixels, normally above the frame. It has grip dots, a four-direction move icon
and a move cursor. Its whole surface, including corners, is a **move** target.
The original border/corners remain available; corners resize only before capture.

The grip follows UI scale from 100% to 400% (with a minimum 100% hit target).
For example, 150% is 216×54 physical pixels. It can move the frame while Ready,
counting down, recording or paused, through the existing acknowledged capture
retarget workflow. It never changes the captured width/height by itself.

## Placement and safety

- Prefer the top edge, then left/right/bottom with centered/end anchors. Side
  grips shorten to the recording height when necessary, so a short recording
  near the screen top does not lose its grip behind the controller below it.
- Keep the grip fully within the X11 root, outside the selected capture pixels,
  outside the old protected rectangle while retargeting, and outside the owned
  controller's observed native outer bounds. Placement/scale changes issue a new
  guide generation and invalidate stale readiness acknowledgements.
- If no safe space exists (including full-root capture), hide the grip rather
  than cover the recording. Existing numeric/keyboard positioning remains.
  This is not a guarantee of placement in arbitrary multi-monitor dead spaces.
- Interactive guides own four thin ARGB strips plus one bounded grip and the
  existing invisible gesture keeper. Passive picker highlights still own only
  their original four strips and keeper, with no grip or input interception.
- Reuse the existing explicit pointer-grab identity, absolute root positions,
  release/cancellation/watchdog and pause/retarget safeguards. No idle root
  pointer listener is introduced. A live gesture survives grip relocation.
- The glyph is drawn with native primitives, without UI text/font dependencies.
  A standard [X11 move cursor](https://www.x.org/releases/X11R7.6/doc/libX11/specs/libX11/libX11.html)
  is optional: a missing cursor font does not disable the grip. New graphics and
  cursor resources are released with the guide's owned connection.

The capture backend and project/GIF pixels are unchanged. Wayland does not gain
a global desktop recording frame from this X11 change. The earlier automatic
GL/Vulkan startup compatibility fix remains in place.

## Regression coverage

Geometry tests cover scaled hit targets, root containment, screen edges, small
and signed rectangles, full-root/hidden cases, controller/old-frame exclusion,
and short top-edge recordings. Desktop tests cover move-only behavior in all
movable stages and guide invalidation on scale/controller changes.

Owned-Xvfb tests exercise the real large grip's painted icon and corner press,
motion/release across presentation changes, unchanged protected capture pixels,
controller exclusion, passive highlight behavior and cleanup. These run with
the existing **44** explicit private-Xvfb tests; they do not claim full hardware
or compositor acceptance. Source-build visual checks showed the normal and 150%
grips in a private GNOME/X11 desktop; package-specific evidence is recorded when
the actual build is verified.

## Verified build and local delivery

Implementation source:
[`2083e3aa63ca7eece74d0e88f9a242ca45390087`](https://github.com/tiansongyu/gifromscreen/commit/2083e3aa63ca7eece74d0e88f9a242ca45390087).
[Linux CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34335811710)
and [portable CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34335811646)
pass. Local Rust 1.88.0 and 1.98.0 each pass **1,882** workspace tests with **58
ignored**, plus strict Clippy. The **44** explicitly invoked owned-Xvfb tests
pass separately; ignored tests are not implicitly counted as passes.

The first visual lab (`/tmp/gfs-wayland-qa._5f9ocki`) confirms the 144×36 grip,
ready-state movement and actual 216×54 grip at 150% UI zoom. The actual scaled
image is `logs/05b-grip-150.png`; the earlier `05` capture followed a failed
window-title lookup and is not scale evidence. That lab was stopped before
testing the final short-edge placement refinement.

The final source build (desktop SHA-256
`1506a96d5a4373919586d65b5ec32a9ab38231dd4affa25121148f1bdac5681b`)
runs in `/tmp/gfs-wayland-qa.jyfk3syl`, an owned GNOME/Xvfb **X11** desktop.
The fixture's “Wayland QA” title is only its reusable fixture name.

- Move a 120×80 recording to y=22 using the top grip: it relocates to the left
  as a 36×80 side grip, above the controller. Drag that grip back to y=300.
- Start recording, move the grip from x=820 to x=1000 while recording, then
  pause. Move back to x=820 while paused, resume and stop into the editor.
- Revision **77** has **42 frames**, all **120×80**, duration **4,056,885 µs**.
  Origin runs are **13** frames at (820,300), **17** at (1000,300), then **12**
  at (820,300). Every stored pixel equals the fixture's expected red/green
  pixels: no grip, border, controller or shadow pixels enter this bounded case.
- GUI export and CLI export after normal app close are byte-identical:
  **459 bytes**, SHA-256
  `26234a1334e3007b0567a94880a777bf90454d50f5baed3b6309a41c3e4b5483`.
  Identical source frames coalesce to three GIF images with delays
  **1,200 + 1,660 + 1,200 ms**; total playback is **4.060 s**.

The actual Ubuntu 22.04 / Rust 1.88.0 package has clean build receipts, verified
source/packaging/lockfile/binary fingerprints and GLIBC requirements ≤2.35.
All **12 portable tests** and the automatic-backend owned-Xvfb smoke pass.

- Archive: `gifromscreen-0.1.2-linux-x86_64.tar.gz`, **27,152,582 bytes**.
- SHA-256: `4256be4e9c99b530d915bfb74426376cc5e8ddc5b171cef6c8bd8ba273f0b86b`.
- Packaged desktop: `b3dc50ec28e11c7e3a804a0187f986c0dbf127084c7926b2d51f12b9eb7b1708`.

That packaged executable also opens the actual recorder and moves a 640×480
frame from (0,0) to (80,60) with its side grip, which relocates to the top.
See `logs/13-package-grip.png` and `14-package-grip-moved.png` in the final lab.
Both desktop app processes exit 0. A late xdotool key event reports BadWindow
after app closure; the remaining synthetic fixture is terminated by scoped stop
(exit -15). GNOME exits 0, lab cleanup completes and both supervisor handles
are consumed. No test desktop or grab remains running.

Package/recording evidence is retained in `/tmp/gfs-grip-012-package.KGksf2Xj`;
`verification.json` SHA-256 is
`06e59f6cbb1051822a0055c0c607ab105fba5f6452197321d0b62e36b861404d`.
The package was placed in `/home/ubuntu/Videos/gifromscreen-0.1.2-linux-x86_64`.
The existing user installation and desktop menu were upgraded through the checked
installer after verifying its ownership manifest; only **575 registered app
files** were replaced. Previous bundles and project data remain, and preferences
have the same hash. The installed executable starts on the user's NVIDIA RTX 4090
using automatic Vulkan selection, without a backend override.

This is a local/CI maintenance build, not a replaced v0.1.0 Release or a claim
of full hardware/Wayland/mixed-DPI/multi-monitor acceptance.
