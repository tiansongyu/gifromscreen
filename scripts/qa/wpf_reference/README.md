# Independent WPF pixel reference

This is a bounded, headless **verification tool for the Linux renderer**, not a
Windows edition of GifFromScreen. It invokes real .NET 9 WPF public APIs and
PNG/WIC decoding. It never reads the screen, opens a window, or uses fonts.

The `WPF pixel reference` workflow starts as manual `workflow_dispatch` while
calibrating numerical behavior. A reference or comparison failure is a failed
run, with artifacts retained for diagnosis; it never updates expected pixels
from Rust output or weakens a tolerance automatically.

## Protocol

`fixtures.json` is the shared **input definition**, not expected image data.
It contains exactly five named fixtures, each with explicit RGBA8 source pixels
and 1–8 ordered operations. Every input and output is limited to 256×256. The
operations use the domain's persisted border/shadow styles; bitmap overlays
are fixed pixel arrays, avoiding platform-dependent font shapes.

The C# generator independently executes the 96-DPI geometry and drawing order
from pinned ScreenToGif `a4d0a67`. Its input is saved to PNG and decoded by WIC;
every operation also passes through RenderTargetBitmap → PNG → WIC before the
next operation, mirroring destructive Apply's actual quantization boundary.
Expected pixels are never calculated with the Rust renderer or a reimplemented
Gaussian kernel.

PNG stores integer pixels per metre, so a nominal 96 DPI image can decode as
95.9866 DPI (3779 ppm, observed on the first real run) or 96.012 DPI (3780 ppm).
This suite measures an explicitly physical, 96-DPI coordinate space. It records
the original decoded DPI and retains its exact WIC pixels/format/palette, then
rebuilds only the working bitmap's DPI metadata at 96 for the next draw. The
generator asserts that this does not change any stored pixels, dimensions,
format or palette. The comparator validates both decoded and working DPI; it
does not use a pixel tolerance. This normalization does not claim to reproduce
ScreenToGif's historical mixture of rounded ImageDpi and unrounded bitmap DIP
sizes. Actual fractional-DPI behavior remains a separate gate.

The generator writes `index.json`, input/stage PNGs, straight-alpha RGBA8 and
pre-PNG PBGRA bytes to a new directory. The index records dimensions, SHA-256
for each file, exact input-definition and generator-source hashes, and actual
SDK/runtime/Windows/renderer/WIC provenance. Only an explicit environment
allowlist is read for provenance. `.gitattributes` fixes generator/definition
files to LF on both operating systems, preventing checkout-only hash changes.

The Rust integration test validates all paths, hashes, dimensions, source-file
sets and cumulative artifact bytes before treating an image as reference data.
It compares the WIC input against the raw input definition too. Each Rust stage
is rendered from its own complete operation prefix; reference pixels are never
fed back into the Rust chain to hide cumulative differences. All four channels
are compared exactly, including RGB under zero alpha. Failures report pixel
counts, coordinates and maximum channel differences, with actual/diff PNGs.

Reference provenance and hashes bind a run's inputs and code, but are not a
standalone signature: use artifacts from the successful Windows producer job
of the trusted repository workflow. A failed producer's partial artifacts are
diagnostic data, not accepted golden images.

## Run

Dispatch the repository workflow on the desired committed revision:

```sh
gh workflow run wpf-reference.yml --ref main
```

The Windows job selects .NET `9.0.x` using this directory's `global.json` and
builds only the generator. `run_wpf_reference.ps1` supervises its own process
tree for 120 seconds, checks 512 MiB working memory and bounded log size, and
refuses to overwrite existing outputs. The generator independently bounds
definitions to 64 KiB and emitted artifacts to 16 MiB. No secrets, publishing
permissions, host-service changes, or billing changes are required. Hosted jobs
use the repository's existing Actions quota.

The Linux job downloads the same run's independent reference and builds only
the platform-independent Rust renderer test, not the native desktop workspace.
For local replay of downloaded reference artifacts, use new/empty output paths:

```sh
GFS_WPF_REFERENCE_DIR=/absolute/path/to/downloaded/reference \
GFS_WPF_COMPARE_DIR=/absolute/path/to/new/comparison \
cargo +1.88.0 test --locked -p gif-from-screen-render --test wpf_reference \
  compare_windows_wpf_reference -- --ignored --exact --nocapture --test-threads=1
```

Missing environment variables fail explicitly. The ignored test does not run
as an ordinary offline unit test. Separate mechanical unit tests use clearly
synthetic envelopes to test parsing, unsafe paths, hashes and mismatch reporting;
their success is **not** evidence of WPF pixel equivalence.

Rust-only fixes may be compared with the same reference artifact while the
definition and generator source hashes still match. Changing either requires
a fresh Windows generation; editing an index/hash to bypass this is not a valid
comparison. First inspect mismatch evidence and its runtime provenance, then
fix the intended implementation or demonstrate a generator error against the
primary contract. Do not replace a real reference with a self-generated golden.

These first five fixtures cover inner-border alpha, mixed outer borders,
negative hard shadows, radius-two Gaussian behavior and a fractional ordered
overlay/effect chain. They do not certify every WPF parameter, fractional DPI,
font shaping, hardware rendering, native capture, or complete ScreenToGif parity.
