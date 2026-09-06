# Native expanded-image effect QA — 2026-09-07

Result: the bounded native editing/reopen/GIF sequence passed. This is a private
nested GNOME software-rendered acceptance run, not Windows pixel certification
or physical screen/input capture.

## Provenance

- Lab `/tmp/gfs-wayland-qa.izpk93ho`, bounded to 1,800 seconds; supervisor PID
  1266007, start ticks 154247039, private Xvfb `:99` and private desktop services.
- Frozen app SHA-256
  `778b9b75b8c154fe19a3a14deb5352797914b52facc5febf34a5b027472261bf`.
  Built from the pending schema-4 cohort over base `51c312e`, not a clean-tag
  build. Later whole-program budget regression fixes do not change the pixels
  exercised here; they are checked separately in automated tests.
- Extra-app supervisor PID 1301101, start ticks 154388666; same frozen binary.
- Project `lab/output/blank-animation.gfsproj`, created through the visible
  New blank animation form. No existing user project was edited.

## Visible UI sequence and checked state

1. Created a transparent 160×80 animation, initial delay 100 ms. Authored A in a
   40×40 text box at `(20,10)`, using the visible text tool and its background.
   Copied/pasted twice, leaving three 100 ms project frames. Only frame 1 was
   selected for the later effect operations.
2. Added a shadow with radius 3.50 px, direction 45.00°, opacity 60.25%; its
   initial distance remained 10.00 px, producing 170×90 frames in revision 5.
   Then explicitly edited distance to 2.25 px and clicked Replace effect #1.
   Revision 6 stored exact values `350/225/4500/6025` and changed all three
   frames plus canvas to 165×85. Undo/Redo exercised that replacement (7/8).
3. Added a mixed border: top +2 px, right 0, bottom −6 px, left −8 px. Revision
   9 stored `2000/0/-6000/-8000`, applied to every frame and changed the canvas
   to 173×91. A's source box moved by the combined shadow/border placement;
   outer left/bottom and inner top edges were visible.
4. Added B only on frame 1, at `(100,30)`, also 40×40. A was already in stage 1,
   before both image operations. B remained at the tail with no stage anchor:
   its box had a crisp edge, not A's previously applied shadow (revision 10).
5. With just one frame selected, Clear effects removed the canvas-changing
   effects from all frames and restored 160×80 with both text boxes preserved
   (revision 11). Native Undo restored the complete 173×91 state (revision 12).
   Saved checkpoint and closed the application normally.
6. CLI export of revision 12 produced a 1,985-byte, 173×91 GIF. Three selected
   project frames became two GIF frames: 100 ms for A+B and 200 ms for the two
   equal A-only frames. Total playback remained 300 ms.
7. Relaunched the same frozen app, reopened through Recent projects and
   visually checked the restored layout. Closed normally. A second export
   compared equal byte for byte to the first.

Both GIFs have SHA-256
`7de0461ae1a2cc624b1b68c84dd4b30ff7289facdb2c2658e2eda50c00083af4`:
`lab/output/image-effects.gif` and `lab/output/image-effects-reopened.gif`.
The final checkpoint is schema 4, revision 12; all three image programs and
100 ms durations agree. A's three owned cells anchor to stage 1; B belongs only
to frame 1 and has no stage anchor. These are generated frames, not evidence of
captured physical keyboard or cursor events.

Screenshots in `lab/logs/` include `shadow-05-text-a.png`,
`shadow-10-controls.png`, `shadow-11-fractional-applied.png`,
`shadow-13-replaced.png`, `shadow-15-border-applied.png`,
`shadow-16-b-after-effects.png`, `shadow-17-cleared.png`,
`shadow-18-undo-checkpoint.png`, and `shadow-20-reopened.png`.

## Cleanup and boundaries

Final local automated cohort: **1,160 tests passed** with
`cargo +1.98.0 test --workspace --all-features --quiet`; six opt-in/environment
tests remained excluded from that default run. Workspace/all-target/all-feature
strict Clippy, formatting and whitespace checks passed. Rust 1.88 workspace
all-target/all-feature check passed, with domain, project, renderer, authoring
and lifecycle slices additionally tested on the minimum toolchain. The added
whole-program regressions cover a later oversized shadow hidden by a final
crop, and enforce GIF dimensions at the actual output without rejecting a
bounded wide intermediate that is subsequently cropped.

Both app instances exited normally. The extra-app execution handle returned
status 0. Explicit lab stop returned `LAB_EXIT ... status=0`; final status is
`stopped`, reason `lab stop requested`, `cleanup_complete: true`. No private
desktop/application was left running; no host desktop services were restarted.

Automated coverage additionally checks stage-aware recorded points/cursor
patches, opaque/transparent pixel fixtures, radius-1 negative weights, effect
replacement, Copy/Yoyo/Save As, settings v1→v2, schema 4 durable recovery,
cancellation and whole-program size budgets. See the [effect contract and
remaining numeric boundaries](EXPANDED-IMAGE-EFFECTS.md). This native run does
not close Windows/WIC golden pixels, fractional-DPI comparison, hardware GNOME,
KDE, physical cameras or native long-duration recording gates.
