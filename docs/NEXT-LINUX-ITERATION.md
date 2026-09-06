# Next Linux fidelity and usability work

This list retains specific gaps after the native GNOME recording acceptance. It does not replace the full [feature matrix](FEATURE_MATRIX.md) or [release gates](LINUX-STATUS.md).

## Separate capture sampling from GIF playback timing

The real manual-snapshot test verifies capture control and the current implementation's measured timing; it does **not** prove ScreenToGif manual playback parity. In pinned ScreenToGif 2.43.2, `BaseScreenRecorder.HasFixedDelay/GetFixedDelay` makes manual, per-minute and per-hour captures use a separately configured fixed playback delay. Defaults in `Resources/Settings.xaml` are 1000 ms for manual and 66 ms for minute/hour playback. Our current form explicitly sets only the final manual frame delay, while earlier frames use active time between clicks.

Implement an explicit playback timing policy independently of capture cadence. Preserve raw active capture timestamps and event clocks; fixed playback must not rewrite those timestamps. Expose fixed frame delay for manual/timelapse use, retain elapsed-time playback as an explicit alternative, and distinguish capture elapsed time from GIF duration in status. Check upstream duplicate/changed-frame handling before defining fixed-delay accumulation; do not silently infer it from the existing elapsed-time collector.

Acceptance: three widely separated manual clicks can produce three configured equal playback delays; pause and periodic sampling do not add unintended GIF waits; incremental and batch projects agree; export quantization and undo/reopen preserve the chosen timing policy.

Primary reference: [BaseScreenRecorder](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/BaseScreenRecorder.cs), [fixed defaults](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Resources/Settings.xaml).

## Preserve independent capture-clock identities

Increasing timestamps alone cannot establish that two inserted original recordings share a clock. Add a frame-owned capture clock context, separate from immutable raw input, containing an optional confirmed sequence identity and a source sampling timestamp. Only matching nonempty identities may carry a key/click hold between frames. New recording runs get independent identities; pause and crop movement keep them. Save As, clipboard copies and project insertion preserve the source context, not the destination project's identity.

Legacy migration must freeze valid source sampling times before moving a clip, without modifying raw events. A legacy coordinate confirmation is not automatically proof of a common recording clock; require an explicit interval declaration, do not merge existing distinct identities, and preserve a per-frame/manual fallback where the original time relationship is unknown. A field-only clock command can be combined with binding confirmation in one undoable edit.

Acceptance: separate recordings with nearby increasing times never exchange held labels; same-session event-empty frames still inherit them; old clips moved earlier/later keep their original event timing; undo/reopen and copying preserve identity. See the [current scope boundary](ANNOTATION-AUTHORING-SCOPE.md).

## Monitor controller and remaining platform acceptance

The native monitor test proves that a controller overlapping the crop appears in the GIF. The app cannot promise automatic self-exclusion on a general Wayland monitor source. A visible warning and window-source recommendation are required. Further work should provide a compact controller and appropriate global-shortcut/timed-capture fallbacks, so monitor recording remains usable without misleading physical-position or exclusion promises.

Nested GNOME software-rendered tests do not replace KDE, hardware rendering, mixed DPI/multiple monitors, device interruption, physical camera or full-resolution long-duration acceptance. Overlay reordering is still time-anchored rather than upstream's frame-baked behavior; this remains a separate editor fidelity task.
