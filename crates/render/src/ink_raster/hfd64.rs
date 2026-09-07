//! WPF's two-level Bezier64/HfdBasis64 fallback in 36.28 fixed-point arithmetic.
//! Unlike substituting an ideal cubic sampler, this retains its intermediate
//! control-point reconstruction and rounding between the high and low levels.
//! Derived from dotnet/wpf a04736ac `core/geometry/bezier.{h,cpp}` (MIT).
//! Upstream copyright/license: `packaging/licenses/upstream/dotnet-wpf-MIT.txt`.

use super::{Budget, CancellationToken, InkError, Point, Result};

const HIGH_ERROR: i64 = 12_288_i64 << 32;
const LOW_ERROR: i64 = 3_i64 << 31;

#[derive(Default)]
struct Basis {
    e: [i64; 4],
}

impl Basis {
    fn new(p: [i64; 4]) -> Self {
        Self {
            e: [
                p[0] << 28,
                (p[3] - p[0]) << 28,
                (6 * (p[1] - 2 * p[2] + p[3])) << 28,
                (6 * (p[0] - 2 * p[1] + p[2])) << 28,
            ],
        }
    }

    fn error(&self) -> i64 {
        self.e[2].abs().max(self.e[3].abs())
    }
    fn parent_error(&self) -> i64 {
        (self.e[3] << 2)
            .abs()
            .max(((self.e[2] << 3) - (self.e[3] << 2)).abs())
    }
    fn value(&self) -> i64 {
        (self.e[0] + (1 << 27)) >> 28
    }

    fn controls(&self) -> [i64; 4] {
        let [e0, e1, e2, e3] = self.e;
        // Native signed /18 truncates toward zero; do not use div_euclid.
        [
            e0,
            e0 + (6 * e1 - e2 - 2 * e3) / 18,
            e0 + (12 * e1 - 2 * e2 - e3) / 18,
            e0 + e1,
        ]
        .map(|value| (value + (1 << 27)) >> 28)
    }

    fn half(&mut self) {
        self.e[2] = (self.e[2] + self.e[3]) >> 3;
        self.e[1] = (self.e[1] - self.e[2]) >> 1;
        self.e[3] >>= 2;
    }

    fn double(&mut self) {
        self.e[1] = (self.e[1] << 1) + self.e[2];
        self.e[3] <<= 2;
        self.e[2] = (self.e[2] << 3) - self.e[3];
    }

    fn step(&mut self) {
        self.e[0] += self.e[1];
        let previous = self.e[2];
        self.e[1] += previous;
        self.e[2] += previous - self.e[3];
        self.e[3] = previous;
    }
}

pub(in crate::ink_raster) struct Cubic {
    high: [Basis; 2],
    low: [Basis; 2],
    high_steps: u32,
    low_steps: u32,
    initialized: bool,
    finished: bool,
}

fn too_many_steps() -> InkError {
    InkError::Limit("Ink cubic subdivision counter overflow".into())
}

impl Cubic {
    pub(super) fn new(points: [Point; 4]) -> Self {
        Self {
            high: [
                Basis::new(points.map(|p| i64::from(p.x))),
                Basis::new(points.map(|p| i64::from(p.y))),
            ],
            low: [Basis::default(), Basis::default()],
            high_steps: 1,
            low_steps: 0,
            initialized: false,
            finished: false,
        }
    }

    fn refine<C: CancellationToken + ?Sized>(
        bases: &mut [Basis; 2],
        steps: &mut u32,
        error: i64,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        while bases.iter().any(|axis| axis.error() > error) {
            budget.work(1)?;
            *steps = steps.checked_mul(2).ok_or_else(too_many_steps)?;
            for axis in &mut *bases {
                axis.half();
            }
        }
        Ok(())
    }

    fn adjust<C: CancellationToken + ?Sized>(
        bases: &mut [Basis; 2],
        steps: &mut u32,
        error: i64,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        if bases.iter().any(|axis| axis.error() > error) {
            budget.work(1)?;
            *steps = steps.checked_mul(2).ok_or_else(too_many_steps)?;
            for axis in &mut *bases {
                axis.half();
            }
        }
        while *steps & 1 == 0 && bases.iter().all(|axis| axis.parent_error() <= error) {
            budget.work(1)?;
            for axis in &mut *bases {
                axis.double();
            }
            *steps >>= 1;
        }
        Ok(())
    }

    fn begin_low<C: CancellationToken + ?Sized>(
        &mut self,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        self.low = [
            Basis::new(self.high[0].controls()),
            Basis::new(self.high[1].controls()),
        ];
        self.low_steps = 1;
        Self::refine(&mut self.low, &mut self.low_steps, LOW_ERROR, budget)?;
        self.high_steps -= 1;
        if self.high_steps != 0 {
            for axis in &mut self.high {
                axis.step();
            }
            Self::adjust(&mut self.high, &mut self.high_steps, HIGH_ERROR, budget)?;
        }
        Ok(())
    }

    pub(super) fn next<C: CancellationToken + ?Sized>(
        &mut self,
        budget: &mut Budget<'_, C>,
    ) -> Result<Option<Point>> {
        budget.work(1)?;
        if self.finished {
            return Ok(None);
        }
        if !self.initialized {
            Self::refine(&mut self.high, &mut self.high_steps, HIGH_ERROR, budget)?;
            self.initialized = true;
        }
        if self.low_steps == 0 {
            self.begin_low(budget)?;
        }
        for axis in &mut self.low {
            axis.step();
        }
        let point = Point {
            x: i32::try_from(self.low[0].value()).map_err(|_| too_many_steps())?,
            y: i32::try_from(self.low[1].value()).map_err(|_| too_many_steps())?,
        };
        self.low_steps -= 1;
        if self.low_steps == 0 && self.high_steps == 0 {
            self.finished = true;
        } else if self.low_steps != 0 {
            // A finished low piece is discarded by begin_low. Adjusting that
            // unused state cannot affect any emitted point.
            Self::adjust(&mut self.low, &mut self.low_steps, LOW_ERROR, budget)?;
        }
        Ok(Some(point))
    }
}
