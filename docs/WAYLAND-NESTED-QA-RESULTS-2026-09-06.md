# Nested GNOME acceptance: execution record

Status: **native window recording, pause/retarget/resume, Stop and GIF export passed on 2026-09-07; remaining platform gates are listed below**.

This follows [the isolated acceptance plan](WAYLAND-NESTED-QA-PLAN.md). The environment now runs real nested GNOME Shell 42.9, xdg-desktop-portal 1.14.4, its GNOME 42.1 backend, PipeWire 0.3.48 and pipewire-media-session 0.4.1. It does not use a fake portal or pre-granted permissions. Application implementation remains Rust; the supervisor/GTK fixture and the native SPA interoperability probe are test infrastructure.

## Reproducible harness

Implemented under `scripts/qa/`:

- `wayland_nested.py`: isolated launch, readiness, status, bounded GUI commands, explicit stop, separately supervised replacement apps, PID/start identity tracking and bounded logs.
- `wayland-fixture.py`: native GTK Wayland window with four distinct quadrants, frame counter and moving marker; no global input/device access.
- `pipewire.conf`, `media-session.conf`, `client.conf`: explicit private graph without hardware discovery, RTKit, audio, camera, BlueZ or logind modules.
- `test_wayland_nested.py`: environment isolation, untouched home variable, private file permissions, module allowlist and exact-child cleanup tests.

Commands (run from the repository; desktop binary must have been built first):

```bash
PYTHONDONTWRITEBYTECODE=1 /usr/bin/python3 -m unittest discover -s scripts/qa -p 'test_*.py' -v
/usr/bin/python3 scripts/qa/wayland_nested.py start --app target/debug/gif-from-screen --seconds 14400 --startup-timeout 45
```

The launcher prints the allocated `LAB_CREATED` directory. `status LAB` never launches a replacement. `exec LAB -- COMMAND...` injects only that live lab's connection environment and limits the command to 30 seconds. `run-app LAB --app PATH --seconds 600` attaches a separately supervised app without restarting any compositor, bus or portal. Lifetimes are bounded to 1–14400 seconds; startup is separately bounded to 1–120 seconds. A longer bounded debugging session does not disable signals, extend existing processes or relax isolation. `stop LAB` requests orderly cleanup of the registered children. Stop promptly when testing ends; do not substitute a stale directory or the host display.

Current harness tests: **6 passed**. CLI parsing and Python compilation passed; generated logs/environment files were observed with mode 0600 and lab/runtime directories with mode 0700. Native graph inspection found **zero `PipeWire:Interface:Device` objects**. Missing real system services produce expected GNOME warnings on the isolated bus; no host service was enabled to silence them.

## Executed instances and evidence

| Instance | Observed outcome |
| --- | --- |
| `/tmp/gfs-wayland-qa.557a8gpg` | GNOME created its Wayland socket, but Media Session 0.4.1 prefixed an absolute `-c` argument with `MEDIA_SESSION_CONFIG_DIR` again. Corrected the harness to pass `media-session.conf` as a basename. All registered children exited; cleanup complete. |
| `/tmp/gfs-wayland-qa.pltsmnva` | Full environment ready on private Xvfb `:99`. Portal capabilities: source types `3`, cursor modes `7`. Real fixture and app visible. Exposed the dropped-runtime connection bug, then normal chooser Cancel/Share worked with a newly built debug app. Exposed actual SPA format negotiation failure. Reached its original 1200-second lifetime and cleaned up all registered children and its extra app. |
| `/tmp/gfs-wayland-qa.69wjziev` | Fresh private instance with frozen debug binary, private Fontconfig and bounded software-renderer thread counts. Normal window chooser appeared. Outer supervisor received a termination signal and requested clean stop; signal sender was not captured. All registered children exited. Do not attribute this signal to another agent without evidence. |
| `/tmp/gfs-wayland-qa.yovlpt81` | 3600-second instance, with the outer supervisor signal-traced into `/tmp/gfs-wayland-signals.5luxUa/outer.trace`. Real chooser visibly selected the uniquely titled fixture; Share exposed the fixed-choice decoding bug. Both SPA fixes delivered a real first frame and a 197×148 crop at 63,103. Attempting the controller left the old preview visible; the later diagnosis below establishes an early error, not a GPU deadlock. At approximately 2026-09-07 00:14, its configured lifetime ended; main and extra-app supervisors and execution handles completed cleanup. The signal trace remained empty. |
| `/tmp/gfs-wayland-qa.mv0g8aqa` | Fresh 1800-second instance, frozen no-resize SHA-256 `7ce8231379a589d83d920d4cad54e2d865b1d813c7a1960e62e39a34bba4d484`, default renderer, app PID 604965. Cancel, reauthorization, first frame and selection passed. Internal tracing proved controller opening returned early because Wayland has no global viewport origin. The final corrected build was reached just before the configured deadline; stale-session guards rejected subsequent clicks. Normal teardown and cleanup complete were verified before the next launch. |
| `/tmp/gfs-wayland-qa.tqzedhq5` | Fresh 14400-second bounded lab with the same isolation, private Xvfb `:99`, supervisor 663781. Frozen SHA-256 `28b2cca80a7bd167c4830784932ecc6c3befbe850bbbe8f095e7594112ec7522` includes local-viewport controller repair and VideoCrop normalization, using the unchanged default renderer. Window recording/export, resized-source recovery, application cancellation and manual snapshots passed. At 01:26:02 the outer supervisor received SIGTERM and requested normal cleanup, before the 4-hour deadline. All main/extra-app handles finished; status and extra-app records report cleanup complete. The signal sender was not captured. |
| `/tmp/gfs-wayland-qa.rfs2o6g2` | Fresh 14400-second lab created only after every previous handle was terminal. Same isolated configuration, private Xvfb `:99`, supervisor 704710, frozen SHA-256 `5aef5a720eab5a368c9783d2316422aea43e1421d4b6abfd96529d741ec48365`. Monitor recording, live crop movement, exact 20-second automatic Stop, GUI export, independent GIF decode, scoped Discard and title-bar Close-save passed. The original execution handle was periodically polled as well as checking status. After testing, an explicit scoped stop completed with launcher exit 0 and `cleanup_complete: true`; no lab was left running. |

Each lab retains `instance.json`, `launcher.json`, `status.json`, `children.json`, private connection metadata, startup properties, graph dump and bounded per-process logs. New instances freeze a copy of the selected executable under `bin/` and record its SHA-256; repository revision alone must not be mistaken for proof that an older binary contains uncommitted fixes.

The first ready instance's portal backend also observed the legacy user font directory through the system Fontconfig configuration. There was no home rewrite, but this was weaker read isolation than intended. Subsequent harness instances use an explicit private Fontconfig file that includes only system fonts and private/read-only system cache locations.

## Product defects found with the real stack

### 1. Catalog probing left ashpd's shared connection on a dropped runtime

The old release executable could enumerate portal sources, then hung indefinitely in source preparation without showing the trusted chooser. Its app-level Cancel only changed the notice to cancellation requested. The environment and portal properties remained responsive.

Root diagnosis: ashpd 0.13.13 caches a session connection, while the application previously created and dropped independent current-thread Tokio runtimes during probing. Retaining an owned fresh connection/proxy/runtime, with bounded probing and close, corrected this path (`bec43eb`). On the same private bus, **six real portal connections across dropped probe runtimes passed**. A newly built debug app immediately displayed the normal GNOME chooser, and clicking its native Cancel returned to recorder settings with the output directory still empty.

Evidence: first ready lab's `logs/new-portal-chooser.png`, `logs/portal-cancel-result.png`, and `logs/cancel-preparation.png` for the former stalled app. Native chooser Cancel passing alone does not prove programmatic cancellation; that path now has an additional real-stack check below.

On the still-live `yovlpt81` lab, `GFS_ISOLATED_WAYLAND_TEST=1` and the ignored native test `cancelling_real_pending_chooser_does_not_require_a_permission_response` passed in **0.51 seconds**, with no pointer/keyboard input or Share response. The GNOME backend logged handling the pending request at 23:42:52. After cancellation, `logs/native-cancel-clean.png` showed no chooser; read-only Mutter introspection still showed only the pre-existing Session/u2 and Stream/u2, and PipeWire nodes remained 23 (dummy), 29 (existing source) and 32 (existing consumer). No additional session or capture survived this test. End-to-end application-button cancellation still needs a fresh GUI test after the controller repair.

### 2. Preferred BGRx was omitted from the actual SPA enum members

The real PipeWire daemon reported `no more input formats`: the consumer serialized BGRx only as its preferred/default value, followed by BGRA/RGBA/RGBx alternatives. Native SPA skips the default slot when matching enum members. Adding BGRx to the alternatives allows negotiation with Mutter's BGRx offer.

`crates/capture-linux/tests/native_format_filter.c` is a test-only probe compiled against the installed SPA headers. It runs the actual `spa_pod_filter`, fixes the result, and checks Mutter-style 1280×720, nominal rate 0/1, maximum rate 60/1. The Rust regression verifies:

- BGRx, BGRA, RGBA and RGBx each negotiate;
- unoffered I420 is rejected;
- reconstructing the former missing-default-member offer reproduces the native rejection.

This is not just serialization round-tripping through our own parser. Required native compiler/header prerequisites are not silently skipped.

### 3. Native fixed scalar values can retain `Choice(None)` wrappers

After the enum correction, GUI Share no longer produced the PipeWire negotiation error. The application instead reported `non-raw-video PipeWire format`. Returning the **actual C-filtered and fixed POD** to the Rust parser reproduced the same failure in the regression.

The parser now accepts fixed `Choice(None)` wrappers around media type, subtype, pixel format and dimensions, as well as direct scalar values. It still rejects unresolved enum/range offers. The native interoperability regression passes for all four supported formats. Live retest with the frozen app SHA-256 `187ba3c1cf2844aaf2cc83cdcd26e50e4b921a4a7a714458a275f32dc2ad916b` delivered the fixture's colored quadrants and counter `006004`; the user-visible frozen preview reported 1280×720. `logs/parser-fixed-share.png` records this result.

Evidence: current lab's `logs/window-selected.png` shows the normal chooser's checked fixture row; `logs/share-failure.png` records the parser failure after Share. The source chooser was not forged or replaced.

### 4. Recorder controls below a 720-pixel viewport

New cursor/input explanatory rows pushed the start control and error status below the visible recorder page at 1280×720. The page now has a vertical scroll area. Scrolling in the real nested desktop exposed both `Open recorder frame` and the native error notice. This usability fix has live UI evidence, independent of the still-pending capture acceptance.

### 5. Controller opening required a global origin that Wayland does not expose

The visual selector accepted a 197×148 region at source-local 63,103. Clicking `Open source-local recorder controller` left the prepared preview visible, with no usable controls. `logs/crop-selected.png`, `logs/controller-opened.png`, `logs/controller-alt-tab.png` and `logs/controller-overview.png` preserve that symptom. Early debugger samples happened to show GPU presentation calls. They **do not establish a GPU deadlock**, and the earlier driver-hang hypothesis is withdrawn.

On 2026-09-07 at approximately 00:03–00:08, a frozen dedicated-root-controller variant was also tested in the same lab with `WGPU_BACKEND=opengl`: SHA-256 `24201744deb18893e216eddf94eab792bce74160377a1ce6b0999a686977c2bf`, app 584059 / helper 584048. Its mappings included Mesa EGL/swrast and no libvulkan. Chooser, first frame and selection worked, but Start did not appear. This did not fix the application error and is not a renderer recommendation.

Read-only debugger snapshots were detached after inspection; `logs/gl-controller-gdb.txt` and the GL screenshots remain historical observations, not a causal diagnosis. No system ptrace setting or global renderer policy was changed. Those attempts never reached recording.

A later no-resize controller variant (SHA-256 `7ce8231379a589d83d920d4cad54e2d865b1d813c7a1960e62e39a34bba4d484`, helper 598495) was copied and launched just before the lab expired. Its first GUI command was rejected by the stale-lab guard after normal teardown. Therefore the no-resize variant has **no completed GUI acceptance result** in this instance. Both it and the GL helper reported cleanup complete; a fresh isolated lab is required for further testing.

The subsequent `mv0g8aqa` instance supplied decisive internal evidence: the handler was entered but controller state was not installed. `viewport.inner_rect` was `None` because its global origin is unavailable on Wayland. The error returned early and the prepared page did not show the notice. The repair uses `input.screen_rect().size()` for local controller layout and displays preparation notices. It retains one dedicated root surface on Wayland without visibility/position assumptions; X11 retains its separate physical recorder frame. Regression tests cover missing global coordinates, root-only layout, pause/resume/Stop ordering and close-time project preservation.

In `tqzedhq5`, `logs/working-controller.png` shows the repaired controller with Start/Cancel and other pages hidden. `logs/recording-start.png`, `logs/recording-active.png`, `logs/paused-retarget-confirmed.png`, `logs/resumed-green.png`, `logs/live-retarget-gold.png` and `logs/stopping.png` prove the completed workflow using the default renderer. No GPU switch was needed.

### 6. Valid window pixels are smaller than the negotiated buffer

The same normally authorized fixture session exposes `org.gnome.Mutter.ScreenCast.Stream.Parameters` with size 1280×720. The captured window occupies only part of that black-backed buffer. This does not justify treating the entire padded buffer as the window's content.

The saved pre-fix graph (`logs/pipewire-window-stream.json`) shows producer Port 30 advertising `VideoCrop` metadata of size 16, while consumer Port 33 requested only `Busy`. Both negotiated BGRx 1280×720, nominal frame rate 0/1 and maximum 60/1. The former consumer neither requested nor decoded `SPA_META_VideoCrop`.

Mutter 42.9 intentionally allocates a monitor-sized window stream to accommodate resizing; its window source separately supplies current window-buffer bounds intersected with the stream rectangle through VideoCrop. [Window stream sizing](https://raw.githubusercontent.com/GNOME/mutter/42.9/src/backends/meta-screen-cast-window-stream.c), [VideoCrop calculation](https://raw.githubusercontent.com/GNOME/mutter/42.9/src/backends/meta-screen-cast-window-stream-src.c)

The consumer now requests `SPA_META_VideoCrop` before buffer allocation for the caller-validated Window source kind. A scoped native buffer guard copies and validates native metadata, including table bounds, duplicate entries, payload size, alignment and signed coordinates, and returns the buffer exactly once. Attached zero-size content is pending, never a fallback to padding. Truly absent metadata uses full transport bounds; malformed or out-of-bounds attached rectangles fail explicitly. Monitor sources do not mistake unused zero crop slots for empty windows.

Source/output dimensions become ready only after the first valid owned content frame is successfully delivered. User crops are effective-content-local, then translated into transport coordinates only for pixel copying; authoring provenance stays content-local. Same-size content may move within a buffer. A later content-size change stops fixed-canvas capture with a source-lost error and requires a new recording rather than silently resizing/scaling or exposing padding. Copying retains stride/buffer validation and startup cancellation checks between rows.

The frozen `tqzedhq5` app's actual first window preview is **692×509**, not 1280×720 (`logs/normalized-first-frame.png`). This includes the fixture's 640×420 GTK client plus legitimate client-side decorations/shadows; those are not transport padding. The subsequent project contains actual cropped pixels, verified below. Native and pure geometry fixture tests cover absent/zero/partial-zero/negative/overflow/out-of-bounds metadata, offsets, changing dimensions, padded strides, first-frame initialization and cancellation. Capture tests with **all native features** passed: **89 passed, 3 explicitly isolated tests ignored**; strict all-feature Clippy passed.

### 7. Embedded cursor policy must survive without separate cursor metadata

The first successful 419-frame project honestly retains a discovered omission: its frame metadata says `cursor_embedded: false` although Embedded was requested. It has not been rewritten. A subsequent patch carries the effective negotiated Embedded/Hidden policy into every delivered native frame even without a separately decoded cursor image. Automatic selects the supported effective policy first. Regression tests combine VideoCrop pixel/size/offset handoff with Automatic, explicit Embedded and Hidden. The flag prevents adding a second cursor; it does **not** assert that a pointer is visible in that frame.

A fresh saved-project retest caught a second omission: `FixedCropSession` copied this policy only when a separate cursor image existed. Its metadata bridge now copies the policy unconditionally; a no-image/no-position regression checks both Embedded and Hidden through cropping. All six fixed-crop tests and strict desktop Clippy passed. The first two native projects retain their historical false flags. A third normally authorized native session with the complete bridge fix produced `manual-cursor.gfsproj`: all three saved frames now correctly contain `cursor_embedded: true`, with null separate cursor assets. This closes the end-to-end policy regression.

## Successful native window project and GIF

The repaired app recorded the normally authorized fixture through the visible controller:

- A 3-second countdown, then continuous 10 FPS recording into fixed 181×141 output.
- Pause confirmed before moving the crop from (60,111) to (396,111), then Resume.
- While recording, another drag moved the same-sized crop to (396,325).
- Stop returned to the editor, retaining 419 frames in `tqzedhq5/output/normalized-window.gfsproj`.
- The GUI's Export GIF action produced `tqzedhq5/output/normalized-window.gif`, 898 bytes, confirmed by `logs/export-completed.png`.

Read-only manifest inspection found 134, 182 and 103 frames at those three origins, respectively. Their immutable RGBA assets contain the intended red, green and gold fixture regions. The project duration is 41,879,732 microseconds. Across the pause/resume origin boundary, capture timestamps differ by only 63,240 microseconds; the maximum frame duration anywhere is 107,435 microseconds. The real pause is excluded, not encoded as a long hold.

Independent ImageMagick decoding found three 181×141 GIF images after duplicate-frame merging. The colors include red `(209,56,61)`, green `(30,143,79)` with the fixture's white marker, and gold `(232,176,40)` with the fixture's dark text strip. Delays are 13.37, 18.21 and 10.30 seconds: total **41.88 seconds**, consistent with GIF centisecond quantization. No controller or transport padding appears in these selected regions. The GIF and manifest are both mode 0600. This is a real authorized PipeWire capture and GUI export, not generated substitute pixels.

## Follow-up recovery and application cancellation

Frozen app SHA-256 `48389ea98977282e23b50a261c8cd07616bb7f03f5208a124ed102d007f7a62a`, helper 684769 in the same lab, reopened the original 419-frame project through the GUI (`logs/reopened-project.png`). With a new trusted chooser still pending and no selected/granted source, clicking the application's own **Cancel preparation** button dismissed the native chooser and returned to settings with an explicit session-closed notice (`logs/app-cancel-pending-chooser.png`). Neither native Cancel nor Share was clicked for this cancellation.

After normal reauthorization, full-window capture started at 692×509. Dragging the real fixture's resize corner stopped capture explicitly with `window content dimensions changed during fixed-canvas capture; start a new recording`. The UI retained the autosave path (`logs/source-lost-retained.png`). GUI Open project replayed the recovery journal without lock takeover and restored **209 frames** (`logs/recovered-after-size-change.png`); Save checkpoint then persisted a 692×509, 21.014222-second manifest. Read-only RGBA inspection of its first asset confirms all four fixture colors at interior sample coordinates, including blue `(38,87,209)`, with no transport-size substitution. The fixed content size is preserved and no newly resized frame is appended. A stale progress caption still said `Capturing` beside the explicit failure and has been reported for UI correction.

Frozen app SHA-256 `5aef5a720eab5a368c9783d2316422aea43e1421d4b6abfd96529d741ec48365`, helper 697694, then authorized the resized fixture normally. Its fresh effective source measured 799×566; the selected output was 200×150. Manual mode stayed at zero saved frames after Start/countdown. Two explicit snapshot clicks stored two identical red frames (one immutable shared asset, two distinct clips). Pause allowed moving the fixed selection from (100,100) to (485,100); Resume alone left the count at two. A third click captured the green region, and Stop restored an editor with exactly three clips. `logs/manual-zero-frames.png`, `logs/manual-resumed-no-frame.png` and `logs/manual-stopped-three.png` preserve the states. The manifest retains click-interval durations 21.311877 and 11.603824 seconds, followed by the configured 100 ms last-frame duration, and all three Embedded policy flags are true.

The original app and helper 684769 were closed normally through the GUI. The final lab's outer SIGTERM request contains `parent=218754`, identifying the then-current Codex host as its parent process, **not identifying the signal sender**. The supervisor respected the signal and stopped only its own registered children. Main and final helper execution handles completed and all cleanup records were verified; the monitor step was rejected by the stale-lab guard before it could use obsolete connection details.

## Native monitor movement, real occlusion and scoped Discard

The fresh `rfs2o6g2` lab used the trusted monitor chooser's selected **Built-in display** and normal Share. The first effective source was 1280×720, correctly unaffected by the Window-only VideoCrop handling. A 200×150 region started at (400,200), then was dragged during recording to (700,200). Normal Alt+Tab brought the fixture or the controller to the foreground; it did not grant permissions or change the capture source.

Automatic Stop saved exactly **200 frames and 20,000,000 microseconds**: 40 frames at the first origin and 160 at the second. Every saved frame has Embedded policy true. The first frame contains the controller's dark pixels and part of its orange rectangle; frame 21 contains the fixture's red region, and the final frame's center is the green fixture color `(30,143,79)`. Thus the monitor path records real desktop occlusion. The controller is **not magically excluded from a monitor capture**; the current large root controller can cover the region. This needs prominent UI guidance and a compact-control option, not a claim of transparent physical-border parity on Wayland. Window-source capture is preferable when the intended content is one window.

Evidence: `logs/monitor-chooser.png`, `logs/monitor-prepared.png`, `logs/monitor-live-retarget.png`, `logs/monitor-fixture-foreground.png` and `logs/monitor-auto-stopped.png`. GUI Export produced `output/monitor-movement.gif`, **22,605 bytes** (`logs/monitor-export-complete.png`). Independent ImageMagick decoding found 66 images, each 200×150, with delays totaling exactly 2000 centiseconds. These preserve the occluded and unobscured portions rather than replacing them with a frozen or fabricated source image. GIF and project manifest are mode 0600.

A second normally authorized monitor recording wrote `discard-confirm.gfsproj` with real assets/journal. After 135 captured frames, clicking the explicit Discard control removed only this session's autosave directory and did not produce a GIF (`logs/discard-before.png`, `logs/discard-after.png`). The previous monitor GIF and manifest SHA-256 values remained unchanged: `7511a8e0dd0b29df3f1a296840655643756232a912e045d3dab603af4e9a88d2` and `ea2085a199cb656dcd8b79c569f07c3e391c624b145435ee5ca9d8a69eadc5d3`. This deliberately discarded test recording is not recoverable through the application. Read-only post-Discard Mutter introspection showed no Session/Stream children; the private PipeWire graph contained only Dummy-Driver node 23 and no device/capture nodes.

A third normally authorized monitor session tested the actual title-bar Close button while recording. Clicking the root window's top-right close control stopped and saved instead of discarding or quitting: the same app returned to its editor with `controller-close.gfsproj`, **14 frames, 200×150 and 1.400513 seconds** (`logs/close-save-recording.png`, `logs/close-save-returned.png`). This provides native evidence for the close-preserves-project regression as distinct from the explicit Discard path.

## Remaining acceptance gates

- Window preview normalization, controller opening, continuous recording, fixed-size movement while recording and paused, active timing, Stop and independently decoded GUI GIF export passed.
- Saved-project reopen, native source-size-change recovery, manual snapshot lifecycle and explicit scoped Discard passed; additional physical source-loss paths remain separate tests.
- Embedded cursor provenance passed in a new normally authorized, cropped native project; the older fixtures have not been retrofitted.
- The native pending-chooser test and complete application Cancel button path passed without a permission response; continue to preserve the no-residual-session boundary.
- Nested monitor authorization, crop movement, real controller overlap, timed Stop and independently decoded export passed. Add clear monitor-occlusion guidance; no automatic self-exclusion is claimed.
- Teardown of all recorded labs and their extra-app helpers passed, including the explicit final stop of `rfs2o6g2`. No recorded lab is still live.
- Physical GNOME/KDE, multiple monitors, mixed DPI, real GPU/DMA-BUF and release-environment acceptance remain separate gates.
