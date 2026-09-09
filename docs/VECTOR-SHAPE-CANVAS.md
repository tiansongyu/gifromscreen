# Vector shape canvas (Linux main)

This implements the next slice identified by the [pinned shape audit](SHAPE-PARITY-2026-09-09.md).
It is newer than the frozen Preview 3 download. It is not a claim of full
ScreenToGif pixel/interaction parity, native acceptance, or a new release.

## Use

In the editor, select the target frames, open Overlays → Shapes and choose
**Open shape canvas**. Drag on the preview to insert rectangles, ellipses,
triangles or the closed right-pointing block arrow. Stroke, fill, rounded
rectangle radius and rotation have separate controls. Width/radius preserve
hundredths of a physical image pixel; viewport zoom never changes the stored style.

Switch to Select to click, Ctrl-select or marquee-select shapes. Selected objects
move to the front without changing their relative order. Drag to move, use the
eight handles to resize, and the circular handle to rotate. Handles follow the
object's rotation; resizing uses local axes and keeps the opposite edge fixed
until the layout box reaches the canvas boundary. A group shares a single legal
size-delta interval, avoiding order-dependent minimum-size violations.

Alt/Ctrl/Shift + wheel rotate selected objects by 90°/1°/20° respectively, only
when the canvas has keyboard focus and the pointer is over it. These wheels are
removed before egui's global zoom/scroll handling. Delete/Backspace only delete
draft objects when the canvas owns focus; clicking a text field transfers input
before a following Delete from the same native batch. Ordinary wheels and other
widgets retain their input. All controls/notices in this new tool have en/zh messages.

Apply adds the complete ordered group to every selected frame in one undoable
command. Noncontiguous selections stay noncontiguous. Close discards only this
draft. A project/revision/reference/selection change makes it stale: confirmed
objects remain visible, Apply is disabled, and Restart explicitly starts over.
Mapping changes, focus loss and Escape roll back the unfinished gesture only.
No project or journal writes happen while drawing or generating the preview.

## Persistence and rendering

- New `OverlayContent::VectorShape` version 1 uses schema 8. Existing `Shape`
  payloads and their Line/Arrow/Rectangle/Ellipse pixel path are unchanged.
  In particular, the new BlockArrow never reinterprets an old line arrow.
- Geometry is persisted in signed/unsigned hundredths, with checked ends and
  finite fixed bounds. Radius and stroke are 0–100 px; angles normalize into
  [0°, 360°). Transparent and zero-width styles are valid.
- `VectorCanvasPbgra8PngV1` is an explicit new isolated paint stage. All its
  frame-owned Normal-blend vector marks composite onto one transparent PM canvas;
  that canvas then composites onto the frame once before straight-alpha conversion.
  The grouping is persisted, not inferred from whichever layers are visible.
- The renderer, picking and marquee share fixed line/cubic contours, including
  independently clamped rectangle radii and the original triangle/block-arrow
  vertices. Picking tests geometry, including transparent interiors, not alpha
  pixels or just an axis-aligned bounding box.
- Curves are linearized with bounded subdivision before the pinned tiny-skia
  line-only stroker. A8 coverage is generated in global canvas coordinates;
  only composition is tiled. Independent small-mask rasterization was rejected
  because it changes edge quantization. The exact single-mask comparison stays
  as a regression test.

The renderer's antialiasing is independent of WPF. The [real Windows reference
suite](../scripts/qa/wpf_reference/README.md) is the strict fidelity gate; geometry
unit tests and manually checked PM arithmetic do not establish WPF equality.
Inherited WPF layout rounding and fractional-DPI behavior need separate evidence.
The rotated local-axis interaction is intentional; exact upstream group-delta
and boundary behavior are not certified by these tests.

## Bounds and failure behavior

Drafts hold at most 256 objects. Input retains at most 512 events/1 MiB and uses
bounded hit queries. Preview work has one asynchronous slot, generation checks,
cancellation and a 16 MiB working limit. Shutdown waits for its worker.
Full-resolution rendering retains the shared surface limit, a 32,768-segment
curve limit and a 100-million-unit work bound. A8 masks and stroker/scan scratch
are counted; a single dependency mask fill can only check cancellation before
and after that library call, not on each internal scanline.

Apply checks the anchor, reference paint dimensions, all shapes, 40,000-cell /
100,000-mark limits and the 16 MiB serialized author-command budget before a
single journal edit. Invalid/empty/oversized groups leave manifest/journal bytes
unchanged. Successful small previews do **not** certify every full-resolution
export will fit its working budget; that stronger preflight remains open.

## Acceptance still required

Automated tests cover fast native event batches, input/focus/mapping ownership,
rotated handle geometry, cancellation, typed limits, atomic failures, selection
gaps, isolated PM rounding, undo/redo and independent project reopen. These are
not screenshots of the running Linux desktop.

Before promoting this tool in a new release: run native X11/Wayland interaction
at Fit/100%/200%, UI zoom/language changes, copy/Save As/reopen/GIF workflows;
inspect the new control layout and complete the independent WPF comparisons.
General platform, remaining UI localization and 27 additional-catalog gates
remain in the [full Linux plan](NEXT-LINUX-ITERATION.md).
