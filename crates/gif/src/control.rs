use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Read-only cancellation boundary used by encoders and quantizers.
pub trait CancellationToken: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// A clonable cancellation token suitable for a background export job.
#[derive(Clone, Debug, Default)]
pub struct CancellationFlag {
    cancelled: Arc<AtomicBool>,
}

impl CancellationFlag {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::Release);
    }
}

impl CancellationToken for CancellationFlag {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Cancellation token for callers that do not need cancellation.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancel;

impl CancellationToken for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodePhase {
    Reading,
    AnalyzingPalette,
    Quantizing,
    Writing,
    Finalizing,
    Complete,
}

/// Monotonic counters reported by an encoding job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncodeProgress {
    pub phase: EncodePhase,
    pub frames_read: u64,
    pub frames_written: u64,
    pub total_frames_hint: Option<u64>,
}

impl EncodeProgress {
    /// Best-effort completion fraction based on the source's exact size hint.
    ///
    /// `None` means the source does not know its final length. A completed job
    /// always reports `Some(1.0)`.
    pub fn fraction(self) -> Option<f32> {
        if self.phase == EncodePhase::Complete {
            return Some(1.0);
        }

        self.total_frames_hint.map(|total| {
            if total == 0 {
                0.0
            } else {
                (self.frames_read as f64 / total as f64).clamp(0.0, 1.0) as f32
            }
        })
    }
}

pub trait ProgressSink {
    fn report(&mut self, progress: EncodeProgress);
}

impl<F> ProgressSink for F
where
    F: FnMut(EncodeProgress),
{
    fn report(&mut self, progress: EncodeProgress) {
        self(progress);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopProgress;

impl ProgressSink for NoopProgress {
    fn report(&mut self, _progress: EncodeProgress) {}
}
