# Deferred development snapshot — 2026-09-09

The user requested a release freeze instead of further feature completion.
This branch preserves the unfinished schema-9/vector-v2 integration based on
`67f11e7819808e2c8c9bf65c5717dbe0dac22503`; it is **not part of v0.1.0**.

Domain/project migration, the new renderer stage and several authoring tests
had targeted verification. The combined UI work was not complete: it references
layout-metrics interfaces not yet present. Do not treat the snapshot as buildable,
release-ready, a complete migration or a finished reproduction of ScreenToGif.

No new draft default or released project format was switched to v2. The release
keeps schema 8 and the existing v1 authoring/rendering contract. Resume only as a
new explicit development decision, using the retained integration plan and tests.

The original reversible stash is also retained in the originating workspace:
`c88d690d8e662ecb60ad24b8c867433d2b6b3608`.
