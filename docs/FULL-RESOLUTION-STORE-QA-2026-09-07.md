# Full-resolution sink/store QA — 2026-09-07

Result: **PASS**. This is an unpaced, synthetic-frame sink/persistence test. It is
**not** physical screen capture, a native 30 FPS measurement, a long-running
recorder test, GIF encoding, or power-loss/crash fault injection.

## Provenance and safety

- Local run started at `2026-09-06T22:20:32Z` (`2026-09-07 06:20:32 Asia/Shanghai`).
- Checkout base: `7f3e717fedd043155fa7ba2cd7b0841d4022837e`, with the pending ordered
  frame-editing cohort present in the worktree. This was not a clean-tag build.
- `rustc 1.98.0 (88d9e12ae 2026-08-18)`, LLVM `22.1.8`;
  `cargo 1.98.0 (797e8a9bc 2026-08-05)`.
- Target: `x86_64-unknown-linux-gnu`; Linux `6.8.0-106-generic`.
- Cargo test profile: **unoptimized + debuginfo**, not release.
- Test source: `crates/application/tests/full_resolution_recording.rs`;
  SHA-256 `73780619ae0814fb48b1c7ebcb7153e2e58042dc458f4f44e088a10aec5997e2`.
- Executed test binary: `target/debug/deps/full_resolution_recording-e76a96471acc9860`;
  SHA-256 `0b1c86de744085b9970392477e16246464e663b96d20729e05b0291a19edce22`.
  Both hashes were checked again after execution and were unchanged.
- `/tmp` had `605,586,046,976` bytes available before the run. The host reported
  60 GiB RAM and approximately 51 GiB available memory. These are shared-host
  observations, not exclusive benchmark allocations.
- The test was read completely before execution. It creates its own
  `tempfile::TempDir`; no existing project, source image, desktop, device, or
  host service was accessed or changed by this verification.
- A 280-second TERM deadline and 10-second KILL grace bounded the process group
  below five minutes. A pollable execution handle was retained. No timeout,
  signal, or manual cleanup was needed.

## Commands

Precompile without running the ignored test:

```sh
timeout --signal=TERM --kill-after=10s 280s \
  cargo +1.98.0 test -p gif-from-screen-application \
  --test full_resolution_recording --no-run
```

Measured run:

```sh
/usr/bin/time -v timeout --signal=TERM --kill-after=10s 280s \
  cargo +1.98.0 test -p gif-from-screen-application \
  --test full_resolution_recording \
  full_resolution_sink_only_recording_recovers_all_unique_assets \
  -- --ignored --exact --nocapture --test-threads=1
```

## Workload and assertions

The generated session emits 1,000 distinct solid-color `1280 × 720` RGBA8 frames
without waiting for real-time cadence. Each asset occupies `3,686,400` bytes;
all 1,000 independent assets occupy **3,686,400,000 bytes (3.433 GiB)**, below the
test's 4 GiB asset budget. Sink-only collection admits one normalized frame's
worth of buffer accounting; this is not an assertion that the whole process
uses only one frame of memory.

Synthetic timestamps are spaced by `33,333 µs`; the explicit final-frame delay
is `100,000 µs`. The asserted stored playback duration is therefore
`33,399,667 µs`. This duration is metadata, not elapsed capture time.

The test deliberately drops the incremental writer **without calling
`finish()`**, then opens the project with `LockPolicy::FailIfPresent`. Assertions
confirmed:

- 1,000 frames and 1,000 manifest assets; every frame identity and duration match.
- Collection transitions the generated session to `Stopped`.
- No asset issues and a clean journal recovery.
- **976 journal records replayed**, within the checkpoint-derived bound of
  `2 × 512 + 1 = 1,025` records.
- Every asset is read through the store's BLAKE3-verifying read API; byte lengths
  and sequence-color markers match. Disk asset count and total file sizes match
  the independent-frame expectation.
- Process high-water resident growth stays below the test's 256 MiB bound.

## Measurements

| Measurement | Observed result |
|---|---:|
| Persist/collect | 7.750570547 s |
| Reopen/recover | 1.213877571 s |
| Explicit verification of all assets | 0.681837915 s |
| Test harness elapsed | 9.99 s |
| Timed command wall clock | 10.04 s |
| Test-process RSS high-water baseline | 5,804 KiB |
| Test-process RSS high-water after capture | 27,888 KiB |
| Test-process final RSS high-water | 29,384 KiB |
| Test-process high-water growth | 23,580 KiB |
| Timed cargo/timeout command maximum RSS | 67,592 KiB |
| Timed command user / system CPU time | 3.40 s / 1.79 s |
| Major page faults / swaps | 0 / 0 |
| Exit status | 0 |

The process-local RSS figures come from `/proc/self/status` `VmHWM`. The larger
`/usr/bin/time` maximum includes the command tree; it is not the recorder's own
RSS. Shared-host cache state and concurrent load were not controlled. No native
capture throughput claim is derived from these timings.

## Cleanup

The test dropped the reopened project, explicitly closed its `TempDir`, and
asserted that the temporary project path no longer existed before printing:

```text
All benchmark assets and the temporary project were removed.
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The test process (`PID 1190122`) was also confirmed absent after completion.
All approximately 3.43 GiB of generated assets were test-owned temporary data
and were removed by the test; no user data was deleted. The ordinary Cargo
build artifacts remain in `target/`.
