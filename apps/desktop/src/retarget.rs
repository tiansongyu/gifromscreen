use gif_from_screen_capture::{PhysicalRect, PhysicalSize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetargetCompletion {
    Applied,
    Rejected,
    WorkerExited,
}

/// Pure state machine that serializes region moves around one in-flight update.
#[derive(Debug)]
pub(crate) struct RegionRetargetPlan {
    canvas_size: PhysicalSize,
    applied: PhysicalRect,
    desired: PhysicalRect,
    in_flight: Option<PhysicalRect>,
    queued: Option<PhysicalRect>,
    enabled: bool,
}

impl RegionRetargetPlan {
    pub(crate) const fn applied(&self) -> PhysicalRect {
        self.applied
    }
    pub(crate) const fn new(initial: PhysicalRect) -> Self {
        Self {
            canvas_size: initial.size(),
            applied: initial,
            desired: initial,
            in_flight: None,
            queued: None,
            enabled: true,
        }
    }

    /// Observes native viewport geometry and returns a target that may be sent.
    ///
    /// Samples with even a one-pixel size discrepancy are deliberately ignored:
    /// native DPI/window-manager rounding must never turn a move into a canvas
    /// resize. While an update is pending, only the newest position is retained.
    pub(crate) fn observe(&mut self, candidate: PhysicalRect) -> Option<PhysicalRect> {
        if !self.enabled || candidate.size() != self.canvas_size || candidate == self.desired {
            return None;
        }
        self.desired = candidate;
        if self.in_flight.is_some() {
            self.queued = Some(candidate);
            return None;
        }
        if candidate == self.applied {
            return None;
        }
        self.in_flight = Some(candidate);
        Some(candidate)
    }

    /// Completes the sole in-flight request and returns the newest coalesced
    /// position when another update may be sent immediately.
    pub(crate) fn complete(
        &mut self,
        completion: RetargetCompletion,
        allow_next: bool,
    ) -> Option<PhysicalRect> {
        let completed = self.in_flight.take()?;
        match completion {
            RetargetCompletion::Applied => self.applied = completed,
            RetargetCompletion::Rejected => {}
            RetargetCompletion::WorkerExited => {
                self.disable();
                return None;
            }
        }

        if !self.enabled || !allow_next {
            self.queued = None;
            return None;
        }
        let next = self.queued.take()?;
        if next == self.applied {
            return None;
        }
        self.in_flight = Some(next);
        Some(next)
    }

    pub(crate) fn disable(&mut self) {
        self.enabled = false;
        self.queued = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect {
        PhysicalRect::new(x, y, width, height).unwrap()
    }

    #[test]
    fn ignores_duplicates_and_native_one_pixel_size_jitter() {
        let initial = region(10, 20, 640, 480);
        let mut plan = RegionRetargetPlan::new(initial);

        assert_eq!(plan.observe(initial), None);
        assert_eq!(plan.observe(region(20, 20, 641, 480)), None);
        assert_eq!(plan.observe(region(20, 20, 640, 479)), None);
        assert_eq!(
            plan.observe(region(20, 20, 640, 480)),
            Some(region(20, 20, 640, 480))
        );
    }

    #[test]
    fn keeps_one_request_in_flight_and_coalesces_to_the_latest_position() {
        let mut plan = RegionRetargetPlan::new(region(0, 0, 10, 10));
        assert_eq!(
            plan.observe(region(1, 0, 10, 10)),
            Some(region(1, 0, 10, 10))
        );
        assert_eq!(plan.observe(region(2, 0, 10, 10)), None);
        assert_eq!(plan.observe(region(3, 0, 10, 10)), None);

        assert_eq!(
            plan.complete(RetargetCompletion::Applied, true),
            Some(region(3, 0, 10, 10))
        );
        assert_eq!(plan.complete(RetargetCompletion::Applied, true), None);
        assert_eq!(plan.observe(region(3, 0, 10, 10)), None);
    }

    #[test]
    fn moving_back_while_pending_is_not_lost() {
        let initial = region(0, 0, 10, 10);
        let mut plan = RegionRetargetPlan::new(initial);
        assert_eq!(
            plan.observe(region(5, 0, 10, 10)),
            Some(region(5, 0, 10, 10))
        );
        assert_eq!(plan.observe(initial), None);

        assert_eq!(
            plan.complete(RetargetCompletion::Applied, true),
            Some(initial)
        );
        assert_eq!(plan.complete(RetargetCompletion::Applied, true), None);
    }

    #[test]
    fn rejection_does_not_retry_until_the_user_moves_again() {
        let rejected = region(5, 0, 10, 10);
        let mut plan = RegionRetargetPlan::new(region(0, 0, 10, 10));
        assert_eq!(plan.observe(rejected), Some(rejected));
        assert_eq!(plan.complete(RetargetCompletion::Rejected, true), None);
        assert_eq!(plan.observe(rejected), None);
        assert_eq!(
            plan.observe(region(6, 0, 10, 10)),
            Some(region(6, 0, 10, 10))
        );
    }

    #[test]
    fn rejection_advances_to_the_latest_coalesced_position() {
        let mut plan = RegionRetargetPlan::new(region(0, 0, 10, 10));
        assert!(plan.observe(region(5, 0, 10, 10)).is_some());
        assert_eq!(plan.observe(region(6, 0, 10, 10)), None);

        assert_eq!(
            plan.complete(RetargetCompletion::Rejected, true),
            Some(region(6, 0, 10, 10))
        );
    }

    #[test]
    fn terminal_states_clear_queued_moves_and_never_send_again() {
        for completion in [
            RetargetCompletion::Applied,
            RetargetCompletion::WorkerExited,
        ] {
            let mut plan = RegionRetargetPlan::new(region(0, 0, 10, 10));
            assert!(plan.observe(region(1, 0, 10, 10)).is_some());
            assert_eq!(plan.observe(region(2, 0, 10, 10)), None);
            plan.disable();

            assert_eq!(plan.complete(completion, false), None);
            assert_eq!(plan.observe(region(3, 0, 10, 10)), None);
        }
    }
}
