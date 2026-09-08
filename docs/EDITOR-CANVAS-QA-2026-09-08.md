# Editor zoom and direct crop

## Contract

Fit fits the composed image within the preview column and a 360-point viewport,
including enlarging very small images. 100% and 200% instead map each composed
image pixel to one or two physical display pixels, independently of UI scale.
Both exact modes reuse one full-resolution nearest-filter texture; Fit retains
its separate bounded downsampled texture. Scrollbars, wheel and middle-drag pan
without altering the saved image. Every annotation painter shares the clipped
image viewport. Middle-drag starts only in the current visible viewport.

Exact views retain the device texture-side limit and 64 MiB cache budget. An
oversized image gets an explicit error and a usable Fit choice, not silent
downsampling labeled 100%. CPU composition retains its existing bounded surface
budget. Tiled exact rendering is not implemented; this is not arbitrary-size or
physical mixed-DPI acceptance.

“Crop on preview…” creates a separate draft, bound to the project, revision and
current selection. Dragging in either direction and numeric X/Y/W/H edit the
same composed-image coordinates, never downsampled texture coordinates. Apply
uses the existing ordered whole-image crop command on **all** frames, updating
the canvas atomically. It does not replace original assets or retime frames.
Undo/Redo and recovery use the existing journal. Cancel never edits the project.
Playback, transition/Cinemagraph authoring and drawing do not concurrently own
the crop gesture. Stale selection/revision/project invalidates the draft.

Focus loss, pointer loss while held or a changed image mapping cancels the
unfinished gesture and restores the previous draft. A primary release before
leaving the window in the same input batch completes the gesture at the actual
release position; later pointer movement must not extend or cancel that crop.

## Automated evidence

The desktop suite has 624 tests after this addition. New tests cover physical
zoom at 1, 1.25 and 2 pixels/point; exact extent and viewport clipping; scrolling
without texture replacement; middle-drag hit testing; full-resolution nearest
textures, Fit separation, reuse and revision invalidation; resource rejection
before asset I/O; reverse drag and release/loss ordering; invalid numeric bounds;
stale project/revision/selection; and composed pixels through whole-animation
crop, Undo/Redo, checkpoint and reopen.

## Bounded native acceptance

Private lab `/tmp/gfs-wayland-qa._lktjgzc`, Xvfb 1440×1000, GNOME/Mutter 42.9,
software-rendered X11; started through `scripts/qa/wayland_nested.py` with an
1,800-second bound. No host desktop or service was used.

Initial app SHA-256:
`49358e7a2fd63bc3f9723d2f852e635cd43f504300ff2280774e4b60033a9e14`.
`demo-project` created a fresh owned 36-frame 160×96 project, revision 36, with
50 ms per frame. Native screenshots retain these observations:

- `04-editor.png`: Fit; `05-native.png`: actual 160×96 image at 100%.
- `06-crop-ready.png`: 200% 320×192 image and full-image draft.
- `08-held.png`: drag from screen (544,547) to (744,667), image origin
  (524,527), yields X=10, Y=10, W=100, H=60. Apply disabled until release.
- `09-applied.png`, `10-undo.png`, `11-redo.png`: all 36 thumbnails and current
  image change to 100×60, restore to 160×96 and return to 100×60.

The first rapid release/leave attempt (`07-drag.png`) exposed erroneous draft
cancellation. It is retained as a failed observation, not hidden. The patched
binary `extra-app-3453630`, SHA-256
`7234da81d0c2111efb926e250543f0cebfcd74437a6e8645313d08ea56c78a62`,
reopened the project via Recent projects. `13-release-outside.png` confirms
release immediately followed by moving outside the app retains the new
10,10,70,35 draft; `14-numeric.png` records width changed to 80. Cancel left
the previously applied 100×60 project unchanged. Both apps closed normally;
the patched app reported exit 0, and the lab was explicitly stopped.

CLI export after native Undo/Redo and normal close reopened revision 39 with
36 selected frames. `cropped.gif` decodes to 100×60, 30 images, **1,800 ms**,
20,467 bytes. Identical neighboring output frames merge, retaining their delay
(one GIF image is 350 ms; the others are 50 ms). Export after the patched app's
reopen/cancel/normal close produces byte-identical `reopened.gif`:
`a1530f13577cd1fe67dbac6b5ca8aa7fb205f7aa8d9d067d1a55a61568cfe815`.

This native run establishes the small-image editing/reopen/export workflow.
Large-image native panning, hardware/mixed-DPI and broad compositor acceptance
remain separate gates; automated geometry tests do not establish those results.
