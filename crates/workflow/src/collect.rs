use std::time::{Duration, Instant};

use gif_from_screen_capture::{
    CaptureBackend, CaptureError, CaptureErrorKind, CaptureRequest, CaptureSession,
    CaptureSessionState, CapturedFrame, FramePoll, PhysicalSize, PixelFormat, RecoveryHint,
};
use gif_from_screen_gif::{CancellationToken, RgbaFrame};

use crate::{
    RecordingControl, RecordingFrameSink, RecordingFrameSinkOperation, WorkflowError,
    WorkflowPhase, WorkflowProgress, WorkflowProgressSink,
    control::{ControlOutcome, SnapshotTriggerRejection},
};

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_TAIL_FRAME_DURATION: Duration = Duration::from_millis(100);
const DEFAULT_FRAME_BUFFER_LIMIT_BYTES: u64 = 512 * 1024 * 1024;

/// Normal completion condition for frame collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CollectionLimit {
    /// Continue until the source ends or a controlled recording receives Stop.
    ///
    /// Callers using an endless native source should use
    /// [`crate::collect_controlled`] or provide a cancellation token.
    UntilStopped,
    /// Stop after this span of session timestamps, measured from the first
    /// retained frame. A boundary frame at or beyond the span is not retained.
    Duration(Duration),
    /// Stop immediately after retaining this many frames.
    MaxFrames(u64),
}

/// Controls whether unchanged native samples occupy their own stored frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FrameRetention {
    /// Retain every normalized native frame.
    #[default]
    All,
    /// Retain the first frame and frames whose normalized RGBA pixels differ
    /// from the preceding retained frame.
    ///
    /// Skipped samples still extend presentation time. This reduces project
    /// memory without changing the animation observed at any timestamp.
    ChangesOnly,
}

/// Bounds and timing policy for frame collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectOptions {
    /// Normal completion condition.
    pub limit: CollectionLimit,
    /// Pixel-change filtering applied after native format normalization.
    pub frame_retention: FrameRetention,
    /// Maximum normalized RGBA bytes retained before returning an error.
    pub frame_buffer_limit_bytes: u64,
    /// Maximum duration of an individual blocking capture poll.
    pub poll_interval: Duration,
    /// Duration assigned to the last frame when no duration boundary supplies
    /// an exact ending timestamp.
    pub tail_frame_duration: Duration,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            limit: CollectionLimit::Duration(Duration::from_secs(5)),
            frame_retention: FrameRetention::default(),
            frame_buffer_limit_bytes: DEFAULT_FRAME_BUFFER_LIMIT_BYTES,
            poll_interval: DEFAULT_POLL_INTERVAL,
            tail_frame_duration: DEFAULT_TAIL_FRAME_DURATION,
        }
    }
}

/// Aggregate metadata for a completed collection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CollectionSummary {
    /// Number of normalized frames retained.
    pub frames: u64,
    /// Sum of the presentation durations assigned to retained frames.
    pub duration_us: u64,
    /// Bytes occupied by normalized RGBA pixel buffers.
    pub rgba_bytes: u64,
}

/// Owned, normalized frames ready for a GIF encoder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectedRecording {
    frames: Vec<RgbaFrame>,
    summary: CollectionSummary,
}

impl CollectedRecording {
    /// Returns all frames in presentation order.
    pub fn frames(&self) -> &[RgbaFrame] {
        &self.frames
    }

    /// Returns aggregate collection metadata.
    pub const fn summary(&self) -> CollectionSummary {
        self.summary
    }

    /// Consumes the collection and returns its encoder frames.
    pub fn into_frames(self) -> Vec<RgbaFrame> {
        self.frames
    }
}

#[derive(Debug)]
struct NormalizedCapture {
    timestamp_us: u64,
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}

struct CollectionFinishState {
    captures: Vec<NormalizedCapture>,
    rgba_bytes: u64,
    first_timestamp: Option<u64>,
    last_observed_timestamp: Option<u64>,
}

#[derive(Default)]
struct SinkOnlyCollectionState {
    previous_timestamp: Option<u64>,
    previous_sequence: Option<u64>,
    first_timestamp: Option<u64>,
    last_observed_timestamp: Option<u64>,
    last_retained: Option<NormalizedCapture>,
    retained_frames: usize,
    stream_index: u64,
    rgba_bytes: u64,
    finalized_duration_us: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SinkFrameOutcome {
    Continue,
    DurationReached,
    FrameLimitReached,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopReason {
    DurationReached,
    FrameLimitReached,
    EndOfStream,
    UserStopped,
}

#[derive(Clone, Copy, Debug)]
struct ValidatedOptions {
    limit: ValidatedLimit,
    frame_retention: FrameRetention,
    poll_interval: Duration,
    tail_duration_us: u64,
    frame_buffer_limit_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
enum ValidatedLimit {
    UntilStopped,
    Duration {
        duration_us: u64,
        deadline: Option<Instant>,
    },
    MaxFrames(u64),
}

/// Starts a capture session and synchronously collects normalized RGBA frames.
///
/// Consecutive frame durations come exclusively from the session-relative,
/// monotonic capture timestamps. For duration-limited collection, the final
/// retained frame ends exactly at the requested timestamp span. For frame-count
/// limits or an early end-of-stream, `tail_frame_duration` supplies the
/// otherwise unknowable final duration.
///
/// On any failure or cancellation, the active native session is discarded on
/// a best-effort basis. The function is intentionally synchronous and can be
/// moved wholesale to an application-owned background thread.
///
/// # Errors
///
/// Returns [`WorkflowError`] for invalid options, capture failures, malformed
/// frame streams, GIF-incompatible dimensions, memory-bound violations,
/// cancellation, or an empty stream.
pub fn collect(
    backend: &dyn CaptureBackend,
    request: CaptureRequest,
    options: &CollectOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectedRecording, WorkflowError> {
    collect_internal(
        backend,
        request,
        options,
        cancellation,
        progress,
        None,
        None,
    )
}

/// Starts a capture session that can be paused, resumed, stopped, or discarded.
///
/// A stop request keeps collected frames and completes normally. A discard
/// request returns [`WorkflowError::Discarded`] and best-effort discards the
/// native session. Control commands are observed at most one `poll_interval`
/// after they are sent. With [`gif_from_screen_capture::CaptureCadence::Manual`],
/// no native frame is admitted until [`crate::RecordingController::trigger_snapshot`]
/// establishes a fresh-frame boundary. Each accepted trigger retains exactly
/// one frame even when `frame_retention` is [`FrameRetention::ChangesOnly`].
///
/// # Errors
///
/// Returns [`WorkflowError`] for invalid options, capture/control failures,
/// cancellation, discard, malformed frames, resource limits, or an empty
/// recording.
pub fn collect_controlled(
    backend: &dyn CaptureBackend,
    request: CaptureRequest,
    options: &CollectOptions,
    control: &mut RecordingControl,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectedRecording, WorkflowError> {
    collect_internal(
        backend,
        request,
        options,
        cancellation,
        progress,
        Some(control),
        None,
    )
}

/// Starts controlled capture while durably observing every retained frame.
///
/// Each retained frame is sent to `sink` immediately with the configured tail
/// duration as a provisional value. The preceding duration is corrected when
/// the next retained timestamp arrives, and the final duration is corrected at
/// the stop boundary. Sink calls run synchronously on the capture worker so a
/// successful return means the corresponding journal operation completed.
///
/// # Errors
///
/// Returns [`WorkflowError`] for the same conditions as [`collect_controlled`],
/// or [`WorkflowError::FrameSink`] when incremental persistence fails.
pub fn collect_controlled_with_sink(
    backend: &dyn CaptureBackend,
    request: CaptureRequest,
    options: &CollectOptions,
    control: &mut RecordingControl,
    sink: &mut dyn RecordingFrameSink,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectedRecording, WorkflowError> {
    collect_internal(
        backend,
        request,
        options,
        cancellation,
        progress,
        Some(control),
        Some(sink),
    )
}

/// Captures directly into a durable sink without retaining the full recording in memory.
///
/// Only the most recent retained RGBA frame is kept for dimension and
/// `ChangesOnly` comparisons. `CollectionSummary::rgba_bytes` still reports the
/// cumulative bytes sent to the sink, while `frame_buffer_limit_bytes` bounds
/// each resident normalized frame rather than the whole on-disk recording.
/// Timing, pause, retarget, stop, and discard semantics match
/// [`collect_controlled_with_sink`].
///
/// # Errors
///
/// Returns [`WorkflowError`] for capture/control failures, invalid frames,
/// cancellation/discard, per-frame memory bounds, or sink persistence errors.
pub fn collect_controlled_to_sink(
    backend: &dyn CaptureBackend,
    request: CaptureRequest,
    options: &CollectOptions,
    control: &mut RecordingControl,
    sink: &mut dyn RecordingFrameSink,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectionSummary, WorkflowError> {
    let result = (|| {
        let mut options = validate_options(options)?;
        ensure_not_cancelled(cancellation)?;
        progress.report(WorkflowProgress::capture(
            WorkflowPhase::StartingCapture,
            0,
            Duration::ZERO,
        ));
        let mut session = backend.start_session(request)?;
        let result = collect_prestarted_controlled_to_sink_inner(
            &mut *session,
            &mut options,
            control,
            sink,
            cancellation,
            progress,
        );
        if result.is_err() {
            discard_best_effort(&mut *session);
        }
        result
    })();
    if let Err(error) = &result {
        control.reject_all_snapshots(&snapshot_rejection_for_error(error));
    }
    result
}

/// Collects an already-started controlled capture directly into a durable sink.
///
/// The session must initially be in [`CaptureSessionState::Recording`] or
/// [`CaptureSessionState::Paused`]. This function never creates or restarts a
/// capture session, allowing a caller to retain a session that was opened for
/// source selection or a frozen preview without displaying a second native
/// chooser. A paused session remains paused until its [`RecordingControl`]
/// receives a resume request.
///
/// Timing, pause, retarget, stop, discard, persistence, and memory-bound
/// semantics match [`collect_controlled_to_sink`]. On any error or cancellation,
/// the supplied session is discarded on a best-effort basis.
///
/// # Errors
///
/// Returns [`WorkflowError::Capture`] with
/// [`CaptureErrorKind::InvalidStateTransition`] when the session is not initially
/// recording or paused. Other errors match [`collect_controlled_to_sink`].
pub fn collect_prestarted_controlled_to_sink(
    session: &mut dyn CaptureSession,
    options: &CollectOptions,
    control: &mut RecordingControl,
    sink: &mut dyn RecordingFrameSink,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectionSummary, WorkflowError> {
    let result = (|| {
        let mut options = validate_options(options)?;
        ensure_not_cancelled(cancellation)?;
        progress.report(WorkflowProgress::capture(
            WorkflowPhase::StartingCapture,
            0,
            Duration::ZERO,
        ));
        collect_prestarted_controlled_to_sink_inner(
            session,
            &mut options,
            control,
            sink,
            cancellation,
            progress,
        )
    })();
    if let Err(error) = &result {
        discard_best_effort(session);
        control.reject_all_snapshots(&snapshot_rejection_for_error(error));
    }
    result
}

fn collect_prestarted_controlled_to_sink_inner(
    session: &mut dyn CaptureSession,
    options: &mut ValidatedOptions,
    control: &mut RecordingControl,
    sink: &mut dyn RecordingFrameSink,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectionSummary, WorkflowError> {
    validate_prestarted_session_state(session.state())?;
    reset_duration_deadline(&mut options.limit);
    collect_session_to_sink(session, *options, control, sink, cancellation, progress)
}

fn validate_prestarted_session_state(state: CaptureSessionState) -> Result<(), WorkflowError> {
    if matches!(
        state,
        CaptureSessionState::Recording | CaptureSessionState::Paused
    ) {
        return Ok(());
    }
    Err(CaptureError::new(
        CaptureErrorKind::InvalidStateTransition,
        format!(
            "cannot collect a pre-started capture session in {state:?} state; expected Recording or Paused"
        ),
        RecoveryHint::None,
    )
    .into())
}

fn collect_internal(
    backend: &dyn CaptureBackend,
    request: CaptureRequest,
    options: &CollectOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
    mut control: Option<&mut RecordingControl>,
    sink: Option<&mut dyn RecordingFrameSink>,
) -> Result<CollectedRecording, WorkflowError> {
    let result = (|| {
        let options = validate_options(options)?;
        ensure_not_cancelled(cancellation)?;
        progress.report(WorkflowProgress::capture(
            WorkflowPhase::StartingCapture,
            0,
            Duration::ZERO,
        ));

        let mut session = backend.start_session(request)?;
        let result = collect_session(
            &mut *session,
            options,
            cancellation,
            progress,
            control.as_deref_mut(),
            sink,
        );
        if result.is_err() {
            discard_best_effort(&mut *session);
        }
        result
    })();
    if let Err(error) = &result
        && let Some(control) = control
    {
        control.reject_all_snapshots(&snapshot_rejection_for_error(error));
    }
    result
}

fn validate_options(options: &CollectOptions) -> Result<ValidatedOptions, WorkflowError> {
    if options.poll_interval.is_zero() {
        return Err(WorkflowError::InvalidCollectionOption(
            "poll interval must be greater than zero".to_owned(),
        ));
    }
    let tail_duration_us =
        duration_to_nonzero_micros(options.tail_frame_duration, "tail frame duration")?;
    let limit = match options.limit {
        CollectionLimit::UntilStopped => ValidatedLimit::UntilStopped,
        CollectionLimit::Duration(duration) => {
            let duration_us = duration_to_nonzero_micros(duration, "collection duration")?;
            ValidatedLimit::Duration {
                duration_us,
                deadline: None,
            }
        }
        CollectionLimit::MaxFrames(0) => {
            return Err(WorkflowError::InvalidCollectionOption(
                "maximum frame count must be greater than zero".to_owned(),
            ));
        }
        CollectionLimit::MaxFrames(frames) => ValidatedLimit::MaxFrames(frames),
    };
    Ok(ValidatedOptions {
        limit,
        frame_retention: options.frame_retention,
        poll_interval: options.poll_interval,
        tail_duration_us,
        frame_buffer_limit_bytes: options.frame_buffer_limit_bytes,
    })
}

fn duration_to_nonzero_micros(duration: Duration, name: &str) -> Result<u64, WorkflowError> {
    let micros = u64::try_from(duration.as_micros()).map_err(|_| {
        WorkflowError::InvalidCollectionOption(format!(
            "{name} is too large for capture timestamps"
        ))
    })?;
    if micros == 0 {
        return Err(WorkflowError::InvalidCollectionOption(format!(
            "{name} must be at least one microsecond"
        )));
    }
    Ok(micros)
}

#[allow(
    clippy::too_many_lines,
    reason = "the full-buffer collector keeps control ordering, manual admission, timing, and persistence in one auditable loop"
)]
fn collect_session(
    session: &mut dyn CaptureSession,
    mut options: ValidatedOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
    mut control: Option<&mut RecordingControl>,
    mut sink: Option<&mut dyn RecordingFrameSink>,
) -> Result<CollectedRecording, WorkflowError> {
    let mut captures = Vec::new();
    let mut rgba_bytes = 0_u64;
    let mut previous_timestamp = None;
    let mut previous_sequence = None;
    let mut first_timestamp = None;
    let mut last_observed_timestamp = None;
    let mut paused_at = None;
    let mut stream_index = 0_u64;
    let controlled_manual = control.is_some()
        && matches!(
            session.request().cadence,
            gif_from_screen_capture::CaptureCadence::Manual
        );
    reset_duration_deadline(&mut options.limit);

    let stop_reason = loop {
        ensure_not_cancelled(cancellation)?;
        let may_apply_commands = !controlled_manual
            || control
                .as_ref()
                .is_some_and(|control| !control.has_pending_snapshot());
        if may_apply_commands && let Some(control) = control.as_deref_mut() {
            match control.apply_pending(session)? {
                ControlOutcome::Continue => {}
                ControlOutcome::Stop => break StopReason::UserStopped,
                ControlOutcome::Discard => return Err(WorkflowError::Discarded),
            }
        }
        if session.state() == CaptureSessionState::Paused {
            paused_at.get_or_insert_with(Instant::now);
            report_collection_progress(
                progress,
                WorkflowPhase::Paused,
                captures.len(),
                first_timestamp,
                last_observed_timestamp,
            );
            std::thread::sleep(options.poll_interval);
            continue;
        }
        if let Some(started) = paused_at.take() {
            extend_duration_deadline(&mut options.limit, started.elapsed())?;
        }
        if duration_deadline_reached(options.limit) {
            break StopReason::DurationReached;
        }
        if controlled_manual
            && control
                .as_ref()
                .is_some_and(|control| !control.has_pending_snapshot())
        {
            sleep_until_control_poll(options.limit, options.poll_interval);
            continue;
        }
        let poll_interval = bounded_poll_interval(options.limit, options.poll_interval);
        match session.poll_frame(poll_interval)? {
            FramePoll::Frame(frame) => {
                ensure_not_cancelled(cancellation)?;
                let next_stream_index = stream_index
                    .checked_add(1)
                    .ok_or_else(|| collection_counter_overflow("native frame index"))?;
                validate_order(&frame, stream_index, previous_timestamp, previous_sequence)?;
                let timestamp_us = frame.captured_at().as_micros();
                previous_timestamp = Some(timestamp_us);
                previous_sequence = Some(frame.sequence());

                let is_first = first_timestamp.is_none();
                let capture_start = *first_timestamp.get_or_insert(timestamp_us);
                if is_first {
                    arm_duration_deadline(&mut options.limit)?;
                }
                if duration_span_reached(options.limit, capture_start, timestamp_us) {
                    break StopReason::DurationReached;
                }
                last_observed_timestamp = Some(timestamp_us);

                let normalized = normalize_frame(&frame, stream_index, captures.first())?;
                let retain = controlled_manual
                    || options.frame_retention == FrameRetention::All
                    || captures
                        .last()
                        .is_none_or(|previous| previous.pixels != normalized.pixels);
                if retain {
                    retain_normalized_capture(
                        &mut captures,
                        &mut rgba_bytes,
                        normalized,
                        options,
                        &mut sink,
                    )?;
                }
                if controlled_manual && let Some(control) = control.as_deref_mut() {
                    control.complete_snapshot(&frame, retain);
                }
                stream_index = next_stream_index;

                report_collection_progress(
                    progress,
                    WorkflowPhase::Capturing,
                    captures.len(),
                    first_timestamp,
                    last_observed_timestamp,
                );
                if frame_limit_reached(options.limit, captures.len()) {
                    break StopReason::FrameLimitReached;
                }
            }
            FramePoll::Pending => {}
            FramePoll::EndOfStream => break StopReason::EndOfStream,
        }
    };

    if let Some(control) = control {
        control.reject_all_snapshots(&snapshot_rejection_for_stop(stop_reason));
    }

    finish_session_collection(
        session,
        CollectionFinishState {
            captures,
            rgba_bytes,
            first_timestamp,
            last_observed_timestamp,
        },
        stop_reason,
        options,
        sink,
        progress,
    )
}

fn collect_session_to_sink(
    session: &mut dyn CaptureSession,
    mut options: ValidatedOptions,
    control: &mut RecordingControl,
    sink: &mut dyn RecordingFrameSink,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectionSummary, WorkflowError> {
    let mut state = SinkOnlyCollectionState::default();
    let mut paused_at = None;
    let controlled_manual = matches!(
        session.request().cadence,
        gif_from_screen_capture::CaptureCadence::Manual
    );
    let stop_reason = loop {
        ensure_not_cancelled(cancellation)?;
        if !controlled_manual || !control.has_pending_snapshot() {
            match control.apply_pending(session)? {
                ControlOutcome::Continue => {}
                ControlOutcome::Stop => break StopReason::UserStopped,
                ControlOutcome::Discard => return Err(WorkflowError::Discarded),
            }
        }
        if session.state() == CaptureSessionState::Paused {
            paused_at.get_or_insert_with(Instant::now);
            report_collection_progress(
                progress,
                WorkflowPhase::Paused,
                state.retained_frames,
                state.first_timestamp,
                state.last_observed_timestamp,
            );
            std::thread::sleep(options.poll_interval);
            continue;
        }
        if let Some(started) = paused_at.take() {
            extend_duration_deadline(&mut options.limit, started.elapsed())?;
        }
        if duration_deadline_reached(options.limit) {
            break StopReason::DurationReached;
        }
        if controlled_manual && !control.has_pending_snapshot() {
            sleep_until_control_poll(options.limit, options.poll_interval);
            continue;
        }
        let poll_interval = bounded_poll_interval(options.limit, options.poll_interval);
        match session.poll_frame(poll_interval)? {
            FramePoll::Frame(frame) => {
                ensure_not_cancelled(cancellation)?;
                let (outcome, retained) =
                    state.observe_frame(&frame, &mut options, sink, controlled_manual)?;
                if controlled_manual && let Some(retained) = retained {
                    control.complete_snapshot(&frame, retained);
                }
                match outcome {
                    SinkFrameOutcome::Continue => {}
                    SinkFrameOutcome::DurationReached => break StopReason::DurationReached,
                    SinkFrameOutcome::FrameLimitReached => break StopReason::FrameLimitReached,
                }
                report_collection_progress(
                    progress,
                    WorkflowPhase::Capturing,
                    state.retained_frames,
                    state.first_timestamp,
                    state.last_observed_timestamp,
                );
            }
            FramePoll::Pending => {}
            FramePoll::EndOfStream => break StopReason::EndOfStream,
        }
    };
    control.reject_all_snapshots(&snapshot_rejection_for_stop(stop_reason));
    state.finish(session, sink, stop_reason, options, progress)
}

impl SinkOnlyCollectionState {
    fn observe_frame(
        &mut self,
        frame: &CapturedFrame,
        options: &mut ValidatedOptions,
        sink: &mut dyn RecordingFrameSink,
        force_retain: bool,
    ) -> Result<(SinkFrameOutcome, Option<bool>), WorkflowError> {
        let next_stream_index = self
            .stream_index
            .checked_add(1)
            .ok_or_else(|| collection_counter_overflow("native frame index"))?;
        validate_order(
            frame,
            self.stream_index,
            self.previous_timestamp,
            self.previous_sequence,
        )?;
        let timestamp_us = frame.captured_at().as_micros();
        self.previous_timestamp = Some(timestamp_us);
        self.previous_sequence = Some(frame.sequence());
        let is_first = self.first_timestamp.is_none();
        let capture_start = *self.first_timestamp.get_or_insert(timestamp_us);
        if is_first {
            arm_duration_deadline(&mut options.limit)?;
        }
        if duration_span_reached(options.limit, capture_start, timestamp_us) {
            return Ok((SinkFrameOutcome::DurationReached, None));
        }
        self.last_observed_timestamp = Some(timestamp_us);

        let normalized = normalize_frame(frame, self.stream_index, self.last_retained.as_ref())?;
        let retain = force_retain
            || options.frame_retention == FrameRetention::All
            || self
                .last_retained
                .as_ref()
                .is_none_or(|previous| previous.pixels != normalized.pixels);
        if retain {
            self.persist_frame(normalized, *options, sink)?;
        }
        self.stream_index = next_stream_index;
        if frame_limit_reached(options.limit, self.retained_frames) {
            Ok((SinkFrameOutcome::FrameLimitReached, Some(retain)))
        } else {
            Ok((SinkFrameOutcome::Continue, Some(retain)))
        }
    }

    fn persist_frame(
        &mut self,
        normalized: NormalizedCapture,
        options: ValidatedOptions,
        sink: &mut dyn RecordingFrameSink,
    ) -> Result<(), WorkflowError> {
        let frame_bytes = u64::try_from(normalized.pixels.len())
            .map_err(|_| collection_counter_overflow("RGBA byte count"))?;
        let next_rgba_bytes = self
            .rgba_bytes
            .checked_add(frame_bytes)
            .ok_or_else(|| collection_counter_overflow("RGBA byte count"))?;
        let next_retained_frames = self
            .retained_frames
            .checked_add(1)
            .ok_or_else(|| collection_counter_overflow("retained frame count"))?;
        if frame_bytes > options.frame_buffer_limit_bytes {
            return Err(WorkflowError::FrameBufferLimitExceeded {
                required_bytes: frame_bytes,
                limit_bytes: options.frame_buffer_limit_bytes,
            });
        }
        let frame_index = u64::try_from(self.retained_frames)
            .map_err(|_| collection_counter_overflow("retained frame count"))?;
        if let Some(previous) = &self.last_retained {
            let duration_us = normalized.timestamp_us - previous.timestamp_us;
            update_sink_duration(sink, frame_index - 1, duration_us)?;
            self.finalized_duration_us = self
                .finalized_duration_us
                .checked_add(duration_us)
                .ok_or_else(duration_overflow_error)?;
        }
        append_sink_frame(sink, frame_index, &normalized, options.tail_duration_us)?;
        self.rgba_bytes = next_rgba_bytes;
        self.retained_frames = next_retained_frames;
        self.last_retained = Some(normalized);
        Ok(())
    }

    fn finish(
        mut self,
        session: &mut dyn CaptureSession,
        sink: &mut dyn RecordingFrameSink,
        stop_reason: StopReason,
        options: ValidatedOptions,
        progress: &mut dyn WorkflowProgressSink,
    ) -> Result<CollectionSummary, WorkflowError> {
        report_collection_progress(
            progress,
            WorkflowPhase::StoppingCapture,
            self.retained_frames,
            self.first_timestamp,
            self.last_observed_timestamp,
        );
        let last_retained_timestamp = self
            .last_retained
            .as_ref()
            .ok_or(WorkflowError::EmptyCapture)?
            .timestamp_us;
        let first_timestamp = self.first_timestamp.ok_or(WorkflowError::EmptyCapture)?;
        let last_duration_us = calculate_last_duration_values(
            first_timestamp,
            last_retained_timestamp,
            self.last_observed_timestamp,
            stop_reason,
            options,
        )?;
        let final_index = u64::try_from(self.retained_frames - 1)
            .map_err(|_| collection_counter_overflow("retained frame count"))?;
        update_sink_duration(sink, final_index, last_duration_us)?;
        self.finalized_duration_us = self
            .finalized_duration_us
            .checked_add(last_duration_us)
            .ok_or_else(duration_overflow_error)?;
        stop_session_if_live(session)?;
        Ok(CollectionSummary {
            frames: u64::try_from(self.retained_frames)
                .map_err(|_| collection_counter_overflow("retained frame count"))?,
            duration_us: self.finalized_duration_us,
            rgba_bytes: self.rgba_bytes,
        })
    }
}

fn duration_overflow_error() -> WorkflowError {
    WorkflowError::InvalidCollectionOption(
        "collected presentation duration overflowed u64 microseconds".to_owned(),
    )
}

const fn collection_counter_overflow(counter: &'static str) -> WorkflowError {
    WorkflowError::CollectionCounterOverflow { counter }
}

fn retain_normalized_capture(
    captures: &mut Vec<NormalizedCapture>,
    rgba_bytes: &mut u64,
    normalized: NormalizedCapture,
    options: ValidatedOptions,
    sink: &mut Option<&mut dyn RecordingFrameSink>,
) -> Result<(), WorkflowError> {
    let frame_bytes = u64::try_from(normalized.pixels.len())
        .map_err(|_| collection_counter_overflow("RGBA byte count"))?;
    let required_bytes = rgba_bytes
        .checked_add(frame_bytes)
        .ok_or_else(|| collection_counter_overflow("RGBA byte count"))?;
    if required_bytes > options.frame_buffer_limit_bytes {
        return Err(WorkflowError::FrameBufferLimitExceeded {
            required_bytes,
            limit_bytes: options.frame_buffer_limit_bytes,
        });
    }
    if let Some(sink) = sink.as_mut() {
        persist_retained_frame(*sink, captures, &normalized, options.tail_duration_us)?;
    }
    *rgba_bytes = required_bytes;
    captures.push(normalized);
    Ok(())
}

fn finish_session_collection(
    session: &mut dyn CaptureSession,
    state: CollectionFinishState,
    stop_reason: StopReason,
    options: ValidatedOptions,
    sink: Option<&mut dyn RecordingFrameSink>,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectedRecording, WorkflowError> {
    let CollectionFinishState {
        captures,
        rgba_bytes,
        first_timestamp,
        last_observed_timestamp,
    } = state;
    progress.report(WorkflowProgress::capture(
        WorkflowPhase::StoppingCapture,
        u64::try_from(captures.len()).unwrap_or(u64::MAX),
        Duration::from_micros(current_timestamp_span(
            first_timestamp,
            last_observed_timestamp,
        )),
    ));
    let last_duration_us =
        calculate_last_duration(&captures, last_observed_timestamp, stop_reason, options)?;
    if let Some(sink) = sink
        && !captures.is_empty()
    {
        let frame_index = u64::try_from(captures.len() - 1).unwrap_or(u64::MAX);
        sink.update_frame_duration(frame_index, last_duration_us)
            .map_err(|source| WorkflowError::FrameSink {
                operation: RecordingFrameSinkOperation::UpdateFrameDuration,
                frame_index,
                source,
            })?;
    }
    stop_session_if_live(session)?;
    finish_collection(captures, rgba_bytes, last_duration_us)
}

fn report_collection_progress(
    progress: &mut dyn WorkflowProgressSink,
    phase: WorkflowPhase,
    frames: usize,
    first_timestamp: Option<u64>,
    last_observed_timestamp: Option<u64>,
) {
    progress.report(WorkflowProgress::capture(
        phase,
        u64::try_from(frames).unwrap_or(u64::MAX),
        Duration::from_micros(current_timestamp_span(
            first_timestamp,
            last_observed_timestamp,
        )),
    ));
}

fn persist_retained_frame(
    sink: &mut dyn RecordingFrameSink,
    captures: &[NormalizedCapture],
    capture: &NormalizedCapture,
    provisional_duration_us: u64,
) -> Result<(), WorkflowError> {
    if let Some(previous) = captures.last() {
        let frame_index = u64::try_from(captures.len() - 1).unwrap_or(u64::MAX);
        let duration_us = capture.timestamp_us - previous.timestamp_us;
        update_sink_duration(sink, frame_index, duration_us)?;
    }
    let frame_index = u64::try_from(captures.len()).unwrap_or(u64::MAX);
    append_sink_frame(sink, frame_index, capture, provisional_duration_us)
}

fn append_sink_frame(
    sink: &mut dyn RecordingFrameSink,
    frame_index: u64,
    capture: &NormalizedCapture,
    provisional_duration_us: u64,
) -> Result<(), WorkflowError> {
    let frame = RgbaFrame::new(
        capture.width,
        capture.height,
        capture.pixels.clone(),
        provisional_duration_us,
    )?;
    sink.append_provisional_frame(frame_index, &frame)
        .map_err(|source| WorkflowError::FrameSink {
            operation: RecordingFrameSinkOperation::AppendProvisionalFrame,
            frame_index,
            source,
        })
}

fn update_sink_duration(
    sink: &mut dyn RecordingFrameSink,
    frame_index: u64,
    duration_us: u64,
) -> Result<(), WorkflowError> {
    sink.update_frame_duration(frame_index, duration_us)
        .map_err(|source| WorkflowError::FrameSink {
            operation: RecordingFrameSinkOperation::UpdateFrameDuration,
            frame_index,
            source,
        })
}

fn duration_deadline_reached(limit: ValidatedLimit) -> bool {
    matches!(limit, ValidatedLimit::Duration { deadline: Some(deadline), .. } if Instant::now() >= deadline)
}

fn reset_duration_deadline(limit: &mut ValidatedLimit) {
    if let ValidatedLimit::Duration { deadline, .. } = limit {
        *deadline = None;
    }
}

fn arm_duration_deadline(limit: &mut ValidatedLimit) -> Result<(), WorkflowError> {
    let ValidatedLimit::Duration {
        duration_us,
        deadline,
    } = limit
    else {
        return Ok(());
    };
    if deadline.is_none() {
        *deadline = Some(
            Instant::now()
                .checked_add(Duration::from_micros(*duration_us))
                .ok_or_else(|| {
                    WorkflowError::InvalidCollectionOption(
                        "collection duration is too large for a monotonic deadline".to_owned(),
                    )
                })?,
        );
    }
    Ok(())
}

fn extend_duration_deadline(
    limit: &mut ValidatedLimit,
    paused_for: Duration,
) -> Result<(), WorkflowError> {
    let ValidatedLimit::Duration {
        deadline: Some(deadline),
        ..
    } = limit
    else {
        return Ok(());
    };
    *deadline = deadline.checked_add(paused_for).ok_or_else(|| {
        WorkflowError::InvalidCollectionOption(
            "paused collection deadline overflowed the monotonic clock".to_owned(),
        )
    })?;
    Ok(())
}

fn bounded_poll_interval(limit: ValidatedLimit, configured: Duration) -> Duration {
    match limit {
        ValidatedLimit::Duration {
            deadline: Some(deadline),
            ..
        } => deadline
            .checked_duration_since(Instant::now())
            .map_or(Duration::ZERO, |remaining| remaining.min(configured)),
        ValidatedLimit::Duration { deadline: None, .. }
        | ValidatedLimit::UntilStopped
        | ValidatedLimit::MaxFrames(_) => configured,
    }
}

fn sleep_until_control_poll(limit: ValidatedLimit, configured: Duration) {
    let duration = bounded_poll_interval(limit, configured);
    if !duration.is_zero() {
        std::thread::sleep(duration);
    }
}

const fn snapshot_rejection_for_stop(reason: StopReason) -> SnapshotTriggerRejection {
    match reason {
        StopReason::UserStopped => SnapshotTriggerRejection::Stopped,
        StopReason::DurationReached | StopReason::FrameLimitReached | StopReason::EndOfStream => {
            SnapshotTriggerRejection::CollectionEnded
        }
    }
}

fn snapshot_rejection_for_error(error: &WorkflowError) -> SnapshotTriggerRejection {
    match error {
        WorkflowError::Capture(error) => SnapshotTriggerRejection::CaptureFailed(error.clone()),
        WorkflowError::Cancelled => SnapshotTriggerRejection::Cancelled,
        WorkflowError::Discarded => SnapshotTriggerRejection::Discarded,
        _ => SnapshotTriggerRejection::CollectionFailed(error.to_string()),
    }
}

const fn duration_span_reached(limit: ValidatedLimit, first: u64, current: u64) -> bool {
    match limit {
        ValidatedLimit::Duration { duration_us, .. } => current - first >= duration_us,
        ValidatedLimit::UntilStopped | ValidatedLimit::MaxFrames(_) => false,
    }
}

fn frame_limit_reached(limit: ValidatedLimit, retained: usize) -> bool {
    match limit {
        ValidatedLimit::UntilStopped | ValidatedLimit::Duration { .. } => false,
        ValidatedLimit::MaxFrames(maximum) => {
            u64::try_from(retained).unwrap_or(u64::MAX) >= maximum
        }
    }
}

fn validate_order(
    frame: &CapturedFrame,
    frame_index: u64,
    previous_timestamp: Option<u64>,
    previous_sequence: Option<u64>,
) -> Result<(), WorkflowError> {
    let timestamp = frame.captured_at().as_micros();
    if let Some(previous) = previous_timestamp
        && timestamp <= previous
    {
        return Err(WorkflowError::NonMonotonicTimestamp {
            frame_index,
            previous_micros: previous,
            actual_micros: timestamp,
        });
    }
    if let Some(previous) = previous_sequence
        && frame.sequence() <= previous
    {
        return Err(WorkflowError::NonMonotonicSequence {
            frame_index,
            previous,
            actual: frame.sequence(),
        });
    }
    Ok(())
}

fn normalize_frame(
    frame: &CapturedFrame,
    frame_index: u64,
    first: Option<&NormalizedCapture>,
) -> Result<NormalizedCapture, WorkflowError> {
    let size = frame.size();
    let (width, height) = validate_dimensions(size, frame_index, first)?;
    let width_usize = usize::from(width);
    let height_usize = usize::from(height);
    let row_bytes = width_usize
        .checked_mul(4)
        .ok_or(WorkflowError::DimensionsOutOfRange {
            frame_index,
            width: size.width(),
            height: size.height(),
        })?;
    if frame.stride() < row_bytes {
        return Err(WorkflowError::InvalidStride {
            frame_index,
            minimum: row_bytes,
            actual: frame.stride(),
        });
    }
    let required_source =
        frame
            .stride()
            .checked_mul(height_usize)
            .ok_or(WorkflowError::InvalidPixelBuffer {
                frame_index,
                minimum: usize::MAX,
                actual: frame.pixels().len(),
            })?;
    if frame.pixels().len() < required_source {
        return Err(WorkflowError::InvalidPixelBuffer {
            frame_index,
            minimum: required_source,
            actual: frame.pixels().len(),
        });
    }
    let output_len =
        row_bytes
            .checked_mul(height_usize)
            .ok_or(WorkflowError::DimensionsOutOfRange {
                frame_index,
                width: size.width(),
                height: size.height(),
            })?;
    let mut pixels = Vec::with_capacity(output_len);
    match frame.format() {
        PixelFormat::Rgba8 => {
            for row in frame
                .pixels()
                .chunks_exact(frame.stride())
                .take(height_usize)
            {
                pixels.extend_from_slice(&row[..row_bytes]);
            }
        }
        PixelFormat::Bgra8 => {
            for row in frame
                .pixels()
                .chunks_exact(frame.stride())
                .take(height_usize)
            {
                for pixel in row[..row_bytes].as_chunks::<4>().0 {
                    pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
            }
        }
        format => {
            return Err(WorkflowError::UnsupportedPixelFormat {
                frame_index,
                format: format!("{format:?}"),
            });
        }
    }
    debug_assert_eq!(pixels.len(), output_len);
    Ok(NormalizedCapture {
        timestamp_us: frame.captured_at().as_micros(),
        width,
        height,
        pixels,
    })
}

fn validate_dimensions(
    size: PhysicalSize,
    frame_index: u64,
    first: Option<&NormalizedCapture>,
) -> Result<(u16, u16), WorkflowError> {
    let width = u16::try_from(size.width()).map_err(|_| WorkflowError::DimensionsOutOfRange {
        frame_index,
        width: size.width(),
        height: size.height(),
    })?;
    let height = u16::try_from(size.height()).map_err(|_| WorkflowError::DimensionsOutOfRange {
        frame_index,
        width: size.width(),
        height: size.height(),
    })?;
    if let Some(first) = first
        && (first.width != width || first.height != height)
    {
        return Err(WorkflowError::DimensionMismatch {
            frame_index,
            expected_width: u32::from(first.width),
            expected_height: u32::from(first.height),
            actual_width: size.width(),
            actual_height: size.height(),
        });
    }
    Ok((width, height))
}

fn finish_collection(
    captures: Vec<NormalizedCapture>,
    rgba_bytes: u64,
    last_duration_us: u64,
) -> Result<CollectedRecording, WorkflowError> {
    if captures.is_empty() {
        return Err(WorkflowError::EmptyCapture);
    }

    let mut frames = Vec::with_capacity(captures.len());
    let mut duration_us = 0_u64;
    let mut captures = captures.into_iter().peekable();
    while let Some(capture) = captures.next() {
        let frame_duration = captures.peek().map_or(last_duration_us, |next| {
            next.timestamp_us - capture.timestamp_us
        });
        duration_us = duration_us.checked_add(frame_duration).ok_or_else(|| {
            WorkflowError::InvalidCollectionOption(
                "collected presentation duration overflowed u64 microseconds".to_owned(),
            )
        })?;
        frames.push(RgbaFrame::new(
            capture.width,
            capture.height,
            capture.pixels,
            frame_duration,
        )?);
    }
    let summary = CollectionSummary {
        frames: u64::try_from(frames.len()).unwrap_or(u64::MAX),
        duration_us,
        rgba_bytes,
    };
    Ok(CollectedRecording { frames, summary })
}

fn calculate_last_duration(
    captures: &[NormalizedCapture],
    last_observed_timestamp: Option<u64>,
    stop_reason: StopReason,
    options: ValidatedOptions,
) -> Result<u64, WorkflowError> {
    let Some(first) = captures.first() else {
        return Err(WorkflowError::EmptyCapture);
    };
    let last_retained_timestamp = captures
        .last()
        .expect("a first frame implies a final frame")
        .timestamp_us;
    calculate_last_duration_values(
        first.timestamp_us,
        last_retained_timestamp,
        last_observed_timestamp,
        stop_reason,
        options,
    )
}

fn calculate_last_duration_values(
    first_timestamp_us: u64,
    last_retained_timestamp: u64,
    last_observed_timestamp: Option<u64>,
    stop_reason: StopReason,
    options: ValidatedOptions,
) -> Result<u64, WorkflowError> {
    let last_duration_us =
        if let (StopReason::DurationReached, ValidatedLimit::Duration { duration_us, .. }) =
            (stop_reason, options.limit)
        {
            let elapsed = last_retained_timestamp - first_timestamp_us;
            duration_us - elapsed
        } else {
            let skipped_span = last_observed_timestamp
                .unwrap_or(last_retained_timestamp)
                .checked_sub(last_retained_timestamp)
                .ok_or_else(|| {
                    WorkflowError::InvalidCollectionOption(
                        "last observed timestamp precedes the retained frame".to_owned(),
                    )
                })?;
            options
                .tail_duration_us
                .checked_add(skipped_span)
                .ok_or_else(|| {
                    WorkflowError::InvalidCollectionOption(
                        "final presentation duration overflowed u64 microseconds".to_owned(),
                    )
                })?
        };

    Ok(last_duration_us)
}

fn current_timestamp_span(first: Option<u64>, last: Option<u64>) -> u64 {
    first
        .zip(last)
        .map_or(0, |(first, last)| last.saturating_sub(first))
}

fn stop_session_if_live(session: &mut dyn CaptureSession) -> Result<(), WorkflowError> {
    if !session.state().is_terminal() {
        session.stop()?;
    }
    Ok(())
}

fn discard_best_effort(session: &mut dyn CaptureSession) {
    if !matches!(
        session.state(),
        CaptureSessionState::Discarded | CaptureSessionState::Failed
    ) {
        let _ = session.discard();
    }
}

fn ensure_not_cancelled(cancellation: &dyn CancellationToken) -> Result<(), WorkflowError> {
    if cancellation.is_cancelled() {
        Err(WorkflowError::Cancelled)
    } else {
        Ok(())
    }
}
