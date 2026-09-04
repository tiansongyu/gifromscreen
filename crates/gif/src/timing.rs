/// GIF frame-delay resolution in microseconds.
pub const GIF_TICK_US: u64 = 10_000;

/// Converts project microseconds to GIF centiseconds using cumulative rounding.
///
/// Rounding cumulative presentation timestamps, rather than every frame in
/// isolation, prevents per-frame error from accumulating across a long export.
#[derive(Clone, Debug, Default)]
pub struct GifTimingQuantizer {
    cumulative_us: u128,
    assigned_ticks: u128,
}

impl GifTimingQuantizer {
    pub const fn new() -> Self {
        Self {
            cumulative_us: 0,
            assigned_ticks: 0,
        }
    }

    /// Adds one frame duration and returns its delay in 10 ms GIF ticks.
    pub fn quantize(&mut self, duration_us: u64) -> u64 {
        self.cumulative_us += u128::from(duration_us);
        let target_ticks =
            (self.cumulative_us + u128::from(GIF_TICK_US / 2)) / u128::from(GIF_TICK_US);
        let frame_ticks = target_ticks - self.assigned_ticks;
        self.assigned_ticks = target_ticks;

        // Input duration is a u64, so its tick representation always fits u64.
        frame_ticks as u64
    }

    pub fn cumulative_duration_us(&self) -> u128 {
        self.cumulative_us
    }

    pub fn assigned_ticks(&self) -> u128 {
        self.assigned_ticks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distributes_fractional_tick_error() {
        let mut timing = GifTimingQuantizer::new();
        let delays: Vec<_> = [16_667, 16_667, 16_667]
            .into_iter()
            .map(|duration| timing.quantize(duration))
            .collect();

        assert_eq!(delays, [2, 1, 2]);
        assert_eq!(timing.assigned_ticks(), 5);
    }

    #[test]
    fn long_animation_error_stays_below_one_tick() {
        let mut timing = GifTimingQuantizer::new();
        let durations = (0..50_000).map(|index| 8_000 + (index % 17) as u64 * 731);
        let mut input_us = 0_u128;
        let mut output_ticks = 0_u128;

        for duration in durations {
            input_us += u128::from(duration);
            output_ticks += u128::from(timing.quantize(duration));
        }

        let output_us = output_ticks * u128::from(GIF_TICK_US);
        assert!(input_us.abs_diff(output_us) < u128::from(GIF_TICK_US));
    }
}
