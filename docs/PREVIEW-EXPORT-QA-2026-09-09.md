# Native drawing and export acceptance — 2026-09-09

Final source: `974dfe8387cfe2d37a0ac02ddefc69d3d3bb939f`, built from a clean worktree
with explicit Rust 1.98.0 and `--locked`. The final debug desktop SHA-256 is
`e26bb4fbf3acc4cbf7388998a160176720169e7a7fb29c5cce119168ab6c49a0`.
This is a source-build acceptance record, not the portable release binary receipt.
The subsequently published package has its own [Preview 3 acceptance record](LINUX-PREVIEW-3-QA.md).

## Environment and fixtures

The owned private GNOME/X11 lab was `/tmp/gfs-wayland-qa.bsgn6rxp`, using its own
Xvfb `:100`, session bus and software rendering. No host desktop or services were
replaced. Final supervised app instances were `extra-app-771695` and
`extra-app-772587`; each exited normally with status 0 and cleanup confirmed.
Screenshots are numbered under that lab's `logs/` directory, not new README demos.

The CLI created `/tmp/gfs-preview-export-qa.ltRfOEoQ/preview-export.gfsproj`:
36 synthetic frames, 160×96, 50,000 µs per frame, total 1,800,000 µs, revision 36.
Its initial manifest digest is
`5abe22c954686321ae90a534fc04b64abb6403103d018c1ec61beb584d1169e5`;
its initial journal is empty. These are synthetic artwork and XTEST-driven native
window interactions, not a physical mouse/pen or screen-capture-rate benchmark.

## Final-build native checks

1. The persisted preference stayed `{"mode":"system"}`. Launching the same final
   binary with `LC_ALL=en_US.UTF-8` produced English; launching with
   `LC_ALL=zh_CN.UTF-8 LANGUAGE=zh` produced Chinese without rewriting that policy.
   Screenshots 16/17 distinguish the environments; English on the first launch
   is not a failed Chinese preference load.
2. In the Chinese editor, arm ordinary drawing and send Down, two movements and
   Up without sleeps between pointer events. A three-point Ready draft appears.
   Commit writes journal sequence 37, with one first-frame drawing mark at
   `(47,26)`, `(64,39)`, `(90,32)`, pressure 1000 and width 4. No sampling/frame
   durations change. Screenshots 18/19.
3. Export the drawing through the Chinese GUI, then Undo. The mark disappears
   and journal sequence 38 reverses it. Screenshots 21/22.
4. A quick Down/Up makes a one-point Ready draft. Cancel it, arm again, hold the
   pointer and resize the native client to 960×800 before releasing. The old
   unfinished points are discarded, no commit action is offered, and the tool
   remains armed. A fresh Down/Move/Up succeeds. Cancel this temporary draft;
   no extra journal entries are introduced. Screenshots 23–25.
5. Export the undone state: it is byte-identical to the original baseline GIF.
   Redo restores the committed drawing as sequence 39. Close normally, reopen
   via the CLI and export again: that GIF is byte-identical to the GUI drawing
   export. Screenshots 27/28 and CLI revision-39 receipt.

Both GIF states decode to 36 images at 160×96, duration 1.800000 seconds:

| State | Bytes | SHA-256 |
| --- | ---: | --- |
| Baseline / GUI after Undo | 56,940 | `5756aa86082026c4fa8f7a461a324188095aa766a0cf09331bf7e0d76559c9d7` |
| GUI drawing / CLI after reopen | 56,981 | `2114041a1788985f29d5f931ac5da4f2d67b5c485151e950c1c53ceb6015157a` |

The manifest remains at its initial checkpoint; the three durable journal records
recover revision 39. Final journal digest:
`c44be6e1df530794ebacf33a644fce5dd71fe83c28203f874928105ca0ec6954`.

## Pre-commit language preflight

The earlier working-tree binary in the same lab had digest
`bf978aac578c2bc53d1e2bf4f4ef855dc9c8c7913959dd92719238a1b7ecad70`.
It is not relabeled as the final build. Screenshots 01–15 show Chinese preview and
export controls, an invalid `.mp4` error switching to English without being
reissued, and returning to System/Chinese. Explicit English saved as an 86-byte,
mode-0600 preference; System then saved as a policy. An output path containing
literal `{cancel}` exported successfully and matched the baseline bytes. The
manifest and empty journal stayed unchanged throughout that unapplied preflight.

## Automated and platform limits

[Linux CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34255120229) and
[portable CI](https://github.com/tiansongyu/gifromscreen/actions/runs/34255120359)
both completed successfully for the exact final source. Local verification is
recorded in the [localization slice](PREVIEW-EXPORT-LOCALIZATION.md): 1,708 passing
workspace tests, 51 explicit ignores, strict Rust 1.98.0 Clippy, and all 766/38
desktop/localization tests on Rust 1.88.0.

This bounded X11 exercise does not close Wayland, physical-device pressure,
hardware mixed-DPI/multi-monitor, IME/RTL, every editor tool or complete WPF ink
fidelity. Existing README demonstrations retain their original provenance.
