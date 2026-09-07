# Independent Cinemagraph WPF probe

This manual, evidence-only probe does not change the existing WPF reference generator,
its fixture hashes, its Rust comparator, or its strict CI gate. A successful probe means
that complete measured artifacts were produced; differing diagnostic models are findings,
not failures. No Rust production implementation is generated or changed here.

## Source contract

The primary path follows ScreenToGif commit
`a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`:

- `ScreenToGif/Windows/Editor.xaml.cs:2755–2808`: the **first project frame**, not the
  current selection, supplies the reference. Stroke `GetGeometry()` results are unioned,
  then XORed with the complete frame rectangle to obtain the outside clip.
- `ScreenToGif/ImageUtil/ImageMethods.cs:2104–2163`: an unattached `Image` with `Clip`
  is measured and arranged, followed by `GetDescendantBounds`, the original fallback/clamp,
  and `VisualBrush { AutoLayoutContent = false, Stretch = Fill }` drawn into Pbgra32 RTB.
  It is not replaced with a `PushClip` shortcut in the primary result.
- `Editor.xaml.cs:5591–5628`: draw the current frame and the **actual clipped RTB** into
  another Pbgra32 RTB, then save PNG/WIC. There is **no PNG boundary between clipping and
  compositing**. The extra-boundary candidate is a separate diagnostic.

All working images use 96-DPI physical-pixel coordinates and `scale/imageScale = 1`.
Initializing project-frame PNGs and the diagnostic extra-PNG branch preserve actual WIC
pixels; only working DPI metadata is normalized after byte/format checks. Raw decoded
density is recorded. This is not evidence for attached-window or mixed-DPI behavior.
There are no windows, fonts, screen capture, input hooks, or dispatcher message loops.

## Cases and outputs

Ninety fixed 12×10 cases cover the Cartesian product of first/current image alpha
(`opaque`, `zero`, varying partial alpha) and ten clip geometries: empty/full,
integer/fractional rectangles and ellipses, actual Ink single-point/line strokes with
elliptical or rectangular tips. Ink points have explicit pressures and the line starts
outside the frame. Every bitmap is bounded to 32×32.

Each case includes:

- requested RGBA, initialized PNG and initial straight BGRA / premultiplied BGRA;
- actual `Image`/`VisualBrush` clip PBGRA, clipped-white PBGRA, white alpha A8;
- A8-mask and coverage64 predicted clip pixels;
- actual direct final PBGRA, PNG and WIC RGBA;
- extra-clip-PNG and final outputs, plus a `PushClip` diagnostic;
- per-file SHA-256, observed bounds, decoded DPI, mismatch counts and limited witnesses.

`index.json` also records the exact probe commit/source hashes, .NET SDK/runtime,
Windows image, PresentationCore/PresentationFramework, WPF native renderer and WIC
binary versions/hashes. It only reads an environment allowlist, never tokens.

Coverage analysis tests two hypotheses independently:

1. The alpha read from the white clip is an A8 coefficient with WPF-style rounded
   channel multiplication. Every pixel additionally searches all coefficients 0..255.
2. The clip uses WPF software AA's 8×8 coverage count 0..64, with
   `(channel * (count * 4) + 128) >> 8`. White alpha is inverted where representable;
   every pixel additionally searches all 65 coefficients.

The coverage64 formula is the diagnostic described in dotnet/wpf `a04736ac`,
`core/sw/aacoverage.h:19–32` and `core/sw/swlib/aarasterizer.cpp:666–726`. Whether the actual
`Image.Clip` path follows it is determined by measurements, not assumed. `coverage.c64`
uses byte 255 as the sentinel when white alpha is not one of its 65 levels. Full-opacity
white and zero-alpha source cases alone cannot establish general losslessness.

The design decision under investigation is an immutable **typed premultiplied clip**,
not persistence of an arbitrary A8 mask. Even matching tiny cases do not prove a universal
mask representation for all WPF geometry and image transformations.

## Independent ink geometry diagnostic

`ink-geometry.json` is a separate, version-1 diagnostic referenced by
`index.json.additional_diagnostics`. It does not change the original 90 cases, their
definition hash, or their primary Cinemagraph pixel path. It records actual WPF:

- Raw attributes/samples, `GetBezierStylusPoints`, effective samples and
  `Stroke.GetGeometry` path/bounds for curves, cusps, loops, duplicate nodes and
  varying pressure, with both tips and FitToCurve on/off; a three-point case is separate.
- Explicit pressure 0/0.5/1 and IgnorePressure combinations, plus the constructor's
  implicit default pressure. The pen is 4.25 by 3.25 physical units.
- Before/after `Stroke.Transform(matrix, false)` samples, attributes and bounds for
  translation and nonuniform affine scaling. This is the method used by InkCanvas
  selection, not a simulated pointer/selection-adorner test.
- Actual swept eraser `HitTest` and `GetEraseResult` fragments for crossing and missed
  strokes. These outputs do not establish that a separate polygon eraser is equivalent.

`GetBezierStylusPoints` computes a fitted path even when FitToCurve is false;
`effective_samples` explicitly chooses the samples used by the stroke outline.
Geometry output is bounded to 64 raw samples, 4096 fitted samples per observation,
65536 path characters, finite coordinates within +/-64, 128 observations and 65536
total samples/controls. Its JSON is capped at 2 MiB inside the existing aggregate
16 MiB and 115/120-second limits. No new pixels or reference tolerances are introduced.

Source hashes now enumerate all top-level `.cs` files and verify an explicit bounded
allowlist. Adding this diagnostic changes the generator/source and README hashes, so
a new workflow run must regenerate evidence; old artifacts must not be edited to match.
The project compiles only top-level `*.cs` files, matching that inventory; nested
files cannot silently participate through the SDK's default recursive glob.

## Run and limits

Use the independent **Manual Cinemagraph WPF probe** workflow (`workflow_dispatch` only),
or on Windows with .NET 9 and PowerShell 7:

```powershell
Push-Location scripts/qa/cinemagraph_probe
dotnet build CinemagraphProbe.csproj --configuration Release
$env:GITHUB_SHA = (git rev-parse HEAD).Trim()
./run-probe.ps1 -OutputDirectory "$env:TEMP/gfs-cinemagraph-probe-unique-name"
Pop-Location
```

Output and sibling `.logs` directories must not already exist. The runner supervises only
its child process tree: 120 seconds, 512 MiB working set, 16 MiB artifacts (at most 4096
files), 2 MiB logs. The generator also checks a 115-second phase limit, working set and
artifact bytes. Partial output is retained on failure; absence of `index.json` means it
is not a completed probe. Nothing is automatically dispatched, deleted or overwritten.
