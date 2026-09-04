# gif-from-screen-gif

Portable GIF encoding port plus the default built-in adapter for GifFromScreen.

Implemented now:

- pull-based streams of owned, straight-alpha sRGB RGBA8 frames;
- per-frame microsecond durations with cumulative 10 ms GIF tick rounding;
- exact duplicate-frame merging;
- once, finite, and infinite loop behavior;
- deterministic local or memory-bounded global palettes with 2–256 entries;
- selectable Median Cut, Grayscale, Most Used, Octree, Wu, and NeuQuant
  built-in quantizers;
- custom fixed palettes plus Web Safe 216, monochrome, and Windows 16 presets;
- no dithering; ordered Bayer 4x4, dotted halftone, fixed blue noise, and
  interleaved-gradient noise; plus Floyd-Steinberg, Atkinson, Burkes, Sierra
  Lite, Two-row Sierra, full three-row Sierra, Jarvis–Judice–Ninke, Stucki, and
  Stevenson–Arce strategies;
- 1-bit transparency via an alpha threshold;
- changed-rectangle encoding with disposal-to-background when opaque pixels
  become transparent;
- progress events and cooperative cancellation;
- delay splitting for a frame longer than GIF's `u16` delay field;
- explicit encoder finalization so trailer/write errors are returned.

Choose a built-in quantizer through the encoding options; Median Cut remains the
default:

```rust
use gif_from_screen_gif::{EncodeOptions, QuantizerStrategy};

let options = EncodeOptions {
    quantizer: QuantizerStrategy::MostUsed,
    ..EncodeOptions::default()
};
```

Select dithering independently from palette generation:

```rust
use gif_from_screen_gif::{DitherMode, EncodeOptions};

let options = EncodeOptions {
    dither: DitherMode::Sierra,
    ..EncodeOptions::default()
};
```

`Bayer4x4` uses a fixed ordered matrix. The nine error-diffusion modes scan
left-to-right deterministically: `FloydSteinberg`, `Atkinson`, `Burkes`,
`SierraLite`, `TwoRowSierra`, `Sierra` (the full three-row variant),
`JarvisJudiceNinke`, `Stucki`, and `StevensonArce`. Transparent pixels neither
receive nor emit diffusion error, and their hidden RGB values cannot alter
neighboring opaque pixels. Every mapper cooperatively checks the export
cancellation token while it works.

The three ScreenToGif/KGySoft-style coordinate patterns are deterministic and
use a fixed 25% strength, producing a shared sRGB channel adjustment in
`-32..=31`. `Dotted` repeats the public 8×8 dotted-halftone matrix. `BlueNoise`
repeats a pregenerated 64×64 tile from Bart Wronski's MIT-licensed
BlueNoiseGenerator, with the attribution retained beside the embedded table.
`InterleavedNoise` evaluates the nonrandom Jimenez/Wronski interleaved-gradient
formula from integer pixel coordinates. These modes keep the same spatial
pattern across animation frames; transparent pixels bypass both noise and
palette lookup, so hidden RGB values cannot affect opaque results.

`MedianCut` is the balanced general-purpose choice. `Grayscale` converts source
colors to deterministic BT.601 luma values before palette reduction. `MostUsed`
prioritizes frequent colors and resolves ties lexicographically. `Octree` builds
a fixed-depth RGB tree from the bounded 5-bit/channel histogram, collapses the
least-populated deepest nodes with Morton-path tie-breaking, and emits frontier
leaves in Morton order. A complete tree is capped at 37,449 nodes, independent
of input dimensions or frame count. `Wu` builds population, RGB, and
squared-error integral moments on a fixed 33×33×33 lattice, then repeatedly
applies the cut with the greatest reduction in within-box variance. Exact ties
retain box order, then RGB axis order, then the lowest cut. Its shared
32³ histogram plus moment lattice use under 3 MiB regardless of input dimensions
or frame count, and accumulator overflow is reported rather than saturated. All
six learned strategies work with `PaletteMode::LocalPerFrame` and
`PaletteMode::Global`, reserve transparency inside the requested color count,
and honor cooperative cancellation. Supplying a custom `FrameQuantizer` through
`BuiltinGifEncoder::new` overrides the option.

`NeuQuant` trains the permissively licensed `color_quant` implementation on an
even deterministic sample capped at 65,536 opaque pixels. Training always uses
the library's documented 64–256 neuron range and a valid 1–30 sample factor;
requests below 64 colors frequency-prune the learned codebook with stable RGB
tie-breaking. Transparent pixels never enter the network and index zero remains
reserved exclusively for transparency. Global source-frame buffering remains
governed by `EncodeOptions::global_palette_buffer_limit_bytes`.

Fixed palettes bypass palette learning while retaining the same nearest-color,
ordered-dither, and error-diffusion mapping pipeline:

```rust
use gif_from_screen_gif::{BuiltinGifEncoder, FixedPaletteQuantizer, PredefinedPalette};

let monochrome = FixedPaletteQuantizer::from_predefined(PredefinedPalette::Monochrome);
let encoder = BuiltinGifEncoder::new(Box::new(monochrome));
# let _ = encoder;
```

`FixedPaletteQuantizer::new` accepts 2–256 tightly packed RGB entries and an
optional existing transparent index. Predefined palettes can prepend a
transparent entry with `from_predefined_with_transparency`. That entry is never
used for opaque pixels, even when an opaque entry has identical RGB values.
Local and global modes return the same fixed palette. If a frame needs
transparency but none is designated, or `EncodeOptions::max_colors` is smaller
than the fixed palette, encoding fails explicitly and never truncates colors.
`WebSafe216` uses the six channel levels 00/33/66/99/CC/FF in RGB order;
`Windows16` uses the conventional Windows/HTML 16-color ordering encoded in the
public palette definition.

`RgbaFrame` carries an optional dirty rectangle and `FrameQuantizer` remains
replaceable. Additional palette/optimization implementations can therefore be
added behind the same application port:

- more aggressive transparent-rectangle and disposal optimization;
- lossy similar-frame merging.

Atomic `.partial` output, fsync, rename, queueing, and retry policy belong to the
application export service; this crate deliberately accepts an arbitrary
`std::io::Write` destination.

Durations are rounded from cumulative presentation timestamps. Consequently a
distinct frame shorter than 5 ms can legally receive a zero GIF delay. This is
the only way to keep long-animation drift below one 10 ms tick without changing
frame content, but some viewers clamp zero-delay frames. A future export policy
may optionally coalesce or clamp such frames and expose the resulting timing
tradeoff to the user.
