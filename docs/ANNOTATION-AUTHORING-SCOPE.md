# Persistent annotation authoring scope

An annotation group's generated marks are not its authoring scope. A short key/click hold can produce one marked frame even though the user selected many frames. Increasing the hold must be able to reach the selected, previously unmarked frames.

New groups store complete eligible authoring coverage in frame-owned cells, with canonical local fractions and persistent per-track run numbers. Their marks paint the whole owner frame. Legacy timed groups use `OverlayTrack.annotation_scope` independently of `items`: `None` means the original selection was not saved, and a legacy update conservatively uses existing marker coverage; `Some([])` is explicitly empty. Scope is project-owned, not part of reusable global `AnnotationRequest` presets.

## Editing invariants

- Updating a group uses its saved scope, never the current frame selection. Its project/revision must still match; confirmation of legacy input coordinates retains a stricter selection anchor.
- Event holds use original capture clocks, independently of fixed or measured GIF playback. Scope intersections are edited-timeline intervals; clipping an item does not change the event clock, frame number, or full-frame-end progress value. Changing playback duration never rewrites raw event/sample timestamps; see [Capture playback timing](CAPTURE-PLAYBACK-TIMING.md).
- Positive gaps restart input history, including gaps that fall within or between partially covered frames.
- Insertions exclude new frames. Frame-owned scope stays attached to its owner, with unchanged stored fractions; legacy retiming splits scope at old frame intersections before mapping, rather than widening long intervals across inserted time.
- Frame deletion removes the owner's cells, but never merges surviving frame-owned run identities. Local fractions resolve outward only when needed, avoiding cumulative drift. The whole-track inverse preserves pre-edit state through undo/redo and recovery.
- Title/project insertion does not inherit existing frame-owned marks. Source bundles remap owner identities while keeping scopes and history references; legacy imported groups retain their old time shifting. Baking removes consumed coverage/marks and keeps hidden or unconsumed content.
- Empty regenerated groups keep their authoring recipe and scope. New empty manual groups still produce a no-event notice rather than an empty edit.

Ordinary authoring now creates **frame-owned** marks that follow reordering, copying and retiming. Existing timed groups keep their old time anchors and gap-closing semantics until the user explicitly chooses Attach to frames in Layers. That conversion preserves current frame-start appearances and known scope, but cannot infer absent authoring coverage or input history. See [frame-owned implementation and remaining work](FRAME-OWNED-OVERLAYS-PLAN.md).

## Recorded-input safety

Raw input and its coordinate applicability are separate. The centralized capture-binding guard excludes unverified legacy frames and archived mixed-source composites from new recorded-input groups. Excluded frames are real scope gaps and interrupt carried labels even if they contain no raw event of their own. Eligible original frames without events remain in scope for future hold expansion.

Known imported/generated frames are explicitly `NotRecorded`: this includes title/blank creation and metadata-free image/video/camera/board writers. Real screen samples carry a native metadata record even when input-event collection is disabled, and remain `Original`. This classification is made by the producer, not inferred later from an empty event array or absent legacy clock. Loop cross-fade frames are `ArchivedAfterComposite` because their pixels are mixed and may contain already-baked labels. Neither state can be confirmed as original screen input.

New groups can process their safe portion, with typed legacy/archive skip counts surfaced to the editor or automatic-task result. Updating an existing recorded-input group is stricter: if any of its authored scope has an unusable binding, the entire update is rejected and its request, marks, pixels and history remain unchanged. A group never stores one new global recipe over a silently mixed old/new result.

Legacy input-coordinate confirmation uses the existing annotation worker/loan and requires an explicit stable-selection confirmation. It writes one atomic undoable edit containing binding and, when needed, field-only clock commands, and adds no annotations. It cannot relabel archived composites or `NotRecorded` frames as original. Manual annotations and progress overlays do not replay capture coordinates and are unaffected by this guard.

New recorded key/click groups persist bounded shared JSON replay pools and per-owner prefix references. Re-edit uses source delivery times and transitions, not the destination timeline or still-existing predecessor frames. Missing/corrupt pools, mismatched contexts, invalid bindings or exceeded budgets reject the whole update. Event-empty cells retain their scope/history when a short hold generates no mark, so a later longer hold can restore it. Duplicate scope references do not draw the same source contribution twice; distinct actual click events remain distinct.

Pools are at most 8 MiB/65,536 events each, and input-pool preparation is at most 64 MiB. A small copied selection may still reference a pool containing later unconsumed events. `.gfsproj` is an editable, history-bearing project, not a sanitized sharing format; exported GIFs contain rendered pixels, not the replay metadata. Cursor authoring uses the owner's verified cursor metadata without storing unrelated key history.

## Capture-clock persistence and schema compatibility

`FrameClip.capture_clock` now stores `CaptureClockContext { id: Option<CaptureClockId>, sampled_at: TimeUs }`, separately from raw `CaptureMetadata` and pixel applicability. Each new screen-recording writer or batch persistence operation receives a fresh identity. Pause, retargeting and playback-delay changes keep that identity; Save As, frame clipboard copies and project insertion preserve the original source context rather than substituting the destination project's identity.

Recorded key/click history only carries across matching nonempty identities with increasing sampling times and continuous eligible authoring scope. Unknown or different identities clear that history even if timestamps appear compatible. Thus the formerly unrepresented boundary between independently recorded segments now has an explicit implementation. This does not assert that all other authoring-scope fidelity gaps are resolved.

For legacy frames, moving/copying freezes an existing raw sampling timestamp, or the original source-frame start when no raw timestamp was saved. This preserves the previous per-frame interpretation without inventing a shared identity. The input-confirmation UI separately offers an explicit declaration that each selected continuous unknown-clock interval comes from one recording with trustworthy sampling times. Declared unknown intervals get separate fresh identities; existing known clocks and selection gaps are never merged. Pixel confirmation alone does not authorize cross-frame holds. Without trustworthy original timing, leave the declaration unchecked and use per-frame/manual annotations.

Capture-clock metadata was introduced additively in schema 1 and remains readable there. Current new projects use schema 2 for the frame-owned rendering representation; reading schema 1 does not rewrite it. Before a v2 command is committed to a v1 project, the store durably stamps the previously committed state with a v2 header. This upgrade is sticky even after visual Undo, so older readers reject a new rendering format rather than silently losing marks. The capture-clock compatibility details remain:

- Missing `FrameClip.capture_clock` deserializes as `None` and is omitted when serializing `None`. No source identity is inferred from a legacy timestamp, project ID, label or asset hash. Missing `capture_binding` still defaults to `LegacyUnknown`.
- Missing `OverlayTrack.annotation_scope` remains `None`: use existing marker coverage, not a guessed original selection. `Some([])` remains distinct from missing scope and represents explicitly removed authored time.
- When a clock is present, a nil identity is invalid; when a raw capture timestamp is also present, `sampled_at` must equal it. Clock-only commands do not copy or mutate raw event buffers. Current journal replay, undo and checkpoint/reopen preserve the context.
- Journals may contain `SetCaptureClocks`. Older schema-1 executables do not necessarily understand that command or preserve optional capture fields when saving; do not use them to round-trip an edited project. Schema-2 projects are explicitly unsupported by those executables. Keep a backup when moving projects between application versions.

## Bounds and verification

Scope is sorted, non-overlapping, inside the timeline and limited to 40,000 fragments. Generated frame intersections have the same limit; one preparation selects at most 10,000 frames. Errors are explicit, with no silent truncation of authored coverage.

Coverage is exercised by domain interval/retiming tests, `annotation_scope_tests.rs`, durable editor annotation tests, title/project-insertion tests, motion/binding tests and the confirmation worker tests. The tests cover unmarked-frame hold expansion, partial gaps, original clocks after duration edits, inserted-frame exclusion, visible versus unmarked baked coverage, atomic blocked updates, conservative legacy fallback, empty groups, selection-independent updates, undo/redo and reopen.

`crates/application/tests/fixed_playback.rs` covers 18 real project stores/exports across batch and incremental writers, with exact delays, untouched source timestamps, independent identities and journal recovery. New authoring/replay tests cover copy/delete-source/re-edit, prefix isolation, gaps, unknown clocks, sparse delivery, hidden assets and limits. The [native authoring record](FRAME-AUTHORING-QA-2026-09-07.md) distinguishes actual UI interactions from its synthetic event fixture. Geometry and broader platform acceptance remain open.

Separate [native manual fixed-playback evidence](NATIVE-PLAYBACK-QA-2026-09-07.md) verifies one private nested GNOME recording through pause/crop movement and CLI reopen/GIF export: its three 1 s project frames retain their nonuniform native sampling times and shared recording identity. This validates that bounded recording/persistence path, not the full legacy-declaration/re-edit UI or cross-compositor/hardware scope gates.
