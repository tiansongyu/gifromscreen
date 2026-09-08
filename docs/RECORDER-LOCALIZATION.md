# Recorder language integration

The main-branch English/Simplified Chinese message set now includes the recording
form, cadence/playback/retention/cursor settings, preview region selection,
Wayland preparation/controller, and compact X11 controls/window snapping.
This is a continuation of the [first language slice](LOCALIZATION-QA-2026-09-08.md),
not a claim that every application screen or all 29 languages are translated.
The published Preview 1 predates this recorder-text integration; its download
and historical demonstration remain pinned to their original source.

## Contracts retained

- `Localizer` is an explicit view argument. No renderer calls `setlocale`, reads
  environment variables, changes source IDs, rewrites paths/window names or
  stores translated enum values in projects/settings.
- Static messages use stable typed IDs. Counts, negative coordinates, dimensions,
  durations and external diagnostics are named arguments. Numeric display keeps
  the previous precision and numeric-input contract; playback is not conflated
  with sampling time. Workflow phase/source-kind display uses enum matching,
  not parsing or replacing English debug strings.
- The X11 short Start/Pause/Resume/Stop/Discard labels retain their original
  English wording and action mapping. Stable IDs and the native claimed-button
  geometry handshake remain independent of translated labels.
- Both invisible measurement and visible painting of the Wayland control bar
  receive the same localizer. Primary controls remain before secondary content
  and can scroll within their bounded panel on small windows.
- When the actual catalog changes, unfinished preview-space gestures are
  cancelled before painting. Confirmed regions, source dimensions, settings,
  capture state, native PPP and user zoom are untouched. Two unavailable choices
  both falling back to English do not spuriously cancel a gesture.
- Window-snap notices retain typed message/argument identity and are translated
  when painted, including already completed work. Native diagnostics are literal.
  Other existing app-wide `Option<String>` notices are only localized at creation
  where migrated; their general live-switch conversion is still open.

## Automated acceptance

The current catalog contains 283 en/zh messages, with complete parameter parity
and explicit fallback for other languages. This denominator is the implemented
message set, not the number of all application strings.

The dedicated recorder-localization tests cover all Chinese template characters
through egui's actual proportional/monospace fonts, preparation-button stable
IDs and unchanged settings, confirmed-region/PPP/zoom preservation across
languages and recorder states, and cancelling only unfinished preview gestures.
Their 55 Chinese button/size/font combinations validate painted primary-button
rectangles and actual clicks, including a 320 × 240 viewport and enlarged text.
These tests pass on Rust 1.88.0 and 1.98.0. X11 controls and snap workflows retain
their existing English regressions and add Chinese/state/geometry checks.

Final automated cohort: 694 desktop tests and 29 localization tests pass on both
Rust 1.88.0 and 1.98.0. The full Rust 1.98 all-target/all-feature workspace passes
1,627 tests, with 51 explicitly ignored environment/benchmark gates. Strict
Rust 1.98 Clippy and formatting pass. The older Rust 1.88 strict-Clippy run still
stops at the pre-existing source-over `needless_range_loop` in `renderer.rs`;
passing MSRV compile/tests is not reported as passing that separate lint run.
The X11 controller's 15 tests and window-snap's 26 tests include seven new Chinese,
zoom, numeric-input, ready/claimed-button and stale-result regressions.

This is not native desktop, physical GPU, multi-monitor, RTL or IME acceptance.
Native observations and the final cohort totals are recorded after running the
corresponding executable, not inferred from an embedded-font cmap or a screenshot
of the earlier preview release.

## Remaining language coverage

The shortcut configuration/registration panel, several retarget/snapshot and
backend failure notices, other import/camera/board/editor/export/tool screens,
and the other 27 catalogs still require migration and validation. Complex-script
shaping, BiDi, logical/visual cursor movement and IME remain separate release
gates. The [full localization plan](LOCALIZATION-PLAN.md) is unchanged in scope.

## Native follow-up findings

The first Chinese Wayland run of commit `904a5ae` prepared a source, applied
the exact (50,100), 200 × 150 crop, recorded 30 frames for a 3-second timed stop,
and reopened/exported a 3,000 ms GIF. It also exposed a real localized-notice bug:
the countdown's named argument `seconds` had accidentally been translated to the
display label for that unit. The toolbar countdown still worked, but the separate
notice showed `Unknown message argument`. The argument key is now a literal;
an actual `begin_recording` regression checks the resulting notice in both en/zh.
This is why generic catalog tests alone are not full call-site acceptance.

The source drop-down exposed another omission: the two synthetic Portal chooser
entries have application-owned English descriptions. A presentation-only adapter
now recognizes precisely Wayland + no geometry + the backend's known ID and
matching kind, and translates those two prompts. Real window/source names and
descriptors remain untouched. Tests use the backend's public source factory and
reject relabeling for other backends, unknown IDs, mismatched kinds or geometry.
The message set now has 285 entries; these follow-up corrections are newer than
the initial 283-message cohort above and do not rewrite its native evidence.

After the follow-up, 701 desktop tests and 30 localization tests pass on Rust
1.88/1.98; the complete Rust 1.98 workspace passes 1,635 tests with 51 explicitly
ignored gates. Strict Rust 1.98 Clippy and formatting pass. The new tests include
six synthetic-Portal-label identity cases and the real countdown-entry call path.

### Native X11 after the fix

Commit `3689ddb`, debug executable SHA-256
`ec963746d4dbd619f7d019006128193207915fc7e06e8a81498f071b111a5eff`,
private GNOME/Xvfb lab `/tmp/gfs-wayland-qa.rkrcoxxh` (1440 × 1000), System
locale `zh_CN.UTF-8`/`LANGUAGE=zh`:

- Actual settings and the independent recorder panel render Chinese. Confirmed
  crop is 320 × 220 at (100,230); normal pages are hidden during recording.
- `03-countdown-fixed.png` shows the Chinese countdown and initial-countdown
  notice, with no formatting error. After acknowledged pause, a native border
  drag moves the fixed-size crop to (280,230), followed by resume and stop.
- The project stores 46 frames: 24 at the first origin and 22 at the second,
  4,557,986 µs total, one clock `64e071c75ac74c8f9ae57b54537d718c`, revision 84.
- GUI export and a CLI re-export after normal application close are byte-identical:
  320 × 220, two coalesced GIF images, 4,560 ms, 3,090 bytes, SHA-256
  `f70f2db9eaf095ffd66b2e043208814d487876c7a84f191b27409b5222eb6448`.
  `ffprobe -min_delay 0` separately decoded/count-checked the output.
- The lab was explicitly stopped and reports `cleanup_complete: true`; its
  launcher exited 0. Screenshots and the interaction script remain in `logs/`.

The harness initially failed to resolve a full Unicode title through xdotool's
search API. It then used the verified owned process ID plus an ASCII title
prefix; the application title itself is Chinese. No failed harness lookup was
counted as a recorder action. This is an isolated software-rendered check, not
physical mixed-DPI, GPU, multi-monitor or global-shortcut certification.

### Native Wayland after the fix

The same `3689ddb` executable/hash was tested in a separate owned nested GNOME
lab `/tmp/gfs-wayland-qa.nrhi2i3h` (outer Xvfb `:100`, native Wayland app).
`06-zh-portal-options.png` shows both virtual Portal prompts in Chinese.
The trusted system Share dialog remains in the system's own language; it selected
the project-authored fixture, which supplied a 692 × 509 stream.

The prepared crop (50,100), 200 × 150 remained unchanged through the compact
controller. `14-zh-countdown-one-second.png` shows the correct Chinese countdown
and initial-countdown notice without `Unknown message argument`.
The 3-second/10-FPS run stopped automatically into a 30-frame project, revision
60, with 3,000,000 µs playback duration and one clock
`3a163785ac80441ea662016c9ec51247`. Raw sample timestamps agree with capture-clock
samples; the first 29 measured delays match adjacent samples.

After normal application exit, CLI reopen/export produced a 200 × 150 GIF,
10 encoded images, 3,000 ms, 3,210 bytes, SHA-256
`9ca2e9679fdff84cbf70e88448f66e245967b044fd673a80270088ab6eda4910`.
Pillow and `ffprobe -min_delay 0` agree on the encoded duration. The application
exited 0, its project lock was removed, and the lab's eight child processes were
confirmed gone after explicit scoped stop (`cleanup_complete: true`, launcher 0).
The local structured evidence is `logs/zh-wayland-qa-summary.json`, SHA-256
`e398a90a46aed71e50403d9606e0c795ce1f2b1588cccd031a3b08e87e0d7dce`.

The native runs validate the stated Chinese recorder paths, not complete editor
translation, physical desktop coverage, complex-script input or 29-language readiness.
