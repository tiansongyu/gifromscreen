# Nested GNOME acceptance: execution record

Status: **in progress; capture/export acceptance is not yet complete**.

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
/usr/bin/python3 scripts/qa/wayland_nested.py start --app target/debug/gif-from-screen --seconds 3600 --startup-timeout 45
```

The launcher prints the allocated `LAB_CREATED` directory. `status LAB` never launches a replacement. `exec LAB -- COMMAND...` injects only that live lab's connection environment and limits the command to 30 seconds. `run-app LAB --app PATH --seconds 600` attaches a separately supervised app without restarting any compositor, bus or portal. `stop LAB` requests orderly cleanup of the registered children. Do not substitute a stale directory or the host display.

Current harness tests: **5 passed**. CLI parsing and Python compilation passed; generated logs/environment files were observed with mode 0600 and lab/runtime directories with mode 0700. Native graph inspection found **zero `PipeWire:Interface:Device` objects**. Missing real system services produce expected GNOME warnings on the isolated bus; no host service was enabled to silence them.

## Executed instances and evidence

| Instance | Observed outcome |
| --- | --- |
| `/tmp/gfs-wayland-qa.557a8gpg` | GNOME created its Wayland socket, but Media Session 0.4.1 prefixed an absolute `-c` argument with `MEDIA_SESSION_CONFIG_DIR` again. Corrected the harness to pass `media-session.conf` as a basename. All registered children exited; cleanup complete. |
| `/tmp/gfs-wayland-qa.pltsmnva` | Full environment ready on private Xvfb `:99`. Portal capabilities: source types `3`, cursor modes `7`. Real fixture and app visible. Exposed the dropped-runtime connection bug, then normal chooser Cancel/Share worked with a newly built debug app. Exposed actual SPA format negotiation failure. Reached its original 1200-second lifetime and cleaned up all registered children and its extra app. |
| `/tmp/gfs-wayland-qa.69wjziev` | Fresh private instance with frozen debug binary, private Fontconfig and bounded software-renderer thread counts. Normal window chooser appeared. Outer supervisor received a termination signal and requested clean stop; signal sender was not captured. All registered children exited. Do not attribute this signal to another agent without evidence. |
| `/tmp/gfs-wayland-qa.yovlpt81` | Current 3600-second instance, with the outer supervisor signal-traced into `/tmp/gfs-wayland-signals.5luxUa/outer.trace`. Real chooser visibly selected the uniquely titled fixture; Share exposed the fixed-choice decoding bug. A subsequent frozen debug app with both SPA fixes delivered a real first frame and accepted a 197×148 crop at 63,103. Opening the separate controller then stalled the app; diagnosis is ongoing. This instance is not yet a completed teardown gate. |

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

### 5. Separate controller handoff stalls before the controls appear

The visual selector accepted a 197×148 region at source-local 63,103. Clicking `Open source-local recorder controller` left the final main-window surface visible and unresponsive, with no usable separate controls. GNOME and the fixture kept updating; normal Activities overview showed the fixture and the old application surface. The process remained alive. `logs/crop-selected.png`, `logs/controller-opened.png`, `logs/controller-alt-tab.png` and `logs/controller-overview.png` preserve the boundary. Separate diagnosis collected two stacks waiting in Lavapipe presentation of the ROOT viewport; winit's Wayland visibility operation is a no-op. The root-to-child viewport handoff is being repaired and needs a fresh GUI test. This blocks recording/stop/export acceptance, not just visual polish.

### 6. Valid window pixels are smaller than the negotiated buffer

The same normally authorized fixture session exposes `org.gnome.Mutter.ScreenCast.Stream.Parameters` with size 1280×720. The captured window occupies only part of that black-backed buffer. This does not justify treating the entire padded buffer as the window's content.

The saved live graph (`logs/pipewire-window-stream.json`) shows the producer's output Port 30 advertising `VideoCrop` metadata of size 16, while consumer Port 33 requests only `Busy`. Both negotiated BGRx 1280×720, nominal frame rate 0/1 and maximum 60/1. The current consumer does not request or decode `SPA_META_VideoCrop`.

Mutter 42.9 intentionally allocates a monitor-sized window stream to accommodate window resizing; its window source separately supplies the current window-buffer bounds intersected with the stream rectangle through VideoCrop. Therefore effective-window-crop handling is a confirmed missing integration requirement, although this run has not decoded an actual per-buffer crop rectangle yet. [Window stream sizing](https://raw.githubusercontent.com/GNOME/mutter/42.9/src/backends/meta-screen-cast-window-stream.c), [VideoCrop calculation](https://raw.githubusercontent.com/GNOME/mutter/42.9/src/backends/meta-screen-cast-window-stream-src.c)

## Remaining acceptance gates

- First real fixture pixels and visual region selection passed; retain that evidence while resolving the controller handoff.
- Request/decode VideoCrop, preserve exact source-kind/size metadata and verify effective window content without treating padding as content.
- Window-source region selection, fixed-size movement while recording and paused, cadence/manual capture, active timing, stop/discard, native-source loss and recovery.
- Reopen the captured project and inspect decoded exported GIFs for intended window content and no controller recursion.
- The real native pending-chooser cancellation check passed without a permission response or residual session; verify the complete application Cancel button path too.
- Only after the window path succeeds: authorize the nested monitor and verify crop movement and intentional controller overlap behavior.
- Complete and record final teardown of the current lab, retaining bounded owner-only evidence.
- Physical GNOME/KDE, multiple monitors, mixed DPI, real GPU/DMA-BUF and release-environment acceptance remain separate gates.
