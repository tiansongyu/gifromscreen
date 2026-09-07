# Explicit paint precision and independent WPF comparison

New Normal-blend artwork uses schema 5 `Composite` stages with
`precision: "wpf_pbgra8_png_v1"`. Each authoring action gets its own stage;
multiple marks belonging to that action share one boundary. This models the
reference editor's destructive Apply → RenderTargetBitmap → PNG/WIC cycle
without destroying the source, marks or edit history.

## Compatibility and execution

Missing precision means `legacy_straight_rgba8`. Its serialized default is
omitted, preserving old journal checksums and rendering. The first stage stays
legacy so existing timed tracks retain their old paint space. Old tail cells,
including hidden, zero-opacity and empty-scope cells, are sealed without
changing their precision. Normal text, shapes, strokes, watermarks, titles and
input/progress authors now create explicit WPF stages. Re-editing existing
groups retains their original stage and precision.

A visible WPF stage converts the complete current image to premultiplied RGBA8,
composites its marks in that space and converts it back once. Untouched base
pixels participate in this lossy boundary too. A hidden/empty stage with no
active marks is skipped. Separate authoring actions must not be coalesced:
blue `[0,0,255,253]` on transparent pixels becomes `[0,0,254,253]` after one
boundary, then `[0,0,253,253]` after a subsequent transparent draw.

Rounded multiplication uses `t = channel * alpha + 128`, then
`(t + (t >> 8)) >> 8`. WIC unpremultiplication uses a fixed reciprocal
`floor(65536 * 255 / alpha)`, not ideal real-number division. Source-over rounds
the destination contribution separately. The software shadow's color is
converted from sRGB to scRGB before native color-byte truncation; a stored
`[15,50,100]` shadow therefore uses `[1,8,32]` linear bytes. These rules are
shared by the new image effects and WPF paint stages, not legacy effects.

Multiply and Screen remain explicit straight-alpha enhancements. A newly
authored enhancement gets its own legacy-precision stage; it cannot silently
use Normal-only WPF source-over. Domain validation rejects non-Normal tracks
bound to a WPF stage even when hidden. Future blend-mode editing must make the
precision policy explicit rather than silently rewriting old pixels.

Schema 5 follows the durable minimum-schema upgrade protocol. Headers 1–4
reject visible WPF-precision payloads, including nested journal commands;
Undo never downgrades the header. Asset registration, old-tail sealing, frame
replacement and the new track commit atomically. Capture-clock fallback updates
are applied after frame replacements so an older snapshot cannot overwrite
them. Author preparation checks stage counts and a 16 MiB metadata budget
before committing. Original capture input and source pixels remain unchanged.

## Actual Windows reference evidence

The independent producer in
[Actions run 34071253697](https://github.com/tiansongyu/gifromscreen/actions/runs/34071253697)
successfully executed real WPF and PNG/WIC, with:

- .NET SDK 9.0.317; runtime 9.0.19; PresentationCore 9.0.1926.37102.
- Windows 10.0.26100; WIC 10.0.26100.33296.
- Runner image `win25-vs2026`, version `20260824.214.3`.
- Native WPF build commit `75bc144dfb125fb821e28adb648dd35618b23693`.
- Peak generator working memory 49,496,064 bytes.

The original Linux comparison correctly failed. All dimensions and alpha
channels matched, but small border/source-over differences and colored-shadow
differences exposed the numeric rules above. The reference artifacts, input
definition and generator were not changed to fix the Rust result.

After the fixes, local replay of those same independently produced artifacts
passed all 5 input images and 8 operation stages with **zero RGBA-byte or size
differences**, including RGB under transparent alpha. Report SHA-256:
`582f19ef42c6bf335e775a243ae119b99d45f4431d36129e66d22ad44ee4efa1`.
Definition SHA-256:
`700e371bd6536d815e5412e3e5e994a7e7262193a4b6dc28765a2e93a6d7c732`.
The workflow's original failed comparison is not relabeled as a passing run.
A fresh complete [hosted run 34073441806](https://github.com/tiansongyu/gifromscreen/actions/runs/34073441806)
at committed revision `35c246bab300dbe7bb55f89608be9b6afac26c25` passed both
the actual Windows producer and strict Linux/MSRV comparator. Relevant
renderer/domain/reference changes now trigger this independent check on push
or pull request. A follow-up corrects the provenance description's DPI epsilon
and single-precision whitelist to match its existing implementation; that source
hash change requires newly generated references, not an edited old index.

Local workspace checks on Rust 1.98.0 and the 1.88.0 minimum toolchain each
passed 1,198 automated tests (7 explicit opt-in tests remain ignored in the
ordinary run). Strict whole-workspace Clippy passed on 1.98.0. The independent
WPF replay also passed explicitly on 1.88.0. Coverage includes durable schema
upgrade/recovery, legacy wire defaults and blend modes, multiple marks versus
multiple PNG boundaries, cancellation, hidden stages, source clocks, asset
registration, exact Undo/Redo, clipboard ownership and GIF export.
One additional transparent-title/translucent-text regression passed on both
toolchains after those full runs, bringing the tested total to 1,199. It checks
the explicit title stage, exact reference pixels, Undo/Redo and journal reopen.

See [the reference protocol](../scripts/qa/wpf_reference/README.md) for bounds,
source/file hashes, provenance checks, exact comparison and local replay.
The protocol deliberately fixes physical 96-DPI working coordinates while
retaining original WIC pixels and decoded DPI metadata. This does not certify
historical fractional-DPI behavior, all effect parameters, vector antialiasing,
text shaping, hardware capture or complete ScreenToGif parity. The hosted
Windows component is a verification tool for the Linux application only.
