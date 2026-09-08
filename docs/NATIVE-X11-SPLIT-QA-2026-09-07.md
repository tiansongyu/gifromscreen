# Native X11 split recorder QA — 2026-09-07

**Scoped result:** ARGB-guide ordinary-region, 1×1, pause-and-retarget movement,
and full-monitor controller-recovery profiles pass the checks below. Ordinary,
tiny, moved-region, full-monitor and recovered-full GIF exports are verified.
The first two ordinary-region recordings are retained **failed shadow cases**.
This is not certification of every drag position, recovery race, or compositor.

## Environment and provenance

These are private GNOME/Mutter **X11** software-rendered desktop runs, despite
the historical `wayland-qa`/fixture filenames. The desktop is 1440×1000. Root
operated the native UI; initial independent pixel audits read checkpointed
manifests, raw assets, existing screenshots and GIFs without opening project write
sessions. A later, explicitly authorized product recovery of two dead-owner QA
projects is recorded below. No lock was manually deleted, no existing artifact
was overwritten, and that recovery did not manipulate a live desktop.

Run-local evidence directories, not committed fixtures:

- **L1:** `/tmp/gfs-wayland-qa.mv73xetc`. Supervisor reports `phase=stopped`,
  `cleanup_complete=true`; its bounded session ended after 1,800 seconds.
- **L2:** `/tmp/gfs-wayland-qa.uzs2decy`. The 3,600-second bound ended the session;
  status is `stopped`, reason `bounded acceptance lifetime ended`, cleanup true.
  The move project initially retained a stale lock: this was **not** normal
  product closure. The later product recovery below preserves that distinction.
- **L3:** `/tmp/gfs-wayland-qa.cs7u0yif`. After the successful recovery run, root
  observed normal app `Alt+F4` exit 0 and CLI export exit 0, then requested the
  lab stop. Status is `stopped`, reason `lab stop requested`, cleanup true.

All three supervisors confirm cleanup. Only L1 normal2 and L2 move subsequently
used the product's explicit stale-lock takeover path, after verifying their old
app and supervisor processes were absent.

Successive development binaries must not be conflated:

| Cases | Frozen binary under its lab's `bin/` | SHA-256 |
|---|---|---|
| L1 normal1, before shadow hint | `gif-from-screen` | `18e98d81110920b7c244b9839810b6a88429d6b49b2bcf234637defdc45c6806` |
| L1 normal2 and full | `extra-app-2276008` | `9ac8aa43bac929cdd8ea66f0f6efdc6f850dce94cbc7a62cfca6ca823133f0be` |
| L2 ARGB normal3 | `gif-from-screen` | `210c256adf4c263bb5d9a582779b2ab943b0e6020fd85805f199b7d374b465d6` |
| L2 tiny and move, actual controller geometry observer | `extra-app-2372511` | `a05466fc6cea09c076590d738181a6c98c751758199e4066cbc31701f6d07262` |
| L3 successful full-monitor recovery | `extra-app-2432792` | `e8b74a4948a9e8fa7b675248f070ecf2faa948e0d01ac37441667079140d9555` |

Hashes are bound by the frozen-instance records; the two L1 binaries were also
independently rehashed. This is development-cohort evidence, not a clean release
tag or certification of every subsequent build.

## Cases and artifacts

All paths below are relative to the corresponding lab's `output/`. Regions are
global physical `(x, y, width, height)`, not UI points. Each of the first five raw
timelines lasts exactly 3 seconds; move lasts 8 seconds and recovery 6 seconds.
Requested cadence was 10 FPS; actual counts are not inferred from that setting.

| Case | Raw project | Region / raw frames | Checked outcome | GIF |
|---|---|---|---|---|
| normal1 | L1 `gif-from-screen.gfsproj` | `(45,114,640,420)` / 30 | **FAIL:** perimeter shadow | `split-normal.gif`, 30 frames, 3,000 ms, 240,919 B; encoding succeeds but source pixels fail |
| normal2 | L1 `normal2-native.gfsproj` | `(45,114,640,420)` / 30 | **FAIL:** same shadow despite zero frame-extents hint | Product-recovered `normal2-native.gif`, 30 images, 3,000 ms, 240,342 B; source-pixel failure remains |
| full | L1 `full-native.gfsproj` | `(0,0,1440,1000)` / 27 | **PASS within mask/boundary scope below** | `full-native.gif`, 27 frames, 3,000 ms, 5,378,415 B |
| normal3 | L2 `normal3-native.gfsproj` | `(45,114,640,420)` / 30 | **PASS:** ARGB guide; raw perimeter 0 diff | `normal3-native.gif`, 30 images, 3,000 ms, 181,971 B |
| tiny | L2 `tiny-native.gfsproj` | `(100,300,1,1)` / 30 | **PASS:** every raw pixel equals fixture | `tiny-native.gif`, 1 coalesced image, 3,000 ms, 68 B |
| move | L2 `move-native.gfsproj` | `(45,114)` → `(145,164)`, fixed 640×420 / 81 | **PASS within retained-origin/pixel scope below** | Product-recovered `move-native.gif`, 81 images, 8,000 ms, 1,379,283 B |
| full recovery | L3 `gif-from-screen.gfsproj` | `(0,0,1440,1000)` / 54 | **PASS:** Paused → Resume → timed completion | `recovered-full.gif`, 54 images, 6,000 ms, 10,756,009 B |

## Ordinary-region pixel oracle and shadow counterexamples

The fixture is `scripts/qa/wayland-fixture.py`, SHA-256
`5c90e776b6c9c3a7de0590a17e4cc31ba91d6985d5a99b6170985c9843e5d3a3`.
An independent in-memory Cairo 1.16.0 render uses its literal quadrant colors and
rectangles, not colors sampled from the recording. Expected RGBA quadrants are
`[209,56,61,255]`, `[30,143,79,255]`, `[38,87,209,255]`, `[232,176,40,255]`.
The fixture's text, moving square and counter do not touch the outer 4 px.

Each perimeter check examines the union of all four 4 px bands: **8,416 unique
pixels per frame**, corners counted once; 252,480 pixels per ordinary recording.
Assets are checked as packed RGBA8 with their exact descriptor/byte length.
The projects have no post-capture transform, effect, render step, overlay or
transition that could explain a changed edge.

- **normal1 and normal2:** every checked pixel differs; all 757,440 RGB channels
  are darker by 1–18, with alpha unchanged. All 30 perimeter byte sequences in
  each project, and between the two projects, are identical. For example `(0,0)`
  should be `[209,56,61,255]` but is `[195,53,57,255]`. Internal quadrant samples
  match the oracle; top/side colors gradually recover about 20 px inward. This
  is local shadow contamination, not GIF quantization or whole-image color drift.
- Root observed `_GTK_FRAME_EXTENTS=[0,0,0,0]` on guide window `0x1000000` during
  normal2. Combined with identical captured edge bytes, this is a counterexample
  to treating the property hint alone as a no-shadow guarantee on this Mutter.
- **normal3:** all 252,480 pixels and all RGBA channels match exactly, including
  the extreme corners. The controller is shown separately below the selection
  in `L2/02-argb-ready.png`; root reports no Retry was needed. `03-argb-editor.png`
  shows the resulting 30-frame project. This passes the stationary ordinary-region
  edge test for the actual ARGB-guide build, not every compositor or moving guide.

Perimeter SHA-256 uses row-major RGBA bytes restricted to the four-band union:

| Bytes | SHA-256 |
|---|---|
| Oracle and every normal3 frame | `d67445490277343f2bab51fa73444957578584a88814850b66a8a17a850893fd` |
| Every normal1/normal2 frame | `c6aa3af61a0e34d3e4128249423bbd48ff014af4f5f3f070896435630ad791da` |

## Full-monitor native and GIF scope

Root's recorded native sequence used **no global bindings**, a 3-second countdown
and a 3-second timed stop. `L1/09-full-controller.png` shows Ready controls;
`10-full-active.png` shows the desktop/fixture without visible controller or guide;
`11-full-editor.png` shows automatic return to the editor with 27 frames. The
product CLI subsequently reopened and exported the project. Screenshots support
visible states; the action ordering is root's native observation.

Independent raw checks across all 27 frames found:

- Full 1440×1000 RGBA8 assets, 5,760,000 bytes each, alpha 255; origin `(0,0)`.
  The last complete row, last complete column and four extreme corners match
  `10-full-active.png` exactly; the right/bottom source edges were not cropped.
- A static wallpaper mask matches the screenshot with **0 diff at 1,046,445
  pixels per frame**. It excludes the top 32 px, fixture/shadow rectangle
  `[20,710)×[60,560)`, and cursor rectangle `[20,75)×[800,845)`.
- The complete fixture client `[45,685)×[114,534)` has a 4 px perimeter that
  matches the same quadrant oracle in all 27 frames: 227,232 pixels, 0 diff.
- A wider static mask, excluding only the top bar, moving-square lane, counter
  text and cursor, has 23,632 differing pixels in frame 0, restricted to the
  fixture's own titlebar `[45,685)×[77,114)`; maximum channel delta 38. Frames
  1–26 match that mask exactly. Focus appearance is a plausible explanation,
  not independently proven causation. **Whole frames are not bitwise identical.**

Raw durations range from 67,107 to 130,746 µs. The first 26 equal the next raw
sampling interval; the final 67,107 µs completes exactly 3 seconds. This is not
proof of sustained 10 FPS under the software-rendered test conditions.

All 27 GIF images decode, and their encoded image rectangles are each
`(0,0,1440,1000)`. Delays are `20×110 + 5×120 + 1×130 + 1×70 = 3,000 ms`;
loop count is 0. GIF palette quantization means the raw/PNG pixel comparisons
must **not** be relabeled as GIF/RGBA byte equality.

## Tiny selection and metadata

`L2/08-observed-tiny-ready.png` shows the independent 420×300 control window and
the 1×1 selection; `09-tiny-editor.png` shows 30 frames and `Rendered 1×1`.
This build observes actual controller geometry rather than trusting stale frame
extents. All 30 raw frames are exactly `[209,56,61,255]`: 0 pixel/channel errors.
Their shared four-byte frame asset is legitimate deduplication of a static
point, not evidence of missing frame records. Its SHA-256 is
`ab463e316960a288d22d6d4279bb2a8cb5de028b5cd88794a855e0b234bcc1d9`.

The initial five audited projects have checkpointed empty journals, source-preserving
frames and one consistent capture-clock ID per recording. Each stored
`sampled_at` equals raw `captured_at`, increases strictly, and each nonfinal frame
duration equals the next sample interval. Each project keeps its requested
origin and physical canvas; no UI-font/viewport size is substituted for it.
This checks persisted consistency, not every pause/retarget or source-clock path.

Normal3 and tiny were subsequently exported through the normal product CLI.
Independent decoding confirms 30 normal3 images at 100 ms each, all image
rectangles `(0,0,640,420)`; GIF quantization is present (first pixel
`[210,61,66,255]`, not the raw `[209,56,61,255]`). Tiny's identical raw frames
coalesce into one `(0,0,1,1)` image lasting 3,000 ms, with the exact expected
RGBA pixel. Neither the coalescing nor palette conversion changes the raw claims.

## Movement: retained old and final regions

Root kept the same `extra-app-2372511` process after tiny, returned to Screen,
and recorded the 8-second move case. Evidence is `L2/11-move-ready.png`,
`12-move-active.png` and `13-move-editor.png`.

The 81 raw frames retain **13 frames at `(45,114)` and 68 at `(145,164)`**, all
640×420. No intermediate drag positions have retained frames. This validates
the tested pause/update/resume path, not continuous sampling at every drag point.
One clock ID, `da632b606ca54c01906867ea96383c2a`, spans raw samples 8–7,924,045 µs;
`sampled_at` matches raw timestamps and the 80 nonfinal durations match their
differences. Frame 12 lasts 23,943 µs; frame 13 begins at 1,223,999 µs. The final
75,963 µs completes exactly 8 seconds. GUI gesture ordering comes from root's
native actions, not from timestamps alone.

The independent pixel audit retained these exact scopes and exceptions:

- All 13 old-region 4 px perimeters match the quadrant oracle. In its 218,848-pixel
  static mask, frames 0–9 match Ready exactly; frames 10–12 each differ at 231
  pixels, exactly the support of the stored visible 24×24 embedded cursor.
  Reblending that cursor leaves 13 pixels with maximum RGB delta 1: consistent
  with legitimate cursor rendering, **not** a claim of whole-mask byte equality.
- All 68 final-region frames match Active at 226,224 predefined static pixels
  per frame, excluding only the moving-square lane and counter. Last complete
  rows/columns also match; post-move cursor visibility is false.
- Old UI footprints intersecting the final region match with 0 diff per frame:
  old bottom guide 2,176 pixels, right guide 1,496, controller 17,640, and its
  16 px halo 25,868. This tests actual underlying pixels, not an orange-color search.
- To avoid accepting a shared new shadow in both Active and raw, Ready is also
  an independent reference: 157,224 static fixture pixels, the final region's
  rightmost 4 columns (1,680 pixels), and an old-UI-free bottom segment (776 pixels)
  all match in every final-region frame. Masks were not enlarged after inspection.

Raw metadata has no post-capture overlays, transforms, effects or render steps;
the journal is checkpointed and empty. The supervisor later timed out with a
stale project lock. The later product recovery and GIF validation are described
below; they do not turn the original bounded-stop exit into a normal app closure.

## Explicit product recovery of the two stale QA locks

This follow-up touched only `L1/output/normal2-native.gfsproj` and
`L2/output/move-native.gfsproj`. Before takeover, both lab statuses were `stopped`
with `cleanup_complete=true` and reason `bounded acceptance lifetime ended`.
The frozen child records identified the old app processes with `alive=false` and
exit code `-15`; `/proc/PID/stat` was absent for each app and all its recorded
app/lab/outer supervisors:

| Project | Old lock/app PID | Recorded app start ticks | App / lab / outer supervisor PIDs |
|---|---:|---:|---|
| normal2 | 2276016 | 158155600 | 2276008 / 2262695 / 2262674 |
| move | 2372640 | 158543438 | 2372511 / 2317345 / 2317323 |

A temporary, hardcoded QA caller at `/tmp/gfs-qa-product-recovery.b85doxwO`
first validated each manifest with the domain model and all immutable asset
lengths/BLAKE3 identities: normal2 had 31 assets / 32,258,304 bytes; move had
82 assets / 87,093,504 bytes. It rechecked dead-process identities immediately
before calling the existing `ActiveProject::open(..., LockPolicy::TakeOver)`.
The product preserved each original lock as `project.lock.stale-1`, validated
`AssetCheck::FullDigest`, and released its new ownership lock on normal Drop.
Revisions remained 56 and 151, journal replay was clean with zero replayed
records, and manifest/journal bytes were unchanged. No recovery code changed a
manifest, frame, or lock manually.

The ordinary product CLI then opened each project using its unchanged
`FailIfPresent` export path and created the previously absent GIF destination:

- normal2: 30 images, all 640×420; 30 × 100 ms = 3,000 ms; 240,342 bytes.
- move: 81 images, all 640×420; 79 × 100 ms + 20 ms + 80 ms = 8,000 ms;
  1,379,283 bytes.

Every GIF image was decoded; encoded image rectangles also cover the complete
640×420 canvas and both loop values are zero. Both `project.lock` files were
absent after CLI completion, both preserved locks retained their original
bytes, and manifest/asset integrity remained unchanged. The CLI executable
SHA-256 was `a2f056d98fdb89d4a30db8ca4593fd5168f688f91e358786121768d2d6e185bc`.
This closes the two pending product recovery/export checks, not the original
normal2 shadow defect, unrelated projects' locks, or broader crash-recovery and
compositor gates. GIF quantization is not a claim of raw/PNG byte equality.

## Full-monitor restoration during recording

Only L3's `extra-app-2432792` is the successful recovery profile. Earlier 01/03
startup candidates failed before capture and are not counted as successes.
Mutter reported `WM_STATE=Iconic` and `_NET_WM_STATE_HIDDEN` while X `MapState`
remained `IsViewable`; requiring an unmapped window incorrectly blocked startup.
The corrected gate was used for this successful run. Native geometry observations
alone cannot determine compositor hiding or establish a presentation fence.

`L3/05-correct-recovered.png` shows **Paused, 10 frames, GIF 1.0 s, Resume** after
controller recovery. Root clicked Resume; `06-resumed-editor.png` shows 54 frames
after timed completion. Clock `d41e35b4217e4025aa8249b9a16bd131` remains unchanged;
samples 9–5,946,453 µs are strictly increasing and equal stored `sampled_at`.
The transition from sample 9 (997,754 µs) to sample 10 (1,109,268 µs) is 111,514 µs;
the wall-clock recovery wait was not added to playback. Total raw duration is
exactly 6,000,000 µs, with full 1440×1000 size and origin `(0,0)` throughout.

`03-corrected-start.png` is used **only as a static desktop reference**, not as an
active screenshot of the successful attempt. A predefined mask excludes the top
32 px, fixture titlebar `[45,685)×[77,114)`, moving-square lane
`[45,685)×[184,212)`, counter `[57,673)×[468,520)`, and cursor
`[20,80)×[800,850)`. All 54 frames match its 1,317,288 remaining pixels exactly.
Within the old opaque-controller rectangle `[0,1040)×[32,829)`, 753,508 static
pixels match; within its 16 px halo, 782,196 match. Last rows/columns and the
fixture's 8,416-pixel perimeter also have 0 diff. There is no post-capture
transform/effect/overlay or dynamically expanded mask hiding a difference.

Root confirmed CLI exit 0: exported revision 108, 54 images, 10,756,009 bytes.
The independent full GIF decode verifies 54 complete `(0,0,1440,1000)` images,
delays `39×110 + 13×120 + 1×100 + 1×50 = 6,000 ms`, loop 0, and a stable file hash.
This closes this software-Mutter recovery profile, not every WM or recovery race.

## Artifact identities

| Artifact | SHA-256 |
|---|---|
| normal1 manifest | `64c52b7d7e228203139623a43a09a0f957247edd0f419c23cabeca26bc0cf461` |
| normal2 manifest | `666db8d4e7b23254c63caff42c592ab5530c7ba88369bfd35afe0854c52755ab` |
| full manifest | `294cd406a38cd88eebd249aa2321d91c21fe62b8dc97b4c7d36cf5c5761a72d7` |
| normal3 manifest | `97bc9dac5edbf6d997e91ba664f1c4a70b635c27b465ef4c69e06f26aa90b741` |
| tiny manifest | `be36f5e7b3af2a00d036126fa2860d0b1554878a8ac3ee2fc7c9d48836d2e919` |
| move manifest | `ffbe2212e697e5b0037b8e8f2eccf9b857cc7f77a0de6a7874a78f5d8bde8b91` |
| full-recovery manifest | `eda9734fe4f57e43632a5a8523c69b16bba5d8e1fdafd25f7a56326a682c2b98` |
| `split-normal.gif` | `51b4e0ab0a81ffd151b6275e85bc72242b609b9556609903a334c7bccdb1ff14` |
| `normal2-native.gif` | `c43dac2bc0cb83b59547539b2bf43cd74a65a43ef270bf89b6b20bcec55356fb` |
| `full-native.gif` | `259d637fbb2d6108866757ea64df4a90776c918f1b14694d9c8b0d7145aaf002` |
| `normal3-native.gif` | `07338fb0dae6e043f0dd623b38b31bc7039f1c7736526a5784f16385c973925f` |
| `tiny-native.gif` | `b8e6d566457afdd6c165c064a574a136c3b4c14725a2a69062d9cf2063c49852` |
| `move-native.gif` | `01a58283206cc7ac5deedaab53d401242650e85858038579f464c0a117b616f5` |
| L1 normal2 `project.lock.stale-1` | `db239e1c48a5eba2db4429475d0b2577424bb2ef7e86646edf856625bffc7504` |
| L2 move `project.lock.stale-1` | `bad62d64ac272ecb8e18b7792c1c80298dbe133589696969d2920175f14c0a30` |
| `recovered-full.gif` | `2d229a376d8a748bbc8e090b47691163c9f8d6d7a1dba6947c6714effdd9404a` |
| L1 `10-full-active.png` | `0fcc156363331035a4381448111fa6760be6442dec4795fdd55cfcdd877b6502` |
| L2 `02-argb-ready.png` | `e86f2206274a924b71f26fa563536b0efe5d556d67dc58e5228ca4ea35682173` |
| L2 `08-observed-tiny-ready.png` | `9a7843747fca0cb7996ea861401aa139a761ac35ad4fb15609e0730d95d4137e` |
| L2 `09-tiny-editor.png` | `e6e7dcea5a7a57a513446ef9a85852b26044d05f250100488073448aea5afc96` |
| L2 `11-move-ready.png` | `a7f6543abd91beade511257d448170567d324f31326756feaa7593a89f953dbc` |
| L2 `12-move-active.png` | `033d22d08062dcf6b8410e8fb695e33c68119854aa92b746104d4e0126dfd9af` |
| L3 `03-corrected-start.png` | `e7eb30928232c3fbf2249b26d7118ace6dd0d4ddd9fcc0522b8490effe23781b` |
| L3 `05-correct-recovered.png` | `d532c3606bf0e3975a4d1830eacc7276fdb38e342da9e5337cfe61a7d8dae3cb` |
| L3 `06-resumed-editor.png` | `1a4791f394c4317c4152867d0b952f0a5a62e7fe40556d1f8323ff7cc54df5c9` |

## Still open

- The tested move retains only old/final regions. Other trajectories, rapid
  reversals, stop/cancel races and broader stale-acknowledgement scenarios still
  require native acceptance; no every-intermediate-position claim is made.
- Active full-monitor Paused → Resume succeeds in the L3 profile, but other
  restoration paths, indefinite/manual stop recovery and failure races remain
  separate acceptance work.
- ARGB shadow behavior on other compositors, hardware-rendered GNOME, KDE,
  mixed DPI/multiple monitors, monitor changes and zoom remains unverified here.
  An X-server geometry round trip or fixed settling delay is not a compositor
  presentation fence.
- `L2/02-argb-ready.png` has a noise block around `[0,35]×[296,332]`, outside both
  capture `R` and the current guide strips. Its cause is unproven and remains a
  separate visible UI artifact; it neither invalidates the measured raw-edge
  pass nor becomes resolved merely because the later editor covers that area.
- The two explicit dead-owner recoveries above do not establish every
  crash/power-loss or concurrent-owner race. Supervisor cleanup was not normal
  product closure. The broader [Linux release gates](LINUX-STATUS.md) remain
  separate follow-up work.
