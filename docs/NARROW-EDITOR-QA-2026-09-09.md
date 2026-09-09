# Narrow editor: short native release-baseline regression

**Editor checks passed.** The shared narrow-window change was tested on exact
committed source `67f11e7819808e2c8c9bf65c5717dbe0dac22503`, independently of the
uncommitted schema-9/WPF tree. An unrelated narrow/150% home-card display limit
is recorded below; no source changes were made during this check.

## Reproducible source and environment

- Clean detached worktree: `/tmp/gfs-narrow-editor-native.VuRVHF/source`.
- Build: `cargo +1.98.0 build --locked -p gif-from-screen -p gif-from-screen-cli --target-dir /tmp/gfs-narrow-editor-native.VuRVHF/target`.
- Desktop SHA-256:
  `aa87c0ffa7e4519f596cfc277365587c77c5399d8f874f4bf50d1718d568eb9b`.
- CLI SHA-256:
  `ac7ad0749b31d4b0a6a3cedc6beae90f02d90d6cf06dc3ff4f8ef1b0ca523f77`.
- Cargo.lock SHA-256:
  `516f8119016707a5310a02b5352e5587bf618ac0e10e9b2ff6664005374089f0`.
- Private Xvfb/GNOME X11 lab: `/tmp/gfs-wayland-qa.6grcnk18`, created by the
  frozen worktree's scoped harness with private bus, Xauthority and configuration.
  The four-hour safety bound was not extended; explicit stop followed acceptance.

A new CLI demo project supplies 36 synthetic 160×96 frames, 50 ms each. Its
original manifest and GIF are retained under
`/tmp/gfs-narrow-editor-native.VuRVHF/evidence`. No prior QA project or screenshot
was overwritten, and no host desktop/device/clipboard was used.

## Actual GUI checks

1. Open the new project normally at 1040×760. The preview and inspector remain
   side by side (02). Enter `Keep {value}`, Ubuntu, 12 px and disable caption
   background; these are literal authoring values.
2. Resize the actual client to **680×760** and send five separately spaced
   Ctrl-plus presses for **150% zoom**. Back, complete brand and Language are
   visible and distinct (03). The Language button actually opens its dialog (04).
3. Switch to Chinese (05). Use ordinary page wheel input to reach the caption,
   font and size rows (06), then Add (07). There is no second inspector viewport
   restricting the form to one/two rows. The preview's own zoom/pan viewport is
   intentionally unchanged.
4. Switch back to English (08). All form values remain. English uses a second
   subtitle line here; Chinese fits one line. This is a deterministic reflow,
   **not an identical header height across languages** and not cumulative growth.
   The English client-header crop `(0,69)–(680,140)` is pixel-identical in 03,
   08 and 12, across scrolling, language round-trip and another resize/zoom cycle.
5. Click the real Add button while narrow. Restore 1040×760 and 100% zoom:
   the preview shows the caption and the paired fields retain their values (09).
   Wide-mode inspector scrolling is present again; its content moves relative
   to the preview as well as the outer page (10–11).
6. Return to 680×760/150%. Click the currently visible Undo button (13): the
   caption disappears. Click Back (14): the actual home page is reached.

The durable journal has exactly sequence **37 Add / 38 Undo**. The Add payload
contains the unchanged literal text, Ubuntu and 12 px. After normal application
close, the CLI opens revision 38 without overriding a lock. Its GIF is exactly
equal to the original baseline: **56,940 bytes**, SHA-256
`5756aa86082026c4fa8f7a461a324188095aa766a0cf09331bf7e0d76559c9d7`.
This short check does not repeat the full text/title acceptance suite.

## Known display limitation and retained evidence

At the home page with **680 native pixels and 150% zoom**, the existing two-column
launch cards have overlapping text. Actual screenshot:
`/tmp/gfs-wayland-qa.6grcnk18/logs/14-back-home.png`.
The header remains usable; return to **100% zoom (Ctrl+0)** for the normal home
layout. This is a known display limitation, not a claim of complete narrow-home
support. No new feature or source fix was started during release convergence.

Screenshot 11 was taken after an initial Undo click used a stale pre-scroll
coordinate and did not activate Undo. It is retained unchanged; **13 plus journal
sequence 38** are the actual Undo evidence. Similarly, an earlier intended
checkpoint click did not activate; the recovery check uses the real journal,
not a claimed checkpoint. All 14 original captures and their exact hashes/sizes
are indexed in `evidence/verification.json`, SHA-256:
`adaef7e984897d549ef830d87a10c83f899658456763a22e6164e518349f3567`.

## Clean shutdown

App and synthetic GTK fixture closed normally through the WM, both exit 0.
The client list was empty before scoped stop. The owned command handle was
consumed with `LAB_EXIT status=0`; the lab records `cleanup_complete=true` and
GNOME exit 0. Recorded PIDs and owned process groups are gone, the private bus
socket and project lock are absent. The clean worktree and stopped evidence lab
are retained; no service is left running for this QA.
