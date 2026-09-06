# Recorded input after pixel edits

Raw `CaptureMetadata` is retained separately from `FrameClip.capture_binding`. The new field describes whether those recorded coordinates still apply to the frame pixels before its editable transform:

- `original`: produced as an original source frame by this version, or explicitly confirmed by the user for a legacy project.
- `legacy_unknown`: default for projects that did not persist this field. Missing history is not treated as evidence that pixels are still original.
- `archived_after_composite`: raw input remains available as historical data, but a mixed-source pixel edit has detached it from the frame's current pixels.
- `not_recorded`: the producer explicitly created a frame without screen-input metadata, such as a title, image/video import or blank frame. It is not a legacy recording to be confirmed.

## Cinemagraph boundary

Cinemagraph combines rendered pixels from the selected frame and a frozen reference frame. It therefore marks selected frames `archived_after_composite` in the same atomic edit as the new pixels and baked overlay removal. Capture positions, cursor assets/hotspots, capture-time `cursor_embedded`, key events, mouse events, origins, and timestamps remain unchanged.

Generated loop-crossfade frames are also mixed-source composites, even though their raw metadata is empty. They interrupt carried recorded-input labels rather than inheriting another frame's input over their baked pixels.

Recorded cursor/key/click authoring uses a shared binding guard. Archived and unverified legacy frames do not receive automatic replay; even an input-empty blocked frame interrupts labels carried from a neighboring frame. Explicit reason counts distinguish this from an ordinary no-event task. Manual annotations and progress remain available.

Hidden tracks, zero-opacity tracks, and zero-opacity raster items are not consumed by baking. Visible annotation groups subtract the baked interval from their authoring scope; an empty item list does not delete a group while meaningful authoring scope remains.

## Legacy confirmation

The **Recorded input provenance** editor section offers **Confirm original input association** only after the user checks that the selected legacy frames still contain their original captured pixels, with crop/resize represented by editable transforms. This action does not restore pixels or infer a coordinate mapping.

Confirmation covers all selected legacy frames, including event-empty frames between key presses, so subsequent hold intervals can cross the confirmed range. At least one selected frame must contain recorded input; an old PNG-only selection does not offer a meaningless confirmation. Any selected archived composite or `not_recorded` frame prevents confirmation, even if that frame has no events. Already-original flags are preserved.

Preparation is cancellable and limited to 100,000 selected frames. A single `SetCaptureBindings` command stores only frame identities and association flags; its inverse likewise contains no copied event buffers. One Undo restores the previous bindings. Journal replay, checkpoint/reopen, frame clipboard copies, and project insertion preserve the binding and original raw input.

## Native-window acceptance with synthetic data

An isolated Xvfb display was used with a Rust-generated two-frame legacy project, not real keyboard recording. The first 100 ms frame contains a synthetic Ctrl+C event; the second has no event, and both start as unverified legacy frames.

- The confirmation button is disabled until the acknowledgement is checked. It changes both selected frame flags in one `SetCaptureBindings` command, including the event-empty frame. Raw data and pixels do not change.
- Undo restores both legacy flags and clears the checkbox; Redo restores the confirmed association.
- Recorded-key authoring adds the label to both frames at a 500 ms hold. Editing the group to 50 ms leaves one visible marker but preserves both authored frame intervals. Extending back to 500 ms restores the second frame's label. Native previews and thumbnails reflect each change.
- Undo of the extension removes only the second marker, and Redo restores it. Closing normally and exporting the journal-recovered project succeeds at revision 11 (two GIF frames, 2,039 bytes).

Local screenshots and the synthetic fixture are retained under `/tmp/gfs-binding-fixture.POyTqL`. This checks the actual native UI and persistence path; it does not establish physical-device capture or complete ScreenToGif parity.

## Verification

```sh
cargo test -p gif-from-screen-domain binding
cargo test -p gif-from-screen --bin gif-from-screen capture_binding
cargo test -p gif-from-screen --bin gif-from-screen editor_workspace::motion
```

Regressions cover legacy JSON, empty-frame carry barriers, preserved raw metadata, cropped/resized source frames followed by Cinemagraph, prevention of duplicate cursor replay, hidden overlays, surviving authoring scope, undo/redo/reopen, clipboard/project insertion, explicit interval confirmation, cancellation, archived-frame refusal, and a 100,000-frame forward/inverse binding edit.
