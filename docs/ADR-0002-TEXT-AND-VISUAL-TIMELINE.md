# ADR 0002: persistent shaped text and bounded visual editing

Status: implemented baseline, 2026-09-06. Follow-up editing/layout work remains separately tracked.

## Text

Use the Rust `cosmic-text` 0.14 series for Unicode shaping, bidirectional text, wrapping, font fallback, and glyph rasterization. Its Rust requirement fits the project's 1.88 floor; that combination has been checked locally. The [upstream implementation](https://github.com/pop-os/cosmic-text/tree/v0.14.2) is the API reference. The authoring worker includes the existing epaint Ubuntu/Hack baseline fonts and discovers installed fonts for other scripts.

The renderer must not resolve machine-dependent fonts during export. `OverlayContent::Text` retains its editable source attributes and an optional `TextRaster` reference to immutable straight-alpha RGBA8 pixels. Authoring registers that asset and its timed track in one journal command. Reopening and exporting read the saved pixels, so installed-font changes do not change an existing caption. Old text content without raster data fails explicitly instead of disappearing silently.

Bounds: 16 KiB source text, 1–512 px font size, 4096 px per raster edge, a glyph-work budget, and a cleared glyph cache between requests. Missing glyphs, missing named fonts, and clipped content are errors. A single worker slot remains occupied until a cancelled task completes, preventing rapid cancellation from spawning unlimited workers.

Authoring captures project path, ID, revision, and complete selection. Completion may commit only if that anchor still matches. Discontinuous selections create separate spans in the same track. This is shared with watermark and drawing integrity checks.

## Timeline and preview

Use one metadata-only `PreviewRenderPlan` for both normal previews and thumbnails. The thumbnail worker loads and verifies immutable assets, runs the same CPU compositor, and scales the resulting image. The UI only uploads textures for visible frames. Cache keys include project path/ID/revision, frame identity, and requested size.

Bounds: one worker, 32 queued plans, two completed images, 32 MiB/256 cached texture entries, and 128 MiB limits on each source set and rendering surface. Scrolling or changing projects supersedes queued work and discards stale results. A 50,000-frame test verifies visible-range scheduling rather than full timeline materialization.

Preview playback accumulates deadlines and seeks to the frame containing the elapsed position. Long delays skip complete cycles rather than iterating through every missed frame. `Loop preview` is independent of the saved GIF repetition setting. Export-generated transition frames are still a separate pending preview capability.

## Verification and limits

The first integrated desktop batch passed 222 desktop tests and full-workspace strict Clippy. Caption tests include undo/redo, reopening, selected-span gaps, pixel-consistent GIF output, cancellation, and target changes. Installed Chinese/Arabic font shaping was explicitly exercised on the development host; the portable CI tests use bundled fonts.

Native Xvfb screenshots verify actual thumbnail cards and tool navigation, not GNOME/KDE compositor behavior. Full ScreenToGif equivalence, all-bug closure, text typography options such as outlines, and Linux release certification are not implied by this ADR.
