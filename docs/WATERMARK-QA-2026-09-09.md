# Watermark: native Linux localization and editing acceptance

This records actual GNOME/X11 GUI use of the watermark localization and paired
form layout. It is not a screen-recording test, Wayland/Portal acceptance,
full-language coverage, Windows pixel-reference approval or a new Release.

## Frozen build and isolation

- Exact source: `b226acaba499bd3bc094b5ee981e7ece5f416692`.
- Clean, detached worktree: `/tmp/gfs-watermark-native.Z5Qrn9/source`.
  The concurrent WPF worktree and its binaries were not built or modified.
- Command, from that worktree:
  `cargo +1.98.0 build --locked -p gif-from-screen -p gif-from-screen-cli --target-dir /tmp/gfs-watermark-native.Z5Qrn9/target`.
- Compiler: Rust 1.98.0, `88d9e12ae178fab0fb5cc050a94da85685d449ea`,
  `x86_64-unknown-linux-gnu`, LLVM 22.1.8.
- Desktop SHA-256:
  `5ecbb5b5a8659fb267a2ace937ba9aea5078d99dc6e66ac2d60b04b700e991c1`.
- CLI SHA-256:
  `26c1d68591be2108a0bdd9a3ae2a884d7fb4b852c3c0a55ce9bebb4951d7c784`.
- Cargo.lock SHA-256:
  `516f8119016707a5310a02b5352e5587bf618ac0e10e9b2ff6664005374089f0`.

The frozen worktree's `scripts/qa/wayland_nested.py start --display-server x11`
allocated `/tmp/gfs-wayland-qa.hdsdvnsu`: private Xvfb, Xauthority, GNOME Shell
42.9, session bus and configuration directories, with software rendering.
The 14,400-second safety bound was retained; the lab was explicitly stopped as
soon as the actual GUI work finished. No host desktop, input device, clipboard,
Portal permission or project owner was accessed or overridden.

Both application launches copied the same verified binary into the lab. The
launch metadata records the source revision, SHA, process identity and mode.
All screenshots are actual private-desktop captures in the lab's `logs/`.

## Actual workflow

1. The new CLI `demo-project` fixture created `evidence/watermark.gfsproj`:
   36 frames, 160×96, 50 ms each, initial revision 36. Its initial GIF is
   `evidence/baseline.gif`. A separate deterministic synthetic PNG checker,
   `evidence/logo-{name}.png`, is 32×16 RGBA, 113 bytes, with opaque magenta and
   half-transparent cyan squares. Neither fixture contains a captured desktop.
2. Open the project through the GUI without the owner-takeover checkbox. Select
   frames **1 and 3**, using the filmstrip's Control-click selection. Both
   selected cards and the count of two are visible in screenshot 05.
3. Open Overlays → Image. Inspect the English label/value rows (06). Clear the
   name and trigger the actual Add button. The Chinese required-name receipt is
   visible in 12. Switch to English without retrying: the same retained receipt
   becomes `Watermark name is required.` in 14. No edit was committed.
4. Enter the literal source path ending in `logo-{name}.png` and the name
   `QA {name} 水印`. Set X=12, Y=16; retain W=H=0, both opacities 255, Z=2 and
   Normal blend. Switching languages keeps the field data and coordinates.
   The zero dimensions resolve independently to the source's 32×16 dimensions.
5. Test the actual window's narrow mode and five Ctrl-plus zoom steps. This
   build advertises a **680-pixel minimum native width**; Mutter clamps a
   requested 480-pixel width to 680. No WM hint was bypassed. At 150% UI zoom,
   the approximately 453-logical-pixel content uses the stacked editor layout.
   The paired fields and their existing scroll container remain usable (20–24).
   The zero-dimension hint and Add button are reachable; the actual narrow-window
   click successfully adds the watermark (25). This is **not** a claim that a
   480-native-pixel window was tested. The separate egui 480-pixel regression is
   automated evidence, not a substitute for this native measurement.
6. Reset zoom and restore the original 1040×760 client. Only filmstrip cards
   1 and 3 show the checker (26). Click Undo: the checker disappears (27).
   Click Redo: it returns with the same saved identities and contents (28).
7. Click the Chinese Project → Save checkpoint button (30), then use the
   Chinese GIF export panel with **All** frames, local palette, Median cut,
   no dithering, no changed-rectangle optimization and no overwrite permission.
   Export succeeds with 36 selected / 36 encoded frames and 57,191 bytes (33).
8. Close the application normally. The CLI opens revision 39 without overriding
   a lock and exports `cli-reopened.gif`, exactly equal to the GUI file.
9. Launch the same frozen desktop again. The saved Chinese language preference
   is restored (34). Reopen through the actual Recent projects entry: the
   correct first-frame preview is present. The Layers panel contains the
   unchanged name `QA {name} 水印`, one frame-owned track and two marks (36).
   Switch to English: that user name stays literal (37). Close normally.

## Persistence and independent output checks

The checkpoint is revision **39**, with journal sequences **37 / 38 / 39** for
Add / Undo / Redo. Add registered one 2,048-byte RGBA asset and authored one mark
on each of frames 1 and 3, in stage 2. Their separate scope run IDs remain 1 and 2.
Each mark stores position (12,16), size 32×16, opacity 255 and Z=2. The restored
track, mark IDs, contents, stage tags and name equal the original Add payload.
The non-watermarked frames retain their original metadata and playback timing.

FFmpeg independently decodes the baseline and GUI GIF to per-frame RGBA hashes:
**only frames 1 and 3 differ; all other 34 frame hashes match**. FFprobe reports
36 images, 160×96 and **1.800000 seconds**. GUI and CLI-reopened GIFs are
byte-for-byte equal, each 57,191 bytes, SHA-256:
`e1b978f26eb89e578184247aef1fbc7c5e9ebce63200fac8b71ccbc6355e1698`.

Additional SHA-256 values:

| Artifact | SHA-256 |
| --- | --- |
| Baseline GIF | `5756aa86082026c4fa8f7a461a324188095aa766a0cf09331bf7e0d76559c9d7` |
| Synthetic PNG | `f5dad3fdd34c1d884baf9bba899720cb67d375a41b4642e9a8310694649b02ec` |
| Checkpoint manifest | `6e4a50d0671cf40eb172139b1a9bfcc1b57163a802cc94a4280d146cdcd50820` |
| Journal | `ab696bb3d6e0e673ceafcc4586217a726283644d19d37bbbe9ba6c8bd22bf343` |

The local evidence root is `/tmp/gfs-watermark-native.Z5Qrn9/evidence`.
`verification.json` records assertions, exact screenshot/file hashes, sizes,
the retained track and verified exited process identities. Its SHA-256 is
`1334b45d379de28857a09d20dab075584a95bed50e336c66ea17f4769db94ec1`.
The two `.framemd5` files retain every frame's independent decoded hash.

## Cleanup and evidence boundaries

Both desktop processes and the synthetic GTK fixture closed through normal WM
close actions, each exit 0. The WM client list was empty before explicit lab
stop. Both supervised command handles were consumed and returned 0; the lab and
extra-app metadata report `cleanup_complete=true`. All recorded PIDs and owned
process groups are gone, the private bus socket is gone, and no project lock
remains. The language file is owner-only 0600 in an owner-only 0700 directory.
The stopped lab, clean worktree and evidence are retained for inspection.

Earlier screenshots and unsuccessful QA commands are deliberately retained:
an initial rapid click did not trigger validation and still showed the old Open
receipt; 12/14 are the actual validation evidence. Fast xdotool Unicode typing
delivered the wrong Chinese key sequence, so the intended UTF-8 name was pasted
through a bounded **private** clipboard owner; 19 and the persisted track prove
the corrected value. A one-request clipboard owner expired before paste; its
bounded replacement was consumed/terminated. Screenshot 32 was taken during
encoding despite its filename; only 33 proves completed export. A premature
hash check correctly found no final GIF while atomic export was still running.
The verification helper was corrected to compare the actual Redo
`restore_overlay_track` payload, not assume a second Upsert, and to use hashing
available in the local Python version. No product source or expected pixels were
changed to obtain these results.

This bounded native path found no new watermark product defect. It does not
establish behavior for every raster decoder, malformed file, compositor, device,
or locale. The catalog remains 796 en/zh messages with the other 27 catalog
targets explicitly falling back; full-interface translation remains open.
