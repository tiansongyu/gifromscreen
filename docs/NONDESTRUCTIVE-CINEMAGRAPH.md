# Non-destructive rectangular freeze

Schema 6 replaces the earlier rectangular-freeze flattening with an ordered `FreezeRegion`
step. It retains the original frame asset, capture input, clock, transforms,
effects, and every existing layer. One immutable rendered reference is shared
by the selected frames. Unselected frames and their cells stay unchanged.

This is an extension, now labeled **Rectangular freeze** in the UI, not exact
ScreenToGif Cinemagraph parity. The [pinned reference audit](CINEMAGRAPH-REFERENCE.md)
establishes that upstream uses the first frame, freehand ink geometry and
premultiplied source-over. This tool keeps its chosen current-frame/rectangle/
RGBA-overwrite behavior; opening an existing project does not reinterpret it.

## Render contract

The step stores `baseline_asset`, `baseline_size`, `region` and `invert`.
Normal mode keeps the rectangle live and copies reference pixels outside it;
inverted mode freezes inside the rectangle. Copying overwrites all four RGBA
bytes, including fully transparent pixels and their RGB. It is not source-over.
The reference is a content-addressed asset, not a mutable frame identity, so
deleting/reordering its original frame cannot invalidate or recursively render it.

Earlier layers, including hidden, zero-opacity and empty-scope groups, keep
their paint stages. Previously unanchored selected cells are sealed at a legacy
boundary before the freeze. Showing earlier hidden artwork changes the live
region; the frozen region keeps the saved reference. Later artwork still draws
after the freeze. Existing precision boundaries are neither removed nor merged.
The Layers inspector now provides explicit Hide/Show controls for this workflow.
They change only visibility, retain all content and assets, and support Undo/Redo
and journal recovery. The tooltip explains why previously frozen copies remain
unchanged when an earlier layer is hidden or shown.

The source image at this step must match the saved baseline view exactly.
Earlier geometry edits that would invalidate that size are rejected atomically;
later geometry edits transform the frozen result normally. The baseline is only
raw RGBA8. An equal-area view can have a different shape from its canonical
descriptor, because content hashes cover bytes rather than dimensions. For
example, rotating a uniform 2×1 image into 1×2 reuses identical bytes. No resize
or registry/cache-shape mutation is used. Compressed images cannot be treated
as arbitrary raw views.

## Recorded input and edit history

`CaptureBinding` continues to describe the retained source, not a guessed
association with mixed output pixels. Original recorded groups can be re-edited
at their pre-freeze stage. New recorded key/click/cursor authors after a freeze
are blocked, including held-input propagation across event-empty frames.
Manual annotations remain available. An unknown stage is conservative; a
previously archived or unverified source is never silently promoted to Original.
The UI distinguishes source provenance from the frozen output restriction.

The stage-aware guard is shared by new authors, old timed-group replacement and
frame-owned input-pool replacement. Position/cursor mapping explicitly rejects
crossing a freeze rather than inventing a coordinate transform for mixed pixels.

An atomic edit registers the baseline when needed, seals old tails and appends
the step. Undo/Redo restores the complete source/layer state. Schema headers 1–5
cannot replay a schema-6 payload; the durable upgrade is written before the
journal and remains sticky through Undo. Existing flattened projects and loop
crossfades keep their original representation and archival rules.

## Resource ownership and bounds

Every step reference participates in asset ownership and deletion protection.
Preview and GIF export load freeze references through their existing bounded,
deduplicated, length- and digest-checked raster providers. Clipboard paste
validates baseline availability and raw-view compatibility. Copy/Yoyo retain
the complete program; project insertion and Save As retain registered assets.
Cross-project conflicting canonical shapes still follow the existing strict
insertion rule; this change does not globally weaken source-image shape checks.

The editor accepts 1–1,000 selected frames. It validates all source frames before
publishing the reference, but stores only one new rendered bitmap rather than
one image per frame. Pure command preparation retains the existing 4,096-step
and 16 MiB metadata limits. Image plus baseline scratch is bounded to 64 MiB
in the desktop motion path, and intermediate geometry cannot hide an oversized
freeze behind a small final crop. Raster-provider retained buffers have their
own aggregate limits; these are not a whole-process RSS promise. Rendering and
row/chunk copying are cancellable. Cancellation after immutable-blob publication
can leave an unreferenced asset, but never a partial timeline or GIF.

## Verification and remaining scope

The complete workspace/all-target/all-feature suites pass on Rust 1.98.0 and
1.88.0: **1,225 passed**, with seven explicit opt-in tests excluded from the
ordinary run. Strict whole-workspace Clippy, formatting and whitespace checks
pass. The separate WPF reference test also passes on 1.88.0 against the unchanged
independent artifacts from hosted run `34074758981` (all 13 inputs/stages exact).

Automated coverage checks transparent overwrite and inversion, direct/detached
render equivalence, hidden-stage reveal, source/clock preservation, earlier
recorded-group re-edit, new-input blocking, raw shape aliasing, missing/damaged
baselines, working-set rejection, cancellation, schema upgrade/recovery, asset
deletion protection, copy after source deletion, Save As/reopen, exact Undo/Redo,
and local/global-palette GIF export with frozen transition endpoints.

This resolves the prior **hidden intermediate-stage rejection for rectangular
freeze**. It does not implement the reference Cinemagraph path, certify every upstream
compositing nuance, or close physical desktop release gates. The bounded
[native freeze/reveal/Undo/reopen/GIF sequence](NATIVE-FREEZE-QA-2026-09-07.md)
now passes with a byte-identical export after reopening. WPF fixtures remain a separate,
strict numerical regression gate for borders, shadows and paint precision.
