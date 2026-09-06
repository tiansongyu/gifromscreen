# Capture cadence and GIF playback timing

Capture cadence decides **when to request another source sample**. Playback timing decides **how long each retained sample appears in the project and exported GIF**. These clocks are deliberately separate: changing a playback delay never rewrites native capture or input-event timestamps.

## Pinned ScreenToGif behavior

This audit uses ScreenToGif 2.43.2, commit `a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd`, not an assumption about the latest upstream release.

| Capture mode | Sampling interval | Playback delay |
| --- | --- | --- |
| Manual | Each accepted trigger, after optional trigger delay | `PlaybackDelayManual`, default 1,000 ms |
| Interaction | Supported input activity, after optional trigger delay | `PlaybackDelayInteraction`, default 500 ms |
| Per minute | Integer `60,000 / frequency` ms | `PlaybackDelayMinute`, default 66 ms |
| Per hour | Integer `3,600,000 / frequency` ms | `PlaybackDelayHour`, default 66 ms |
| Per second, measured | Integer `1,000 / FPS` ms target | Elapsed time between successful capture timing calls; first call uses the nominal interval |
| Per second, fixed | Integer `1,000 / FPS` ms target | Integer `1,000 / FPS` ms for each accepted frame |

The non-per-second modes always use their own fixed playback delay; `FixedFrameRate` only selects fixed timing for per-second capture. Trigger delay and sampling interval are separate settings. See [`BaseScreenRecorder.cs`, lines 61–109](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/BaseScreenRecorder.cs#L61), and the defaults in [`Settings.xaml`, lines 85–93](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Resources/Settings.xaml#L85).

In fixed mode, `FrameRate.GetMilliseconds()` returns the configured interval without reading a stopwatch. If processing exceeds the sampling interval, the recorder starts the next capture iteration without synthesizing missed frames. See [`Framerate.cs`, lines 37–74](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif.Util/Framerate.cs#L37), and [`BaseScreenRecorder.cs`, lines 185–239](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Controls/BaseScreenRecorder.cs#L185).

### Unchanged samples and delivery gaps

Ordinary unchanged-frame filtering does **not** accumulate fixed playback delay. The DirectX changed-only capturer returns before calling `FrameRate.GetMilliseconds()` when no usable resource or no changed region exists. Only an accepted frame receives the constant delay. Cursor changes can independently make a frame eligible. See [`DirectChangedImageCapture.cs`, lines 23–28, 84–90, 132–138, 271–275 and 316–321](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Capture/DirectChangedImageCapture.cs#L23), with the corresponding cached path in [`DirectChangedCachedCapture.cs`](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Capture/DirectChangedCachedCapture.cs#L17).

There is a distinct upstream **save-failure/black-frame exception**. GDI `CachedCapture` can queue a sample whose pixel read failed, or reject an all-black sample under `PreventBlackFrames`. Its save routine adds that already-assigned delay to the preceding stored frame, if one exists. This is not the changed-only early-return path and is not generalized into a fixed-delay accumulation rule. See [`CachedCapture.cs`, lines 65–75 and 172–189](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Capture/CachedCapture.cs#L172).

### Manual snapshot caveat

Upstream manual snapshots do not perform an RGBA equality comparison. However, the DirectX manual path may return the previous frame count if no new resource/update is available; the recorder only retries while the **total** frame count is zero. Consequently, upstream does not guarantee that every later manual click produces a frame. See [`DirectImageCapture.cs`, lines 623–649 and 712–716](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Capture/DirectImageCapture.cs#L623), and [`Recorder.xaml.cs`, lines 966–1011](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Windows/Recorder.xaml.cs#L966). The GDI path delegates to the ordinary successful screenshot capture and queues it without pixel comparison: [`BaseCapture.cs`, lines 144–151](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Capture/BaseCapture.cs#L144), [`ImageCapture.cs`, lines 55–74](https://github.com/NickeManarin/ScreenToGif/blob/a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd/ScreenToGif/Capture/ImageCapture.cs#L55).

Our explicit acknowledged manual-snapshot contract keeps one frame per successful trigger, including identical pixels under `ChangesOnly`. It does not reproduce that DirectX dropped-click limitation.

## Rust workflow contract

`CollectOptions.playback_timing` is `PlaybackTiming::Measured` by default, preserving existing callers. `PlaybackTiming::Fixed(Duration)` supplies one positive, checked microsecond duration per retained frame, including the final frame. The measured tail setting is ignored in fixed mode. Sampling cadence, collection limits, pause/resume and native timestamps are unaffected.

- Fixed `ChangesOnly` retains the first sample and meaningful pixel/input/cursor changes. Omitted samples and native delivery gaps add no playback time. Recorded event-only changes are not discarded merely because their pixels match.
- Measured `ChangesOnly` extends the previous retained frame until the next retained sample, and includes the observed omitted tail. This models a frame's presentation ending at the next sample. Upstream instead assigns the elapsed interval to the newly captured frame; the implementations are not claimed to be identical at this interval-assignment boundary.
- An incremental fixed-delay frame is journaled immediately with its final duration. It never needs a later `update_frame_duration`; measured frames retain the existing provisional-tail correction path.
- `CollectionSummary.capture_duration_us` and `WorkflowProgress.capture_duration` describe observed active capture time, excluding pauses and synthetic tail time. `duration_us` and `playback_duration` describe GIF/project playback. A configured capture duration is not a cap on fixed GIF playback.
- Original `CaptureMetadata` events remain untouched. Each newly persisted recording has its own `CaptureClockId`; `FrameClip.capture_clock.sampled_at` remains the source sampling instant. Shared project identifiers, labels, asset pixels and timestamp values are not evidence of a shared source clock.
- GIF export rounds cumulative presentation timestamps to 10 ms ticks. For example, three 66 ms project frames total 198 ms in the project and 200 ms in GIF, not three independently rounded 70 ms delays. Encoder duplicate-frame merging preserves that total presentation time and is separate from capture-time filtering.

## Integration verification and limits

`crates/application/tests/fixed_playback.rs` uses the public synthetic capture backend and real project storage. Each case exercises batch persistence plus two independent incremental recording writers. Incremental projects are reopened directly from their successful frame journal, without a final recording checkpoint. The resulting project is reopened again and exported through the real GIF renderer/encoder; the output is decoded and its frame delays summed.

Coverage includes wide manual capture gaps, identical acknowledged manual snapshots, pause/resume control, minute/hour intervals, fixed-FPS delivery gaps, fixed `ChangesOnly` with input-only samples, measured `ChangesOnly`, immutable raw input timestamps/positions/flags, matching clock identities within one recording, independent identities between recordings, and quantized GIF duration.

```sh
cargo +1.98.0 test -p gif-from-screen-application --test fixed_playback
cargo +1.98.0 clippy -p gif-from-screen-application --test fixed_playback -- -D warnings
cargo +1.88.0 test -p gif-from-screen-application --test fixed_playback
```

These are deterministic application integration tests, not a physical-device or compositor benchmark. The synthetic backend supplies already pause-excluded capture timestamps; the tests verify that collection and persistence do not reintroduce a pause gap, rather than claiming to validate a native backend's timestamp producer. Native manual/periodic scheduling, desktop policy controls and platform-specific delivery behavior need their own acceptance evidence. The save-failure exception above is documented, not emulated as an alternative silent data-loss policy.
