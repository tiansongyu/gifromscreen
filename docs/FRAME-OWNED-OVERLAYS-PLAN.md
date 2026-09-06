# Frame-owned overlays: next implementation slice

Status: design proposal, not implemented. Existing time-anchored tracks must retain their current pixels and semantics. This slice addresses temporal ownership, not every difference from destructive frame compositing.

## Behavior being closed

Pinned ScreenToGif 2.43.2 writes annotations into frame images; reversing frames moves those images and Yoyo copies the already annotated image. Relevant source locations are [`Editor.xaml.cs`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L5591) and [`Util/Other.cs`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Util/Other.cs#L284). Our current overlay timing regression explicitly preserves time anchors on reorder. Changing that old behavior silently would damage existing projects.

## Proposed representation

Add an optional frame-owned representation to a track. Each cell owns a `FrameId`, stable authoring-run identity, frame-local authoring scope and generated marks. A frame can have multiple disjoint cells. Reuse existing overlay content, track identity and annotation recipe types.

- Absent frame cells: execute the complete existing time-anchored path unchanged.
- Present frame cells: legacy absolute items/scope must be absent or empty, so there is only one authoritative representation.
- Frame-local boundaries use validated normalized rational coordinates; resolve against the current frame duration with explicit outward rounding. Do not repeatedly convert stored boundaries during retiming.
- Reorder changes frame order, not stored marks. Already generated progress numbers do not silently recalculate.
- Capture-clock identity remains separate from authoring-run identity and playback duration.

One domain module should validate, resolve, remap and subtract this representation. Preview, thumbnails, asset enumeration, presentation planning and GIF export consume the same resolved view. Do not persist a second absolute-time cache. Reuse the existing upsert/compound/undo mechanism where possible rather than introducing a command for every toolbar operation.

This is a new visible rendering payload. Unlike capture-clock metadata, ignoring it would silently erase visible annotations. Use an explicitly supported schema revision (proposed schema 2) with backward reading of schema 1; unsupported older readers must refuse the new schema. A migration must not convert or regenerate existing tracks.

## Required acceptance cases

1. Reverse and move unequal-duration A/B/C frames: new marks follow their owning frame IDs, including precomputed progress; old tracks remain unchanged.
2. Partial/gapped authoring: shorten hold, reorder/retime, then extend hold. Only the originally selected frame-local scope is eligible; deleting an unselected gap must not join the new model's distinct authoring runs.
3. Clipboard, Yoyo and project insertion: a common frame bundle copies marks, scope, recipe and referenced assets, then remaps old to new frame/track/overlay IDs without linking the copy's edits to its source. Include copying only an event-empty frame that displays a held label from an earlier frame. Re-edit needs bounded contributing event context or an explicit limitation; it must not silently lose that label.
4. Title/project insertion, deletion and baking: inserted frames do not inherit unrelated cells; deletion and baking consume only corresponding local coverage. Hidden/zero-opacity overlays are not baked. Undo, redo and reopen restore the exact state.
5. Mixed old/new projects: original pixel hashes and time anchors remain unchanged, all render consumers agree, and unsupported readers reject the new schema instead of ignoring new marks.

Geometry fidelity after baking then rotating/resizing remains a separate acceptance requirement. Passing these temporal cases alone must not be reported as full frame-baked parity.
