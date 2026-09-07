# Native Cinemagraph ink acceptance — 2026-09-07

The bounded native first-frame ink → selected-frame apply → Undo/Redo → reopen
→ GIF sequence passed in isolated nested GNOME. This does not establish exact
WPF ink geometry or physical capture-device compatibility.

## Provenance and the defect found

Lab `/tmp/gfs-wayland-qa.rb6_z191` used private Xvfb `:99`, nested GNOME and
software rendering, supervised for at most 1,800 seconds. Supervisor PID
1803648, start ticks 156335563. Only copied/generated QA projects were edited;
host desktop services and everyday user projects were not touched.

The initial `cab6627` executable exposed an actual coordinate defect: egui's
justified column enlarged `Image::ui`'s response rectangle without enlarging its
painted image. A 30×30 dot displayed about 110×90, and its source coordinates
were correspondingly incorrect. That trial draft was discarded without Apply.
Commit `43ad2b3` now paints and interacts with the same exact image rectangle;
its real-egui column/hit-test regression passes on Rust 1.98 and 1.88.

The fixed native executable SHA-256 was
`07ebefbdd12b5305a32b75f1824301f2024399a9a19fdbf7c7deb0c7be9ffba9`.
It also included the four-line immediate mode-change gesture cancellation fix
subsequently committed in `577c6f361ce63340086fd4cb5652cbdd5eaeea6a`. Building
that clean commit produced the same binary hash. The successful authoring and
reopen sessions both used that exact binary:

- Authoring supervisor PID 1814020, start ticks 156369548, bounded to 900 seconds.
- Reopen supervisor PID 1826015, start ticks 156397308, bounded to 600 seconds.

## Native sequence

1. Copied the stopped/unlocked generated three-frame QA project from
   `/tmp/gfs-wayland-qa.izpk93ho/output/blank-animation.gfsproj` to
   `lab/output/cinemagraph-native.gfsproj`. Each 173×91 frame lasts 100 ms;
   A appears in all frames, B only in frame 1. The source has earlier shadow,
   border and frame-owned text stages.
2. Ctrl-selected frames 1 and 3, with frame 3 current. Starting Cinemagraph
   showed **Cinemagraph reference · frame 1**, including B, without changing
   the two selected targets. A single click created a round 30×30 dot and
   a multi-event drag created a second stroke.
3. **Erase stroke** removed the dot (two strokes became one). **Erase part**
   swept vertically through the remaining line and split it into two fragments.
   Selected one fragment, moved it and resized its point coordinates with the
   corner handle. Escape during another move restored the previous geometry.
4. Enabled **Fit new strokes to curves**, drew a five-position stroke, and
   applied the three remaining strokes. Their live regions stayed left of B.
   The background operation returned the editor, cleared the draft, and reported
   two affected frames. B appeared in frames 1 and 3; frame 2 retained only A.
5. One native Undo restored frame 3 to A-only; one Redo restored A+B. Saved a
   checkpoint at revision 15 and exited normally. CLI export succeeded after
   the project lock was released.
6. Reopened from Recent projects with the same binary and visually verified all
   three thumbnails. A new transient dot survived switching to Smooth loop and
   back to Cinemagraph. **Close draft** discarded it without editing the project.
   Closed normally and exported again; both GIF files were byte-identical.

Screenshots are in `lab/logs/cine-01-*.png` through `cine-29-*.png`; useful
checkpoints are `cine-08-two-strokes` (initial defect), `cine-11-fixed-strokes`,
`cine-12-stroke-erased`, `cine-13-part-erased`, `cine-15-moved`,
`cine-16-resized`, `cine-17-escape`, `cine-19-fitted`, `cine-21-target-gap`,
`cine-22-undo`, `cine-23-redo`, `cine-25-reopened`, and `cine-27-retained`.

## Stored data and output checks

The final schema is 7, revision 15. Frame 2 compares entirely unchanged against
the copied input. For all three frames, fields outside the ordered render
program compare unchanged, including source identity, duration, capture data
and `not_recorded` binding. Earlier steps remain intact; the B layer's tail was
sealed at its original stage before adding the new operation.

Assets increase by one. Frames 1 and 3 share the typed reference
`52e70eb3be238dc3d280cef8990d376750949d18c800609f313f383a29c09108`.
Its 62,989-byte container has a valid 17-byte schema-7 PM header and 173×91
RGBA-order premultiplied pixels: 4,230 transparent, 11,091 opaque and 422 partial
alpha pixels. Every color channel is no greater than alpha.

Both GIF exports are **3,079 bytes**, 173×91, three images at 100 ms each
(300 ms total). SHA-256:
`ddbe987c5f543cf574cfcb7d11a62661de229f474ba0ead49c82e797169258fa`.

The final code cohort passed **1,349 workspace tests** on both Rust 1.98.0 and
1.88.0 and strict workspace Clippy. Seven controls tests include 24 small-window/
large-font Apply/Close click checks, stale/busy states and same-frame mode
cancellation. Two additional real-egui input tests verify complete rollback
after mapping changes and no ink deletion when a same-batch click/Delete targets
a later text field. Small-window buttons are scroll-reachable, not fixed footers.

## Cleanup and remaining scope

All three applications exited normally. Both extra-app supervisors returned
status 0; explicit lab stop returned `LAB_EXIT ... status=0`. The final lab
record says `stopped` and `cleanup_complete: true`. Artifacts remain for review;
private desktop processes do not.

The ellipse eraser's 64-sided approximation, WPF stroke-node/Boolean fidelity,
degenerate VisualBrush handling, hardware pressure and broader platform gates
remain open. See [the authoring contract](CINEMAGRAPH-AUTHORING.md) and
[remaining geometry work](CINEMAGRAPH-GEOMETRY-PLAN.md). Native usability evidence
does not replace independent numerical comparison.
