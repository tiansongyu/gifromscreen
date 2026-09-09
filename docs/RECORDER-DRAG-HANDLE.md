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
