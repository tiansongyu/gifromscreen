//! Explicit WPF single-visual PM surface, separate from persisted vector V1.
//!
//! `Pbgra32` software layout clipping uses a transparent intermediate followed
//! by a second coverage pass over that completed visual. No PNG/WIC boundary
//! occurs here, and primitive masks are never intersected with the clip mask.

use std::mem::size_of;

use gif_from_screen_domain::{PhysicalSize, Rgba, VectorShape};

use super::{
    MAX_VECTOR_PREVIEW_SHAPES,
    wpf_brush::{WpfBrushPaths, prepare_wpf_brush_paths},
};
use crate::{
    CancellationToken, InkError, InkFigure, InkLimits, InkPath, InkSegment,
    PremultipliedRgbaSurface, RenderLimits, rasterize_ink_paths, surface::checked_byte_len,
    wpf_pixels,
};

type Result<T> = std::result::Result<T, InkError>;

const PHASE_WORK: u64 = 20_000_000;
const MAX_WORK: u64 = 100_000_000;
const PIXEL_BLOCK: usize = 1024;

/// Renders one independently prepared WPF-style visual into PM RGBA8 bytes.
/// Fill precedes stroke; a layout clip covers their completed result once.
/// Track opacity and later canvas/stage compositing belong to the caller.
///
/// Equivalent to [`render_wpf_vector_shapes`] on a one-element slice.
///
/// # Errors
/// Returns the batch renderer's validation, resource and cancellation errors.
pub fn render_wpf_vector_shape<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    size: PhysicalSize,
    limits: RenderLimits,
    cancel: &C,
) -> Result<PremultipliedRgbaSurface> {
    render_wpf_vector_shapes(std::slice::from_ref(shape), size, limits, cancel)
}

/// Renders an ordered WPF shape canvas without changing vector V1 pixels.
/// Shapes without layout clips draw fill/stroke directly onto the canvas;
/// only clipped visuals use a private PM intermediate followed by the mask.
/// The whole private canvas is discarded on error. Empty input is transparent;
/// more than [`super::MAX_VECTOR_PREVIEW_SHAPES`] objects is an explicit error.
///
/// Here `max_surface_bytes` bounds the entire working set: output, actual owned
/// geometry capacities, any clipped-visual PM temporary, and the current mask
/// and raster-scanner allocations. The entire batch has one 100M work ceiling.
/// At most 20M pixel visits are reserved, then remaining work is divided among
/// four possible phases per object (prepare/fill/stroke/clip). Raster phases
/// retain their own subscan/edge charges. Unused reservations are not measured
/// or recycled: a complex single phase can be rejected despite spare actual
/// aggregate work. This is not the production 1080p/4K authoring policy.
///
/// # Errors
/// Rejects invalid/unsupported brush geometry, work or aggregate-memory limits,
/// allocation failure and cancellation. Never returns partially painted output.
pub fn render_wpf_vector_shapes<C: CancellationToken + ?Sized>(
    shapes: &[VectorShape],
    size: PhysicalSize,
    limits: RenderLimits,
    cancel: &C,
) -> Result<PremultipliedRgbaSurface> {
    let mut budget = Budget::new(limits, cancel)?;
    if shapes.len() > MAX_VECTOR_PREVIEW_SHAPES {
        return Err(InkError::Limit("WPF canvas object limit exceeded".into()));
    }
    for shape in shapes {
        budget.check()?;
        shape.validate().map_err(InkError::Invalid)?;
    }
    let byte_len = checked_byte_len(size)?;
    budget.reserve_batch(byte_len / 4, shapes.len())?;
    let mut output = zeroed(byte_len, &mut budget)?;
    for shape in shapes {
        paint_visual(shape, size, &mut output, &mut budget)?;
    }
    budget.check()?;
    let surface = PremultipliedRgbaSurface::new(size, output)?;
    budget.check()?;
    Ok(surface)
}

fn paint_visual<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    size: PhysicalSize,
    canvas: &mut [u8],
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    let retained_before = budget.retained_bytes;
    // The canvas is already retained; reserve a minimum mask while preparing
    // geometry. A clipped visual's extra PM allocation is checked afterward.
    let mut geometry_limits = budget.phase_limits(canvas.len() / 4)?;
    geometry_limits.max_bytes = geometry_limits
        .max_bytes
        .checked_sub(size_of::<WpfBrushPaths>())
        .ok_or_else(memory_limit)?;
    let paths = prepare_wpf_brush_paths(shape, &geometry_limits, budget.cancel)?;
    budget.retain(geometry_bytes(&paths, budget.cancel)?)?;
    if let Some(path) = &paths.layout_clip {
        let mut visual = zeroed(canvas.len(), budget)?;
        paint_primitives(shape, &paths, size, &mut visual, budget)?;
        let coverage = mask(path, size, budget)?;
        clip_mask(&mut visual, &coverage, budget.cancel)?;
        composite_visual(canvas, &visual, budget.cancel)?;
    } else {
        paint_primitives(shape, &paths, size, canvas, budget)?;
    }
    drop(paths);
    // All current-object geometry, mask and optional temporary have been
    // dropped. Only the shared canvas remains retained for the next object.
    budget.retained_bytes = retained_before;
    budget.check()?;
    Ok(())
}

fn paint_primitives<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    paths: &WpfBrushPaths,
    size: PhysicalSize,
    output: &mut [u8],
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    if let Some(color) = shape.fill.filter(|color| color.alpha != 0) {
        paint_path(&paths.fill, color, size, output, budget)?;
    }
    if let Some(path) = &paths.stroke {
        paint_path(path, shape.stroke, size, output, budget)?;
    }
    Ok(())
}

fn composite_visual<C: CancellationToken + ?Sized>(
    canvas: &mut [u8],
    visual: &[u8],
    cancel: &C,
) -> Result<()> {
    if canvas.len() != visual.len() || !canvas.len().is_multiple_of(4) {
        return Err(InkError::Invalid(
            "WPF visual dimensions do not match its canvas".into(),
        ));
    }
    for (index, (target, source)) in canvas
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(visual.as_chunks::<4>().0)
        .enumerate()
    {
        if index.is_multiple_of(PIXEL_BLOCK) {
            check_cancel(cancel)?;
        }
        *target = wpf_pixels::over(*source, *target);
    }
    check_cancel(cancel)
}

fn paint_path<C: CancellationToken + ?Sized>(
    path: &InkPath,
    color: Rgba,
    size: PhysicalSize,
    output: &mut [u8],
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    let mask = mask(path, size, budget)?;
    paint_mask(output, &mask, color, budget.cancel)
}

fn mask<C: CancellationToken + ?Sized>(
    path: &InkPath,
    size: PhysicalSize,
    budget: &mut Budget<'_, C>,
) -> Result<Vec<u8>> {
    let limits = budget.phase_limits(0)?;
    // The paths stay borrowed. The rasterizer's memory budget includes its
    // output mask, edge vectors, sort/crossing/interval arrays and row scratch;
    // all retained geometry and PM bytes have already been subtracted above.
    rasterize_ink_paths(
        std::slice::from_ref(path),
        size,
        false,
        &limits,
        budget.cancel,
    )
}

fn paint_mask<C: CancellationToken + ?Sized>(
    output: &mut [u8],
    mask: &[u8],
    color: Rgba,
    cancel: &C,
) -> Result<()> {
    check_mask(output, mask)?;
    let color = wpf_pixels::premultiply([color.red, color.green, color.blue, color.alpha]);
    for (index, (pixel, &coverage)) in output
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(mask)
        .enumerate()
    {
        if index.is_multiple_of(PIXEL_BLOCK) {
            check_cancel(cancel)?;
        }
        check_coverage(coverage)?;
        let source = color.map(|value| scale_coverage(value, coverage));
        *pixel = wpf_pixels::over(source, *pixel);
    }
    check_cancel(cancel)
}

fn clip_mask<C: CancellationToken + ?Sized>(
    output: &mut [u8],
    mask: &[u8],
    cancel: &C,
) -> Result<()> {
    check_mask(output, mask)?;
    for (index, (pixel, &coverage)) in output
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(mask)
        .enumerate()
    {
        if index.is_multiple_of(PIXEL_BLOCK) {
            check_cancel(cancel)?;
        }
        check_coverage(coverage)?;
        *pixel = pixel.map(|value| scale_coverage(value, coverage));
    }
    check_cancel(cancel)
}

fn check_mask(output: &[u8], mask: &[u8]) -> Result<()> {
    if !output.len().is_multiple_of(4) || output.len() / 4 != mask.len() {
        return Err(InkError::Invalid(
            "WPF coverage mask does not match the output surface".into(),
        ));
    }
    // The rasterizer promises 0..=64. Checking at the point of use below avoids
    // a second uninterruptible image walk; this shape check is constant time.
    Ok(())
}

fn scale_coverage(channel: u8, coverage: u8) -> u8 {
    // The only producer is rasterize_ink_paths, whose public contract is C64.
    // Use a checked conversion, never saturate an invalid coverage into success.
    u8::try_from((u32::from(channel) * u32::from(coverage) * 4 + 128) >> 8)
        .expect("validated ink raster coverage lies in 0..=64")
}

fn check_coverage(coverage: u8) -> Result<()> {
    if coverage > 64 {
        Err(InkError::Invalid("WPF coverage must be in 0..=64".into()))
    } else {
        Ok(())
    }
}

fn geometry_bytes<C: CancellationToken + ?Sized>(
    paths: &WpfBrushPaths,
    cancel: &C,
) -> Result<usize> {
    let mut bytes = size_of::<WpfBrushPaths>();
    for path in std::iter::once(&paths.fill)
        .chain(paths.stroke.iter())
        .chain(paths.layout_clip.iter())
    {
        check_cancel(cancel)?;
        bytes = add_capacity::<InkFigure>(bytes, path.figures.capacity())?;
        for figure in &path.figures {
            check_cancel(cancel)?;
            bytes = add_capacity::<InkSegment>(bytes, figure.segments.capacity())?;
        }
    }
    Ok(bytes)
}

fn add_capacity<T>(bytes: usize, capacity: usize) -> Result<usize> {
    capacity
        .checked_mul(size_of::<T>())
        .and_then(|n| bytes.checked_add(n))
        .ok_or_else(memory_limit)
}

fn zeroed<C: CancellationToken + ?Sized>(
    length: usize,
    budget: &mut Budget<'_, C>,
) -> Result<Vec<u8>> {
    budget.retain(length)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| InkError::Limit("Could not allocate WPF premultiplied surface".into()))?;
    budget.retain(
        bytes
            .capacity()
            .checked_sub(length)
            .ok_or_else(memory_limit)?,
    )?;
    while bytes.len() < length {
        budget.check()?;
        bytes.resize((bytes.len() + PIXEL_BLOCK * 4).min(length), 0);
    }
    budget.check()?;
    Ok(bytes)
}

fn memory_limit() -> InkError {
    InkError::Limit("WPF visual aggregate memory budget exceeded".into())
}

fn check_cancel<C: CancellationToken + ?Sized>(cancel: &C) -> Result<()> {
    if cancel.is_cancelled() {
        Err(InkError::Cancelled)
    } else {
        Ok(())
    }
}

struct Budget<'a, C: CancellationToken + ?Sized> {
    cancel: &'a C,
    max_bytes: usize,
    retained_bytes: usize,
    reserved_work: u64,
    phase_work: u64,
}

impl<'a, C: CancellationToken + ?Sized> Budget<'a, C> {
    fn new(limits: RenderLimits, cancel: &'a C) -> Result<Self> {
        check_cancel(cancel)?;
        Ok(Self {
            cancel,
            max_bytes: limits.max_surface_bytes,
            retained_bytes: 0,
            reserved_work: 0,
            phase_work: PHASE_WORK,
        })
    }

    fn check(&self) -> Result<()> {
        check_cancel(self.cancel)
    }

    fn retain(&mut self, bytes: usize) -> Result<()> {
        self.check()?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.max_bytes)
            .ok_or_else(memory_limit)?;
        Ok(())
    }

    fn reserve_batch(&mut self, pixels: usize, objects: usize) -> Result<()> {
        // Shared initialization+validation; per object at most fill, stroke,
        // temporary initialization, clip and over-canvas. Four channels form
        // one pixel visit. No independent 100M allowance per object.
        let visits = u64::try_from(objects)
            .ok()
            .and_then(|n| n.checked_mul(5))
            .and_then(|n| n.checked_add(2))
            .ok_or_else(memory_limit)?;
        let work = u64::try_from(pixels)
            .ok()
            .and_then(|n| n.checked_mul(visits))
            .filter(|n| *n <= PHASE_WORK)
            .ok_or_else(|| InkError::Limit("WPF visual pixel-work reservation exceeded".into()))?;
        self.reserve_work(work)?;
        let phases = u64::try_from(objects).map_err(|_| memory_limit())? * 4;
        self.phase_work = (MAX_WORK - work).checked_div(phases).unwrap_or(0);
        Ok(())
    }

    fn reserve_work(&mut self, work: u64) -> Result<()> {
        self.check()?;
        self.reserved_work = self
            .reserved_work
            .checked_add(work)
            .filter(|n| *n <= MAX_WORK)
            .ok_or_else(|| {
                InkError::Limit("WPF visual aggregate work reservation exceeded".into())
            })?;
        Ok(())
    }

    fn phase_limits(&mut self, reserved_bytes: usize) -> Result<InkLimits> {
        self.reserve_work(self.phase_work)?;
        let max_bytes = self
            .max_bytes
            .checked_sub(self.retained_bytes)
            .and_then(|n| n.checked_sub(reserved_bytes))
            .ok_or_else(memory_limit)?;
        Ok(InkLimits {
            max_bytes,
            max_work: self.phase_work,
            ..InkLimits::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InkFillRule, NeverCancel};
    use gif_from_screen_domain::{VectorShapeBounds, VectorShapeKind};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn rectangle(stroke: u32, fill: Rgba, line: Rgba) -> VectorShape {
        VectorShape {
            bounds: VectorShapeBounds {
                x_hundredths: 100,
                y_hundredths: 100,
                width_hundredths: 600,
                height_hundredths: 600,
            },
            stroke_width_hundredths: stroke,
            stroke: line,
            fill: Some(fill),
            ..VectorShape::default()
        }
    }

    fn render(shape: &VectorShape) -> PremultipliedRgbaSurface {
        render_wpf_vector_shape(
            shape,
            PhysicalSize::new(8, 8).unwrap(),
            RenderLimits::default(),
            &NeverCancel,
        )
        .unwrap()
    }

    fn pixel(surface: &PremultipliedRgbaSurface, x: usize, y: usize) -> [u8; 4] {
        let index = (y * usize::try_from(surface.size().width.get()).unwrap() + x) * 4;
        surface.pixels()[index..index + 4].try_into().unwrap()
    }

    #[test]
    fn integer_rectangle_has_exact_physical_extent_and_transparent_exterior() {
        let shape = rectangle(
            0,
            Rgba {
                red: 231,
                green: 41,
                blue: 17,
                alpha: 255,
            },
            Rgba::TRANSPARENT,
        );
        let surface = render(&shape);
        for y in 0..8 {
            for x in 0..8 {
                let expected = if (1..7).contains(&x) && (1..7).contains(&y) {
                    [231, 41, 17, 255]
                } else {
                    [0; 4]
                };
                assert_eq!(pixel(&surface, x, y), expected, "({x},{y})");
            }
        }
    }

    #[test]
    fn actual_fill_then_stroke_keeps_pm_rounding_and_hollow_center() {
        let surface = render(&rectangle(
            200,
            Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 128,
            },
            Rgba {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 128,
            },
        ));
        assert_eq!(pixel(&surface, 1, 1), [0, 0, 128, 128]);
        assert_eq!(pixel(&surface, 2, 2), [64, 0, 128, 192]);
        assert_eq!(pixel(&surface, 3, 3), [128, 0, 0, 128]);
    }

    #[test]
    fn clip_covers_completed_pm_visual_not_each_primitive_separately() {
        // Comparator mechanics, not a synthetic Windows golden: two C32
        // primitives followed by one C32 layer clip. Primitive-intersection
        // would give alpha60, whereas a completed-layer clip must give56.
        let red = Rgba {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 128,
        };
        let blue = Rgba {
            red: 0,
            green: 0,
            blue: 255,
            alpha: 128,
        };
        let mut bytes = [0; 4];
        paint_mask(&mut bytes, &[32], red, &NeverCancel).unwrap();
        paint_mask(&mut bytes, &[32], blue, &NeverCancel).unwrap();
        assert_eq!(bytes, [48, 0, 64, 112]);
        clip_mask(&mut bytes, &[32], &NeverCancel).unwrap();
        assert_eq!(bytes, [24, 0, 32, 56]);
    }

    #[test]
    fn all_c64_channels_use_one_integer_coverage_boundary() {
        for channel in 0..=255_u8 {
            for coverage in 0..=64_u8 {
                let expected = (u32::from(channel) * u32::from(coverage) + 32) / 64;
                assert_eq!(u32::from(scale_coverage(channel, coverage)), expected);
            }
        }
    }

    #[test]
    fn zero_axis_layout_clip_means_empty_not_no_clip() {
        let mut shape = rectangle(
            100,
            Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
            Rgba {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
        );
        shape.bounds.width_hundredths = 49;
        assert!(render(&shape).pixels().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn geometry_accounting_uses_capacity_not_length_and_no_pixel_copy() {
        let mut figures = Vec::with_capacity(11);
        let segments = Vec::with_capacity(37);
        figures.push(InkFigure {
            start: crate::InkPoint::default(),
            segments,
            closed: true,
        });
        let paths = WpfBrushPaths {
            fill: InkPath {
                fill_rule: InkFillRule::NonZero,
                figures,
            },
            stroke: None,
            layout_clip: None,
        };
        let expected = size_of::<WpfBrushPaths>()
            + paths.fill.figures.capacity() * size_of::<InkFigure>()
            + paths.fill.figures[0].segments.capacity() * size_of::<InkSegment>();
        assert_eq!(geometry_bytes(&paths, &NeverCancel).unwrap(), expected);
        assert!(expected > size_of::<WpfBrushPaths>() + size_of::<InkFigure>());
    }

    #[test]
    fn aggregate_bytes_and_phase_work_fail_before_successful_output() {
        let shape = rectangle(0, Rgba::TRANSPARENT, Rgba::TRANSPARENT);
        let result = render_wpf_vector_shape(
            &shape,
            PhysicalSize::new(8, 8).unwrap(),
            RenderLimits {
                max_surface_bytes: 256,
            },
            &NeverCancel,
        );
        assert!(matches!(result, Err(InkError::Limit(_))));
        let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        budget.reserve_batch(1_000_000, 2).unwrap();
        for _ in 0..8 {
            assert_eq!(budget.phase_limits(0).unwrap().max_work, 11_000_000);
        }
        assert!(matches!(budget.phase_limits(0), Err(InkError::Limit(_))));
        assert_eq!(budget.reserved_work, MAX_WORK);
        let result = render_wpf_vector_shape(
            &shape,
            PhysicalSize::new(4001, 1000).unwrap(),
            RenderLimits::default(),
            &NeverCancel,
        );
        assert!(matches!(result, Err(InkError::Limit(_))));
    }

    #[test]
    fn raster_scratch_is_not_hidden_behind_output_and_mask_limits() {
        let shape = rectangle(
            0,
            Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            Rgba::TRANSPARENT,
        );
        let paths = prepare_wpf_brush_paths(&shape, &InkLimits::default(), &NeverCancel).unwrap();
        let geometry = geometry_bytes(&paths, &NeverCancel).unwrap();
        // There is room for all retained metadata, the 8x8 PM output and C64
        // mask, but deliberately none for even the first scanner edge vector.
        let mut budget = Budget::new(
            RenderLimits {
                max_surface_bytes: geometry + 256 + 64,
            },
            &NeverCancel,
        )
        .unwrap();
        budget.retain(geometry + 256).unwrap();
        assert!(matches!(
            mask(&paths.fill, PhysicalSize::new(8, 8).unwrap(), &mut budget),
            Err(InkError::Limit(_))
        ));
    }

    #[test]
    fn invalid_mask_shape_or_coverage_is_rejected_without_saturation() {
        let color = Rgba {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        assert!(matches!(
            paint_mask(&mut [0; 4], &[64, 64], color, &NeverCancel),
            Err(InkError::Invalid(_))
        ));
        assert!(matches!(
            paint_mask(&mut [0; 4], &[65], color, &NeverCancel),
            Err(InkError::Invalid(_))
        ));
        assert!(matches!(
            clip_mask(&mut [0; 4], &[255], &NeverCancel),
            Err(InkError::Invalid(_))
        ));
    }

    struct CancelAt {
        calls: AtomicUsize,
        limit: usize,
    }
    impl CancellationToken for CancelAt {
        fn is_cancelled(&self) -> bool {
            self.calls.fetch_add(1, Ordering::Relaxed) >= self.limit
        }
    }

    #[test]
    fn cancellation_at_each_observed_stage_never_returns_partial_pm() {
        let mut shape = rectangle(
            1000,
            Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 128,
            },
            Rgba {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 128,
            },
        );
        shape.bounds.width_hundredths = 100;
        shape.bounds.height_hundredths = 200;
        let size = PhysicalSize::new(8, 8).unwrap();
        let count = CancelAt {
            calls: AtomicUsize::new(0),
            limit: usize::MAX,
        };
        render_wpf_vector_shape(&shape, size, RenderLimits::default(), &count).unwrap();
        let checkpoints = count.calls.load(Ordering::Relaxed);
        assert!(checkpoints > 20 && checkpoints < 10_000);
        for limit in 0..checkpoints {
            let cancel = CancelAt {
                calls: AtomicUsize::new(0),
                limit,
            };
            assert!(
                matches!(
                    render_wpf_vector_shape(&shape, size, RenderLimits::default(), &cancel),
                    Err(InkError::Cancelled)
                ),
                "checkpoint {limit}"
            );
        }
    }

    #[test]
    fn transparent_invalid_or_unknown_shape_never_bypasses_validation() {
        let mut shape = rectangle(0, Rgba::TRANSPARENT, Rgba::TRANSPARENT);
        shape.version = 0;
        let result = render_wpf_vector_shape(
            &shape,
            PhysicalSize::new(8, 8).unwrap(),
            RenderLimits::default(),
            &NeverCancel,
        );
        assert!(matches!(result, Err(InkError::Invalid(_))));
        // All four kinds are still dispatched to the real preparer. Fill-only
        // paths are available without pretending unsupported widening passed.
        shape.version = gif_from_screen_domain::VECTOR_SHAPE_VERSION;
        for kind in [
            VectorShapeKind::Rectangle,
            VectorShapeKind::Ellipse,
            VectorShapeKind::Triangle,
            VectorShapeKind::BlockArrow,
        ] {
            shape.kind = kind;
            assert!(render(&shape).pixels().iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn every_shape_uses_real_fill_and_stroke_paths_without_mutating_metadata() {
        // Consumer lifecycle coverage, not independent WPF pixel equivalence.
        // Native conformance remains the external Windows fixture comparison.
        for kind in [
            VectorShapeKind::Rectangle,
            VectorShapeKind::Ellipse,
            VectorShapeKind::Triangle,
            VectorShapeKind::BlockArrow,
        ] {
            let mut shape = rectangle(
                50,
                Rgba {
                    red: 210,
                    green: 42,
                    blue: 19,
                    alpha: 192,
                },
                Rgba {
                    red: 17,
                    green: 99,
                    blue: 231,
                    alpha: 128,
                },
            );
            shape.kind = kind;
            shape.rotation_hundredths = 1700;
            let original = shape;
            let surface = render(&shape);
            assert!(
                surface
                    .pixels()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0),
                "{kind:?}"
            );
            assert!(
                surface
                    .pixels()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[..3].iter().all(|channel| *channel <= pixel[3]))
            );
            assert_eq!(shape, original);
        }
    }

    #[test]
    fn paint_and_clip_cancel_before_the_next_pixel_block() {
        let color = Rgba {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        let mask = vec![32; PIXEL_BLOCK * 2];
        let mut pixels = vec![0; PIXEL_BLOCK * 8];
        let cancel = CancelAt {
            calls: AtomicUsize::new(0),
            limit: 1,
        };
        assert_eq!(
            paint_mask(&mut pixels, &mask, color, &cancel),
            Err(InkError::Cancelled)
        );
        assert_eq!(&pixels[0..4], &[128, 0, 0, 128]);
        assert!(pixels[PIXEL_BLOCK * 4..].iter().all(|byte| *byte == 0));
        pixels.fill(128);
        let cancel = CancelAt {
            calls: AtomicUsize::new(0),
            limit: 1,
        };
        assert_eq!(
            clip_mask(&mut pixels, &mask, &cancel),
            Err(InkError::Cancelled)
        );
        assert_eq!(&pixels[0..4], &[64; 4]);
        assert!(pixels[PIXEL_BLOCK * 4..].iter().all(|byte| *byte == 128));
    }

    #[test]
    fn unclipped_shapes_draw_each_primitive_directly_into_shared_canvas() {
        let shape = rectangle(
            200,
            Rgba {
                red: 210,
                green: 37,
                blue: 93,
                alpha: 17,
            },
            Rgba {
                red: 41,
                green: 199,
                blue: 72,
                alpha: 117,
            },
        );
        let combined = render_wpf_vector_shapes(
            &[shape, shape],
            PhysicalSize::new(8, 8).unwrap(),
            RenderLimits::default(),
            &NeverCancel,
        )
        .unwrap();
        // At (2,2) each shape has fully covered fill and stroke. The reference
        // order is fill->stroke->fill->stroke. Grouping each pair first loses
        // distinct 8-bit rounding boundaries even though alpha stays the same.
        assert_eq!(pixel(&combined, 2, 2), [40, 139, 55, 190]);
        let single = render(&shape);
        let isolated = wpf_pixels::over(pixel(&single, 2, 2), pixel(&single, 2, 2));
        assert_eq!(isolated, [41, 139, 54, 190]);
        assert_ne!(pixel(&combined, 2, 2), isolated);
    }

    #[test]
    fn batch_empty_is_transparent_and_object_overflow_is_not_truncated() {
        let size = PhysicalSize::new(8, 8).unwrap();
        assert!(
            render_wpf_vector_shapes(&[], size, RenderLimits::default(), &NeverCancel)
                .unwrap()
                .pixels()
                .iter()
                .all(|byte| *byte == 0)
        );
        let shapes = [VectorShape::default(); MAX_VECTOR_PREVIEW_SHAPES + 1];
        assert!(matches!(
            render_wpf_vector_shapes(&shapes, size, RenderLimits::default(), &NeverCancel),
            Err(InkError::Limit(_))
        ));
        let mut shapes = [VectorShape::default(); 2];
        shapes[1].version = 0;
        assert!(matches!(
            render_wpf_vector_shapes(&shapes, size, RenderLimits::default(), &NeverCancel),
            Err(InkError::Invalid(_))
        ));
    }
}
