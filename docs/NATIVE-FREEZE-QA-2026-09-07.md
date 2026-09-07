# Native non-destructive freeze acceptance — 2026-09-07

The bounded native hide/freeze/reveal/Undo/Redo/reopen/GIF sequence passed in
private nested GNOME. This is editing evidence, not physical capture,
freeform-mask parity or universal WPF certification.

## Provenance

- Commit `d5c48fc923f3ca59569af16d499ef98f485d681c`, clean source at lab creation.
- Frozen desktop SHA-256:
  `099537b5c672ce5d165410e853838968ca9309a914eec9bc59b06905497180c6`.
- Lab `/tmp/gfs-wayland-qa.paihqtxx`, bounded to 1,800 seconds, private Xvfb
  `:99` and GNOME software rendering. Supervisor PID 1560028, start ticks
  155383618; initial application PID 1560183.
- Reopen supervisor PID 1568364, start ticks 155423416, bounded to 600 seconds;
  its copied application has the same hash.
- `lab/output/freeze-native.gfsproj` is a copy of the earlier generated
  [image-effect QA](IMAGE-EFFECTS-QA-2026-09-07.md) project. The original was
  stopped/unlocked and remained unchanged. No everyday user project or host
  desktop service was modified.

## Visible sequence

1. Opened the copied schema-4, revision-12 project through the form: three
   100 ms, 173×91 frames. Three frame-owned A layers precede shadow/border;
   the fourth B layer belongs only to frame 1, after those effects.
2. Selected all three frames and used Hide on A's three layers. Their Show
   buttons and thumbnails confirmed independent visibility changes. B remained
   on frame 1 only, at revision 15.
3. Set Cinemagraph to `(0,0,86,91)`, non-inverted, and clicked Apply. Revision
   16 retained all layers and source frames and appended three freezes sharing
   one reference. B now appeared on every frame, outside the live rectangle.
4. Showed the three A layers. A returned inside the live region; B stayed frozen
   on every frame (revision 19). Four native Undo clicks reversed the visibility
   changes and freeze, restoring B to frame 1 only. Four Redo clicks restored
   A+B on all frames.
5. Hid B's original layer after freezing. Its saved frozen copy remained visible
   on every frame, as the tooltip explains. Undo restored the layer property.
6. Saved a checkpoint at revision 29 and closed normally with Alt+F4. CLI export
   succeeded after lock release. Relaunched the same binary, opened from Recent
   projects and visually checked the restored three A+B frames. Closed normally
   and exported again; both GIFs compare equal byte for byte.

## Checked data and output

Final header: schema 6, revision 29. Excluding their ordered render programs,
all frames compare unchanged against the original: identities, source assets,
raw capture metadata, bindings, clocks, transforms/effects and 100 ms delays.
These generated frames correctly remain `not_recorded`. Earlier image steps
survive; A retains stage 1, while B's former tail is sealed at legacy stage 2.

Registered assets increased from three to four. Every freeze references the
same immutable 173×91 raw view, content identity
`13c6f16c6c86906d1f56cbbca430948da5f157586fb3b9dd73492f0b1e82f4a7`.

`lab/output/freeze-native.gif` and `freeze-native-reopened.gif` are both
**1,133 bytes**, 173×91, one encoded image, **300 ms** total. Three equal
project frames merge without losing playback time. Their SHA-256 is
`1b8818d1283aa9a4015e33318f7812c9fd32472d2c46a1e9b0c0fb1dc53c2acf`.

Screenshots in `lab/logs/`: `freeze-07-layers.png`, `freeze-08-hidden.png`,
`freeze-11-ready.png`, `freeze-12-applied.png`, `freeze-13-frozen-all.png`,
`freeze-14-revealed.png`, `freeze-15-undo.png`, `freeze-16-source-b-hidden.png`,
`freeze-18-checkpoint.png`, and `freeze-20-reopened.png`.

## Cleanup and scope

Both applications exited with status 0. Explicit lab stop returned
`LAB_EXIT ... status=0`; lab and extra-app records show `stopped` and
`cleanup_complete: true`. Test artifacts remain; private desktop processes do not.

The schema-6 cohort passed 1,225 complete workspace tests on Rust 1.98.0 and
1.88.0 plus strict Clippy. The later visibility cohort passed all 473 desktop
tests, three layer UI tests on 1.88.0, and strict workspace Clippy; it adds one
test beyond the earlier total. Linux CI and portable packaging passed on
`d5c48fc`; independent hosted WPF comparison passed on `230fc49`.

The [contract](NONDESTRUCTIVE-CINEMAGRAPH.md) keeps freeform masks, wider numeric
coverage and physical platform acceptance separate. This native sequence does
not close those broader gates by implication.
