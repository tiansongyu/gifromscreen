# Persistent annotation authoring scope

An annotation group's generated marks are not its authoring scope. A short key/click hold can produce one marked frame even though the user selected many frames. Increasing the hold must be able to reach the selected, previously unmarked frames.

`OverlayTrack.annotation_scope` stores the complete eligible authoring intervals independently of `items`. New groups store full selected-frame intervals, retaining boundaries and positive gaps. `None` denotes an older group whose original selection was not saved; updating it conservatively uses its existing marker coverage. `Some([])` is an explicitly empty scope after all authored time has been removed. Scope is project-owned, not part of reusable global `AnnotationRequest` presets.

## Editing invariants

- Updating a group uses its saved scope, never the current frame selection. Its project/revision must still match; confirmation of legacy input coordinates retains a stricter selection anchor.
- Event holds use original capture clocks. Scope intersections are edited-timeline intervals; clipping an item does not change the event clock, frame number, or full-frame-end progress value.
- Positive gaps restart input history, including gaps that fall within or between partially covered frames.
- Insertions exclude new frames. Retiming splits scope at every old frame intersection before mapping; merely mapping a long interval's endpoints would incorrectly include inserted time.
- Frame deletion removes corresponding scope, and duration changes scale partial bounds outward. The existing whole-track inverse preserves exact pre-edit scope through undo/redo and journal recovery, including rounding cases.
- Title insertion and destination-side project insertion split and shift scope around the inserted interval. Imported source groups shift their scope to the destination time. Baking removes the baked coverage from editable scope as well as from visible marks; remaining unmarked scope is not discarded just because no items remain.
- Empty regenerated groups keep their authoring recipe and scope. New empty manual groups still produce a no-event notice rather than an empty edit.

The current repository deliberately keeps ordinary overlays and authoring scopes **time-anchored when frames are reordered**. This is not full ScreenToGif parity: its baked annotations ordinarily move with the frame pixels. Also, deleting an unselected gap closes that gap; newly adjacent authoring intervals execute continuously, while their original capture clocks remain unchanged. Historical selection-run IDs are not persisted in this version. These are explicit current semantics, not claims of one-to-one upstream behavior.

## Recorded-input safety

Raw input and its coordinate applicability are separate. The centralized capture-binding guard excludes unverified legacy frames and archived mixed-source composites from new recorded-input groups. Excluded frames are real scope gaps and interrupt carried labels even if they contain no raw event of their own. Eligible original frames without events remain in scope for future hold expansion.

Known imported/generated frames are explicitly `NotRecorded`: this includes title/blank creation and metadata-free image/video/camera/board writers. Real screen samples carry a native metadata record even when input-event collection is disabled, and remain `Original`. This classification is made by the producer, not inferred later from an empty event array or absent legacy clock. Loop cross-fade frames are `ArchivedAfterComposite` because their pixels are mixed and may contain already-baked labels. Neither state can be confirmed as original screen input.

New groups can process their safe portion, with typed legacy/archive skip counts surfaced to the editor or automatic-task result. Updating an existing recorded-input group is stricter: if any of its authored scope has an unusable binding, the entire update is rejected and its request, marks, pixels and history remain unchanged. A group never stores one new global recipe over a silently mixed old/new result.

Legacy input-coordinate confirmation uses the existing annotation worker/loan, requires an explicit stable-selection confirmation, writes one undoable binding command and adds no annotations. It cannot relabel archived composites as original. Manual annotations and progress overlays do not replay capture coordinates and are unaffected by this guard.

A remaining source-clock identity gap is explicit: two independently recorded `Original` screen segments inserted into one project can have increasing, apparently compatible timestamps. Without a per-frame capture-sequence identity, held labels can cross that boundary when creating a new group over both segments. A future frame-owned sequence ID must preserve original session membership through insertion/copy; coordinates, timestamp order and asset hashes must not be used as guessed session identities. This batch does not claim to solve that separate case.

## Bounds and verification

Scope is sorted, non-overlapping, inside the timeline and limited to 40,000 fragments. Generated frame intersections have the same limit; one preparation selects at most 10,000 frames. Errors are explicit, with no silent truncation of authored coverage.

Coverage is exercised by domain interval/retiming tests, `annotation_scope_tests.rs`, durable editor annotation tests, title/project-insertion tests, motion/binding tests and the confirmation worker tests. The tests cover unmarked-frame hold expansion, partial gaps, original clocks after duration edits, inserted-frame exclusion, visible versus unmarked baked coverage, atomic blocked updates, conservative legacy fallback, empty groups, selection-independent updates, undo/redo and reopen.
