# Advanced editor native acceptance — 2026-09-09

This records bounded, real GNOME/X11 window operations on synthetic artwork.
It does not certify physical devices, Wayland, mixed DPI, all languages or every
editor tool. The [translation scope](ADVANCED-EDITOR-LOCALIZATION.md) and
[remaining shape fidelity](SHAPE-PARITY-2026-09-09.md) stay separate.

## Builds and owned environments

Both debug builds used explicit Rust 1.98.0 and `--locked`, from clean source:

| Source | Desktop SHA-256 |
| --- | --- |
| `2f3c2d215351a8ebdea16ed3cba0e39612e93519` | `7df70098bd8fddeb2b0fdc8c166afcf47d367d048944362049ad21326539f4a2` |
| `9f497da64197111e14cef722942fe8b2cd6e64c2` | `bff50d1595bb86c61e9e54402b8f810011a8c771fed14a2c45a859d70a923ac6` |

The first owned lab was `/tmp/gfs-wayland-qa.xz4s0kks`, with its own Xvfb `:100`,
GNOME, session bus and software rendering. It ran the first build, then the frozen
layout-fix build as `extra-app-903365`. Screenshots 01–22 are in its `logs/`.
The lab reached its declared 1,800-second lifetime, reported cleanup complete and
returned 0. A later command correctly refused its stale connection details.
The final shape check used a newly allocated 360-second lab,
`/tmp/gfs-wayland-qa.nnl5l_cp`, not a guessed/reused connection. That app closed
normally; explicit scoped stop confirmed cleanup and launcher exit 0.

## Fixture and translation checks

`/tmp/gfs-advanced-editor-qa.4SHUeufb/advanced.gfsproj` was created by the CLI:
36 frames, 160×96, 50,000 µs each, total 1,800,000 µs, initial revision 36.
Initial manifest digest:
`e1203d1d8b0cd3f09f30c09a90f97264cfa51a1e5120ae858cd851852024f2cd`.
The initial journal was empty.

Under `LC_ALL=zh_CN.UTF-8 LANGUAGE=zh`, emptying the Blur radius and clicking Add
produced the Chinese required-field notice. Choosing English changed that already
displayed notice to `blur radius is required` without reissuing the failed action.
Returning to System restored Chinese. Neither manifest nor journal changed.
The statistics fold later remained open across an English/Chinese switch, keeping
36 frames, one selected frame, 160×96, 1.800000 s total and 0.050000 s delays.

## Real editing and durable outcomes

1. Create a Rectangle named literal `Keep {name}` on the first selected frame.
   Journal sequence 37 contains its frame-owned compound command. The layer row
   translates around the name without substituting its braces.
2. Hide the layer (sequence 38) and export through the Chinese GUI: the result is
   byte-identical to the original no-shape baseline. Show it again (sequence 39),
   retaining the artwork, and export the visible state.
3. Enter `-3` in the image-border Top control, leaving Right/Bottom/Left at 1.
   Sequence 40 is one compound edit: 36 frame replacements and one canvas update.
   All 36 stored styles contain `top_milli=-3000`, other edges `1000`; the canvas
   becomes 160×99. This confirms the whole-animation and fixed-point boundary.
4. Undo (sequence 41) restores 160×96 and the preceding visible shape. Save
   checkpoint, then Save & compact using the Project tab. The checkpoint is
   revision 41 and compaction empties the journal. Close normally and export via
   the CLI: its output matches the visible GUI export byte-for-byte.

Both GIF states decode to 36 images, 160×96, duration 1.800000 seconds:

| State | Bytes | SHA-256 |
| --- | ---: | --- |
| Baseline / hidden layer | 56,940 | `5756aa86082026c4fa8f7a461a324188095aa766a0cf09331bf7e0d76559c9d7` |
| Visible layer / reopened | 57,046 | `ac4049e4e06afd0edff3c66a43a20610ed2383a2c832dbc969c1315bd4b96ebb` |

The revision-41 checkpoint digest is
`18eece827668f577cd138db3d8d36b5900746ce3981f8eff673b2fd68cc45709`.

## Layout regression found, fixed and rechecked

Native screenshots 01 and 05 exposed labels separated from their wrapped inputs:
Radius, track opacity and split RGBA values. This caused the follow-up `9f497da`
fix, not a claim that initial unit tests had already established good native layout.
Screenshots 18–22 show the corrected scalar pairing, shape metadata fields and
grouped drawing RGBA channels. Scrolling exposes the drawing action; a real quick
Down/Up produces one Ready point with reachable Commit/Cancel controls. No draft
point was committed before the first lab's controlled lifetime ended.

Because that termination left a project lock, the new lab initially rejected the
open request. Before selecting the explicit takeover checkbox, the old recorded
owner PID 903393 was verified absent and its supervisor confirmed stopped/cleaned.
The application preserved the old lock as `project.lock.stale-1` (SHA-256
`ea629ff121f644ed1f9c74092a94d03019960e0722fde0d13377455679f36fd3`).
No lock file was manually deleted or an active owner overridden.

In the new lab, scrolling the corrected Shape inspector exposes the full Add
button and labeled RGBA groups. A real Add creates sequence 42; the editor Undo
button creates sequence 43 and restores the prior artwork. After normal close,
CLI reopen/export at revision 43 again matches the 57,046-byte visible GIF.
The checkpoint remains unchanged; the final two-record journal digest is
`bc943759ba2e89dfceda284d9bcad02d36644b27b5a8796fc2f44873bbad596b`.

## Automated checks and retained limits

For `9f497da`, [Linux CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34262877777)
and [portable CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34262877855)
completed successfully. The earlier `2f3c2d2` runs also passed:
[Linux](https://github.com/tiansongyu/gifromscreen/actions/runs/34260118976),
[portable](https://github.com/tiansongyu/gifromscreen/actions/runs/34260119040).
Local final verification: 1,737 passing workspace tests, 51 explicit ignores,
strict Rust 1.98.0 Clippy, and all 792 desktop tests on Rust 1.88.0. The 743-entry
catalog's 41 tests pass on both toolchains.

Large-font/narrow-width geometry and input combinations are headless test evidence;
these native captures use the default desktop scale. Text/title, watermark,
import/recorder-source/automation language migration and broader desktop gates
remain open. No new public release or README demonstration is claimed by this
record; Preview 3 remains its earlier frozen artifact.
