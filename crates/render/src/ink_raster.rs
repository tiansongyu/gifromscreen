//! Bounded WPF-style 28.4 path filling, not WPF Boolean or `VisualBrush` layout.
//!
//! The edge/coverage rules are adapted from dotnet/wpf a04736ac, MIT licensed:
//! `WpfGfx/core/sw/swlib/aarasterizer.cpp` and `core/sw/aacoverage.h`.
//! See `packaging/licenses/upstream/dotnet-wpf-MIT.txt` for the upstream notice.

use std::mem::size_of;

use gif_from_screen_domain::PhysicalSize;

use crate::{
    CancellationToken, PremultipliedRgbaSurface, RgbaSurface,
    ink::{InkError, InkFillRule, InkLimits, InkPath, InkPoint, InkSegment},
    surface::checked_byte_len,
};

#[path = "ink_raster/hfd.rs"]
mod hfd;

mod region;
pub(crate) use region::{InkRegionMask, rasterize_ink_paths_region_measured};

type Result<T> = std::result::Result<T, InkError>;

/// Fills each path using its own fill rule, then unions the resulting regions.
/// Each byte counts 0..=64 covered samples. `outside` complements that union.
/// Open figures close implicitly for filling, as they do in WPF.
///
/// # Errors
/// Rejects invalid coordinates, unsupported coordinate magnitude, exhausted point,
/// segment, work or memory budgets, allocation failures and cancellation.
pub fn rasterize_ink_paths<C: CancellationToken + ?Sized>(
    paths: &[InkPath],
    size: PhysicalSize,
    outside: bool,
    limits: &InkLimits,
    cancellation: &C,
) -> Result<Vec<u8>> {
    rasterize_ink_paths_measured(paths, size, outside, limits, cancellation)
        .map(|(coverage, _)| coverage)
}

/// The unchanged raster operation plus its actual accumulated budget charge.
/// This is the existing algorithm's work units, not elapsed time or a new
/// estimate. No extra output or geometry buffers are retained for measurement.
pub(crate) fn rasterize_ink_paths_measured<C: CancellationToken + ?Sized>(
    paths: &[InkPath],
    size: PhysicalSize,
    outside: bool,
    limits: &InkLimits,
    cancellation: &C,
) -> Result<(Vec<u8>, u64)> {
    let byte_len = checked_byte_len(size)?;
    let mut budget = Budget::new(limits, cancellation, 0)?;
    prepare_work(size, &mut budget)?;
    let mut output = zeroed(byte_len / 4, &mut budget)?;
    let width = usize::try_from(size.width.get())
        .map_err(|_| InkError::Limit("Ink width exceeds the address space".into()))?;
    raster_rows(paths, size, &mut budget, |y, row| {
        for (index, coverage) in row.iter().enumerate() {
            if index.is_multiple_of(1024) && cancellation.is_cancelled() {
                return Err(InkError::Cancelled);
            }
            output[y * width + index] = if outside { 64 - coverage } else { *coverage };
        }
        Ok(())
    })?;
    budget.check()?;
    Ok((output, budget.work))
}

/// Produces a typed premultiplied snapshot of the reference outside the live
/// paths' geometric union. No intermediate PNG or full-size coverage image is
/// allocated. Source pixels are never mutated.
///
/// # Errors
/// Returns the same bounded geometry errors as [`rasterize_ink_paths`]. The
/// memory budget also includes the borrowed source and returned PM pixels.
pub fn clip_ink_reference<C: CancellationToken + ?Sized>(
    source: &RgbaSurface,
    live_paths: &[InkPath],
    limits: &InkLimits,
    cancellation: &C,
) -> Result<PremultipliedRgbaSurface> {
    let size = source.size();
    let byte_len = checked_byte_len(size)?;
    let mut budget = Budget::new(limits, cancellation, byte_len)?;
    prepare_work(size, &mut budget)?;
    let mut output = zeroed(byte_len, &mut budget)?;
    let width = usize::try_from(size.width.get())
        .map_err(|_| InkError::Limit("Ink width exceeds the address space".into()))?;
    raster_rows(live_paths, size, &mut budget, |y, row| {
        for (x, coverage) in row.iter().enumerate() {
            if x.is_multiple_of(1024) && cancellation.is_cancelled() {
                return Err(InkError::Cancelled);
            }
            let offset = (y * width + x) * 4;
            let pixel = source.pixels()[offset..offset + 4]
                .try_into()
                .map_err(|_| InkError::Invalid("Invalid source pixel shape".into()))?;
            let premultiplied = crate::wpf_pixels::premultiply(pixel);
            for (channel, value) in premultiplied.into_iter().enumerate() {
                output[offset + channel] = scale_coverage(value, 64 - coverage);
            }
        }
        Ok(())
    })?;
    budget.check()?;
    let snapshot = PremultipliedRgbaSurface::new(size, output)?;
    budget.check()?;
    Ok(snapshot)
}

fn scale_coverage(channel: u8, coverage: u8) -> u8 {
    u8::try_from((u16::from(channel) * u16::from(coverage) * 4 + 128) >> 8)
        .expect("coverage is at most 64")
}

struct Budget<'a, C: CancellationToken + ?Sized> {
    limits: &'a InkLimits,
    cancellation: &'a C,
    work: u64,
    memory: usize,
    points: usize,
    segments: usize,
}

impl<'a, C: CancellationToken + ?Sized> Budget<'a, C> {
    fn new(limits: &'a InkLimits, cancellation: &'a C, borrowed_bytes: usize) -> Result<Self> {
        let mut budget = Self {
            limits,
            cancellation,
            work: 0,
            memory: 0,
            points: 0,
            segments: 0,
        };
        budget.check()?;
        budget.memory(borrowed_bytes)?;
        Ok(budget)
    }

    fn check(&self) -> Result<()> {
        if self.cancellation.is_cancelled() {
            Err(InkError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn work(&mut self, count: u64) -> Result<()> {
        self.check()?;
        self.work = self
            .work
            .checked_add(count)
            .filter(|total| *total <= self.limits.max_work)
            .ok_or_else(|| InkError::Limit("Ink raster work budget exceeded".into()))?;
        Ok(())
    }

    fn memory(&mut self, bytes: usize) -> Result<()> {
        self.memory = self
            .memory
            .checked_add(bytes)
            .filter(|total| *total <= self.limits.max_bytes)
            .ok_or_else(|| InkError::Limit("Ink raster memory budget exceeded".into()))?;
        Ok(())
    }

    fn point(&mut self) -> Result<()> {
        self.work(1)?;
        self.points = self
            .points
            .checked_add(1)
            .filter(|count| *count <= self.limits.max_points)
            .ok_or_else(|| InkError::Limit("Ink input point limit exceeded".into()))?;
        Ok(())
    }

    fn segment(&mut self) -> Result<()> {
        self.work(1)?;
        self.segments = self
            .segments
            .checked_add(1)
            .filter(|count| *count <= self.limits.max_segments)
            .ok_or_else(|| InkError::Limit("Ink flattened segment limit exceeded".into()))?;
        Ok(())
    }
}

fn prepare_work<C: CancellationToken + ?Sized>(
    size: PhysicalSize,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    // Charge the unavoidable output pixels and eight scans per row before
    // allocating, so a huge thin surface cannot consume an unbounded loop.
    let pixels = u64::from(size.width.get()) * u64::from(size.height.get());
    budget.work(pixels + u64::from(size.height.get()) * 8)
}

fn reserved<T, C: CancellationToken + ?Sized>(
    count: usize,
    budget: &mut Budget<'_, C>,
) -> Result<Vec<T>> {
    budget.check()?;
    let bytes = count
        .checked_mul(size_of::<T>())
        .ok_or_else(|| InkError::Limit("Ink allocation byte count overflows".into()))?;
    budget.memory(bytes)?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(count)
        .map_err(|_| InkError::Limit("Could not allocate bounded ink working memory".into()))?;
    if buffer.capacity() > count {
        budget.memory((buffer.capacity() - count) * size_of::<T>())?;
    }
    Ok(buffer)
}

fn zeroed<C: CancellationToken + ?Sized>(
    count: usize,
    budget: &mut Budget<'_, C>,
) -> Result<Vec<u8>> {
    let mut buffer = reserved(count, budget)?;
    buffer.resize(count, 0);
    budget.check()?;
    Ok(buffer)
}

/// Physical coordinates in sixteenths of a pixel, after the rasterizer's
/// half-pixel offset has been restored. Limits are deliberately WPF's range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Point {
    x: i32,
    y: i32,
}

fn quantize(point: InkPoint) -> Result<Point> {
    fn coordinate(value: f64) -> Result<i32> {
        if !value.is_finite() {
            return Err(InkError::Invalid("Ink coordinates must be finite".into()));
        }
        // Native TransformRasterizerPointsTo28_4 first works in float32.
        #[allow(clippy::cast_possible_truncation)]
        let transformed = (value as f32) * 16.0 - 8.0;
        if !(-8_388_608.0..=8_388_608.0).contains(&transformed) {
            return Err(InkError::Limit(
                "Ink coordinate exceeds WPF's safe 28.4 range".into(),
            ));
        }
        // CFloatFPU::Round uses half-UP, including negative half integers.
        #[allow(clippy::cast_possible_truncation)]
        let rounded = (f64::from(transformed) + 0.5).floor() as i32;
        Ok(rounded + 8)
    }
    Ok(Point {
        x: coordinate(point.x)?,
        y: coordinate(point.y)?,
    })
}

#[derive(Clone, Copy)]
struct Edge {
    start: Point,
    end: Point,
    start_y: i64,
    end_y: i64,
    path: usize,
    direction: i32,
}

fn ceil_div(numerator: i64, denominator: i64) -> i64 {
    let quotient = numerator.div_euclid(denominator);
    quotient + i64::from(numerator.rem_euclid(denominator) != 0)
}

fn add_edge<C: CancellationToken + ?Sized>(
    edges: &mut Vec<Edge>,
    mut start: Point,
    mut end: Point,
    path: usize,
    height: u32,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    budget.segment()?;
    let direction = if start.y <= end.y { 1 } else { -1 };
    if start.y > end.y {
        std::mem::swap(&mut start, &mut end);
    }
    let start_y = ceil_div(i64::from(start.y), 2).max(0);
    let end_y = ceil_div(i64::from(end.y), 2).min(i64::from(height) * 8);
    if start_y >= end_y {
        return Ok(());
    }
    if edges.len() == edges.capacity() {
        let remaining = budget.limits.max_segments.saturating_sub(edges.capacity());
        let additional = edges.capacity().max(16).min(remaining);
        if additional == 0 {
            return Err(InkError::Limit("Ink edge capacity limit exceeded".into()));
        }
        let before = edges.capacity();
        let extra_bytes = additional
            .checked_mul(size_of::<Edge>())
            .ok_or_else(|| InkError::Limit("Ink edge byte count overflows".into()))?;
        let old_bytes = before
            .checked_mul(size_of::<Edge>())
            .ok_or_else(|| InkError::Limit("Ink edge byte count overflows".into()))?;
        // A realloc may briefly retain both the old and new allocations.
        budget.memory(extra_bytes)?;
        budget.memory(old_bytes)?;
        edges
            .try_reserve_exact(additional)
            .map_err(|_| InkError::Limit("Could not allocate ink edges".into()))?;
        budget.memory -= old_bytes;
        if edges.capacity() > before + additional {
            budget.memory((edges.capacity() - before - additional) * size_of::<Edge>())?;
        }
    }
    edges.push(Edge {
        start,
        end,
        start_y,
        end_y,
        path,
        direction,
    });
    Ok(())
}

fn prepare_edges<C: CancellationToken + ?Sized>(
    paths: &[InkPath],
    height: u32,
    budget: &mut Budget<'_, C>,
) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    for (path_index, path) in paths.iter().enumerate() {
        budget.work(1)?;
        for figure in &path.figures {
            budget.point()?;
            let first = quantize(figure.start)?;
            let mut current = first;
            for segment in &figure.segments {
                match segment {
                    InkSegment::LineTo(point) => {
                        budget.point()?;
                        let next = quantize(*point)?;
                        add_edge(&mut edges, current, next, path_index, height, budget)?;
                        current = next;
                    }
                    InkSegment::CubicTo {
                        control1,
                        control2,
                        to,
                    } => {
                        for _ in 0..3 {
                            budget.point()?;
                        }
                        let end = quantize(*to)?;
                        let mut curve = hfd::Cubic::new([
                            current,
                            quantize(*control1)?,
                            quantize(*control2)?,
                            end,
                        ]);
                        while let Some(next) = curve.next(budget)? {
                            add_edge(&mut edges, current, next, path_index, height, budget)?;
                            current = next;
                        }
                    }
                }
            }
            // Fill semantics close even an open figure. Explicit closing lines
            // need no extra segment; degenerate empty figures contribute nothing.
            if current != first {
                add_edge(&mut edges, current, first, path_index, height, budget)?;
            }
        }
    }
    sort(&mut edges, |edge| edge.start_y, budget)?;
    Ok(edges)
}

/// In-place heapsort keeps both cancellation and the actual comparison/swap
/// work bounded, without an uninterruptible allocation or library sort call.
fn sort<T, K: Ord, C: CancellationToken + ?Sized>(
    values: &mut [T],
    key: impl Fn(&T) -> K,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    fn sift<T, K: Ord, C: CancellationToken + ?Sized>(
        values: &mut [T],
        mut root: usize,
        end: usize,
        key: &impl Fn(&T) -> K,
        budget: &mut Budget<'_, C>,
    ) -> Result<()> {
        while let Some(left) = root
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .filter(|left| *left < end)
        {
            budget.work(1)?;
            let mut child = left;
            if left + 1 < end && key(&values[left]) < key(&values[left + 1]) {
                child += 1;
            }
            if key(&values[root]) >= key(&values[child]) {
                break;
            }
            values.swap(root, child);
            root = child;
        }
        Ok(())
    }
    let length = values.len();
    for root in (0..length / 2).rev() {
        sift(values, root, length, &key, budget)?;
    }
    for end in (1..length).rev() {
        budget.work(1)?;
        values.swap(0, end);
        sift(values, 0, end, &key, budget)?;
    }
    budget.check()
}

#[derive(Clone, Copy)]
struct Crossing {
    path: usize,
    x: i64,
    direction: i32,
}

#[derive(Clone, Copy)]
struct Interval {
    left: i64,
    right: i64,
}

fn crossing(edge: &Edge, y: i64) -> Crossing {
    let dy = i64::from(edge.end.y) - i64::from(edge.start.y);
    let dx = i64::from(edge.end.x) - i64::from(edge.start.x);
    // Coordinates are bounded to +/- (2^23+8), so products are below 2^50.
    // An active y lies between the endpoints; this cannot overflow i64.
    let numerator = i64::from(edge.start.x) * dy + (y * 2 - i64::from(edge.start.y)) * dx;
    Crossing {
        path: edge.path,
        x: ceil_div(numerator, dy * 2),
        direction: edge.direction,
    }
}

fn path_intervals<C: CancellationToken + ?Sized>(
    crossings: &[Crossing],
    paths: &[InkPath],
    intervals: &mut Vec<Interval>,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    intervals.clear();
    let mut cursor = 0;
    while cursor < crossings.len() {
        let path = crossings[cursor].path;
        let rule = paths[path].fill_rule;
        let mut winding = 0_i64;
        let mut left = 0;
        while cursor < crossings.len() && crossings[cursor].path == path {
            budget.work(1)?;
            let x = crossings[cursor].x;
            let was_inside = inside(winding, rule);
            while cursor < crossings.len()
                && crossings[cursor].path == path
                && crossings[cursor].x == x
            {
                budget.work(1)?;
                winding += i64::from(crossings[cursor].direction);
                cursor += 1;
            }
            let is_inside = inside(winding, rule);
            if !was_inside && is_inside {
                left = x;
            }
            if was_inside && !is_inside && left < x {
                intervals.push(Interval { left, right: x });
            }
        }
        if winding != 0 {
            return Err(InkError::Invalid(
                "Ink contour has an unbalanced scanline".into(),
            ));
        }
    }
    Ok(())
}

fn inside(winding: i64, rule: InkFillRule) -> bool {
    match rule {
        InkFillRule::NonZero => winding != 0,
        InkFillRule::EvenOdd => winding & 1 != 0,
    }
}

fn add_union<C: CancellationToken + ?Sized>(
    intervals: &mut [Interval],
    row: &mut [u8],
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    sort(
        intervals,
        |interval| (interval.left, interval.right),
        budget,
    )?;
    let mut cursor = 0;
    let end = i64::try_from(row.len()).unwrap() * 8;
    while cursor < intervals.len() {
        budget.work(1)?;
        let left = intervals[cursor].left.max(0);
        let mut right = intervals[cursor].right;
        cursor += 1;
        while cursor < intervals.len() && intervals[cursor].left <= right {
            budget.work(1)?;
            right = right.max(intervals[cursor].right);
            cursor += 1;
        }
        let right = right.min(end);
        let mut x = left;
        while x < right {
            budget.work(1)?;
            let pixel = usize::try_from(x / 8).unwrap();
            let next = right.min((x / 8 + 1) * 8);
            row[pixel] += u8::try_from(next - x).unwrap();
            x = next;
        }
    }
    Ok(())
}

fn raster_rows<C: CancellationToken + ?Sized>(
    paths: &[InkPath],
    size: PhysicalSize,
    budget: &mut Budget<'_, C>,
    mut consume: impl FnMut(usize, &[u8]) -> Result<()>,
) -> Result<()> {
    let edges = prepare_edges(paths, size.height.get(), budget)?;
    let mut active: Vec<usize> = reserved(edges.len(), budget)?;
    let mut crossings = reserved(edges.len(), budget)?;
    let mut intervals = reserved(edges.len() / 2, budget)?;
    let mut row = zeroed(usize::try_from(size.width.get()).unwrap(), budget)?;
    let mut incoming = 0;
    for y in 0..size.height.get() {
        budget.check()?;
        row.fill(0);
        for sample in 0..8 {
            budget.check()?;
            let scan = i64::from(y) * 8 + sample;
            while incoming < edges.len() && edges[incoming].start_y <= scan {
                budget.work(1)?;
                active.push(incoming);
                incoming += 1;
            }
            crossings.clear();
            let mut index = 0;
            while index < active.len() {
                budget.work(1)?;
                let edge = &edges[active[index]];
                if edge.end_y <= scan {
                    active.swap_remove(index);
                } else {
                    crossings.push(crossing(edge, scan));
                    index += 1;
                }
            }
            sort(&mut crossings, |point| (point.path, point.x), budget)?;
            path_intervals(&crossings, paths, &mut intervals, budget)?;
            add_union(&mut intervals, &mut row, budget)?;
        }
        consume(usize::try_from(y).unwrap(), &row)?;
    }
    budget.check()
}

#[cfg(test)]
#[path = "ink_raster_tests.rs"]
mod tests;
