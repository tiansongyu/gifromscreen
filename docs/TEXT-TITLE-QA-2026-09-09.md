# Text and titles: native Linux acceptance

Actual GNOME/X11 acceptance of the [text/title UI migration](TEXT-TITLE-LOCALIZATION.md).
This is not a new Release, a Windows text-reference comparison, physical capture
acceptance, or proof of complete interface/font/language coverage.

## Exact build and private environment

- Source: `266ec23592926a6ddae0b2b21eb04e93d28d09e5`.
- Clean detached worktree: `/tmp/gfs-text-title-native.Q8jUDJ/source`.
- Build command from that worktree:
  `cargo +1.98.0 build --locked -p gif-from-screen -p gif-from-screen-cli --target-dir /tmp/gfs-text-title-native.Q8jUDJ/target`.
- Rust 1.98.0, `88d9e12ae178fab0fb5cc050a94da85685d449ea`,
  `x86_64-unknown-linux-gnu`, LLVM 22.1.8.
- Desktop SHA-256:
  `cf5fe21697a8e545a55557ce577c2a9b97cfbbf278beebcbb42a93f86b85f72e`.
- CLI SHA-256:
  `3a9ee8ee21761eb2739cb5236b10370c946ebe50791e744334ce7bab7c747d21`.
- Cargo.lock SHA-256:
  `516f8119016707a5310a02b5352e5587bf618ac0e10e9b2ff6664005374089f0`.

The unchanged scoped `wayland_nested.py --display-server x11` harness allocated
`/tmp/gfs-wayland-qa.i_0ahts_`: private Xvfb, GNOME Shell 42.9, Xauthority,
session bus and configuration directories, with software rendering. Both app
launches froze the same desktop binary. No dirty concurrent WPF/board source,
host desktop, host clipboard, recording device or existing project was used.
The four-hour supervisor bound was a safety limit, not a target runtime; the lab
was explicitly stopped immediately after GUI acceptance.

## Actual operations and screenshots

Screenshots 01–34 are actual windows in the lab's `logs/` directory. The newly
created CLI `demo-project` fixture starts at revision 36 with 36 synthetic frames,
160×96 and 50 ms per frame. The original manifest and GIF are separately retained.

1. Open the project normally, without an owner-takeover option, and Control-click
   to select frames **1 and 3**. Open Overlays → Text & titles (02).
2. Click Add with empty text. The English `Enter some text first.` notice (04)
   becomes Chinese after changing language, without repeating the operation (05).
3. Enter the literal caption `A {name}` and unavailable font `Missing-{font}`.
   The real background font/rasterization path returns the localized font-not-
   installed notice (06). Switch to English: the retained notice changes language,
   while both literal input strings remain unchanged (07). After both rejected
   operations, the manifest is byte-identical to the initial copy and the journal
   is still empty.
4. Choose the valid font name `Ubuntu`, set 12 px and disable the caption
   background; retain left alignment and automatic box dimensions (08).
5. Resize the client from 1040×760 to **680×760** and send five separately spaced
   Ctrl-plus presses for 150% UI zoom. At approximately 453 logical pixels wide,
   the stacked layout retains paired font/size fields and the real Add button
   through its scroll container (09–13). Click that narrow-layout button:
   caption `A {name}` is committed only to frames 1 and 3. Reset zoom and restore
   1040×760; their filmstrip and preview show the caption (14).
6. Open the actual saved-text selector and choose `Layer 1 · Text · 2 frames`
   (15). Change the loaded source text to `B {name}` and switch to Chinese (16).
   The existing picture stays `A {name}` until Save text changes is clicked.
   Save updates the existing group in place (17), not a newly created track.
7. Choose New text, enter `T {title}`, enable title mode and leave **At start**,
   duration 1000 ms and the default opaque canvas background (18). Insert:
   a new first frame is selected, with no inherited caption artwork (19–20).
8. Click Undo: the title disappears and the original timeline returns (21).
   Click Redo: the exact title frame/track is restored (22). Redo preserves the
   reconciled current original frame; explicitly click the first title to select
   the intended next insertion anchor (23).
9. Enter `U {title}`, choose **After current frame** and 500 ms (24–25). Insert:
   the second title appears immediately after the selected first title (26–27).
10. Save a checkpoint (28). Through the Chinese GUI, export **all 38 frames**,
    local palettes, Median cut, no dither, no changed-rectangle optimization and
    no overwrite permission. Completion reports 38/38 and 59,403 bytes (29–30).
11. Close normally. The CLI opens revision 42 without overriding a lock and
    exports a byte-identical GIF. Relaunch the same frozen app: Chinese is
    restored from its private saved preference (31). Reopen the recent project,
    select the original caption frame, and load the exact saved group (32–33).
    Text `B {name}`, Ubuntu and 12 px remain editable and unchanged. The two
    same-name Title groups have distinct absolute layer numbers in the selector.
    Switch to English and retain the loaded values (34), then close normally.

## Durable identity, scope and independent GIF checks

The journal sequences are:

| Sequence | Actual operation |
| --- | --- |
| 37 | Add `A {name}` to original frames 1 and 3 |
| 38 | Replace that group's text with `B {name}` |
| 39 | Insert `T {title}` at the start |
| 40 / 41 | Undo / Redo the first title |
| 42 | Insert `U {title}` after the explicitly selected first title |

The caption keeps its exact track ID, both mark IDs, frame owners, stage 2,
scope runs, positions, dimensions, opacity, alignment, font and font size across
the edit. Only source text and the immutable raster asset change. After title
insertion, the owners are still the original frame IDs, now at positions **3 and
5**. All other original frame descriptors are unchanged; the original frame
ordering and 50 ms durations are retained. The first title's Redo payload equals
its original track and preserves the original title frame identity.

FFmpeg independently decodes both GIFs to per-frame RGBA hashes. After excluding
the two inserted title frames, **only original frames 1 and 3 differ**; all 34
other original frame hashes match. FFprobe confirms **38 frames, 160×96,
3.300000 seconds**: 1.000 s title + 0.500 s title + 1.800 s original animation.

GUI and CLI-reopened GIFs are byte-identical, each **59,403 bytes**, SHA-256:
`0a66778aaf99d29374b47b0915def3bfd43a295449087674ac463c3d9d5f6325`.
Final manifest SHA-256:
`ee43ae2a19bec410190a5a9cf3f274c45115a35041f68900907932e4300a03eb`.
Journal SHA-256:
`9909074dc01351fe16e8707268b1ad8467fcf1354c3e8f3869e085f3381834c3`.

Local evidence: `/tmp/gfs-text-title-native.Q8jUDJ/evidence`.
`verification.json` retains exact assertions, track data, every screenshot/file
hash and size, output probe and process identities. Its SHA-256 is
`392aa02b5bbea4662494068e409cce657dcea1c90015592edd51d2c398b39c47`.
The `.framemd5` files preserve all decoded frame hashes.

## Cleanup and remaining boundaries

Both app instances and the GTK fixture closed through normal WM actions, each
exit 0. The WM client list was empty before explicit scoped stop. Both command
handles were consumed and returned 0; both supervisor records report
`cleanup_complete=true`. All recorded PIDs and owned process groups are gone;
the private bus socket and project lock are absent. Preferences remain owner-only
0600. The clean detached worktree and stopped lab are retained for inspection.

The narrow shared editor requires separate scrolling of the outer page and
inner inspector; at 150% only a few inspector rows can be visible at once. The
header description is also crowded by the Language button (09–13). Actions were
reachable, but these observations are not a claim of ideal small-window layout.
No production source was changed during this acceptance.

This run verifies the Ubuntu/Latin caption path and Chinese application UI, not
all installed fonts or complex-script shaping. The unchanged shared egui color
picker's internal tooltip text remains the separate toolkit-localization boundary
described in the feature document; no localized picker claim is made here. The
848-message en/zh catalog at this pinned commit still has explicit fallback for
the other 27 targets. Headless tests, unrelated native capture tests and WPF
comparisons are not counted as passes in this native walkthrough.
