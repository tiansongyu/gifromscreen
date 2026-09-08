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
