# AppImage on native Wayland: bounded acceptance

The packaged application passes native GNOME Portal authorization/cancellation,
timed recording, pause → crop movement → resume → Stop, GUI export and reopening
through its own packaged CLI. This is a real AppImage test, not a direct launch
of the unpackaged ELF. Broader hardware/KDE, desktop registration, source-material
and release gates remain open.

## Frozen binary and isolation

Private lab `/tmp/gfs-wayland-qa.c1e21eur` ran an owned Xvfb and nested GNOME
Shell 42.9, Portal 1.14.4/GNOME backend 42.1, PipeWire 0.3.48 and Media Session
0.4.1. Its virtual Wayland monitor was 1280×720; rendering was software, not a
physical GPU/display test. The ordinary GNOME chooser was used, not simulated
responses or pre-granted permissions. The initial lifetime was 1800 seconds;
the lab was explicitly stopped when testing finished.

The frozen `bin/gif-from-screen` is the actual AppImage, SHA-256
`b48e2e578fb8efff9e226d00e644431c1ce46e340a998df0657c88030e6a43ff`.
It ran via normal FUSE mounting. During execution its app PID was 4034385 and
the mount was `/tmp/.mount_gif-frflkHpn`. Observed process mappings showed
libwayland-client/egl, libxkbcommon and libpipewire from that mount. Once shared,
all six configured PipeWire client modules and SPA support/D-Bus/journal modules
were observed from the private library directories as well. Portal/PipeWire
servers were the lab's isolated processes; graphics drivers remained host
components. No daemon or unrestricted permission was bundled to bypass consent.

## Cancel and first timed recording

The first monitor chooser was cancelled normally (`05-first-chooser.png` →
`07-chooser-cancelled.png`). The app returned to settings. Read-only graph
inspection showed only Dummy-Driver node 23 and no device/capture nodes; Mutter
introspection showed no Session/Stream children. No project had been created.

The next chooser selected the uniquely named GTK fixture as a **window**
(`08-window-chooser.png`, `10-window-selected.png`). Share prepared a real
692×509 PipeWire frame, including that source's window presentation. Exact
source-local crop (50,100), 200×150 was applied. The independent compact
controller hid the ordinary pages and explained its frozen/source-local nature;
it is not a global physical overlay on Wayland (`13-controller-ready.png`).

Start, the configured three-second countdown and a 3000 ms recording limit
produced 30 frames. `15-recording-active.png` shows active controls;
`16-timed-editor.png` shows the result. The project is revision 60, 200×150,
3,000,000 us, origin (50,100) on every frame, and three distinct raw assets.
Every raw pixel is the fixture's red (209,56,61,255) or moving white marker;
29,216–30,000 of each frame's 30,000 pixels are red. No controller pixels appear
in this cropped window-source result. That is not general monitor self-exclusion.

GUI Export writes `output/gif-from-screen.gif` (945 bytes, three GIF images,
3000 ms). After normal app close, the **same AppImage's CLI** reopened and exported
`reopened-timed.gif`; the two files are byte-identical, SHA-256
`41f019beb1a0caa4913ef3f189b47ef9a711887e57f4cd5ec52fa12273a1ac99`.
Duplicate merging accounts for the three encoded images, not lost project frames.

## Pause, move and continue without another chooser

A second window authorization created `wayland-move.gfsproj` with no automatic
time limit. Recording began at the same 200×150 crop. Pause was acknowledged
(`26-paused-first-origin.png`), then 35 ordinary right-arrow button clicks moved
the fixed-size crop by 350 source pixels to (400,100), while still paused
(`27-paused-moved.png`). Resume recorded the green region, then Stop returned to
the editor (`28-resumed-second-origin.png`, `29-manual-stop-editor.png`).
This movement used source-local controls; it did not reopen or forge the chooser.

The saved revision 81 has 41 frames, 4,023,053 us, all 200×150: the first 17 at
(50,100) are red/white; the remaining 24 at (400,100) are green (30,143,79,255)
or white. It retains one capture-clock identity throughout, independent of the
first project's clock. The origin-change boundary has a 12,071 us sample gap,
not the long wall-clock pause. All stored per-frame capture-clock samples match
the corresponding raw timestamps. Input event lists are empty; these samples
do not establish Wayland global-input support or visible-cursor fidelity.

Packaged CLI reopening/export produces `wayland-move.gif`: 8634 bytes, 27 images,
raw encoded delay sum **4020 ms**, SHA-256
`73e04c85392279305c40d160ee192af1f0d5f7954b0815d8a730f4aa45ec32c4`.
One encoded image (index 10) has a 10 ms delay. Default ffprobe reports 4110 ms
because it clamps that delay to 100 ms; `-min_delay 0`, direct GIF control-block
parsing and Pillow all agree on 4020 ms. This is decoder policy, not an extra
90 ms encoded by the exporter. Neither value includes the long pause.

## Independent file checks and teardown

The read-only `logs/verify-appimage-wayland.py` and its offline Rust/BLAKE3 helper
checked all 71 frames and 29 registered assets against descriptors, byte lengths
and their actual BLAKE3 IDs. They also recorded SHA-256 fingerprints, raw color
sets, clocks, origins and independent GIF control-block/Pillow results. No
project, asset, lock or GIF was modified by that inspection.

Report: `logs/wayland-artifact-evidence.json`, SHA-256
`35c5ca11e1fb1a573058329c2cf5b3b58179fef01feb1af10eadb997292274c9`.
Script SHA-256:
`42a6b3668f4dc20d2bf8d35a9ba9771d1bf833b3bbe999e336629f6bf44f5abe`.

Normal titlebar Close ended the app with exit 0. Both its payload and FUSE
server exited. Final graph/session inspection again found only Dummy-Driver and
no capture Session/Stream (`31-final-graph.json`, `32-final-session-tree.txt`).
Explicit lab stop completed with launcher exit 0 and `cleanup_complete: true`;
all registered children are terminal. Evidence directories are intentionally
retained, not live desktop sessions.
