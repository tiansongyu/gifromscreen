# Cinemagraph: pinned behavior and remaining implementation

This audit uses ScreenToGif 2.43.2, commit
[`a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`](https://github.com/NickeManarin/ScreenToGif/tree/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd).
It corrects an important distinction: the implemented schema-6 **Rectangular
freeze** is a useful extension, not the reference Cinemagraph behavior.
Its existing projects and RGBA-overwrite semantics remain unchanged.

## Source-backed contract

[`ApplyCinemagraphButton_Click`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L2761)
requires at least one ink stroke. It takes the **first project frame**, unions
each stroke's `GetGeometry()` result, and XORs that union with the image
rectangle. Inside the image, this leaves the area outside the painted motion
region. The pen color/highlighter appearance is not itself an opacity mask.
Geometry is mapped from the displayed image back into image coordinates.

The first frame is placed in an `Image` with this clip, measured and arranged,
then rendered through
[`GetScaledRender(UIElement, ...)`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/ImageUtil/ImageMethods.cs#L2104):
a bounds-adjusted `VisualBrush` drawn into a PBGRA32 `RenderTargetBitmap`.
That bitmap goes directly to
[`OverlayAsync(..., false)`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L5591),
which draws each **selected** current image followed by the clipped reference,
then saves a PNG. Undo snapshots all frames, but this does not mean that the
operation's pixels are applied to all frames.

There is **no PNG/WIC round trip between the clipped reference render and its
overlay**. Introducing one can change low-alpha colors. Transparent reference
pixels leave current pixels present under source-over, unlike Rectangular
freeze's exact RGBA overwrite. Semi-transparent reference pixels may leave
animation showing through; the reference does not forcibly make them opaque.

The [`CinemagraphGrid` and ink canvas](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml#L3670)
provide pen, point eraser, selection and stroke eraser; independent pen/eraser
width and height (1–100), rectangle/ellipse tips and pen curve fitting. These
are authoring requirements, not satisfied by a rectangular numeric control.

## What is implemented, and what is not

| Behavior | Rectangular freeze (implemented) | Reference Cinemagraph (remaining) |
|---|---|---|
| Reference image | Current frame | First project frame |
| Motion region | Numeric rectangle, optional inversion | Union of authored ink geometry |
| Pixel rule | Copy all four RGBA bytes | Clipped premultiplied source-over |
| Target scope | Selected frames | Selected frames |
| Authoring tools | Rectangle coordinates | Pen, both erasers, selection, tip geometry, curve fitting |

The [schema-6 implementation](NONDESTRUCTIVE-CINEMAGRAPH.md) and
[native acceptance](NATIVE-FREEZE-QA-2026-09-07.md) establish only the left-hand
column. Earlier use of the Cinemagraph name for that extension must not be
treated as full parity evidence. The UI now calls it Rectangular freeze.

## Implementation decisions under verification

The reference-compatible path must retain the existing non-destructive ordered
history, schema upgrade and source-input stage guards. It must also preserve
the clipped reference's premultiplied precision until final compositing.
A separate real-WPF probe is being prepared to measure clip coverage and the
effect of accidental intermediate PNG boundaries before choosing the durable
pixel representation. An unmarked premultiplied buffer must not be passed as a
straight-alpha `RgbaSurface` or silently accepted as an ordinary source image.

Acceptance includes differing first/current frames, gapped selections,
transparent and low-alpha baselines, overlapping strokes, clipped tip edges,
zoom/DPI mapping, both erasers, selection edits, cancellable rendering,
Undo/Redo/reopen, copy after source deletion and preview/GIF equivalence.
Exact WPF clip rasterization, curve fitting and fractional-DPI details remain
unverified until supported by actual independent fixtures; source inspection
alone does not establish numerical equality.
