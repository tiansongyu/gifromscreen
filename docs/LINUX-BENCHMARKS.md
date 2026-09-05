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
