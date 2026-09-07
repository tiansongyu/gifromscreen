//! Native Bezier32/HfdBasis32 adaptation with the upstream 64-bit fallback.
//! Derived from dotnet/wpf a04736ac `core/geometry/bezier.{h,cpp}` (MIT).
//! Upstream copyright/license: `packaging/licenses/upstream/dotnet-wpf-MIT.txt`.

use super::{Budget, CancellationToken, InkError, Point, Result};

const INITIAL_MAGNITUDE: i64 = 24 << 10;
const MAGNITUDE: i64 = 24 << 13;

#[path = "hfd64.rs"]
mod large;

pub(super) enum Cubic {
    Small(SmallCubic),
    Large(large::Cubic),
}

impl Cubic {
    pub(super) fn new(points: [Point; 4]) -> Self {
        match SmallCubic::new(points) {
            Ok(curve) => Self::Small(curve),
            Err(_) => Self::Large(large::Cubic::new(points)),
        }
    }

    pub(super) fn next<C: CancellationToken + ?Sized>(
        &mut self,
        budget: &mut Budget<'_, C>,
    ) -> Result<Option<Point>> {
        match self {
            Self::Small(curve) => curve.next(budget),
            Self::Large(curve) => curve.next(budget),
        }
    }
}

struct Basis {
    e: [i64; 4],
}

impl Basis {
    fn new(p: [i64; 4]) -> Result<Self> {
        let e2 = 6 * (p[1] - 2 * p[2] + p[3]);
        let e3 = 6 * (p[0] - 2 * p[1] + p[2]);
        if e2.abs().max(e3.abs()) >= 24 << 6 {
            return Err(range_error());
        }
        Ok(Self {
            e: [p[0] << 10, (p[3] - p[0]) << 10, e2 << 10, e3 << 10],
        })
    }

    fn error(&self) -> i64 {
        self.e[2].abs().max(self.e[3].abs())
    }
    fn parent_error_divided_by_four(&self) -> i64 {
        self.e[3].abs().max((2 * self.e[2] - self.e[3]).abs())
    }
    fn value(&self) -> i64 {
        (self.e[0] + 4096) >> 13
    }

    fn lazy_half(&mut self, shift: u32) {
        self.e[2] = (self.e[2] + self.e[3]) >> 1;
        self.e[1] = (self.e[1] - (self.e[2] >> shift)) >> 1;
    }

    fn steady(&mut self, shift: u32) {
        self.e[0] <<= 3;
        self.e[1] <<= 3;
        for value in &mut self.e[2..] {
            if shift < 3 {
                *value <<= 3 - shift;
            } else {
                *value >>= shift - 3;
            }
        }
    }

    fn half(&mut self) {
        self.e[2] = (self.e[2] + self.e[3]) >> 3;
        self.e[1] = (self.e[1] - self.e[2]) >> 1;
        self.e[3] >>= 2;
    }

    fn double(&mut self) {
        self.e[1] = 2 * self.e[1] + self.e[2];
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

fn range_error() -> InkError {
    InkError::Limit("Cubic exceeds WPF Bezier32's exact range".into())
}

pub(super) struct SmallCubic {
    x: Basis,
    y: Basis,
    origin: [i64; 2],
    remaining: u32,
    started: bool,
    finished: bool,
}

impl SmallCubic {
    pub(super) fn new(points: [Point; 4]) -> Result<Self> {
        let origin = [
            i64::from(points.iter().map(|p| p.x).min().unwrap()) - 16,
            i64::from(points.iter().map(|p| p.y).min().unwrap()) - 16,
        ];
        let x = points.map(|p| i64::from(p.x) - origin[0]);
        let y = points.map(|p| i64::from(p.y) - origin[1]);
        if x.iter().chain(&y).any(|value| !(0..16_384).contains(value)) {
            return Err(range_error());
        }
        Ok(Self {
            x: Basis::new(x)?,
            y: Basis::new(y)?,
            origin,
            remaining: 1,
            started: false,
            finished: false,
        })
    }

    fn initialize<C: CancellationToken + ?Sized>(
        &mut self,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        let mut shift = 0;
        while self.x.error().max(self.y.error()) > INITIAL_MAGNITUDE << shift {
            budget.work(1)?;
            shift += 2;
            if shift > 10 {
                return Err(range_error());
            }
            self.x.lazy_half(shift);
            self.y.lazy_half(shift);
            self.remaining <<= 1;
        }
        self.x.steady(shift);
        self.y.steady(shift);
        self.x.step();
        self.y.step();
        self.remaining -= 1;
        self.started = true;
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
        if !self.started {
            self.initialize(budget)?;
        }
        let point = Point {
            x: i32::try_from(self.x.value() + self.origin[0]).map_err(|_| range_error())?,
            y: i32::try_from(self.y.value() + self.origin[1]).map_err(|_| range_error())?,
        };
        if self.remaining == 0 {
            self.finished = true;
            return Ok(Some(point));
        }
        if self.x.error().max(self.y.error()) > MAGNITUDE {
            self.x.half();
            self.y.half();
            self.remaining = self.remaining.checked_mul(2).ok_or_else(range_error)?;
        }
        while self.remaining & 1 == 0
            && self.x.parent_error_divided_by_four() <= MAGNITUDE >> 2
            && self.y.parent_error_divided_by_four() <= MAGNITUDE >> 2
        {
            budget.work(1)?;
            self.x.double();
            self.y.double();
            self.remaining >>= 1;
        }
        self.remaining -= 1;
        self.x.step();
        self.y.step();
        Ok(Some(point))
    }
}
