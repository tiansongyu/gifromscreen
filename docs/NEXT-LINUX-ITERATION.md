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

Automated application evidence confirms stable identities within one recording, independent identities between projects even with matching labels/IDs/timestamps, unchanged raw events and recovery. Native authoring/re-edit acceptance is now recorded below, with synthetic source events explicitly distinguished from physical capture. Existing timed tracks retain their old semantics. These results do not imply that all geometry or platform bugs are closed.

## Frame-owned authoring and ordered geometry: connected; specific fidelity gaps remain

Schema 2, the shared frame-aware render plan, bounded frame bundles, Save As and Cinemagraph consumption are implemented and tested. Reverse/retime do not move a mark to a different owner; copying an event-empty frame keeps its frozen held label without needing its old predecessor. Hidden assets and marks remain protected. See [the chosen representation and acceptance cases](FRAME-OWNED-OVERLAYS-PLAN.md).

Normal text, shapes, drawings, watermarks, titles and progress/input authors now create frame-owned marks. Key/click re-authoring uses bounded shared source-input pools and prefixes, including after copying an event-empty held-label frame and deleting the source. Legacy conversion is explicit, whole-track, bounded and undoable; it preserves old progress pixels without inventing unknown scope or history. Duplicate group selectors now identify layer number and coverage. See [native authoring/reopen/export evidence](FRAME-AUTHORING-QA-2026-09-07.md).

Schema 3 now implements ordered complete-image crop, resize, rotation, flips and existing effects. Earlier marks are composited before later image operations; later marks remain freshly drawn. Stage-aware text and recorded-input re-authoring use the original paint space. Crop/resize/rotation address the whole animation and update its canvas atomically. See [the contract](ORDERED-FRAME-EDITING.md) and [native resize/rotate/undo/reopen/conversion/GIF evidence](ORDERED-GEOMETRY-QA-2026-09-07.md). Shared replay pools remain history-bearing assets, not sanitized data; GIF export excludes their metadata.

Expanded image-border/shadow operations are now implemented behind schema 4, keeping old variants unchanged. Shared placement controls rendering and raw-input mapping. The new shadow follows the official WPF software Gaussian/alpha algorithm, with original hundredth-unit polar parameters, independently quantized opacity and background applied last. Mixed signed borders preserve reference scope and default white backing. See [the contract and numerical limits](EXPANDED-IMAGE-EFFECTS.md) and [native A→shadow→border→B, clear/undo/reopen/GIF evidence](IMAGE-EFFECTS-QA-2026-09-07.md). Automatic settings v2 preserves v1 actions instead of silently changing their pixels. Image programs are preflighted for intermediate RGBA memory and final GIF dimensions, including when replacing an early effect.

Schema 5 adds explicit WPF paint precision for new Normal-blend authors while retaining all old stage defaults. A real hosted WPF producer exposed premultiplication, PNG reciprocal and colored-shadow differences; the corrected Rust renderer passes all 13 inputs/stages against its independent artifacts, locally and in a fresh complete hosted run. See [the precision contract and measured evidence](WPF-PAINT-PRECISION.md). Broader numeric coverage and physical platform gates remain open.

A Windows-hosted GitHub Actions fixture job now runs the STA `UseWPF` generator through actual `RenderTargetBitmap`/PNG decode and compares canonical RGBA against the pure Rust domain/render crates. This is algorithm QA for Linux, not a Windows application. Runs remain bounded and preserve exact SDK/runtime/OS provenance; local replay success must not be substituted for the result of a fresh hosted committed-revision run.

The Rectangular freeze extension preserves hidden/zero-opacity intermediate-stage artwork through a schema-6 ordered `FreezeRegion`, instead of rejecting it or flattening away its context. The original source and prior paint stages remain intact, while one immutable baseline supplies frozen RGBA pixels. Earlier recorded groups remain re-editable; output-stage recorded-input replay stays blocked. See [the representation, resource ownership and automated coverage](NONDESTRUCTIVE-CINEMAGRAPH.md).

The [native freeze/reveal/Undo/reopen/GIF acceptance](NATIVE-FREEZE-QA-2026-09-07.md) now passes. Layers provides undoable Hide/Show controls and the reopened GIF is byte-identical. Next: broader replay and numeric corpora, freeform-mask parity where required by the reference audit, and the remaining feature-matrix/platform gates. Retaining raw metadata is still not proof that a newly authored cursor matches mixed output pixels.

The [new pinned Cinemagraph audit](CINEMAGRAPH-REFERENCE.md) confirms an additional
semantic gap: first-frame clipped ink geometry and premultiplied source-over,
not current-frame rectangular RGBA overwrite. The implemented extension is now
explicitly labeled Rectangular freeze. Preserve its saved semantics and implement
the actual Cinemagraph path separately; do not reinterpret its prior acceptance
as upstream parity. The independent clip/PNG-boundary probe now supports the
[schema-7 typed PM snapshot choice](PREMULTIPLIED-SNAPSHOTS.md): 90 native
snapshots match the storage/composition pipeline exactly. Freehand geometry
generation and authoring are now connected: pen, partial/whole-stroke erasers,
selection transforms, per-stroke fitting, cancellable atomic Apply and gapped
targets pass [bounded native Undo/reopen/GIF acceptance](NATIVE-CINEMAGRAPH-QA-2026-09-07.md).
Exact WPF outline/erasure/Boolean fidelity is still open; do not describe the
whole authoring tool as absent or the numerical work as complete.

## Next functional priorities: recorder control and precise positioning

After the connected Cinemagraph workflow, prioritize these user-facing gaps in
parallel with the bounded independent numerical checks. The existing movable
recording frame, pause-excluded clock and fixed playback policy are implemented;
these tasks extend them rather than replace their state machines.

1. **Global recording shortcuts: implementation connected.** The opt-in X11/Portal
   service, persisted bindings and recorder-scoped adapter now route to the existing
   controller. Active Stop/pause/snapshot commands reach the worker without waiting
   for UI repaint. See [the contract and backend tests](GLOBAL-SHORTCUTS.md) and
   [native Mutter start/pause/resume/snapshot/stop/GIF evidence](X11-RECORDER-WINDOWS.md).
   Actual KDE/modern-Portal, physical-key repeat and wider hardware acceptance stay
   open. Buttons and timed stop remain available if registration fails.
2. **Precise region positioning: next integration.** Separate authoritative physical
   selection, native border and responsive control panel. The old combined viewport
   cannot represent a full monitor or tiny selection independently of its controls;
   UI zoom must not change the captured dimensions. Add 1/10 physical-pixel movement and
   pre-record window snapping through `RegionRetargetPlan`. Show the backend's
   accepted rectangle, coalesce movement while one request is in flight, and
   retain fixed canvas size during recording. Optional X11 cursor-follow comes
   after those controls; Wayland source-local movement is not global tracking.
3. **X11 interaction sampling.** Connect bounded XI2 triggers to capture cadence
   without requiring sensitive key metadata persistence. Keep burst limits,
   pause exclusion, input-listener teardown and fixed GIF-delay semantics.
4. **Recover and continue from a lost source.** Preserve the existing project,
   request a new authorized source explicitly, keep the canvas or request a
   compatible crop, and use a new capture clock. Cancelled permission must not
   discard existing frames or include downtime in the GIF.
5. **Editor viewport and direct crop.** Add Fit/100%/200% and a separate bounded
   crop draft using the actual painted-image rectangle. Synchronize numeric and
   drag controls and commit through existing ordered complete-image commands.
   New interpolation choices must not reinterpret older nearest-neighbor saves.

Shortcut implementation constraints: on X11, use exact passive key grabs with
checked conflicts and rollback/unregistration, not `AnyKey`/`AnyModifier` or a
whole-keyboard grab. Lock modifiers, layout changes and autorepeat need tests.
[X.Org key-grab contract](https://xorg.freedesktop.org/archive/X11R6.7.0/doc/XGrabKey.3.html).
On Wayland, use a separately owned GlobalShortcuts Portal session, register once
per session, filter events by session/action, and display the returned bindings
(a requested trigger is not proof of the chosen binding). Detect version/support;
configuration UI requires version 2. Retain explicit cancellation/close and a
usable no-permission fallback.
[GlobalShortcuts Portal contract](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html).

Validate these in isolated X11/private-bus tests and native bounded recording,
including other-app focus, repeat, conflicts, denied/cancelled permission, stale
events after restarting capture and teardown. Physical desktop gates stay open
until separately exercised; their absence does not block this implementation work.

## Monitor controller and remaining platform acceptance

The native monitor test proves that a controller overlapping the crop appears in the GIF. The app cannot promise automatic self-exclusion on a general Wayland monitor source. The visible warning remains, alongside the implemented 720×480 compact controller and responsive controls. Global shortcuts are now connected, but native Portal acceptance and further timed-capture fallbacks remain work; compact sizing is not a physical-position or self-exclusion guarantee.

Nested GNOME software-rendered tests do not replace KDE, hardware rendering, mixed DPI/multiple monitors, device interruption, physical camera or full-resolution long-duration acceptance. Existing timed layers remain legacy until explicitly converted; the specific remaining geometry and feature-matrix items above still need work.

The separate [1,000-frame full-resolution store benchmark](FULL-RESOLUTION-STORE-QA-2026-09-07.md) now passes: 3.43 GiB persisted, journal recovery and every asset digest verified, with bounded measured memory and successful temporary-data cleanup. It is an unpaced synthetic sink test, not native capture, FPS certification, long-duration wall-clock execution or crash/power-loss injection. Do not merge those gates.
