# Remaining Cinemagraph geometry and authoring work

Schema 7 already preserves typed clipped references and matches their native
composition. The remaining producer must generate those clipped pixels from
Linux ink input. This plan records the source/measurement distinctions so that
implementation does not silently replace the reference with ordinary A8 painting.

Implementation update: Rust now has bounded subpixel ink types, pressure-aware
tip/sweep outlines, the default WPF fitting algorithm, and an HFD32/64 adaptive
scan converter with per-path winding and geometric union. The renderer suite
passes 137 tests; its 14 outline and 15 raster tests also pass on Rust 1.88.
Five independent compilations of the original WPF C++ curve algorithm matched
the Rust flattened-vertex digests, including large-coordinate fallback. These
are component/source-rule checks, not proof of complete Stroke.GetGeometry,
point-erasure or degenerate VisualBrush equivalence. The [interactive authoring
integration](CINEMAGRAPH-AUTHORING.md) now passes automated checks and the
[bounded native authoring sequence](NATIVE-CINEMAGRAPH-QA-2026-09-07.md).
Independent fitting/erasing comparison and exact geometry fidelity remain open.

## Verified source rules, not yet a complete Rust ink implementation

The upstream application is pinned to `a4d0a67`; WPF algorithm sources below
are pinned to `a04736acb8edb533756131d3d5fc55f15cd03d6a`.

- ScreenToGif defaults are a 30×30 elliptical pen and disabled curve fitting.
  The reference drawing-attributes converter leaves `IgnorePressure` false.
- [`StrokeNodeEnumerator.GetNormalizedPressureFactor`](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/PresentationCore/MS/internal/Ink/StrokeNodeEnumerator.cs)
  uses float32 `1.5 * pressure + 0.25`, not `2 * pressure`. Stylus pressure
  defaults to 0.5, so the ordinary pen size is retained. Pressure zero still
  gives 25% size; `IgnorePressure` forces a multiplier of one.
- Rectangular tips start at centered ±width/2 and ±height/2 vertices. Elliptical
  tips use a circular-space transform. Adjacent nodes generate connecting
  quadrilaterals, including unequal-pressure tangent geometry; isolated stamps
  or a fixed maximum-width line are not equivalent.
- Ellipse outlines use four cubic segments with the standard coefficient
  approximately `0.5522847498307933984`. WPF's StrokeRenderer also prunes nodes
  and has separate transformed-tip logic. A conceptual union of ideal ellipses
  is not yet proof of equivalent `GetGeometry()` output.
- [`Stroke.GetBezierStylusPoints`](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/PresentationCore/System/Windows/Ink/Stroke.cs)
  uses Himetric conversion, size-dependent fitting/flattening tolerances and
  cumulative-arc-length pressure interpolation. Default fitting error derives
  from the bounding-box width plus height, not total path length. A generic
  spline or point-index interpolation must not be assumed equivalent.
- [`InkCanvasSelection.TransformStrokes`](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/PresentationFramework/MS/Internal/Ink/InkCanvasSelection.cs)
  calls `Stroke.Transform(matrix, false)`: selection movement/resizing changes
  point coordinates, not pressure, tip size or its DrawingAttributes matrix.
  Its affine map uses selection bounds including the tip; the final new ink
  bounds need not scale identically because the tip itself does not shrink.

## Rasterization evidence and next component

The actual [probe corpus](PREMULTIPLIED-SNAPSHOTS.md) records post-Boolean
geometry strings and clipped-white pixels. A temporary in-memory model, not
production Rust, reproduces all 81 non-whole cases using:

1. Float32 coordinates and WPF's 28.4 fixed conversion. Half ties round toward
   positive infinity, not ties-even. The coordinate transform subtracts 0.5
   before multiplication by 16; AA initialization then uses `(q + 8) << 3`.
2. Fixed-point cubic control points and the native Bezier32/HfdBasis32 adaptive
   expansion, with approximately quarter-pixel error. Ideal ellipse sampling
   or globally fixed subdivision counts are not equivalent substitutes.
3. Eight-by-eight scan conversion. The grid is equivalent to `(i/8, j/8)` within
   each pixel, not subpixel centers. Edge and span handling is half-open with
   upward-rounded scan intersections and nonzero winding.
4. PM channel scaling `(channel * coverage_count * 4 + 128) >> 8` for counts
   `0..64`. Ordinary 255-denominator alpha multiplication is different.

Primary sources: [AA conversion/scan setup](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/WpfGfx/core/sw/swlib/aarasterizer.cpp),
[coverage accumulation](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/WpfGfx/core/sw/aacoverage.h),
[rounding](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/WpfGfx/common/shared/real.h),
[cubic expansion](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/WpfGfx/core/geometry/bezier.cpp).

The temporary model also independently reconstructs the four rectangle/ellipse
geometries from fixture parameters (36 alpha combinations) with zero differences.
The small ellipses happen to use four line segments per quarter after expansion;
that count must not be hardcoded for arbitrary sizes. Wrong ties-even rounding
caused six pixels per rectangular-tip line case to differ; correct half-up
removed those differences. These are useful numerical findings, not a committed
Rust implementation or proof of stroke expansion/Boolean union.

The nine synthetic whole-rectangle cases remain distinct: actual VisualBrush
pixels repeat the source's first row, while direct PushClip produces transparent
pixels. Further real Ink full-coverage and overlapping-stroke probes must determine
the reachable behavior before deciding how to handle that degenerate case.
Do not silently erase it from reference results or call a coverage-only fix exact.

## Native editor integration

Use a separate `CinemagraphDraft`, owned by MotionTools, and a small preview
adapter. Do not expand the old DrawingOverlayDraft into a general scene framework.
The existing workspace-loan/background-task path remains responsible for final
source rendering, one shared PM asset and one atomic selected-frame edit.

- Add an exclusive Cine input branch to the existing preview, with playback and
  transition editing disabled. Clearly label the first-frame reference separately
  from the selected target frames; viewing the reference must not alter selection.
- Preserve physical subpixel coordinates. Map through displayed-image bounds
  directly to rendered dimensions; do not reuse the existing integer-floor plus
  half-pixel ordinary-drawing mapping, multiply DPI twice, or use thumbnail size.
- Consume press/move/release events with bounds. A pure click must produce a dot;
  `drag_started` alone loses it. The new tool and ordinary drawing are exclusive.
- Support pen, point eraser, stroke eraser and selection; independent 1–100 width/
  height and ellipse/rectangle tips; per-stroke curve settings. Point erasure
  splits swept geometry, not merely stored points. Selection transforms start
  from the gesture's original state to avoid cumulative drift.
- Keep stable stroke identities. Bind drafts to project/root/revision/selection
  and reference dimensions. A changed target cancels the current gesture, retains
  the draft as stale and disables Apply; never silently switch its destination.
- Apply revalidates the frozen request, renders the complete first frame, builds
  PM directly, validates the compound before publishing its asset, and checks
  cancellation before commit. Success clears the draft; failure/cancel retains
  it for retry and returns the original workspace.
- Keep Apply/Cancel outside scrolling controls. Verify 480-pixel windows and
  enlarged fonts, clear/delete/Escape, source changes, selection gaps, zoom,
  nonuniform selection scaling and actual single-click strokes.

Independent acceptance still needs multi-stroke union, pressure extremes,
IgnorePressure, 4+ point curve fitting, both erasers and native selection edits.
Matching final composition, or a convenient approximate preview, does not close
these authoring and geometry requirements.
