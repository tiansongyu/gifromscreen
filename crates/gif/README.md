# gif-from-screen-gif

Portable GIF encoding port plus the default built-in adapter for GifFromScreen.

Implemented now:

- pull-based streams of owned, straight-alpha sRGB RGBA8 frames;
- per-frame microsecond durations with cumulative 10 ms GIF tick rounding;
- exact duplicate-frame merging;
- once, finite, and infinite loop behavior;
- deterministic local or memory-bounded global median-cut palettes with 2–256 entries;
- no dithering, ordered Bayer 4x4, and Floyd-Steinberg strategies;
- 1-bit transparency via an alpha threshold;
- changed-rectangle encoding with disposal-to-background when opaque pixels
  become transparent;
- progress events and cooperative cancellation;
- delay splitting for a frame longer than GIF's `u16` delay field;
- explicit encoder finalization so trailer/write errors are returned.

The encoder writes full-canvas frames and uses Keep disposal. `RgbaFrame` already
carries an optional dirty rectangle and `FrameQuantizer` is replaceable. The next
palette/optimization implementations can therefore be added behind the same
application port:

- fixed and custom palette planners;
- NeuQuant/Octree/Wu/high-frequency/grayscale quantizers;
- Atkinson, Burkes, and Sierra-family dithering;
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
