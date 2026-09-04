use std::time::{Duration, Instant};

pub(crate) const MAX_COUNTDOWN_SECONDS: u8 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CountdownStart {
    Immediate,
    Started,
    AlreadyRunning,
    OutOfRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CountdownTick {
    Idle,
    Waiting(u8),
    Finished,
}

#[derive(Clone, Copy, Debug)]
struct ActiveCountdown {
    started_at: Instant,
    duration: Duration,
    remaining_seconds: u8,
}

#[derive(Debug, Default)]
pub(crate) struct RecordingCountdown {
    active: Option<ActiveCountdown>,
}

impl RecordingCountdown {
    pub(crate) fn start(&mut self, now: Instant, seconds: u8) -> CountdownStart {
        if self.active.is_some() {
            return CountdownStart::AlreadyRunning;
        }
        if seconds > MAX_COUNTDOWN_SECONDS {
            return CountdownStart::OutOfRange;
        }
        if seconds == 0 {
            return CountdownStart::Immediate;
        }

        self.active = Some(ActiveCountdown {
            started_at: now,
            duration: Duration::from_secs(u64::from(seconds)),
            remaining_seconds: seconds,
        });
        CountdownStart::Started
    }

    pub(crate) fn tick(&mut self, now: Instant) -> CountdownTick {
        let Some(active) = &mut self.active else {
            return CountdownTick::Idle;
        };
        let elapsed = now.saturating_duration_since(active.started_at);
        if elapsed >= active.duration {
            self.active = None;
            return CountdownTick::Finished;
        }

        let remaining = active.duration.saturating_sub(elapsed);
        let rounded_seconds = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() != 0));
        active.remaining_seconds = u8::try_from(rounded_seconds)
            .unwrap_or(MAX_COUNTDOWN_SECONDS)
            .min(MAX_COUNTDOWN_SECONDS);
        CountdownTick::Waiting(active.remaining_seconds)
    }

    pub(crate) fn remaining_seconds(&self) -> Option<u8> {
        self.active.map(|active| active.remaining_seconds)
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn cancel(&mut self) -> bool {
        self.active.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{CountdownStart, CountdownTick, MAX_COUNTDOWN_SECONDS, RecordingCountdown};

    #[test]
    fn zero_seconds_starts_immediately_without_entering_countdown() {
        let mut countdown = RecordingCountdown::default();

        assert_eq!(
            countdown.start(Instant::now(), 0),
            CountdownStart::Immediate
        );
        assert!(!countdown.is_active());
        assert_eq!(countdown.remaining_seconds(), None);
    }

    #[test]
    fn maximum_countdown_uses_ceiling_seconds_and_finishes_at_deadline() {
        let started_at = Instant::now();
        let mut countdown = RecordingCountdown::default();

        assert_eq!(
            countdown.start(started_at, MAX_COUNTDOWN_SECONDS),
            CountdownStart::Started
        );
        assert_eq!(
            countdown.tick(started_at + Duration::from_nanos(1)),
            CountdownTick::Waiting(MAX_COUNTDOWN_SECONDS)
        );
        assert_eq!(
            countdown.tick(started_at + Duration::from_secs(1)),
            CountdownTick::Waiting(MAX_COUNTDOWN_SECONDS - 1)
        );
        assert_eq!(
            countdown.tick(
                started_at
                    + Duration::from_secs(u64::from(MAX_COUNTDOWN_SECONDS))
                        .saturating_sub(Duration::from_nanos(1))
            ),
            CountdownTick::Waiting(1)
        );
        assert_eq!(
            countdown.tick(started_at + Duration::from_secs(u64::from(MAX_COUNTDOWN_SECONDS))),
            CountdownTick::Finished
        );
        assert_eq!(countdown.tick(started_at), CountdownTick::Idle);
    }

    #[test]
    fn repeated_start_does_not_replace_running_deadline() {
        let started_at = Instant::now();
        let mut countdown = RecordingCountdown::default();
        assert_eq!(countdown.start(started_at, 2), CountdownStart::Started);

        assert_eq!(
            countdown.start(started_at + Duration::from_secs(1), 10),
            CountdownStart::AlreadyRunning
        );
        assert_eq!(
            countdown.tick(started_at + Duration::from_secs(2)),
            CountdownTick::Finished
        );
    }

    #[test]
    fn cancel_returns_to_idle_and_allows_another_start() {
        let started_at = Instant::now();
        let mut countdown = RecordingCountdown::default();
        assert_eq!(countdown.start(started_at, 3), CountdownStart::Started);

        assert!(countdown.cancel());
        assert!(!countdown.cancel());
        assert_eq!(countdown.tick(started_at), CountdownTick::Idle);
        assert_eq!(countdown.start(started_at, 1), CountdownStart::Started);
    }

    #[test]
    fn values_above_ten_seconds_are_rejected_without_changing_state() {
        let mut countdown = RecordingCountdown::default();

        assert_eq!(
            countdown.start(Instant::now(), MAX_COUNTDOWN_SECONDS + 1),
            CountdownStart::OutOfRange
        );
        assert!(!countdown.is_active());
    }
}
