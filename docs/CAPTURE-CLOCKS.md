# Independent capture clocks

`FrameClip.capture_clock` stores an optional `CaptureClockContext` with:

- `id`: an optional confirmed recording-clock identity, separate from project and frame identities;
- `sampled_at`: the source sampling instant, independent of GIF playback duration or destination timeline position.

A native recording writer creates one fresh identity per recording workflow. Pause, resume and crop movement keep that writer's identity. A separate writer or batch recording gets a different identity. Frames created without screen metadata remain `NotRecorded` and do not acquire a recording clock.

Held key/click annotations can cross frames only when their identities are nonempty and equal, sampling times increase, and the existing binding/selection barriers permit it. Nearby increasing times from independent recordings are not evidence of one clock. Unknown identities never carry input between frames; eligible frames can still show their own events.

Clipboard copy, Save As and project insertion preserve known contexts. For unidentified legacy frames, their source sampling interpretation is frozen before moving them: native `captured_at` when available, otherwise their source timeline sampling position. Freezing does not assign a common-clock identity and does not change raw key/mouse timestamps. Insertion no longer shifts legacy raw `key.at` values into the destination timeline.

## Explicit legacy migration

Coordinate confirmation and common-clock declaration are separate choices. Confirming coordinates alone leaves clock identities unknown. A second explicit declaration affirms that each selected continuous unknown-clock interval belongs to one recording and has trustworthy source sampling times.

Declared intervals receive fresh identities, including their event-empty frames. Selection gaps and already-known identities separate intervals. Existing identities and sample times are never changed or merged. Previously coordinate-confirmed frames without clock identities can be declared later. When original timing cannot be verified, use per-frame/manual annotations instead of inferring a common clock from timestamps.

`SetCaptureClocks` is a field-only, undoable command. It can share one compound revision with `SetCaptureBindings`; neither command copies or modifies raw event buffers. Reopen/recovery preserves both fields. Stored overlay pixels and existing GIF output do not change until the user regenerates annotations.

## Validation and regressions

Complete manifests and fast incremental appends share clock validation: a confirmed identity cannot be nil, and when native `captured_at` exists, the context sampling time must match it. Invalid fast appends are rejected before changing the manifest, revision, journal or asset descriptors.

Tests cover independent nearby clocks, same-clock empty samples, unknown-clock barriers, two-step legacy confirmation, preservation of distinct known identities, undo/reopen, legacy events moved earlier and later, Save As/copy preservation, and fast-append rejection/recovery. Fixed-playback integration additionally verifies that playback policy does not rewrite source clocks.
