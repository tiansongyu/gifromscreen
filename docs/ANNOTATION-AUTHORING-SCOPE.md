# Persistent annotation authoring scope

An annotation group's generated marks are not its authoring scope. A short key/click hold can produce one marked frame even though the user selected many frames. Increasing the hold must be able to reach the selected, previously unmarked frames.

`OverlayTrack.annotation_scope` stores the complete eligible authoring intervals independently of `items`. New groups store full selected-frame intervals, retaining boundaries and positive gaps. `None` denotes an older group whose original selection was not saved; updating it conservatively uses its existing marker coverage. `Some([])` is an explicitly empty scope after all authored time has been removed. Scope is project-owned, not part of reusable global `AnnotationRequest` presets.

## Editing invariants

- Updating a group uses its saved scope, never the current frame selection. Its project/revision must still match; confirmation of legacy input coordinates retains a stricter selection anchor.
- Event holds use original capture clocks, independently of fixed or measured GIF playback. Scope intersections are edited-timeline intervals; clipping an item does not change the event clock, frame number, or full-frame-end progress value. Changing playback duration never rewrites raw event/sample timestamps; see [Capture playback timing](CAPTURE-PLAYBACK-TIMING.md).
- Positive gaps restart input history, including gaps that fall within or between partially covered frames.
- Insertions exclude new frames. Retiming splits scope at every old frame intersection before mapping; merely mapping a long interval's endpoints would incorrectly include inserted time.
- Frame deletion removes corresponding scope, and duration changes scale partial bounds outward. The existing whole-track inverse preserves exact pre-edit scope through undo/redo and journal recovery, including rounding cases.
- Title insertion and destination-side project insertion split and shift scope around the inserted interval. Imported source groups shift their scope to the destination time. Baking removes the baked coverage from editable scope as well as from visible marks; remaining unmarked scope is not discarded just because no items remain.
- Empty regenerated groups keep their authoring recipe and scope. New empty manual groups still produce a no-event notice rather than an empty edit.

Ordinary authoring tools and existing timed tracks still keep overlays and scope **time-anchored when frames are reordered**. In this legacy representation, deleting an unselected gap closes it and no historical authoring-run IDs are stored. Schema 2 now also supports explicit frame-owned cells with stable per-track run IDs, whole-frame marks, and canonical local scope fractions. Their persistence, rendering, copying and baking are implemented; ordinary creation and input-aware re-authoring are still pending. Existing groups are never silently converted. See [frame-owned implementation and remaining work](FRAME-OWNED-OVERLAYS-PLAN.md).

## Recorded-input safety

Raw input and its coordinate applicability are separate. The centralized capture-binding guard excludes unverified legacy frames and archived mixed-source composites from new recorded-input groups. Excluded frames are real scope gaps and interrupt carried labels even if they contain no raw event of their own. Eligible original frames without events remain in scope for future hold expansion.

Known imported/generated frames are explicitly `NotRecorded`: this includes title/blank creation and metadata-free image/video/camera/board writers. Real screen samples carry a native metadata record even when input-event collection is disabled, and remain `Original`. This classification is made by the producer, not inferred later from an empty event array or absent legacy clock. Loop cross-fade frames are `ArchivedAfterComposite` because their pixels are mixed and may contain already-baked labels. Neither state can be confirmed as original screen input.

New groups can process their safe portion, with typed legacy/archive skip counts surfaced to the editor or automatic-task result. Updating an existing recorded-input group is stricter: if any of its authored scope has an unusable binding, the entire update is rejected and its request, marks, pixels and history remain unchanged. A group never stores one new global recipe over a silently mixed old/new result.

Legacy input-coordinate confirmation uses the existing annotation worker/loan and requires an explicit stable-selection confirmation. It writes one atomic undoable edit containing binding and, when needed, field-only clock commands, and adds no annotations. It cannot relabel archived composites or `NotRecorded` frames as original. Manual annotations and progress overlays do not replay capture coordinates and are unaffected by this guard.

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

For this timing/clock cohort, `crates/application/tests/fixed_playback.rs` passes four tests on Rust 1.98.0 and 1.88.0, covering 18 real project stores/exports through batch and independent incremental writers. It verifies exact project delays, untouched raw sample/key/mouse timestamps, per-recording identity, recovery from an uncheckpointed recording journal, reopen and decoded GIF timing. These synthetic-source integration results are not a native confirmation/re-edit, compositor, hardware or physical-input acceptance claim. The reorder/deleted-gap semantics above, broader platform acceptance and remaining scope work are still open.

Separate [native manual fixed-playback evidence](NATIVE-PLAYBACK-QA-2026-09-07.md) verifies one private nested GNOME recording through pause/crop movement and CLI reopen/GIF export: its three 1 s project frames retain their nonuniform native sampling times and shared recording identity. This validates that bounded recording/persistence path, not the full legacy-declaration/re-edit UI or cross-compositor/hardware scope gates.
