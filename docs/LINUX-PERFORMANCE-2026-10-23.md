# Linux GUI and recording pipeline verification — 2026-10-23

The Linux desktop now renders editor previews off the UI thread, coalesces recording notifications, shares tightly packed RGBA pixels, and writes recording frames on a bounded background worker. Recording completion still waits for durable assets and journal updates. These changes preserve the existing project schema and GIF encoding behavior.

Scope: development worktree based on `e48ccd0`, Rust 1.88.0, Linux x86_64. The measurements below exercise owned Xvfb servers and the actual desktop recording worker. They do **not** establish physical-display latency, GPU presentation FPS, or Wayland Portal performance. Compact machine-readable evidence is in [the results JSON](LINUX-PERFORMANCE-2026-10-23.json).

## Changes

| Path | Behavior |
| --- | --- |
| [editor_preview_async.rs](../apps/desktop/src/editor_preview_async.rs) | One worker, one replaceable pending request and one completed result. Disk reads, rendering, transitions, resizing and RGBA-to-display conversion run in the background. Revision/root/generation checks reject stale results; playback keeps an active render rather than repeatedly cancelling it. |
| [editor_canvas.rs](../apps/desktop/src/editor_canvas.rs), [main.rs](../apps/desktop/src/main.rs) | Ready, loading and error previews share canvas geometry. A previous preview can appear while loading, with editing disabled. Long errors scroll inside the reserved viewport. Retry remains available. |
| [recording_messages.rs](../apps/desktop/src/recording_messages.rs) | Latest progress replaces previous progress. Pause/resume acknowledgements survive coalescing. Completion has priority and cannot sit behind a frame-sized backlog of progress messages. UI processing is bounded to three notifications per frame. |
| [capture/frame.rs](../crates/capture/src/frame.rs), [gif/frame.rs](../crates/gif/src/frame.rs), [workflow/collect.rs](../crates/workflow/src/collect.rs) | Tight RGBA shares the native immutable allocation through normalization and sink submission, removing two full-frame copies in the desktop path. Padded/BGRA input still converts correctly. Existing owned-Vec construction remains a move. |
| [recording_writer.rs](../apps/desktop/src/recording_writer.rs) | FIFO writer with a 96 MiB pixel/metadata budget and 64 data-command limit, including in-flight work. Capture admission does not wait for disk I/O. One reserved flush slot cannot overtake data. |
| [workflow/frame_sink.rs](../crates/workflow/src/frame_sink.rs) | Explicit enqueue and durability-barrier operations. Existing synchronous append/update contracts remain durable. Successful collection and manual snapshot acknowledgement flush queued frames and duration corrections. |
| [main.rs](../apps/desktop/src/main.rs), [x11_controller_ui.rs](../apps/desktop/src/x11_controller_ui.rs) | Automatic stopping and error draining enter Finalizing and disable retargeting. A destructive finalizing action is labelled Discard, consistently in X11 and Wayland. |

The queue budget is not a whole-process RSS limit: capture buffers, asset verification, preview textures and growing project metadata have separate costs. No zero-copy claim is made for X11 capture, color conversion, persistence or GIF quantization.

If storage cannot keep up, recording stops with an explicit recoverable error; it does not silently lower FPS/resolution or discard archival frames. Previously accepted work drains before recovery is offered. A write failure preserves the durable prefix. Explicit discard waits for the in-flight writer to exit before deleting its project, preventing late writes into a deleted directory.

Accepted frames are not yet durable frames. Normal stop, successful completion and manual snapshot acknowledgement wait for durability; a process kill can lose the uncommitted queue tail. Asset/journal synchronization has not been removed to obtain the performance improvement. Raw RGBA assets remain disk intensive; the versioned compressed-spool proposal is separate work.

## Recording measurements

Environment: AMD Ryzen 9 9950X, approximately 60.5 GiB RAM, Linux 6.8.0-106-generic / glibc 2.35, NVMe filesystem, release test executable. The source is a numbered Tk window with changing background colors and a moving marker. Its submitted update cadence is measured separately. No application GUI renderer or physical display is involved in these capture/writer measurements.

| Run | Captured frames | Sampling FPS from capture timestamps | Interval P95 / P99 | Different pixel assets | Stop/drain | Final checkpoint |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1920×1080, 60 FPS, 5 s | 300 | 59.9994 | 16.672 / 16.685 ms | 300 | 2.069 ms | 5.037 ms |
| 1920×1080, 60 FPS, 30 s | 1800 | 59.9999 | 16.679 / 16.692 ms | 1327 | 2.135 ms | 8.179 ms |
| CI-sized debug smoke, 320×240, 10 FPS, 2 s | 20 | 9.9997 | 100.054 / 100.054 ms | 20 | 1.298 ms | 4.814 ms |

All runs completed and reopened with strictly increasing timestamps, matching saved manifests and successful full asset-digest verification. Playback duration was exactly the requested 5/30/2 seconds. The 30-second run crosses the existing 512-frame recording checkpoint interval three times. Source-submission FPS was approximately 59.997, 60.002 and 9.998 respectively.

These are **sampling-cadence** results. X11 timestamps are taken before GetImage, not native presentation PTS. The independently scheduled source and capture loops can sample the same image twice or miss a source update; 1800 samples do not mean 1800 distinct source images. The 30-second run stored 1327 distinct assets, approximately 10.25 GiB. The 5-second run stored 300 distinct assets, approximately 2.32 GiB. Neither result guarantees all physical 60 Hz updates are captured or every disk sustains the worst-case raw data rate.

Reproduce the release measurements with a new output directory:

```sh
python3 scripts/qa/recording_perf.py --output-dir /tmp/gfs-perf-new-5s --seconds 5
python3 scripts/qa/recording_perf.py --output-dir /tmp/gfs-perf-new-30s --seconds 30
```

Requirements: repository Rust toolchain/native build dependencies, Python 3 with Tk, and Xvfb. The harness builds a release desktop test binary, checks conservative free-space requirements, starts its own X server and fixture, and executes the GUI's incremental-recording function. It verifies the selected Xvfb PID owns that display's listening sockets; it never falls back to the host DISPLAY. Reports retain raw timestamps, source samples, environment/build hashes and recovery outcomes. Captured projects are large; retain the JSON/logs and remove only your generated test projects when no longer needed.

## GUI and input verification

The rebuilt desktop binary completed the existing owned-Xvfb demo workflow: home → recorder → recording → editor/playback → GIF export. Screenshots were inspected for recorder/editor layout and overlap. The exported GIF decoded as 640×480, 62 frames, 3100 ms. The application used GL/llvmpipe software rendering; this is a GUI workflow check, not a hardware presentation benchmark.

Desktop binary SHA-256:

```text
39ce6ebd2c7b84574a03ee50b847ce529727bd03c73dd7cba65e10f689190b11
```

Reproduce with a fresh output directory:

```sh
cargo build --release --locked -p gif-from-screen
python3 scripts/demo/record_readme.py \
  --binary "$PWD/target/release/gif-from-screen" \
  --output-dir /tmp/gfs-gui-new-run --version 0.1.6
```

Two existing isolated native-guide regressions also passed: 120 geometry submissions had server-confirmed P95 0.122 ms; direct pointer-to-window movement without UI polling had P95 0.390 ms. They validate the native fast path and final acknowledgement, **not input-to-photon latency**.

## Automated regressions and CI

Final local results: formatting and workspace Clippy with `-D warnings` passed; workspace tests passed **1947**, failed **0**, ignored **63**. The ignored native-guide tests and desktop benchmark described above were run separately in owned environments. The benchmark harness's six Python unit tests also passed.

Relevant checks include:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
python3 scripts/qa/test_recording_perf.py
```

New regressions cover shared allocation identity/lifetime, padded/BGRA correctness, deferred flush failures, manual snapshot acknowledgement barriers, byte/command limits including in-flight data, reserved flush admission while full, writer panic wakeup, discard cleanup ordering, journal recovery, coalesced pause acknowledgement, stale preview rejection, retry, and stacked-layout stability across loading/error/ready states. Existing home-size restoration, EN/ZH text-layout, DPI/zoom, geometry and retarget tests remain enabled.

[Linux CI](../.github/workflows/ci.yml) now runs the harness unit tests and a small debug desktop capture/recovery smoke on an owned X11 server, with JSON/log diagnostics uploaded separately. That smoke is deliberately not a 1080p60 performance gate.

Still unqualified here: physical X11 presentation/input latency, 120/144 Hz, 4K, mixed-DPI hardware, ten-minute recording, and live Wayland Portal/PipeWire capture. Use the [performance protocol](CROSS-PLATFORM-PERFORMANCE.md) for those gates; this report does not promote them to supported performance guarantees.
