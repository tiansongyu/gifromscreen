# gif-from-screen-media

Bounded decoding for media imported into GifFromScreen projects.

Implemented inputs:

- animated GIF through `decode_gif`, composited into complete RGBA8 canvases;
- static PNG, JPEG, BMP, and WebP through `decode_static_image`;
- detected format plus decoded pixels through `decode_static_image_with_format`;
- straight-alpha RGBA8 output with positive per-frame microsecond durations;
- content-based format detection rather than filename extensions;
- dimension, frame-count, retained-pixel, decoder-memory, and address-space
  checks before full static-image pixel decoding;
- configurable handling for zero-delay GIF frames.

Static images use the same `DecodedAnimation` model as GIF imports, with one
frame and `LoopBehavior::Once`:

```rust
use std::io::Cursor;

use gif_from_screen_media::{StaticImageDecodeOptions, decode_static_image};

# let png_bytes = Vec::<u8>::new();
let options = StaticImageDecodeOptions::default();
let result = decode_static_image(Cursor::new(png_bytes), &options);
# let _ = result;
```

Call `decode_static_image_with_format` when source provenance needs a reliable
media type. Its `DecodedStaticImage::format()` value comes from the same content
probe used for decoding, and `StaticImageFormat::media_type()` returns the
canonical MIME string without consulting a filename. `animation()` borrows the
normalized result and `into_animation()` consumes the wrapper. The original
`decode_static_image` API remains a compatibility wrapper and does not repeat
probing or decoding.

The static decoder first identifies an allowlisted format and constructs a
bounded header decoder. It validates the oriented dimensions, one-frame
capacity, RGBA pixel count, native decoder buffer, and platform address space
before decoding the complete image. The same allocation limits are passed to
the full decoder. PNG, JPEG, BMP, and WebP support is compiled explicitly; GIF
is rejected by this entry point so animations cannot be silently flattened.

EXIF orientation is applied when exposed by the active image codec. JPEG and
WebP currently provide this metadata, including width/height swaps before
dimension-limit checks. PNG and BMP are also normalized if their codec exposes
an orientation. Embedded ICC profiles are not yet transformed; decoded channel
values are treated as sRGB.
