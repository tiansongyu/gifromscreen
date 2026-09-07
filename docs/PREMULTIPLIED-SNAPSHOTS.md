# Typed Cinemagraph snapshots — schema 7

The storage and composition path for an **already clipped** Cinemagraph
reference is implemented. The Linux freehand clip producer and complete ink
authoring tools are still separate work; this is not a complete Cinemagraph
feature declaration. See [the pinned behavior audit](CINEMAGRAPH-REFERENCE.md).

## Representation

`AssetKind::PremultipliedSnapshot { size, format_version: 1 }` is deliberately
not an ordinary raster. Its `raster_descriptor()` returns `None`; frames,
cursors, ordinary overlays (including hidden ones), and rectangular freeze
references cannot consume it as straight RGBA. The renderer uses a separate
immutable `PremultipliedRgbaSurface` and an explicit
`FrameAssetProvider::load_premultiplied_rgba8` method. Old providers default to
a clear unsupported error, not a lossy implicit conversion.

The version-1 container is:

| Offset | Bytes | Field |
|---:|---:|---|
| 0 | 7 | ASCII `GFSPM8` followed by NUL |
| 7 | 2 | Little-endian version, exactly 1 |
| 9 | 4 | Little-endian physical width |
| 13 | 4 | Little-endian physical height |
| 17 | `4 × width × height` | Premultiplied channels in **RGBA byte order** |

Every RGB channel must be no greater than its alpha. Zero-alpha pixels therefore
have zero RGB. The exact length and header/descriptor/step dimensions must agree;
equal-area shape aliases are rejected. Dimensions participate in the content
identity. The 17-byte header also makes the encoded length non-divisible by four,
preventing collisions with valid tightly packed ordinary RGBA assets without
changing the global content-hash algorithm or relabeling existing descriptors.

Decode and borrowed validation reject unknown versions, wrong magic, trailing
or missing bytes, invalid PM channels and budget violations. Owned decode removes
the header in place; it does not allocate a second full image or pass through PNG.
Header-only validation supports the bounded streaming Save As validator.

## Ordered execution and persistence

`FrameRenderStep::CinemagraphOverlay { snapshot_asset, snapshot_size }` operates
at its position in history. Earlier tail artwork can be sealed without changing
its precision or visibility. The complete current image is premultiplied,
source-over composited with the already-premultiplied reference and converted
once using the WIC fixed-reciprocal rule. Even a transparent reference retains
that final PNG-like boundary; unlike rectangular freeze it does not erase
current pixels by copying transparent RGBA over them.

The snapshot is never unpremultiplied and re-premultiplied between clipping and
this draw. No resizing or reshaping is inferred. Earlier original-input groups
retain their authoring space; this new mixed-image step blocks new recorded-input
replay after it while leaving source metadata and clocks intact.

Both the new step **and resource-only registration** require schema 7. The
durable header upgrade precedes the journal payload. Old headers reject these
payloads, including nested commands; Undo preserves the upgraded header while
restoring the asset registry and visible state. Referenced snapshots participate
in asset-deletion protection, clipboard/paste, Yoyo and project insertion.

Preview and GIF export keep separate straight and PM stores, sharing one
deduplicated aggregate byte budget. Encoded PM lengths include their header in
that accounting. Rendering checks image-plus-snapshot working space before
loading the reference. Whole-program editing preflight cannot hide an oversized
snapshot behind a small final crop. Save As checks the same streamed bytes it
hashes and copies, retaining only a header and split-pixel state. Insertion uses
borrowed validation without cloning a second large container.

## Independent measured evidence

The complete workspace/all-target/all-feature suite passes on Rust 1.98.0 and
1.88.0: **1,281 passed**, with eight explicit opt-in tests excluded from the
ordinary run. Strict whole-workspace Clippy, formatting and whitespace checks
also pass. The external 90-case snapshot comparison is additionally run explicitly.

[Windows probe run 34078589472](https://github.com/tiansongyu/gifromscreen/actions/runs/34078589472)
at `a631aab274e9c6d396636605dc0597be79475fe9` completed 90 fixed 12×10 cases:
ten geometries crossed with three first-frame and three current-frame alpha
patterns. It used actual Image/VisualBrush clipping and direct RTB composition,
not Rust-generated expected pixels. SDK 9.0.317, .NET/WPF 9.0.19, Windows 26100,
WIC 10.0.26100.33296; observed peak working set 101,117,952 bytes. The 512 MiB
watchdog is a sampled/cooperative ceiling, not a Windows Job hard memory cap.

Measured diagnostics across the 10,800 case-pixels:

- Direct premultiplied source-over model: **zero different pixels**.
- An extra intermediate PNG roundtrip: **558 final RGBA pixels differ**.
- Treating clipped-white alpha as an ordinary A8 mask: **822 clip pixels differ**.
- The software coverage64 model: **642 clip pixels differ**, all in the synthetic
  whole-rectangle case. It matches the other 81 cases, but this does not establish
  every possible ink geometry.
- A PushClip shortcut differs from Image/VisualBrush in **690 pixels**, also all
  in that whole-rectangle case. The measured geometry has a roughly ±8.94e-8
  residual strip; the primary VisualBrush result repeats a sampled source row.
  This needs further clipping/degenerate-geometry investigation, not a silent
  assumption that the two APIs are equivalent.

The separate Rust test consumes **the actual clipped PM reference as input**,
encodes/decodes its typed container and compares final RGBA through three render
routes. All **90 cases × 3 routes** match exactly on Rust 1.98.0 and 1.88.0.
This proves snapshot storage/composition, not Rust ink geometry generation.
It validates all indexed artifacts, safe paths, complete source hashes and the
full fixed case inventory; a copied definition-hash string alone cannot certify
a truncated corpus. Index plus indexed artifacts total 2,140,953 bytes.

Index SHA-256:
`f724de369961549cc08c66fe65266c9ecaf173825c081d981f93e899c21b15fe`.
Definition SHA-256:
`9ed2b610efa32f966f2c1be0a3dad18a4016972907f56b4ad97f7b629b4452b4`.

Run against downloaded trusted workflow artifacts:

```sh
GFS_CINEMAGRAPH_PROBE_DIR=/absolute/path/to/probe \
cargo +1.88.0 test --locked -p gif-from-screen-render \
  --test cinemagraph_reference -- --ignored --nocapture
```

No missing-environment fallback exists. Ordinary synthetic parser tests are not
Windows evidence. Freehand geometry generation, multiple overlapping strokes,
curve fitting, point/stroke erasers, selection edits, zoom/DPI interaction and
native authoring acceptance remain required for the full reference feature.
