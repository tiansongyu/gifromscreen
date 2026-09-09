# Vector canvas: native Linux and independent WPF results

This is bounded acceptance of the [new vector tool](VECTOR-SHAPE-CANVAS.md), not
full Linux/ScreenToGif parity, all-language completion or a new Release.

## Automated verification

Source `ca6fb2abefd2032b9160af656ee0ba1c850c08d2` passes the complete workspace
on explicit Rust 1.98.0 and 1.88.0: **1,773 passed, 51 explicitly ignored on each**.
The ignores require external/native inputs; they are not counted as passes.
Strict 1.98 Clippy (`--workspace --all-targets --locked -- -D warnings`) passes.
The earlier feature snapshot had 1,772 passes; a later committed-input validation
test accounts for the additional test. No old pixel test was deleted or loosened.

The new canvas tests cover real egui event arbitration, fast Down/Move/Up batches,
text-field/canvas Delete isolation, modifier-wheel isolation from native zoom and
scroll, rotated local-axis handles, stale anchors, unfinished-gesture rollback,
one Apply with gapped frame ownership, undo/redo/reopen and failure-before-journal
mutation. Renderer tests retain the original exact full-mask-versus-tiled
composition regression and a literal PM grouping-rounding example.

## Actual Linux desktop

Owned lab `/tmp/gfs-wayland-qa.5dgmjfrj` used private Xvfb/GNOME X11, a private
session bus and software rendering. Its frozen desktop was built with explicit
Rust 1.98.0 from clean `be775665eef9652d28d737f2bf97a253beb91f8d`:
SHA-256 `5fece2ba683d6659585cc06cfac2f4fa6411166e4f62119579c2e98c7951c62d`.
Later reference-only commits did not replace this running binary.
Screenshots 01–23 are in the lab's `logs/` directory; these are actual windows,
not designed mock-ups. The input artwork was explicitly synthetic CLI demo data.

1. Create `vector.gfsproj`: 36 frames, 160×96, 50 ms each. Select frames 1 and 3
   through the real filmstrip and open the Chinese shape canvas.
2. Set radius 8.25, draw a rounded rectangle, then a triangle. Select both; rotate,
   resize via a rotated handle and move the group. Stroke stays 4.00 px.
3. Switch Fit → 100% → 200% → Fit. Switch the application from System/Chinese to
   English. Both confirmed objects remain; their shape properties do not reset.
   The new form keeps labels paired with their numeric/color controls.
4. Before Apply, the manifest remains revision 36 and its original digest
   `00e0e5aacd5d5c1d2897510ff2917300afb02b6b765de25c54bd29daf96eeca3`;
   the journal is still empty. Previewing and changing language did not edit it.
5. Apply creates journal sequence 37: two frame replacements and one vector track,
   two marks per cell, independent scope run IDs 1 and 2, explicit isolated stages.
   The rectangle/triangle retain radius 825 and rotation 3375 in hundredth units.
   Undo is sequence 38; Redo is 39 and restores the same IDs and contents.
6. Save a checkpoint. Use GUI Save As to create `vector-copy.gfsproj`, then open
   that independent copy and export **all** frames through the English GUI.
7. Close the application normally, reopen/export both projects via the CLI.
   Both exports match the GUI file byte-for-byte. The copy has a new project ID
   and revision 0; its timeline is exactly equal to source revision 39.

The GUI/source-reopen/copy-reopen GIF is 59,342 bytes, SHA-256
`c8d6b6150e42e1d30f2ca738a9712ff5d0389bc144442d4c201c4ece3ee46a62`.
FFmpeg decoding confirms 36 images, 160×96 and 1.800000 s. Frame hashes differ
from the original baseline **only at frames 1 and 3**.
Source checkpoint digest:
`6b7569e048f2eb36cf2365322a12a0aaed49af205c6b08c397bfc943cb13bc39`.
Source journal digest:
`4ba7b63b14260f93f7a81f58bd48f4b5ede0b01852de7b7bbce9c9a88a593517`.

Explicit lab stop reports `cleanup_complete=true`; the supervisor returned 0.
No active project owner was overridden and no lock was manually removed.

One XTest `click 4` produced a 2° Ctrl-wheel change. The pinned winit X11 handler
maps both un-emulated XI ButtonPress and ButtonRelease for buttons 4–7 to wheel
events. This synthetic injection is not proof of physical wheel-notch behavior.
The egui tests separately require exactly one degree per delivered Ctrl-wheel
event and unchanged zoom/scroll; physical devices remain a separate gate.

## Independent WPF: not yet passing

[Run 34306803780](https://github.com/tiansongyu/gifromscreen/actions/runs/34306803780)
successfully builds/generates on real Windows with SDK 9.0.318 / .NET 9.0.20,
Windows 10.0.26100. It compiles the hash-pinned original Triangle/Arrow classes.
The strict Linux comparison **fails**; this failure is intentionally retained.
Local replay of its untouched reference gives the same open results:

| Fixture | Differing pixels (16×16) | Maximum channel difference |
| --- | ---: | ---: |
| Pure filled rectangle | 0 | 0 |
| Rounded fraction + triangle group | 121 | 73 |
| Per-axis rounded radius clamp | 44 | 48 |
| Rotated triangle | 113 | 92 |
| Closed block arrow | 132 | 68 |
| Fractional rotated ellipse | 61 | 81 |

All original five border/shadow/ordered-chain fixtures and every input stage
remain exactly equal in all RGBA channels. No expected pixels came from Rust.
Current downloaded evidence is `/tmp/gfs-vector-wpf-ca6fb2a`, with comparison
report `/tmp/gfs-vector-wpf-compare-ca6fb2a/report.json` and real layout diagnostics
in `/tmp/gfs-vector-layout-ca6fb2a/stdout.log`.

The diagnostics establish a concrete layout discrepancy before antialiasing:
requested rectangle `[1.25,1.75,12.5,11.25]` arranges at offset `[1,2]`, size
`[12,11]`; a custom triangle requested at size `[15,11.25]` has RenderSize
`[16,11]`. The original relative rotation origin follows arranged size, not the
requested bounds. These findings are not permission to silently reinterpret
old Shape projects or to add a tolerance to make the comparison green.

The first generator attempt failed its pinned-byte guard on Windows checkout
line endings. Explicit checkout-only `core.eol=lf` fixed it without changing
expected hashes. The comparison also needed a separately bounded 4 MiB stroker
scratch budget, not a budget equal to one 256×256 surface. Finally the reference
was corrected to the original relative rotation origin; earlier fixed-center
results are diagnostic only, superseded by the run above.

Next: establish version-safe WPF layout/coverage behavior with independent
geometry evidence; complete native Wayland/input-device coverage and remaining
full-resolution resource preflight. Keep translation, packaging and the full
[Linux task list](NEXT-LINUX-ITERATION.md) open. Preview 3 remains unchanged.
