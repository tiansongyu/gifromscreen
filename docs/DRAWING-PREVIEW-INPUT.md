# Ordinary drawing preview input

The editor's ordinary free-drawing tool now owns one primary-button sequence
against the exact displayed image mapping. This does not change drawing-board,
Cinemagraph, recorder or WPF ink-rendering semantics.

## Input contract

- Capture the actual Down, ordered movement samples and Up endpoint, including
  clicks below the drag threshold. Raw events are consumed once across repeated
  UI passes. The existing 4096-point draft limit remains authoritative.
- Lock the painted/visible rectangles, rendered image size, widget/layer/viewport
  identities, layer transform and effective pixel scale for an active stroke.
  Position conversion uses the inverse layer transform, then image coordinates;
  screen pixels are not confused with project pixels.
- Abort incomplete points after mapping changes, hidden/disabled previews,
  focus/pointer loss, middle-button panning or incompatible editor modes. A held
  button cannot silently start a replacement stroke; a fresh Down is required.
  Generic input loss preserves the armed target and style. Explicit Cancel and
  the existing language-change cancellation retain their own behavior.
- A completed Ready draft is not erased by layout invalidation. Existing target
  reconciliation still prevents applying it to another project or selection.
  Input adaptation itself never writes project commands or assets.

## Why the input boundary exists

egui 0.32 performs its initial hit test using a frame's final pointer position.
A Down followed by movement in the same native input batch can therefore appear
to belong to a different widget. Click and drag candidates can also be different
widgets. Rejecting all quick Down/Move batches would lose normal drawing input.

The existing eframe `raw_input_hook` provides a small boundary: when a new ordinary
drawing press needs arbitration, deliver the original batch through Down first,
then request a repaint and deliver its tail before the next batch. egui can
arbitrate the real press position; normal fast drags need no user-imposed pause.
No framework fork, dependency upgrade or operating-system input injection is used.
The painted image uses `Sense::click_and_drag`, and another actual click owner
cannot lend its press to the image's drag candidate.

Only events are split. Input time, focus, modifiers, files and viewport metadata
are not rewritten. Pending events survive Ready/cancellation and return to the
same live viewport in FIFO order. Events for a removed viewport are retired, not
injected into a different window. Repeated hooks do not replay a queued tail.

## Resource and acceptance boundaries

The normal drawing event slice is limited to 8192 events. The retained tail is
bounded by both event count and 1 MiB of event/string allocation capacity. Unknown
or oversized payloads are not retained. An oversized framework batch still reaches
egui unchanged in order, with any existing tail first, while drawing fails closed.
That exceptional merge scales with the original framework batch; the adapter
does not claim that all egui input processing has a constant memory/time bound.

All 22 drawing-preview tests (17 egui scenarios and five input-boundary cases)
pass on Rust 1.98.0 and 1.88.0. They exercise real egui input rather than forcing
response flags; boundary cases additionally assert complete FIFO events, metadata,
retired-window handling and allocation limits. Native quick
click/drag, resize, fresh-press and project/GIF acceptance are separate from these
headless cases. Neither establishes physical-device pressure accuracy, full
ScreenToGif/WPF ink fidelity or the absence of every input bug.
