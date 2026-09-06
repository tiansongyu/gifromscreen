# Ordered full-frame editing

## Upstream contract

The baseline remains ScreenToGif 2.43.2, pinned at
[`a4d0a67`](https://github.com/NickeManarin/ScreenToGif/tree/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd).
Its editor reads the current frame PNG and overwrites it after each image edit.
Resize/crop and rotation address all frames; flip, text and region effects use
the selected frames. See the pinned
[resize/crop](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L5445),
[overlay](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L5591),
[rotate/flip](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L6141)
and [blur](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Editor.xaml.cs#L6611)
implementations. These are source-level findings, not a Windows/WPF golden-pixel run.

For `resize, text A, rotate, blur, text B`, A must rotate and blur; B must remain
newly drawn. Applying every transform before every annotation cannot reproduce
this. Nor can adjusting just annotation coordinates: font pixels, alpha edges,
region masks and already drawn cursor images also need the later operation.

## Representation and compatibility

Schema 3 adds `FrameClip.render_steps` and optional `FrameOverlayCell.stage`.
Absent/empty steps and absent anchors are omitted from serialization, preserving
old journal checksums and the old canonical prefix exactly. Schema 1 and 2 stay
readable; merely opening an old project does not move or convert artwork.

The old prefix remains source crop, pre-rotation resize, quarter-turn, flips and
ordered legacy effects. A new image edit appends operations after that prefix.
`Composite { stage_id }` is a stable, frame-local paint boundary. Existing tail
cells are sealed at a boundary before a new operation; later authored cells have
`stage: None` until the next operation. Hidden, zero-opacity and empty authoring
cells are sealed too, so revealing or re-authoring them cannot silently jump to
a different phase. Per-frame work is limited to 4,096 steps, and one constructed
editing command has a 16 MiB metadata budget before any project mutation.

The first step is always a Composite with a unique nonzero identity. Legacy
timed layers are dynamically sampled at that first boundary. Their existing
time anchors, visibility, opacity and z/track/item ordering remain in force,
including mixing with the first batch of owned marks. They are not implicitly
converted to frame ownership. Subsequent boundaries paint their matching owned
cells; unanchored cells paint at the tail. Ordering is chronological across
stages and stable z/track/item order within a stage. Copy/Yoyo remap owner frame
IDs but keep local stage IDs. Project insertion and Save As retain the chain.

Project insertion compares the planned output size of every source and
destination frame before reading pixels or writing assets. Stale canvas-size
headers do not cause false rejection when actual sizes agree. Color space and
background must still agree; the same insertion command normalizes the canvas
to the common actual size, and Undo restores its old metadata. Mixed sizes or
unrenderable old effects fail early rather than creating an unexportable project.

Visible stage payloads require schema 3, including nested/inverse journal
commands. The project store preflights the candidate, durably stamps the
previously committed image state at its current revision with the required
header, then appends the new command. Undo restores images but does not downgrade
the header. A schema-1/2 snapshot must reject schema-3 journal payloads rather
than silently ignoring them. Fast raw recording append rejects staged clips.

## Editing and authoring

The desktop crop, resize and rotation controls operate on all current composed
frames and update the project canvas in one undoable command. Flip and effects
address the selected frames. Automatic border/shadow presets use the same
ordered command path. The legacy library clip-transform property API retains
its explicitly documented source-coordinate semantics; it is no longer the
desktop's current-image editing path.

`FrameGeometryPlan` is the pixel-independent authority for base, stage, step
input and final sizes. Crop and effect regions must fit their actual input, not
a potentially unrelated final/global canvas. Preview, streaming GIF export and
transition endpoints use the same staged renderer. Resources are enumerated
across all active stages; a detached preview plan does not retain an entire
project or replay pool.

New artwork is authored in the current tail space. Re-authoring existing text
or input groups uses the saved stage space. An explicit legacy-to-owned
conversion anchors cells at the owner's first Composite, preserving pixels.
Raw capture positions, events and clocks are never rewritten. Cursor patches
and recorded points are mapped through geometry only up to their authoring
stage; past image effects are not applied to a newly authored input mark.

Effect replacement indexes the legacy prefix followed by chronological effect
steps. Clear effects removes those operations, not their paint boundaries.
Remove last crop/resize removes the most recent matching operation, falling
back to an old prefix property when no staged operation exists. Later marks
retain their stage-local coordinates. This is a deliberate edit of history,
not a silent re-author: updating an input group recalculates its coordinate
mapping. If removing a step makes a later crop/effect invalid, nothing commits.
Undo is the way to restore the exact previous layout. Invalid legacy effect
regions can still be removed or replaced to repair an old unrenderable project.

## Baking boundary and remaining fidelity work

Cinemagraph renders the complete chain, consumes visible marks and resets
consumed geometry/effects so pixels are not transformed twice. Source input
remains archived, not falsely rebound to the baked image. Ordinary staged
frames and surviving hidden tail artwork are supported. A bake with retained
hidden/zero-opacity artwork in an intermediate stage is explicitly rejected
before asset writes: clearing the chain would orphan its coordinates, while
keeping the chain would transform the baked base again. Show/remove that
artwork first or undo the geometry edit. This is a known limitation, not a
completed hidden-layer bake-fidelity claim.

The upstream editor itself does not retain independent editable text after
applying it. Our stage-aware re-authoring is a deliberate additional capability.
Numeric resize/blur fidelity is not yet certified against Windows/WPF. The
current shadow effect retains its canvas; upstream shadow can expand it.
Physical compositor/device, global-shortcut, packaging and other open release
gates remain in [Linux status](LINUX-STATUS.md). Ordered stages do not establish
complete ScreenToGif parity or absence of every bug.
