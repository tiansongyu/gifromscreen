# Frame-owned overlays: implementation and remaining authoring work

Status: schema, rendering, structural editing, copying and baking are implemented and tested. The ordinary authoring tools still create legacy timed tracks; new frame-owned group creation and input-aware re-authoring are not yet connected. A frame-owned group cannot currently be regenerated through the editor; that request is explicitly rejected before changing its marks, recipe, scope, assets or journal. Existing time-anchored tracks retain their current pixels and semantics. This slice addresses temporal ownership, not every difference from destructive frame compositing.

## Behavior being closed

Pinned ScreenToGif 2.43.2 writes annotations into frame images; reversing frames moves those images and Yoyo copies the already annotated image. Relevant source locations are [`Editor.xaml.cs`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L5591) and [`Util/Other.cs`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Util/Other.cs#L284). Our current overlay timing regression explicitly preserves time anchors on reorder. Changing that old behavior silently would damage existing projects.

## Chosen representation

`OverlayTrack.frame_cells` stores an optional frame-owned representation. Each track has at most one cell per `FrameId`; the cell owns multiple disjoint authoring scope spans and one set of generated marks. Each scope span has a stable, nonzero run number local to the track. Multiple scope hits never paint the same frame twice. Existing overlay content, track identity and annotation recipe types are reused.

- Absent frame cells: execute the complete existing time-anchored path unchanged.
- Present frame cells: legacy items must be empty and legacy annotation scope absent, so there is only one authoritative representation.
- Generated marks paint the whole owner frame, matching the upstream frame-image behavior. They have no independent sub-frame playback interval. Local fractions only describe authoring eligibility; this is not a new sub-frame animation feature.
- Frame-local scope boundaries use validated, canonical rational coordinates and explicit outward rounding. Stored boundaries do not change during retiming. Legacy progress ratios and their serialized form remain untouched; new frame-owned progress requires frozen values, not an invented absolute span.
- Reorder changes frame order, not stored marks. Already generated progress numbers do not silently recalculate.
- Capture-clock identity remains separate from authoring-run identity and playback duration.

One render resolver handles legacy point-sampled items and marks belonging to the requested frame ID, with the same stable ordering. Preview plans clone only resolved drawing content. Preview, thumbnails, GIF streaming, asset loading and transition endpoints use this path, without a second persisted time cache. The old asset-query API without a frame ID rejects visible frame-owned marks rather than silently omitting them. Domain traversal includes hidden and zero-opacity asset references for validation and deletion protection.

This is a new visible rendering payload. Unlike capture-clock metadata, ignoring it would silently erase visible annotations. New projects use schema 2; schema 1 remains readable without automatic track conversion. Before committing a v2 payload into a v1 project, the project store validates the entire candidate, then durably writes the previously committed visual state and revision with a schema-2 header, then journals the new edit. A failed or uncertain format write blocks further mutation. Undo restores visual content but never downgrades the format. A v1 snapshot cannot implicitly recover a v2 journal payload; legitimate v2 payloads require the durable header first. Golden v1 journal bytes/checksums remain compatible.

`FrameBundle` copies only selected cells plus necessary track metadata, preserving frozen marks even when their source frames are later removed. It remaps frame/track/mark identities without regenerating input, and is shared by clipboard, Yoyo and project insertion. The 16 MiB metadata budget excludes unrelated cells and never reads pixel files. Existing legacy clipboard/Yoyo semantics are unchanged; legacy project insertion keeps its previous time shifting. Save As preserves the complete frame-owned representation and hidden assets in an independent project.

Cinemagraph baking removes consumed marks from selected owners, preventing repeated alpha composition. It preserves unselected, hidden and zero-opacity content; selected scope-only cells are removed, while a cell retained for an unconsumed zero-opacity raster keeps its scope. Existing raw input remains intact. Undo, redo and reopening have pixel regressions.

## Acceptance and remaining work

1. Reverse and move unequal-duration A/B/C frames: new marks follow their owning frame IDs, including precomputed progress; old tracks remain unchanged.
2. Partial/gapped authoring: shorten hold, reorder/retime, then extend hold. Only the originally selected frame-local scope is eligible; deleting an unselected gap must not join the new model's distinct authoring runs.
3. Clipboard, Yoyo and project insertion: a common frame bundle copies marks, scope, recipe and referenced assets, then remaps old to new frame/track/overlay IDs without linking the copy's edits to its source. Include copying only an event-empty frame that displays a held label from an earlier frame. Re-edit needs bounded contributing event context or an explicit limitation; it must not silently lose that label.
4. Title/project insertion, deletion and baking: inserted frames do not inherit unrelated cells; deletion and baking consume only corresponding local coverage. Hidden/zero-opacity overlays are not baked. Undo, redo and reopen restore the exact state.
5. Mixed old/new projects: original pixel hashes and time anchors remain unchanged, all render consumers agree, and unsupported readers reject the new schema instead of ignoring new marks.

Geometry fidelity after baking then rotating/resizing remains a separate acceptance requirement. Passing these temporal cases alone must not be reported as full frame-baked parity.

Core tests now cover cases 1, the structural preservation part of 2, frozen-content copying in 3, baking in 4 and mixed-format rendering/recovery in 5. They do not complete the ordinary authoring/re-authoring workflow. In particular, copying an event-empty frame preserves its visible held label, but regenerating that label with different hold settings still needs bounded, run-isolated replay context. Preserve event delivery sample times as well as raw event times, releases and modifier transitions. Insufficient context must reject the entire regeneration without clearing existing marks; never scan or copy an unbounded project history as an implicit fallback. New authoring and explicit legacy-to-frame-owned conversion must be connected only with matching preview/export, scope and undo evidence.
