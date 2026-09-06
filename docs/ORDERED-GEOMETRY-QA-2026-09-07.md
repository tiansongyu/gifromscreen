# Linux ordered-geometry acceptance — 2026-09-07

Scope: native editing in the private nested GNOME software-rendered lab, plus
CPU, domain, project-store and application regressions. This is not a physical
display, keyboard-capture or Windows/WPF pixel-parity certification.

## Native run

- Lab: `/tmp/gfs-wayland-qa.psehj4kj`, private Xvfb `:99`, GNOME/Portal/PipeWire
  supervised by the repository's `scripts/qa/wayland_nested.py` harness.
- Bounded lifetime: 1,800 seconds. Main supervisor PID 1152850, start ticks
  153835916; extra-app supervisor PID 1168225, start ticks 153870640.
- Frozen app SHA-256:
  `17cb441854e6f72bcec5a6159adbd8d3d35167992e48012cfe3362620b9f7829`.
  Built from this iteration's working tree over base commit `7f3e717`, not an
  assertion that the old commit already contained ordered geometry.
- Source: a copy of the prior synthetic authoring fixture, three 320×160 frames,
  100 ms each, a timed legacy progress bar at `(0,150,10,8)`, opacity 230.
  Synthetic input events are not physical capture evidence. The master fixture
  at `/tmp/gfs-authoring-native.JIshQQ/source.gfsproj` was not edited.
- Working project: `lab/output/geometry.gfsproj`.

Using real visible desktop controls:

1. Opened the project. With only frame 1 selected, resized to 160×80. All three
   frame programs and the canvas changed in revision 2; the legacy bar shrank
   with the image. The old revision-1 visual state was durably stamped schema 3
   before the new journal entry, rather than writing new pixels into an old header.
2. Rotated right to 80×160 in revision 3. All three thumbnails became portrait.
   Selected frame 2: its orange/gray progress bar moved from the bottom-left to
   the top-left and rotated with the image.
3. Native Undo restored 160×80, then Redo restored 80×160 (revisions 4/5).
   Saved checkpoint and closed normally. CLI export recovered the same state:
   three 80×160 frames, 10 centisecond ticks each, 578 bytes total.
4. Relaunched the same frozen binary and reopened through Recent projects.
   In Layers, clicked Attach to frames while only one frame was selected. The
   whole legacy track became three owned cells in revision 6, preserving its
   track identity and opacity. Every cell anchors to stage 1, before resize and
   rotation, not to the current tail.
5. Closed without a new checkpoint and exported from journal recovery.
   `geometry-before-convert.gif` and `geometry-after-convert.gif` compare equal
   byte for byte. Both SHA-256 values are
   `8e3746d7a0e641cb726dc0bbfc496de863a1849c5df0ad355c95c5c3d987e6f2`.

Screenshots in `lab/logs/` record controls, resize, rotation, undo, reopening and
conversion: `geometry-05-controls.png`, `geometry-06-resized.png`,
`geometry-07-rotated-frame2.png`, `geometry-08-undo.png`,
`geometry-11-overlays.png`, `geometry-12-layer-before.png`,
`geometry-13-attached.png`, `geometry-14-converted-frame2.png`.

Both app instances exited normally. The extra-app handle returned status 0;
explicit lab stop returned `LAB_EXIT ... status=0`. No lab or application was
left running. Final checkpoint is revision 5 and the durable journal contains
revision 6; this is expected journal-backed persistence, not an unsaved edit.

## Automated contracts

Final local cohort: `cargo +1.98.0 test --workspace --all-features --quiet`
passed **1,114 tests**, with six environment/opt-in tests excluded from that
default run. Workspace/all-target/all-feature strict Clippy, formatting and
`git diff --check` passed. Rust 1.88 all-target/all-feature workspace check also
passed; domain/project/renderer and targeted authoring/insertion checks were
additionally exercised on the minimum toolchain. These counts are not a claim
that hardware-gated tests ran.

- Domain/project: stage sizes and bounds, all six existing effect families,
  renderable legacy no-ops, hidden/empty anchors, schema-3 nested payload gates,
  sticky upgrades, rejected invalid payloads and exact durable recovery.
- Renderer: old pixel path unchanged; initial mixed legacy/owned alpha and
  Multiply/Screen order; `A → rotate → blur → B`; marks before/after resize;
  all entry points execute the prefix once; stage IDs and cross-stage z do not
  replace chronological order; detached asset plans, cancellation and limits.
- Editor: all-frame geometry versus selected flip/effects, complete canvas
  update in one undo, hidden/empty sealing, copy-stage preservation, clear and
  indexed replacement, repairing invalid legacy effect regions, opaque maximum
  stage IDs and bounded allocation.
- Desktop: complete pixels through preview/reopen/GIF, old text re-edit in its
  original stage, byte-preserving legacy conversion, new versus existing
  recorded-click/cursor coordinate mapping, multi-step nearest cursor patches
  compared with full-frame embedding, unchanged raw clocks/events, and staged
  Cinemagraph bake/undo/reopen without applying geometry twice.
- Save As: all stage programs, hidden artwork and referenced assets retained;
  source/copy GIFs agree.
- Project insertion: actual output sizes checked on every source/destination
  frame, cancellation before pixel I/O, stale-size metadata normalized in one
  undo step, empty destinations and incompatible/unrenderable legacy projects;
  successful normalized projects reopen and export at their actual dimensions.

See [the ordered-editing contract and explicit limitations](ORDERED-FRAME-EDITING.md).
The native run does not cover every effect control, every geometry combination,
hidden intermediate-layer baking, expanded-canvas shadows or physical desktops.
