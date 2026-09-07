# Cinemagraph authoring implementation

The desktop now connects first-frame ink authoring to schema-7 premultiplied
snapshots. This is a separate operation from schema-6 **Rectangular freeze**;
neither old projects nor that tool's current-frame/direct-overwrite behavior change.

## User workflow

Select the target frames, open **Motion tools → Cinemagraph → Draw motion region**.
The preview explicitly shows project frame 1 without changing those targets.
Paint the area that should remain animated; the reference outside the union of
the strokes is composited over the selected frames. Transparent reference pixels
do not erase the animation. Selection gaps remain untouched.

- Pen: click for a dot or drag; independent width/height from 1 to 100, ellipse
  or rectangle tips, per-new-stroke curve fitting. Mouse pressure is 0.5; this
  interface does not yet collect hardware stylus pressure.
- Erase part / Erase stroke: swept hit detection, fragment creation or whole
  stroke removal, with independent eraser dimensions and tip.
- Select: click/marquee, select all, delete, move and bottom-right-handle resize.
  Transforms change point coordinates, not pen dimensions or stored pressure.
- Escape cancels the current gesture. Clear removes draft ink; Close discards
  the draft. Apply runs in a cancellable background job. Failure/cancellation
  retains the draft and original workspace for retry; success clears the draft.
- Changing the project, revision, reference dimensions or target selection marks
  the draft stale. Restart is explicit; Apply never silently chooses new targets.

## Ownership and limits

`CinemagraphDraft` owns bounded transient samples and gesture rollback state.
`CinemagraphPreview` maps ordered input events to physical subpixel coordinates;
it does not floor to pixel centers or multiply DPI twice. Geometry guides use a
single cancellable background outline task with generation/epoch checks. An
in-flight older result cannot replace a newer draft. Playback and ordinary drawing
are excluded while this authoring mode is active.

The existing motion workspace loan validates the frozen request and target images,
renders the complete first frame, produces one typed immutable PM snapshot and
prevalidates one compound edit before asset publication and journal commit. One
Undo restores the edit; source frames, layers and earlier ordered steps survive.

Draft limits are 256 strokes, 16,384 samples, 1,000 target frames, 4,096 events per
gesture and a 4 MiB estimated draft/rollback budget. Preview input is capped at
256 events per UI frame. Final geometry/clipping/encoding shares a 64 MiB image,
request and geometry working budget; source/output are not independently granted
64 MiB each. Renderer operation/segment limits and cancellation remain active.

## Evidence and open fidelity work

The implementation cohort passes 1,339 workspace tests on Rust 1.98.0 and 1.88.0,
plus strict workspace Clippy. Its new desktop coverage includes 16 draft tests,
8 input/preview tests, 3 worker integration tests and 2 motion-worker recovery
tests. Worker tests check first-frame rather than current-frame reference,
transparent pixels, untouched targets, stale/cancel/invalid requests, asset
registration, Undo, reopen and decoded GIF pixels. These are automated tests,
not a native pointer-device acceptance report.

Initial native QA caught a justified-column layout error: egui's image response
was wider than its painted pixels. The editor now allocates and paints the exact
image rectangle and uses that same rectangle for ink hit testing and coordinate
mapping (including ordinary drawing). A real-egui column regression passes on
both supported test toolchains. Native acceptance continues with the fixed binary.

The [subsequent bounded native sequence](NATIVE-CINEMAGRAPH-QA-2026-09-07.md)
passed pen, both erasers, selection transforms, Escape, fitting, gapped-target
Apply, Undo/Redo and reopen. GIF exports before/after reopening compare byte for
byte. The final input/controls cohort passes 1,349 workspace tests on both
toolchains, including 24 small-window/large-font action checks.

The elliptical eraser currently uses a bounded 64-sided polygon approximation.
The UI discloses this. Full WPF stroke-node pruning, exact erasure cuts, Boolean
geometry and degenerate Image.Clip/VisualBrush behavior remain unverified.
Selection transforms and pressure rules have component coverage, not universal
upstream equivalence. Native desktop interaction and independent WPF geometry
diagnostics are the next acceptance steps; see [the geometry plan](CINEMAGRAPH-GEOMETRY-PLAN.md)
and [typed snapshot evidence](PREMULTIPLIED-SNAPSHOTS.md).
