use std::time::{Duration, Instant};

use gif_from_screen_capture::{
    CaptureBackend, CaptureRequest, CaptureSession, CaptureSessionState, CapturedFrame, FramePoll,
    PhysicalSize, PixelFormat,
};
use gif_from_screen_gif::{CancellationToken, RgbaFrame};

use crate::{WorkflowError, WorkflowPhase, WorkflowProgress, WorkflowProgressSink};

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_TAIL_FRAME_DURATION: Duration = Duration::from_millis(100);
const DEFAULT_FRAME_BUFFER_LIMIT_BYTES: u64 = 512 * 1024 * 1024;

/// Normal completion condition for frame collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CollectionLimit {
    /// Stop after this span of session timestamps, measured from the first
    /// retained frame. A boundary frame at or beyond the span is not retained.
    Duration(Duration),
    /// Stop immediately after retaining this many frames.
    MaxFrames(u64),
}

/// Bounds and timing policy for frame collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectOptions {
    /// Normal completion condition.
    pub limit: CollectionLimit,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopReason {
    DurationReached,
    FrameLimitReached,
    EndOfStream,
}

#[derive(Clone, Copy, Debug)]
struct ValidatedOptions {
    limit: ValidatedLimit,
    poll_interval: Duration,
    tail_duration_us: u64,
    frame_buffer_limit_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
enum ValidatedLimit {
    Duration { duration_us: u64, deadline: Instant },
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
    let options = validate_options(options)?;
    ensure_not_cancelled(cancellation)?;
    progress.report(WorkflowProgress::capture(
        WorkflowPhase::StartingCapture,
        0,
        Duration::ZERO,
    ));

    let mut session = backend.start_session(request)?;
    let result = collect_session(&mut *session, options, cancellation, progress);
    match result {
        Ok(recording) => Ok(recording),
        Err(error) => {
            discard_best_effort(&mut *session);
            Err(error)
        }
    }
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
        CollectionLimit::Duration(duration) => {
            let duration_us = duration_to_nonzero_micros(duration, "collection duration")?;
            let deadline = Instant::now().checked_add(duration).ok_or_else(|| {
                WorkflowError::InvalidCollectionOption(
                    "collection duration is too large for a monotonic deadline".to_owned(),
                )
            })?;
            ValidatedLimit::Duration {
                duration_us,
                deadline,
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

fn collect_session(
    session: &mut dyn CaptureSession,
    options: ValidatedOptions,
    cancellation: &dyn CancellationToken,
    progress: &mut dyn WorkflowProgressSink,
) -> Result<CollectedRecording, WorkflowError> {
    let mut captures = Vec::new();
    let mut rgba_bytes = 0_u64;
    let mut previous_timestamp = None;
    let mut previous_sequence = None;
    let mut first_timestamp = None;
    let mut stream_index = 0_u64;

    let stop_reason = loop {
        ensure_not_cancelled(cancellation)?;
        if duration_deadline_reached(options.limit) {
            break StopReason::DurationReached;
        }
        let poll_interval = bounded_poll_interval(options.limit, options.poll_interval);
        match session.poll_frame(poll_interval)? {
            FramePoll::Frame(frame) => {
                ensure_not_cancelled(cancellation)?;
                validate_order(&frame, stream_index, previous_timestamp, previous_sequence)?;
                let timestamp_us = frame.captured_at().as_micros();
                previous_timestamp = Some(timestamp_us);
                previous_sequence = Some(frame.sequence());

                let capture_start = *first_timestamp.get_or_insert(timestamp_us);
                if duration_span_reached(options.limit, capture_start, timestamp_us) {
                    break StopReason::DurationReached;
                }

                let normalized = normalize_frame(&frame, stream_index, captures.first())?;
                let frame_bytes = u64::try_from(normalized.pixels.len()).unwrap_or(u64::MAX);
                let required_bytes = rgba_bytes.saturating_add(frame_bytes);
                if required_bytes > options.frame_buffer_limit_bytes {
                    return Err(WorkflowError::FrameBufferLimitExceeded {
                        required_bytes,
                        limit_bytes: options.frame_buffer_limit_bytes,
                    });
                }
                rgba_bytes = required_bytes;
                captures.push(normalized);
                stream_index = stream_index.saturating_add(1);

                let captured_duration = current_timestamp_span(&captures);
                progress.report(WorkflowProgress::capture(
                    WorkflowPhase::Capturing,
                    u64::try_from(captures.len()).unwrap_or(u64::MAX),
                    Duration::from_micros(captured_duration),
                ));
                if frame_limit_reached(options.limit, captures.len()) {
                    break StopReason::FrameLimitReached;
                }
            }
            FramePoll::Pending => {}
            FramePoll::EndOfStream => break StopReason::EndOfStream,
        }
    };

    progress.report(WorkflowProgress::capture(
        WorkflowPhase::StoppingCapture,
        u64::try_from(captures.len()).unwrap_or(u64::MAX),
        Duration::from_micros(current_timestamp_span(&captures)),
    ));
    stop_session_if_live(session)?;
    finish_collection(captures, rgba_bytes, stop_reason, options)
}

fn duration_deadline_reached(limit: ValidatedLimit) -> bool {
    matches!(limit, ValidatedLimit::Duration { deadline, .. } if Instant::now() >= deadline)
}

fn bounded_poll_interval(limit: ValidatedLimit, configured: Duration) -> Duration {
    match limit {
        ValidatedLimit::Duration { deadline, .. } => deadline
            .checked_duration_since(Instant::now())
            .map_or(Duration::ZERO, |remaining| remaining.min(configured)),
        ValidatedLimit::MaxFrames(_) => configured,
    }
}

const fn duration_span_reached(limit: ValidatedLimit, first: u64, current: u64) -> bool {
    match limit {
        ValidatedLimit::Duration { duration_us, .. } => current - first >= duration_us,
        ValidatedLimit::MaxFrames(_) => false,
    }
}

fn frame_limit_reached(limit: ValidatedLimit, retained: usize) -> bool {
    match limit {
        ValidatedLimit::Duration { .. } => false,
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
                for pixel in row[..row_bytes].chunks_exact(4) {
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
    stop_reason: StopReason,
    options: ValidatedOptions,
) -> Result<CollectedRecording, WorkflowError> {
    let Some(first) = captures.first() else {
        return Err(WorkflowError::EmptyCapture);
    };
    let first_timestamp_us = first.timestamp_us;
    let last_duration_us = match (stop_reason, options.limit) {
        (StopReason::DurationReached, ValidatedLimit::Duration { duration_us, .. }) => {
            let elapsed = captures
                .last()
                .expect("non-empty collection has a final frame")
                .timestamp_us
                - first_timestamp_us;
            duration_us - elapsed
        }
        _ => options.tail_duration_us,
    };

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

fn current_timestamp_span(captures: &[NormalizedCapture]) -> u64 {
    captures.first().map_or(0, |first| {
        captures
            .last()
            .expect("a first frame implies a last frame")
            .timestamp_us
            - first.timestamp_us
    })
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
