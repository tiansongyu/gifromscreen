# Next Linux fidelity and usability work

This list retains specific gaps after the native GNOME recording acceptance. It does not replace the full [feature matrix](FEATURE_MATRIX.md) or [release gates](LINUX-STATUS.md).

## Playback timing: implemented, bounded native manual acceptance recorded

`PlaybackTiming::Measured` and `PlaybackTiming::Fixed(Duration)` now separate GIF playback from sampling cadence in the workflow. Fixed mode assigns one final delay to each retained frame, including the last; omitted unchanged samples and delivery gaps do not accumulate fixed delay. Raw active capture timestamps and input events remain unchanged. Status distinguishes observed source-sample span from GIF playback duration. The pinned upstream evidence, save-failure exception and measured-mode interval-assignment difference are recorded in [Capture playback timing](CAPTURE-PLAYBACK-TIMING.md).

The Linux recording form now exposes the policy independently of cadence: manual defaults to 1,000 ms per retained snapshot, periodic capture to 66 ms per retained frame, and per-second capture remains measured unless fixed playback rate is enabled. Fixed per-second delay uses the reference recorder's integer `1,000 / FPS` milliseconds. Measured timing remains an explicit alternative; in that mode the manual delay field is correctly labeled as the final-frame delay only.

Automated evidence: `crates/application/tests/fixed_playback.rs` passes all four tests on Rust 1.98.0 and 1.88.0, exercising 18 real project stores and GIF exports across batch and incremental routes. Coverage includes widely spaced/identical manual snapshots, pause/resume, minute/hour sampling, fixed-FPS delivery gaps, unchanged and input-only samples, measured timing, raw metadata, journal recovery and decoded GIF duration. Three 66 ms project frames remain 198 ms before export and cumulatively quantize to 200 ms in GIF.

Separate [native fixed-playback acceptance](NATIVE-PLAYBACK-QA-2026-09-07.md) now covers the new 1,000 ms manual policy in private nested GNOME: three widely spaced snapshots, identical first two frames, pause/crop movement, resume without an extra frame, stop and CLI reopen/export. The project stores three 1 s delays with nonuniform original sample timestamps and one retained clock identity; GIF duplicate merging produces a 2 s red image followed by a 1 s green image. Earlier measured-only evidence is not used to establish this result.

The same [native acceptance](NATIVE-PLAYBACK-QA-2026-09-07.md) now also covers 2-second periodic sampling, fixed 66 ms playback, timed stop, unchanged-sample omission and decoded GIF cumulative quantization. A prepared-page clipping defect found at 1280×720 was repaired and retested at default and enlarged zoom.

Still open: native fixed-FPS timing and source-interruption acceptance, hardware/compositor coverage and remaining release gates. Synthetic timestamps are already pause-excluded; the integration tests alone do not establish native clock production, physical-device timing or a hardware FPS guarantee. A separate native visible-versus-occluded experiment shows the selected GTK window resumes pixel updates when brought in front, in the same recording session (100 clips, 72 distinct assets, 10-second GIF). The controller explains this visibility limit and now uses a compact, zoom-aware window, with [native restoration and control acceptance](COMPACT-CONTROLLER-QA-2026-09-07.md). Do not equate increasing timestamps with source animation freshness or promise to force hidden applications to repaint. The bounded nested results do not close broader desktop gates.

## Capture-clock identity: implemented, compatibility and fidelity gates remain

`FrameClip.capture_clock` now stores an optional confirmed identity and source sampling timestamp separately from immutable raw input. New recording writers/batch recordings receive independent identities. Save As, clipboard copies and project insertion preserve source context; retiming does not turn destination timeline positions into native sampling times. Key/click history is cleared at unknown/different identities, backwards/repeated source times, authoring gaps and unusable input bindings. Timestamp order, coordinates and asset hashes are not guessed session identities.

Legacy source sampling times are frozen before moving/copying clips without changing raw events. Coordinate confirmation and a shared-clock declaration are separate choices. A declaration gives each selected continuous unknown-clock interval its own identity, never merges existing known identities, and combines field-only clock/binding commands in one undoable edit. When original timing is uncertain, retain per-frame or manual annotations rather than asserting a common clock. Optional-field/schema behavior is documented in [Persistent annotation authoring scope](ANNOTATION-AUTHORING-SCOPE.md#capture-clock-persistence-and-schema-compatibility).

Automated application evidence confirms stable identities within one recording, independent identities between new projects even with matching labels/project IDs/timestamps, unchanged raw events, journal recovery and reopen. Native confirmation/re-edit acceptance and the full cross-edit regression gates remain separate; this status does not imply that all scope or platform bugs are closed. Existing timed tracks retain their old semantics. The new frame-owned core preserves run identity and marks through structural edits, but its ordinary authoring and replay-context work below is unfinished.

## Frame-owned authoring: core ready, editor creation and regeneration pending

Schema 2, the shared frame-aware render plan, bounded frame bundles, Save As and Cinemagraph consumption are implemented and tested. Reverse/retime do not move a mark to a different owner; copying an event-empty frame keeps its frozen held label without needing its old predecessor. Hidden assets and marks remain protected. See [the chosen representation and acceptance cases](FRAME-OWNED-OVERLAYS-PLAN.md).

The next step is connecting normal text/shape/input/progress authoring and explicit legacy conversion. Input-aware re-authoring needs bounded, run-isolated replay context with original delivery sample times, releases and modifier transitions; insufficient context must preserve the old group atomically. The editor currently rejects frame-owned regeneration explicitly instead of falling through the legacy empty-scope path. This is an incomplete feature, not completed one-to-one parity. Whole-frame geometry behavior after transforms still needs separate acceptance.

## Monitor controller and remaining platform acceptance

The native monitor test proves that a controller overlapping the crop appears in the GIF. The app cannot promise automatic self-exclusion on a general Wayland monitor source. The visible warning remains, alongside the implemented 720×480 compact controller and responsive controls. Global-shortcut integration and further timed-capture fallbacks remain work; compact sizing is not a physical-position or self-exclusion guarantee.

Nested GNOME software-rendered tests do not replace KDE, hardware rendering, mixed DPI/multiple monitors, device interruption, physical camera or full-resolution long-duration acceptance. Ordinary authoring is still time-anchored until the new core is fully connected; this remains editor fidelity work.
