# Linux iteration: video, project insertion, and presentation playback

Implemented in `e549ab0` and `d467e87`. These additions do not mark the entire parity matrix complete.

## Video input

The landing page has an Import video form. Dropping a supported local video or opening it with `--import-video PATH` opens this form for review; it does not start an import automatically. Choose the start, duration, sampling FPS, optional exact output size, and a new project directory.

Rust supervises installed FFmpeg/ffprobe directly, without a shell. Allowed local container formats are explicitly bounded; network protocols and playlist demuxers are excluded. Raw RGBA frames pass through a rendezvous channel to the incremental project writer, rather than retaining the whole movie. Duration fractions are distributed in microseconds, including a partial final frame and videos shorter than the requested interval.

Default resource limits: 300-second interval, 60 FPS, 10,000 frames, 64 MiB per frame, and 2 GiB cumulative raw frame data. Limits fail before creating a project when metadata permits preflight. Existing directories are not replaced. Cancellation/failure after persistence starts preserves a recoverable partial project and reports its path. Normal window close waits for the supervised import worker to terminate.

Official behavior references: [FFmpeg command options](https://ffmpeg.org/ffmpeg.html), [protocol whitelist](https://ffmpeg.org/ffmpeg-protocols.html#Protocol-Options), [format whitelist](https://ffmpeg.org/ffmpeg-formats.html#Format-Options), and [ffprobe metadata output](https://ffmpeg.org/ffprobe.html). FFmpeg is an optional installed input dependency, not an output format or bundled binary.

## Project insertion

In the editor, expand “Insert recording or imported project”. Pick a closed source `.gfsproj`, then choose the beginning or a current-frame anchor. Source and destination canvases must match. The source is read with its normal exclusive lock; stale locks are never taken over implicitly.

Background preparation verifies raster assets and remaps frame, item, track, and transition identities. Source overlays and internal transitions survive. Destination overlays spanning the insertion point are split so they do not cover inserted content; the broken destination transition is removed. One journal revision commits the result and supports exact undo/redo.

Preparation is limited to 1,000 source frames and 512 MiB of referenced pixels. A cancelled or stale preparation can leave verified, unreferenced content-addressed assets, but does not alter either timeline. Automatic deletion of those assets is deliberately not performed by the insertion worker.

## Playback and native UI

The export and preview paths share a compressed `PresentationPlan`. It stores O(original frames) segments rather than expanding every transition step. Playback binary-searches the current segment, supports repeated loops without clock drift, and preserves a paused transition's sub-frame age. Two rendered endpoints are cached for repeated transition steps. Frame drawing is disabled while displaying playback/transition output.

File dialogs use [rfd's native Linux backend](https://docs.rs/rfd/0.17.2/rfd/); the existing typed-path controls remain available if the desktop portal/Zenity backend is unavailable. A manually changed path is not overwritten by a late dialog result; non-UTF-8 selections are rejected explicitly instead of silently changing the filename.

## Acceptance

- Full workspace: 686 tests passed, three opt-in environment/benchmark tests excluded from the default run; strict Clippy and formatting passed.
- 23 video-specific tests passed with Ubuntu 22.04 FFmpeg 4.4.2, including real seek/resize/EOF pixels, process timeout/cancellation, stderr flooding, backpressure, and recoverable partial output.
- Eight project-insertion scenarios passed, including repeated undo/redo, re-open/export, corruption, same-project aliases, locks, and identity/size conflicts.
- Transition preview tests cover endpoint overlays, pixel output, bounded endpoint reuse, and revision invalidation.
- The complete workspace also passes `cargo +1.88.0 check --workspace --locked`.
- Native Xvfb UI: a one-second 160×96 video was opened through the review form and imported into 15 editable frames with actual thumbnails and preview pixels. This is virtual X11 evidence, not GNOME/KDE Wayland certification.

Still pending: the rest of [LINUX-STATUS.md](LINUX-STATUS.md), including webcam/board sources, release packaging, additional effects and automation, localization, and compositor-specific acceptance. Large local-palette export memory use is the next active performance task; global-palette export retains its documented working-set bound.
