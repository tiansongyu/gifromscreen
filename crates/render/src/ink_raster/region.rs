//! Cropped *writes/scans*, never translated input geometry or a new AA grid.

use gif_from_screen_domain::{PhysicalPoint, PhysicalRect};

use super::{
    Budget, CancellationToken, InkError, InkLimits, InkPath, Interval, PhysicalSize, Result,
    checked_byte_len, crossing, path_intervals, prepare_edges, prepare_work, reserved, sort,
    zeroed,
};

/// A nonempty rectangular view of the unchanged global C64 sample lattice.
pub(crate) struct InkRegionMask {
    pub origin: PhysicalPoint,
    pub size: PhysicalSize,
    pub coverage: Vec<u8>,
    pub work: u64,
}

/// Rasterizes one bounded region of a global canvas. Quantized endpoints,
/// curve flattening and crossings retain their original global coordinates.
/// Crossings outside the requested X range remain present for winding/holes.
pub(crate) fn rasterize_ink_paths_region_measured<C: CancellationToken + ?Sized>(
    paths: &[InkPath],
    canvas_size: PhysicalSize,
    region: PhysicalRect,
    outside: bool,
    limits: &InkLimits,
    cancellation: &C,
) -> Result<InkRegionMask> {
    if !region.fits_within(canvas_size) {
        return Err(InkError::Invalid(
            "Ink raster region must fit within its global canvas".into(),
        ));
    }
    let mut budget = Budget::new(limits, cancellation, 0)?;
    prepare_work(region.size, &mut budget)?;
    let mut output = zeroed(checked_byte_len(region.size)? / 4, &mut budget)?;
    let width = usize::try_from(region.size.width.get())
        .map_err(|_| InkError::Limit("Ink region width exceeds address space".into()))?;
    rows(paths, canvas_size, region, &mut budget, |y, row| {
        for (index, coverage) in row.iter().enumerate() {
            if index.is_multiple_of(1024) && cancellation.is_cancelled() {
                return Err(InkError::Cancelled);
            }
            output[y * width + index] = if outside { 64 - coverage } else { *coverage };
        }
        Ok(())
    })?;
    budget.check()?;
    Ok(InkRegionMask {
        origin: region.origin,
        size: region.size,
        coverage: output,
        work: budget.work,
    })
}

fn rows<C: CancellationToken + ?Sized>(
    paths: &[InkPath],
    canvas_size: PhysicalSize,
    region: PhysicalRect,
    budget: &mut Budget<'_, C>,
    mut consume: impl FnMut(usize, &[u8]) -> Result<()>,
) -> Result<()> {
    // The existing global preparation is intentionally reused unchanged: no
    // subtraction from float points, no re-quantization or curve subdivision.
    let edges = prepare_edges(paths, canvas_size.height.get(), budget)?;
    let mut active: Vec<usize> = reserved(edges.len(), budget)?;
    let mut crossings = reserved(edges.len(), budget)?;
    let mut intervals = reserved(edges.len() / 2, budget)?;
    let mut row = zeroed(usize::try_from(region.size.width.get()).unwrap(), budget)?;
    let mut incoming = 0;
    let start_y = region.origin.y.get();
    let end_y = region
        .end_y()
        .ok_or_else(|| InkError::Invalid("Ink region Y end overflow".into()))?;
    for y in start_y..end_y {
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
            add_region_union(&mut intervals, &mut row, region.origin.x.get(), budget)?;
        }
        consume(usize::try_from(y - start_y).unwrap(), &row)?;
    }
    budget.check()
}

fn add_region_union<C: CancellationToken + ?Sized>(
    intervals: &mut [Interval],
    row: &mut [u8],
    origin_x: u32,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    sort(
        intervals,
        |interval| (interval.left, interval.right),
        budget,
    )?;
    let mut cursor = 0;
    let start = i64::from(origin_x) * 8;
    let end = start + i64::try_from(row.len()).unwrap() * 8;
    while cursor < intervals.len() {
        budget.work(1)?;
        let left = intervals[cursor].left.max(start);
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
            // The only translation is this exact integer destination index,
            // after the global crossing and half-open sample decisions.
            let pixel = usize::try_from(x / 8 - i64::from(origin_x)).unwrap();
            let next = right.min((x / 8 + 1) * 8);
            row[pixel] += u8::try_from(next - x).unwrap();
            x = next;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "region_tests.rs"]
mod tests;
