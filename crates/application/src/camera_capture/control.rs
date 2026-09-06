use std::{
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Camera control state; pausing affects recording, not the live preview.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CameraPhase {
    /// Device may provide preview frames but nothing is persisted.
    Preview,
    /// Incoming frames are submitted to durable storage.
    Recording,
    /// Preview continues while recording time is held.
    Paused,
    /// Stop/save was requested; the decoder is exiting and writer draining.
    Finishing,
    /// Explicit discard was requested for this newly-created recording.
    Discarding,
}

#[derive(Debug)]
struct ClockState {
    phase: CameraPhase,
    elapsed: Duration,
    resumed_at: Option<Instant>,
}

impl ClockState {
    fn elapsed_at(&self, now: Instant) -> Duration {
        self.elapsed
            + self
                .resumed_at
                .map_or(Duration::ZERO, |start| now.saturating_duration_since(start))
    }

    fn hold(&mut self, now: Instant) {
        self.elapsed = self.elapsed_at(now);
        self.resumed_at = None;
    }
}

#[derive(Debug)]
struct Shared {
    clock: Mutex<ClockState>,
    stop: AtomicBool,
}

/// Thread-safe camera controls independent of frame delivery or queue capacity.
#[derive(Clone, Debug)]
pub struct CameraControl(Arc<Shared>);

impl Default for CameraControl {
    fn default() -> Self {
        Self(Arc::new(Shared {
            clock: Mutex::new(ClockState {
                phase: CameraPhase::Preview,
                elapsed: Duration::ZERO,
                resumed_at: None,
            }),
            stop: AtomicBool::new(false),
        }))
    }
}

impl CameraControl {
    /// Begins recording during preview, or resumes a paused recording.
    ///
    /// # Errors
    ///
    /// Rejects commands while already recording or finalizing.
    pub fn record(&self) -> Result<(), String> {
        let mut clock = self.0.clock.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(clock.phase, CameraPhase::Preview | CameraPhase::Paused) {
            return Err("Camera recording cannot start in its current state.".to_owned());
        }
        clock.resumed_at = Some(Instant::now());
        clock.phase = CameraPhase::Recording;
        Ok(())
    }

    /// Holds recording time without stopping camera preview delivery.
    ///
    /// # Errors
    ///
    /// Rejects pause when not currently recording.
    pub fn pause(&self) -> Result<(), String> {
        let mut clock = self.0.clock.lock().unwrap_or_else(PoisonError::into_inner);
        if clock.phase != CameraPhase::Recording {
            return Err("Only an active camera recording can be paused.".to_owned());
        }
        clock.hold(Instant::now());
        clock.phase = CameraPhase::Paused;
        Ok(())
    }

    /// Requests stop/save, or closes a preview that has no recorded frames.
    pub fn stop(&self) {
        let mut clock = self.0.clock.lock().unwrap_or_else(PoisonError::into_inner);
        clock.hold(Instant::now());
        if clock.phase != CameraPhase::Discarding {
            clock.phase = CameraPhase::Finishing;
        }
        self.0.stop.store(true, Ordering::Release);
    }

    /// Explicitly requests discarding only this session's newly-created project.
    pub fn discard(&self) {
        let mut clock = self.0.clock.lock().unwrap_or_else(PoisonError::into_inner);
        clock.hold(Instant::now());
        clock.phase = CameraPhase::Discarding;
        self.0.stop.store(true, Ordering::Release);
    }

    /// Current user-control phase.
    pub fn phase(&self) -> CameraPhase {
        self.snapshot().0
    }

    /// Recording elapsed time excluding every preview and pause interval.
    pub fn active_time_us(&self) -> u64 {
        self.snapshot().1
    }

    pub(super) fn snapshot(&self) -> (CameraPhase, u64) {
        let clock = self.0.clock.lock().unwrap_or_else(PoisonError::into_inner);
        (
            clock.phase,
            u64::try_from(clock.elapsed_at(Instant::now()).as_micros()).unwrap_or(u64::MAX),
        )
    }

    pub(super) fn stop_requested(&self) -> bool {
        self.0.stop.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_holds_time_through_pause_and_shutdown() {
        let start = Instant::now();
        let mut clock = ClockState {
            phase: CameraPhase::Recording,
            elapsed: Duration::ZERO,
            resumed_at: Some(start),
        };
        clock.hold(start + Duration::from_secs(2));
        assert_eq!(
            clock.elapsed_at(start + Duration::from_secs(100)),
            Duration::from_secs(2)
        );
        clock.resumed_at = Some(start + Duration::from_secs(100));
        clock.hold(start + Duration::from_secs(101));
        assert_eq!(clock.elapsed, Duration::from_secs(3));
    }

    #[test]
    fn terminal_controls_do_not_resume_or_turn_discard_into_save() {
        let control = CameraControl::default();
        assert!(control.pause().is_err());
        control.record().unwrap();
        control.pause().unwrap();
        control.record().unwrap();
        control.discard();
        control.stop();
        assert_eq!(control.phase(), CameraPhase::Discarding);
        assert!(control.stop_requested());
        assert!(control.record().is_err());
        assert!(control.pause().is_err());
    }
}
