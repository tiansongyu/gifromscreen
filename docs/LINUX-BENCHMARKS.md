# Linux performance evidence

This ledger records reproducible performance checks without promoting a
micro-benchmark into a real-machine release claim.

## 2026-09-05 — incremental project durability preflight

Command:

```bash
cargo test -p gif-from-screen-application \
  ten_thousand_frame_incremental_recording_has_bounded_replay_tail \
  --all-features -- --ignored --nocapture
```

Debug-build result in the development container:

- 10,000 ordered 1×1 RGBA frames appended to one recoverable project.
- Every preceding provisional duration was corrected through the durable
  journal; identical pixel assets were content-deduplicated.
- Persistence and duration correction completed in `21.151031602s`.
- Automatic checkpoint/compaction ran every 512 appended frames.
- Reopening required 545 journal records and reconstructed all 10,000 frames.
- The complete test, including reopen/replay assertions, completed in `27.93s`.

The test guards the algorithmic and recovery properties of the writer: normal
append and duration correction use indexed journal fast paths rather than
cloning and validating the full accumulated timeline, and the recovery tail is
bounded to at most `2 × 512 + 1` records for the current two-records-per-frame
workflow.

This is not the S0 full-resolution throughput gate. The remaining release test
must record 1080p@30 for 30 minutes and 4K@15 for 10 minutes on representative
X11 and Wayland machines while measuring RSS, disk throughput, dropped-frame
telemetry, recovery latency, and final timing accuracy.

## 2026-09-06 — native X11 checks on a virtual display

Command:

```bash
DISPLAY=:0 cargo test -p gif-from-screen-capture-linux --features native-x11 \
  real_x11_smoke_test_when_display_is_available -- --nocapture
```

Passed in `0.06s` against an existing Xvfb display: X.Org `1.21.1.4`,
`1440×1000`, root depth 24. The display was reachable, and none of the test's
skip branches were taken. The check exercised native screen-region reads,
moving a fixed-size region, rejection of canvas resizing and invalid targets,
XFixes cursor metadata and embedded cursor capture, and an available window.
It did not move, resize, draw into, or otherwise operate the desktop windows.

Xvfb exercises the X11 adapter and protocol boundary. It is not evidence of
GNOME/KDE Wayland portal behavior, physical multi-monitor coordinate accuracy,
or sustained capture on a compositor-driven desktop.

## 2026-09-06 — full-resolution sink-only persistence

Command:

```bash
cargo test -p gif-from-screen-application --test full_resolution_recording \
  -- --ignored --nocapture
```

Environment: debug build, Rust `1.97.1`, Linux `6.8.0-106-generic` x86-64,
AMD Ryzen 9 9950X, ext4-backed temporary directory. This was a single run in
the development environment, not an isolated hardware comparison.

The source generated 1,000 distinct `1280×720` RGBA frames on demand. Each
frame used a unique solid color, avoiding asset deduplication while keeping
generator cost small. Frames passed through the real workflow's sink-only
collector into `IncrementalRecordingProject`; the source never accumulated a
frame list. The collector's normalized-frame budget was exactly one frame,
`3,686,400` bytes. Each provisional delay began at 100 ms; the following sample
corrected 999 preceding delays to 33,333 µs, exercising actual journal writes
rather than same-duration no-ops. The final frame retained its 100 ms tail.

| Measurement | Observed value |
| --- | ---: |
| Distinct full-resolution assets | 1,000 |
| Total asset bytes | 3,686,400,000 (3.43 GiB) |
| Generate, normalize, persist, and correct durations | 7.512195579 s |
| Open unfinalized project and recover journal | 1.161142924 s |
| Read and verify every asset digest, ID, and delay | 0.639357155 s |
| Records replayed after the 512-frame checkpoint | 976 |
| Process VmHWM before collection | 5,140 KiB |
| Process VmHWM after collection | 26,968 KiB |
| Process VmHWM after all verification | 27,880 KiB |
| Entire test, including cleanup | 9.66 s |

All 1,000 ordered frame IDs, corrected durations, canvas-sized pixel buffers,
asset digests, and distinct on-disk files were verified. The recovered timeline
duration was exactly `33,399,667` µs. Reopening deliberately followed dropping
the writer without `finish()`, so the test exercised an uncompacted journal
tail; it did not simulate abrupt process termination or a stale lock. The
temporary project and all 3.43 GiB of generated assets were removed, and the
test asserted that the temporary path no longer existed. A compile-time
4 GiB asset cap and a 256 MiB process-RSS-growth assertion bound this opt-in
test's resource use.

This establishes full-resolution incremental storage and recovery behavior,
which the earlier 1×1 test could not measure. It does not measure native
capture rate: generation was unpaced, timestamps were synthetic, and GIF
encoding, compositor interaction, dropped-frame telemetry, cold-cache disk
performance, and long-duration recording were outside the measurement.
The 1080p/4K sustained native release gates above remain open.
