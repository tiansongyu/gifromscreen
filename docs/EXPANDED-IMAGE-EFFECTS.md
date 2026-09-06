# Expanded image borders and software-reference shadows

Schema 4 adds `ImageBorder` and `ImageShadow` to the existing ordered render
program. These operate on the complete image at their position in history.
Earlier artwork participates in them; later artwork remains freshly drawn.
Old `Effect::Border` and `Effect::Shadow` keep their inset/clipped and box-blur
pixels. Opening old projects or presets does not silently convert those effects.

## Reference and scope

The product baseline is ScreenToGif 2.43.2,
[`a4d0a67`](https://github.com/NickeManarin/ScreenToGif/tree/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd).
Its [BorderAsync and ShadowAsync](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L5925)
establish drawing order, frame scope and canvas placement. Our canonical space
is physical pixels, corresponding to the reference at 96 DPI; historical WPF
fractional-DPI differences are not certified.

- Manual nonnegative inner borders affect selected frames. Any negative/outer
  edge applies the border to every frame, not just padding unselected frames.
- Shadows apply to every frame. Automatic task chains also address all frames.
- Borders draw the chosen background, then the source image, then left, right,
  top and bottom strokes. The compatibility default is white, including below
  transparent source holes; explicit transparent backgrounds are an enhancement.
- Shadows derive their mask from the current source image, not a previously
  filled background. The background is composited after the source/shadow result.

The editor offers the new effects as primary choices and retains clearly named
legacy choices. It shares the same style controls with automatic presets.
Shadow RGB and opacity are independent; the reference ignores shadow color
alpha. An opaque background retains smooth shadow edges in the GIF's binary
transparency model.

## Geometry and numeric representation

| Stored quantity | Unit / range | Use |
|---|---|---|
| Border edges | Signed thousandths of a pixel | Negative expands out; positive draws in. UI uses integer `-500..50` pixels. |
| Shadow radius / distance | Hundredths, `0..10000` | Original `0..100.00` pixel values, not rounded Cartesian offsets. |
| Shadow direction | Hundredths, `0..36000` | Degrees; 0 points right, 90 points up. |
| Shadow opacity | Basis points, `0..10000` | Displays `0..100.00%`; default 60%, not the upstream's inconsistent stored `60`. |

`CanvasPlacement { output_size, source_origin }` is shared by the domain plan,
CPU renderer and recorded-input authoring. Border output dimensions use
ties-to-even rounding of each pair of exterior edges; source left/top margins
are independently floored. The reference's background/stroke geometry has a
different, asymmetric truncation of only left/top, so the renderer does not
substitute a blanket fill of the final canvas. Fractional stored border values
use deterministic axis-aligned area coverage; the primary UI's integer values
cover the reference controls.

Shadow offsets use the original floating-point polar conversion, with positive
Y downward. Each margin is `blur/2 + max(offset toward that side, 0)`. The source
origin is floored; each final dimension floors the full sum of both margins and
the input size. Raster sampling separately casts offsets to f32 and then
truncates toward zero, matching the software path. Keeping these calculations
separate avoids treating the fractional margin radius as the integer blur kernel.

## Software-reference blur and alpha

ScreenToGif writes edited frames through `RenderTargetBitmap`. Official WPF
9.0 source shows the default target uses its software bitmap path:
[managed target](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/PresentationCore/System/Windows/Media/Imaging/RenderTargetBitmap.cs#L227)
and [native factory](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/WpfGfx/core/api/api_factory.cpp#L290).
Our new effect follows that algorithm, not the GPU Quality texture path.

The [software blur implementation](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/WpfGfx/core/resources/BlurEffect.cpp)
uses integer radius (capped at 100), Gaussian sigma `radius/3`, and f32 weights
with equal additive error compensation rather than simple division by the
weight sum. This even produces negative side weights at radius 1. The Rust
implementation preserves that behavior, runs vertical then horizontal alpha
passes, and clamps/quantizes each pass to eight bits using nearest-even rounding.

The [software shadow composition](https://github.com/dotnet/wpf/blob/a04736acb8edb533756131d3d5fc55f15cd03d6a/src/Microsoft.DotNet.Wpf/src/WpfGfx/core/resources/DropShadowEffect.cpp#L194)
quantizes independent opacity to a byte. Its extra alpha is the integer floor
of `blurred_alpha * (255 - source_alpha) * opacity8 / 65536`, not idealized
source-over with denominator 65025. Source premultiplied RGB and shadow RGB
contribute before the explicit background. Legacy blending functions are not
changed by these new rounding rules.

This is an implementation derived from primary software algorithms, not a
claim of final Windows PNG bit equality. Windows/WIC premultiply/unpremultiply,
libm/SIMD details, antialiasing and fractional DPI still need Windows golden
image comparison. Radius/offset/opacity rules and independent fixture pixels
are covered by automated tests.

## Editing, persistence and safety

New image operations need schema 4. The existing durable-upgrade protocol
stamps the previously committed state with the new header before journaling
new visible payloads. Schema 1–3 data remains readable, older headers reject
schema-4 payloads, and Undo never downgrades a format header. Automatic task
settings use version 2 for the new actions; loading old v1 settings leaves the
file unchanged, and explicit Save performs a sticky upgrade when needed.

Adding shadow/outer borders, or replacing/clearing canvas-changing effects,
uses all frames and updates the canvas in the same undoable command. Effect
indices include legacy-prefix effects followed by chronological effect steps.
Replacing a legacy-prefix effect with a new canvas effect is explicitly
rejected: moving it after old artwork would silently change ordering. Keep its
legacy type or explicitly remove it and add a current-image effect instead.
Changing history retains later marks' stage-local coordinates; it does not
silently regenerate them.

Metadata is bounded before cloning/committing. For image-effect programs, the
source/base and every intermediate RGBA surface are checked against 64 MiB,
including later operations affected by an earlier replacement. A final crop
cannot hide an oversized intermediate. GIF's 65,535-pixel dimension limit is
checked on the final image; an otherwise bounded wide intermediate followed by
a valid crop is not falsely rejected. Rendering remains cancellable and checks
its own surface/working-buffer limits. Failed preparation does not mutate the
project or publish partial output.

Frame copying, Yoyo, Save As, image-effect replacement, Undo/Redo, journal reopen,
preview and GIF export are covered by shared-stage integration tests. Input
authoring only translates through the shared placement: it does not paint old
border, shadow or background pixels into a newly authored cursor. Hidden-stage
Cinemagraph baking and physical platform gates remain separate work.
