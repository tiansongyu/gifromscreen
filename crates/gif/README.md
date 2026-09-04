# gif-from-screen-gif

Portable GIF encoding port plus the default built-in adapter for GifFromScreen.

Implemented now:

- pull-based streams of owned, straight-alpha sRGB RGBA8 frames;
- per-frame microsecond durations with cumulative 10 ms GIF tick rounding;
- exact duplicate-frame merging;
- once, finite, and infinite loop behavior;
- deterministic local or memory-bounded global palettes with 2–256 entries;
- selectable Median Cut, Grayscale, Most Used, and Octree built-in quantizers;
- no dithering, ordered Bayer 4x4, Floyd-Steinberg, Atkinson, Burkes,
  Sierra Lite, Two-row Sierra, full three-row Sierra, Jarvis–Judice–Ninke,
  Stucki, and Stevenson–Arce strategies;
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

`MedianCut` is the balanced general-purpose choice. `Grayscale` converts source
colors to deterministic BT.601 luma values before palette reduction. `MostUsed`
prioritizes frequent colors and resolves ties lexicographically. `Octree` builds
a fixed-depth RGB tree from the bounded 5-bit/channel histogram, collapses the
least-populated deepest nodes with Morton-path tie-breaking, and emits frontier
leaves in Morton order. A complete tree is capped at 37,449 nodes, independent
of input dimensions or frame count. All four strategies work with
`PaletteMode::LocalPerFrame` and `PaletteMode::Global`, reserve transparency
inside the requested color count, and honor cooperative cancellation. Supplying
a custom `FrameQuantizer` through `BuiltinGifEncoder::new` overrides the option.

`RgbaFrame` carries an optional dirty rectangle and `FrameQuantizer` remains
replaceable. Additional palette/optimization implementations can therefore be
added behind the same application port:

- fixed and custom palette planners;
- NeuQuant/Wu quantizers;
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
