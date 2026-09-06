# Progress and input annotations

The editor's **Progress & input annotations** section supports:

- A progress bar in all four directions, based on the full timeline's original frame count or frame-end elapsed time. **Reverse bar / percent** reverses only the bar and `{percent}`; `{remaining}` explicitly shows remaining time, while `{frame}` and `{elapsed}` remain ascending. The user's label format is never rewritten.
- Persisted labels with `{frame}`, `{frames}`, `{elapsed}`, `{total}`, `{remaining}`, and `{percent}` tokens, a chosen font, foreground/background colors, opacity, layer order, and a physical-pixel text box.
- Manual key labels and recorded key presses, including modifiers. Releases and auto-repeat events do not create labels.
- Manual click markers, recorded click markers, saved cursor images, and an explicitly selected built-in pointer for manual fallback.
- Reopening an authored annotation group, changing its settings, and updating it as one undoable edit.

## Timing and edit behavior

Creation affects the selected frames only; unselected gaps stay untouched. Input events are sampled at frame granularity. A newly delivered key/click first appears in the frame carrying it, even after a long manual/periodic capture interval; events dated after that frame are never displayed early. The hold starts at this first visible frame and uses the original capture clock with pauses excluded. Carry-over expires normally and cannot cross selection gaps or backwards capture-clock jumps. Click positions are rebased when the recording rectangle moves, using signed capture origins.

Annotations are frozen when authored. Numbering uses original timeline frames and does not add numbers for transition-generated intermediate frames. Duration edits ripple overlay spans; reorder operations retain time-anchored overlays. Updating a group regenerates its content from the current timeline while preserving the original group's exact time coverage, identity, name, and visibility. The current frame selection does not replace the edited group's coverage.

Text is shaped once and stored as immutable RGBA assets. Project reopen, previews, and GIF export use these pixels; fonts are only needed when editing the label again. GIF palette quantization and binary transparency still apply during export.

Text-only progress preserves the chosen background color in its saved text raster. With a bar present, the text raster stays transparent and the bar/background is composed once. Cursor modes expose only their actual controls: recorded cursors use captured positions and immutable colors; built-in cursors allow a manual position; both retain opacity/layer controls without requiring unused text/font/radius settings.

Newly authored progress stores a validated exact fraction and rounds pixel fill and percentage labels to nearest with midpoint ties to even, using integer arithmetic. Older overlays without that optional fraction keep their original millionths-based pixels when reopened or exported. Updating an existing group deliberately regenerates it using the new exact path; it is not a silent project migration.

## Cursor fidelity

Saved cursor pixels follow the frame's crop, resize, quarter-turn rotation, and flips. Small cursor patches use the same nearest-neighbor sampling phase as the complete frame, including clipped hotspots and cursors partly outside the crop. The patch is then rotated/flipped with the shared CPU renderer. Repeated identical cursor placements/transforms are cached during preparation.

This follows the upstream behavior: ScreenToGif 2.43.2 embeds the cursor through `DrawIconEx` in [ImageCapture.cs](https://github.com/NickeManarin/ScreenToGif/blob/2.43.2/ScreenToGif/Capture/ImageCapture.cs) before its editor resizes the complete bitmap in [Editor.xaml.cs](https://github.com/NickeManarin/ScreenToGif/blob/2.43.2/ScreenToGif/Windows/Editor.xaml.cs).

Frames that already contain an embedded cursor are skipped. A visible cursor without a saved cursor asset produces an actionable error; it is never silently replaced with a different pointer. The built-in pointer is a separate manual authoring mode.

Content-addressed assets currently bind one geometry to each raw-byte digest. If a rotated solid cursor has identical bytes but a different geometry, an invisible transparent column distinguishes the stored raster without changing its visible pixels or modifying the existing asset.

## Safety and resource limits

Preparation and persistence run on a background worker with an exclusive, recoverable workspace loan. All generated assets and tracks become one journal-backed edit. Cancellation, validation failure, thread failure, and a poisoned worker lock return the workspace. A journal synchronization failure can require reopening to establish the durable state; it is not reported as guaranteed rollback. Verified but unreferenced immutable assets can remain after cancellation during persistence.

- At most 10,000 selected frames and 40,000 overlay items per operation.
- At most 512 input events in one sampled frame and 256 simultaneously retained event labels/markers.
- At most 64 MiB for a cursor source/intermediate surface, 256 MiB of prepared assets, and 64 MiB of serialized annotation commands.
- Source assets are read with a bounded reader and checked against their dimensions, byte length, and digest before use.

Recorded input availability depends on the recording backend and user opt-in. Manual annotations do not require global input access. A recorded-event automatic task with no matching events is a no-op, allowing later tasks to proceed; manual authoring instead explains that no events were found. GNOME/KDE real-desktop capture acceptance remains separate from these deterministic editor/export tests.

## Verification

```sh
cargo test -p gif-from-screen-domain annotation
cargo test -p gif-from-screen-render --lib
cargo test -p gif-from-screen --bin gif-from-screen annotation
```

Tests cover legacy JSON compatibility, real frame/time progress, selection gaps, event expiration, moving recording origins, shared immutable label assets, undo/redo, reopen, GIF transparency gaps, annotation re-editing, worker cancellation/panic/loan collision, and every options form. A 64-case cursor golden test compares small-patch rendering against embedding the pointer before complete-frame transforms, byte for byte.
