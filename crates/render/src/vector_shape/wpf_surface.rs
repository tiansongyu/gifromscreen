//! Explicit WPF single-visual PM surface, separate from persisted vector V1.
//!
//! `Pbgra32` software layout clipping uses a transparent intermediate followed
//! by a second coverage pass over that completed visual. No PNG/WIC boundary
//! occurs here, and primitive masks are never intersected with the clip mask.

use std::mem::size_of;

use gif_from_screen_domain::{PhysicalRect, PhysicalSize, Rgba, VectorShape};

use super::{
    MAX_VECTOR_PREVIEW_SHAPES,
    wpf_brush::{WpfBrushPaths, prepare_wpf_brush_paths_measured},
};
use crate::{
    CancellationToken, InkError, InkFigure, InkLimits, InkPath, InkSegment,
    PremultipliedRgbaSurface, RenderLimits,
    ink_raster::{InkRegionMask, rasterize_ink_paths_region_measured},
    surface::checked_byte_len,
    wpf_pixels,
};

type Result<T> = std::result::Result<T, InkError>;

// Candidate-only measured batch policy, chosen after real 4K two-shape
// measurement (163,532,284 units). Legacy V1 and InkLimits defaults are unchanged.
const MAX_WORK: u64 = 250_000_000;
const PIXEL_BLOCK: usize = 1024;

mod spatial;
use spatial::{Target, bounds, intersect, union, whole};

/// Borrowed shape metadata and one post-mark opacity, not WPF Shape.Opacity.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WpfVectorVisual<'a> {
    pub shape: &'a VectorShape,
    pub opacity: u8,
}

/// Preserves input z-order and applies opacity once to each completed mark.
/// Unclipped opacity-255 marks retain direct primitive-to-canvas painting.
/// Clipped/translucent marks use a bounded regional PM layer; zero opacity
/// still validates metadata, object count and geometry/memory/work limits.
pub(crate) fn render_wpf_vector_visuals<C: CancellationToken + ?Sized>(
    visuals: &[WpfVectorVisual<'_>],
    size: PhysicalSize,
    limits: RenderLimits,
    cancel: &C,
) -> Result<PremultipliedRgbaSurface> {
    render_wpf_vector_visuals_with_tail_work(visuals, size, limits, 0, cancel)
}

/// Reserves a caller-owned final pixel pass inside the same 250M ceiling.
/// The reservation is not reported as work performed by this surface builder.
/// After success the caller must perform its declared tail work (for example,
/// final PM-over-base plus unpremultiplication); on error it discards the private
/// output. Geometry, mask and pixel work receive only the unreserved remainder.
pub(crate) fn render_wpf_vector_visuals_with_tail_work<C: CancellationToken + ?Sized>(
    visuals: &[WpfVectorVisual<'_>],
    size: PhysicalSize,
    limits: RenderLimits,
    trailing_pixel_visits: u64,
    cancel: &C,
) -> Result<PremultipliedRgbaSurface> {
    let mut budget = Budget::new(limits, cancel)?;
    budget.reserve_tail(trailing_pixel_visits)?;
    render_visual_canvas(visuals.iter().copied(), size, &mut budget)
}

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
    render_wpf_vector_visuals(
        &[WpfVectorVisual {
            shape,
            opacity: 255,
        }],
        size,
        limits,
        cancel,
    )
}

/// Renders an ordered WPF shape canvas without changing vector V1 pixels.
/// Shapes without layout clips draw fill/stroke directly onto the canvas;
/// only clipped visuals use a private PM intermediate followed by the mask.
/// The whole private canvas is discarded on error. Empty input is transparent;
/// more than [`super::MAX_VECTOR_PREVIEW_SHAPES`] objects is an explicit error.
///
/// Here `max_surface_bytes` bounds the entire working set: output, actual owned
/// geometry capacities, any clipped-visual PM temporary, and the current mask
/// and raster-scanner allocations. The entire batch has one 250M work ceiling.
/// Geometry and raster phases receive the remaining batch allowance and return
/// their actually charged work. Only executed initialization, paint, clip,
/// composition and validation pixel passes are charged here. There is no
/// per-object quota or independently reset allowance; raster subscan/edge costs
/// remain part of the shared ceiling.
/// This is a render-only resource policy, not a GIF encoding or capture-FPS
/// promise. The measured 1080p/4K cases do not cover every object-count/scale mix.
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
    render_canvas(shapes, size, &mut budget)
}

fn render_canvas<C: CancellationToken + ?Sized>(
    shapes: &[VectorShape],
    size: PhysicalSize,
    budget: &mut Budget<'_, C>,
) -> Result<PremultipliedRgbaSurface> {
    render_visual_canvas(
        shapes.iter().map(|shape| WpfVectorVisual {
            shape,
            opacity: 255,
        }),
        size,
        budget,
    )
}

fn render_visual_canvas<'a, C: CancellationToken + ?Sized>(
    visuals: impl ExactSizeIterator<Item = WpfVectorVisual<'a>> + Clone,
    size: PhysicalSize,
    budget: &mut Budget<'_, C>,
) -> Result<PremultipliedRgbaSurface> {
    if visuals.len() > MAX_VECTOR_PREVIEW_SHAPES {
        return Err(InkError::Limit("WPF canvas object limit exceeded".into()));
    }
    for visual in visuals.clone() {
        budget.check()?;
        visual.shape.validate().map_err(InkError::Invalid)?;
    }
    let byte_len = checked_byte_len(size)?;
    let mut output = zeroed(byte_len, budget)?;
    for visual in visuals {
        paint_visual(visual, size, &mut output, budget)?;
    }
    budget.charge_pixels(byte_len / 4)?;
    let surface = PremultipliedRgbaSurface::new(size, output)?;
    budget.check()?;
    Ok(surface)
}

fn paint_visual<C: CancellationToken + ?Sized>(
    visual: WpfVectorVisual<'_>,
    size: PhysicalSize,
    canvas: &mut [u8],
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    let shape = visual.shape;
    let retained_before = budget.retained_bytes;
    // The canvas is already retained; reserve a minimum mask while preparing
    // geometry. A clipped visual's extra PM allocation is checked afterward.
    let mut geometry_limits = budget.phase_limits(0)?;
    geometry_limits.max_bytes = geometry_limits
        .max_bytes
        .checked_sub(size_of::<WpfBrushPaths>())
        .ok_or_else(memory_limit)?;
    let (paths, used) = prepare_wpf_brush_paths_measured(shape, &geometry_limits, budget.cancel)
        .map_err(|error| budget.phase_error("brush", error))?;
    budget.charge(WorkKind::Brush, used)?;
    budget.retain(geometry_bytes(&paths, budget.cancel)?)?;
    let mut target = Target {
        area: whole(size),
        pixels: canvas,
    };
    if visual.opacity == 255 && paths.layout_clip.is_none() {
        paint_primitives(shape, &paths, size, &mut target, budget)?;
    } else if visual.opacity != 0 {
        paint_isolated_visual(visual, &paths, size, &mut target, budget)?;
    }
    drop(paths);
    // All current-object geometry, mask and optional temporary have been
    // dropped. Only the shared canvas remains retained for the next object.
    budget.retained_bytes = retained_before;
    budget.check()?;
    Ok(())
}

fn paint_isolated_visual<C: CancellationToken + ?Sized>(
    visual: WpfVectorVisual<'_>,
    paths: &WpfBrushPaths,
    size: PhysicalSize,
    target: &mut Target<'_>,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    let mut area = painted_bounds(visual.shape, paths, size, budget)?;
    if let Some(clip) = &paths.layout_clip {
        area = area
            .zip(bounds(clip, size, budget)?)
            .and_then(|(ink, clip)| intersect(ink, clip));
    }
    let Some(area) = area else {
        return Ok(());
    };
    let mut pixels = zeroed(checked_byte_len(area.size)?, budget)?;
    let mut local = Target {
        area,
        pixels: &mut pixels,
    };
    paint_primitives(visual.shape, paths, size, &mut local, budget)?;
    if let Some(path) = &paths.layout_clip {
        let coverage = mask(path, size, area, budget)?;
        local.apply(&coverage, None, budget)?;
    }
    if visual.opacity != 255 {
        scale_opacity(&mut pixels, visual.opacity, budget)?;
    }
    target.composite(area, &pixels, budget)
}

fn scale_opacity<C: CancellationToken + ?Sized>(
    pixels: &mut [u8],
    opacity: u8,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    if !pixels.len().is_multiple_of(4) {
        return Err(InkError::Invalid(
            "WPF mark opacity requires whole PM RGBA pixels".into(),
        ));
    }
    budget.charge_pixels(pixels.len() / 4)?;
    for (index, channel) in pixels.iter_mut().enumerate() {
        if index.is_multiple_of(PIXEL_BLOCK * 4) {
            budget.check()?;
        }
        *channel = wpf_pixels::mul_byte(*channel, opacity);
    }
    budget.check()
}

fn painted_bounds<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    paths: &WpfBrushPaths,
    size: PhysicalSize,
    budget: &mut Budget<'_, C>,
) -> Result<Option<PhysicalRect>> {
    let fill = if shape.fill.is_some_and(|color| color.alpha != 0) {
        bounds(&paths.fill, size, budget)?
    } else {
        None
    };
    let stroke = paths
        .stroke
        .as_ref()
        .map(|path| bounds(path, size, budget))
        .transpose()?
        .flatten();
    Ok(union(fill, stroke))
}

fn paint_primitives<C: CancellationToken + ?Sized>(
    shape: &VectorShape,
    paths: &WpfBrushPaths,
    size: PhysicalSize,
    output: &mut Target<'_>,
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
    output: &mut Target<'_>,
    budget: &mut Budget<'_, C>,
) -> Result<()> {
    if let Some(area) = bounds(path, size, budget)?.and_then(|area| intersect(area, output.area)) {
        let mask = mask(path, size, area, budget)?;
        output.apply(&mask, Some(color), budget)?;
    }
    Ok(())
}

fn mask<C: CancellationToken + ?Sized>(
    path: &InkPath,
    size: PhysicalSize,
    area: PhysicalRect,
    budget: &mut Budget<'_, C>,
) -> Result<InkRegionMask> {
    let limits = budget.phase_limits(0)?;
    // The paths stay borrowed. The rasterizer's memory budget includes its
    // output mask, edge vectors, sort/crossing/interval arrays and row scratch;
    // all retained geometry and PM bytes have already been subtracted above.
    let mask = rasterize_ink_paths_region_measured(
        std::slice::from_ref(path),
        size,
        area,
        false,
        &limits,
        budget.cancel,
    )
    .map_err(|error| budget.phase_error("raster", error))?;
    budget.charge(WorkKind::Raster, mask.work)?;
    Ok(mask)
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
    budget.charge_pixels(length / 4)?;
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
    maximum_work: u64,
    usage: WorkUsage,
    reserved_tail: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WorkUsage {
    brush: u64,
    raster: u64,
    pixels: u64,
}

impl WorkUsage {
    fn total(self) -> u64 {
        self.brush + self.raster + self.pixels
    }
}

#[derive(Clone, Copy)]
enum WorkKind {
    Brush,
    Raster,
    Pixels,
}

impl<'a, C: CancellationToken + ?Sized> Budget<'a, C> {
    fn new(limits: RenderLimits, cancel: &'a C) -> Result<Self> {
        check_cancel(cancel)?;
        Ok(Self {
            cancel,
            max_bytes: limits.max_surface_bytes,
            retained_bytes: 0,
            maximum_work: MAX_WORK,
            usage: WorkUsage::default(),
            reserved_tail: 0,
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

    fn charge_pixels(&mut self, pixels: usize) -> Result<()> {
        self.charge(
            WorkKind::Pixels,
            u64::try_from(pixels).map_err(|_| memory_limit())?,
        )
    }

    fn charge(&mut self, kind: WorkKind, work: u64) -> Result<()> {
        self.check()?;
        self.usage
            .total()
            .checked_add(work)
            .and_then(|used| used.checked_add(self.reserved_tail))
            .filter(|n| *n <= self.maximum_work)
            .ok_or_else(|| {
                InkError::Limit(format!(
                    "WPF canvas aggregate work exceeded: charged {}, next {work}, reserved tail {}, limit {}",
                    self.usage.total(),
                    self.reserved_tail,
                    self.maximum_work
                ))
            })?;
        let counter = match kind {
            WorkKind::Brush => &mut self.usage.brush,
            WorkKind::Raster => &mut self.usage.raster,
            WorkKind::Pixels => &mut self.usage.pixels,
        };
        *counter += work;
        Ok(())
    }

    fn reserve_tail(&mut self, work: u64) -> Result<()> {
        self.check()?;
        if self
            .usage
            .total()
            .checked_add(work)
            .is_none_or(|n| n > self.maximum_work)
        {
            return Err(InkError::Limit(
                "WPF final-pass work reservation exceeds the batch ceiling".into(),
            ));
        }
        self.reserved_tail = work;
        Ok(())
    }

    fn remaining_work(&self) -> u64 {
        self.maximum_work - self.reserved_tail - self.usage.total()
    }

    fn phase_limits(&self, reserved_bytes: usize) -> Result<InkLimits> {
        self.check()?;
        let max_bytes = self
            .max_bytes
            .checked_sub(self.retained_bytes)
            .and_then(|n| n.checked_sub(reserved_bytes))
            .ok_or_else(memory_limit)?;
        Ok(InkLimits {
            max_bytes,
            max_work: self.remaining_work(),
            ..InkLimits::default()
        })
    }

    fn phase_error(&self, phase: &str, error: InkError) -> InkError {
        match error {
            InkError::Limit(reason) => InkError::Limit(format!(
                "{reason}; {phase} phase had {} work remaining after {} charged (reserved tail {}, limit {})",
                self.remaining_work(),
                self.usage.total(),
                self.reserved_tail,
                self.maximum_work
            )),
            other => other,
        }
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
        budget.charge_pixels(1_000_000).unwrap();
        assert_eq!(
            budget.phase_limits(0).unwrap().max_work,
            MAX_WORK - 1_000_000
        );
        budget.charge(WorkKind::Brush, 123).unwrap();
        assert_eq!(
            budget.phase_limits(0).unwrap().max_work,
            MAX_WORK - 1_000_123
        );
        budget
            .charge(WorkKind::Raster, MAX_WORK - 1_000_123)
            .unwrap();
        assert_eq!(budget.phase_limits(0).unwrap().max_work, 0);
        assert!(matches!(budget.charge_pixels(1), Err(InkError::Limit(_))));
        assert_eq!(budget.usage.total(), MAX_WORK);
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
        let (paths, _) =
            prepare_wpf_brush_paths_measured(&shape, &InkLimits::default(), &NeverCancel).unwrap();
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
            mask(
                &paths.fill,
                PhysicalSize::new(8, 8).unwrap(),
                whole(PhysicalSize::new(8, 8).unwrap()),
                &mut budget
            ),
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

    #[test]
    fn measured_batch_charge_is_exact_and_one_less_is_rejected() {
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
        let shapes = [shape, shape];
        let size = PhysicalSize::new(8, 8).unwrap();
        let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        let expected = render_canvas(&shapes, size, &mut budget).unwrap();
        let used = budget.usage;
        assert!(used.brush > 0 && used.raster > 0 && used.pixels > 0);
        assert_eq!(used.pixels, 64 * 6); // init + four primitives + validation
        let mut exact = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        exact.maximum_work = used.total();
        assert_eq!(render_canvas(&shapes, size, &mut exact).unwrap(), expected);
        assert_eq!(exact.usage, used);
        let mut short = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        short.maximum_work = used.total() - 1;
        assert!(matches!(
            render_canvas(&shapes, size, &mut short),
            Err(InkError::Limit(_))
        ));
    }

    fn scale_shapes(size: PhysicalSize, stroke: u32, second: bool) -> Vec<VectorShape> {
        let mut shape = rectangle(
            stroke,
            Rgba {
                red: 40,
                green: 90,
                blue: 160,
                alpha: 255,
            },
            Rgba {
                red: 10,
                green: 20,
                blue: 30,
                alpha: 255,
            },
        );
        shape.bounds = VectorShapeBounds {
            x_hundredths: 0,
            y_hundredths: 0,
            width_hundredths: u64::from(size.width.get()) * 100,
            height_hundredths: u64::from(size.height.get()) * 100,
        };
        let mut shapes = vec![shape];
        if second {
            shape.kind = VectorShapeKind::Ellipse;
            shape.bounds.x_hundredths = i64::from(size.width.get()) * 25;
            shape.bounds.y_hundredths = i64::from(size.height.get()) * 25;
            shape.bounds.width_hundredths /= 2;
            shape.bounds.height_hundredths /= 2;
            shape.stroke_width_hundredths = 400;
            shape.fill = Some(Rgba {
                red: 220,
                green: 70,
                blue: 30,
                alpha: 173,
            });
            shapes.push(shape);
        }
        shapes
    }

    #[test]
    fn full_1080p_two_filled_and_stroked_shapes_render_at_requested_size() {
        let size = PhysicalSize::new(1920, 1080).unwrap();
        let shapes = scale_shapes(size, 400, true);
        let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        let surface = render_canvas(&shapes, size, &mut budget).unwrap();
        assert_eq!(surface.size(), size);
        assert_eq!(surface.pixels().len(), 1920 * 1080 * 4);
        assert_eq!(pixel(&surface, 0, 0), [10, 20, 30, 255]);
        assert_eq!(pixel(&surface, 200, 200), [40, 90, 160, 255]);
        assert_ne!(pixel(&surface, 960, 540), [40, 90, 160, 255]);
        assert!(budget.usage.total() <= MAX_WORK);
    }

    #[test]
    fn full_4k_two_filled_and_stroked_shapes_use_the_candidate_batch_policy() {
        let size = PhysicalSize::new(3840, 2160).unwrap();
        let shapes = scale_shapes(size, 400, true);
        let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        let surface = render_canvas(&shapes, size, &mut budget).unwrap();
        assert_eq!(surface.size(), size);
        assert_eq!(surface.pixels().len(), 3840 * 2160 * 4);
        assert_eq!(pixel(&surface, 0, 0), [10, 20, 30, 255]);
        assert_eq!(pixel(&surface, 200, 200), [40, 90, 160, 255]);
        assert_ne!(pixel(&surface, 1920, 1080), [40, 90, 160, 255]);
        assert!(budget.usage.total() <= MAX_WORK);
        assert_eq!(InkLimits::default().max_work, 100_000_000);
    }

    #[test]
    #[ignore = "explicit debug/release production-size audit; prints actual charges and preserves failures"]
    fn inspect_production_scale() {
        inspect_scale(MAX_WORK);
    }

    #[test]
    #[ignore = "test-only authorized 500M measurement; does not change production default or fixtures"]
    fn inspect_production_scale_500m() {
        inspect_scale(500_000_000);
    }

    fn inspect_scale(maximum_work: u64) {
        let mut rejected = Vec::new();
        for (name, width, height, stroke, second) in [
            ("1080p-two-filled-stroked", 1920, 1080, 400, true),
            ("4k-one-full-fill", 3840, 2160, 0, false),
            ("4k-one-full-fill-stroke", 3840, 2160, 400, false),
            ("4k-two-filled-stroked", 3840, 2160, 400, true),
        ] {
            let size = PhysicalSize::new(width, height).unwrap();
            let shapes = scale_shapes(size, stroke, second);
            let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
            budget.maximum_work = maximum_work;
            let started = std::time::Instant::now();
            let result = render_canvas(&shapes, size, &mut budget);
            let elapsed = started.elapsed();
            eprintln!(
                "WPF_SCALE {}",
                serde_json::json!({
                    "case":name,"width":width,"height":height,"objects":shapes.len(),
                    "debug_assertions":cfg!(debug_assertions),"architecture":std::env::consts::ARCH,
                    "executable":std::env::current_exe().unwrap(),
                "elapsed_seconds":elapsed.as_secs_f64(),"maximum_work":maximum_work,
                    "charged_brush":budget.usage.brush,"charged_raster":budget.usage.raster,
                    "charged_pixels":budget.usage.pixels,"charged_total":budget.usage.total(),
                "remaining_work":maximum_work-budget.usage.total(),
                    "success":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string),
                    "failed_phase_work_is_unreported":result.is_err(),
                })
            );
            match result {
                Ok(surface) => {
                    assert_eq!(surface.size(), size);
                    assert_eq!(
                        pixel(&surface, 0, 0),
                        if stroke == 0 {
                            [40, 90, 160, 255]
                        } else {
                            [10, 20, 30, 255]
                        }
                    );
                }
                Err(error) => rejected.push(format!("{name}: {error}")),
            }
        }
        assert!(
            rejected.is_empty(),
            "Production-size audit has real failures: {rejected:#?}"
        );
    }

    fn small_shapes(count: usize) -> Vec<VectorShape> {
        let kinds = [
            VectorShapeKind::Rectangle,
            VectorShapeKind::Ellipse,
            VectorShapeKind::Triangle,
            VectorShapeKind::BlockArrow,
        ];
        (0..count)
            .map(|index| VectorShape {
                kind: kinds[index % kinds.len()],
                bounds: VectorShapeBounds {
                    x_hundredths: i64::try_from((index % 32) * 110 + 20).unwrap() * 100,
                    y_hundredths: i64::try_from((index / 32) * 220 + 20).unwrap() * 100,
                    width_hundredths: 4000,
                    height_hundredths: 2400,
                },
                stroke_width_hundredths: 200,
                stroke: Rgba {
                    red: 10,
                    green: 20,
                    blue: 30,
                    alpha: 255,
                },
                fill: Some(Rgba {
                    red: 70,
                    green: 140,
                    blue: 210,
                    alpha: 173,
                }),
                rotation_hundredths: u16::try_from(index % 3).unwrap() * 1700,
                ..VectorShape::default()
            })
            .collect()
    }

    #[test]
    fn full_4k_32_and_256_small_shapes_keep_every_object_without_full_masks() {
        let size = PhysicalSize::new(3840, 2160).unwrap();
        for count in [32, 256] {
            let shapes = small_shapes(count);
            let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
            let surface = render_canvas(&shapes, size, &mut budget).unwrap();
            assert_eq!(surface.size(), size);
            assert_eq!(surface.pixels().len(), 3840 * 2160 * 4);
            for index in 0..count {
                assert!(
                    pixel(&surface, (index % 32) * 110 + 40, (index / 32) * 220 + 32)[3] > 0,
                    "missing object {index}"
                );
            }
            assert!(budget.usage.total() <= MAX_WORK);
        }
    }

    fn full_canvas_reference(
        shapes: &[VectorShape],
        size: PhysicalSize,
    ) -> PremultipliedRgbaSurface {
        // Regression oracle for this spatial optimization only: the prior full
        // masks and blend order, not independently generated Windows pixels.
        let mut canvas = vec![0; checked_byte_len(size).unwrap()];
        let mask_for = |path: &InkPath| {
            crate::rasterize_ink_paths(
                std::slice::from_ref(path),
                size,
                false,
                &InkLimits::default(),
                &NeverCancel,
            )
            .unwrap()
        };
        for shape in shapes {
            let (paths, _) =
                prepare_wpf_brush_paths_measured(shape, &InkLimits::default(), &NeverCancel)
                    .unwrap();
            let mut layer = paths.layout_clip.as_ref().map(|_| vec![0; canvas.len()]);
            let target = layer.as_mut().unwrap_or(&mut canvas);
            if let Some(fill) = shape.fill.filter(|c| c.alpha != 0) {
                paint_mask(target, &mask_for(&paths.fill), fill, &NeverCancel).unwrap();
            }
            if let Some(stroke) = &paths.stroke {
                paint_mask(target, &mask_for(stroke), shape.stroke, &NeverCancel).unwrap();
            }
            if let Some(clip) = &paths.layout_clip {
                clip_mask(target, &mask_for(clip), &NeverCancel).unwrap();
                composite_visual(&mut canvas, &layer.unwrap(), &NeverCancel).unwrap();
            }
        }
        PremultipliedRgbaSurface::new(size, canvas).unwrap()
    }

    #[test]
    fn regional_clipped_visual_over_existing_canvas_matches_previous_full_masks() {
        let size = PhysicalSize::new(80, 60).unwrap();
        let base = VectorShape {
            bounds: VectorShapeBounds {
                x_hundredths: 0,
                y_hundredths: 0,
                width_hundredths: 8000,
                height_hundredths: 6000,
            },
            stroke_width_hundredths: 0,
            fill: Some(Rgba {
                red: 30,
                green: 90,
                blue: 160,
                alpha: 173,
            }),
            ..VectorShape::default()
        };
        let clipped = VectorShape {
            kind: VectorShapeKind::Triangle,
            bounds: VectorShapeBounds {
                x_hundredths: 3000,
                y_hundredths: 2000,
                width_hundredths: 1500,
                height_hundredths: 1125,
            },
            stroke_width_hundredths: 225,
            rotation_hundredths: 3300,
            stroke: Rgba {
                red: 200,
                green: 35,
                blue: 61,
                alpha: 117,
            },
            fill: Some(Rgba {
                red: 40,
                green: 130,
                blue: 210,
                alpha: 173,
            }),
            ..VectorShape::default()
        };
        let shapes = [
            base,
            clipped,
            VectorShape {
                rotation_hundredths: 0,
                bounds: VectorShapeBounds {
                    x_hundredths: -3000,
                    y_hundredths: -2000,
                    ..clipped.bounds
                },
                ..clipped
            },
        ];
        let expected = full_canvas_reference(&shapes, size);
        let actual =
            render_wpf_vector_shapes(&shapes, size, RenderLimits::default(), &NeverCancel).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            pixel(&actual, 0, 0),
            wpf_pixels::premultiply([30, 90, 160, 173])
        );
    }

    #[test]
    #[ignore = "explicit 32/256-small-shape 4K audit; retain baseline failures and optimized measurements"]
    fn inspect_many_small_4k() {
        let size = PhysicalSize::new(3840, 2160).unwrap();
        let mut rejected = Vec::new();
        for count in [32, 256] {
            let shapes = small_shapes(count);
            let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
            let start = std::time::Instant::now();
            let result = render_canvas(&shapes, size, &mut budget);
            eprintln!(
                "WPF_SMALL {}",
                serde_json::json!({
                    "width":3840,"height":2160,"objects":count,"shape_dimensions":[40,24],
                    "debug_assertions":cfg!(debug_assertions),"executable":std::env::current_exe().unwrap(),
                    "elapsed_seconds":start.elapsed().as_secs_f64(),"maximum_work":MAX_WORK,
                    "brush":budget.usage.brush,"raster":budget.usage.raster,"pixels":budget.usage.pixels,
                    "charged_total":budget.usage.total(),"error":result.as_ref().err().map(ToString::to_string),
                    "success":result.is_ok(),"failed_phase_work_is_unreported":result.is_err(),
                })
            );
            match result {
                Ok(surface) => {
                    assert_eq!(surface.size(), size);
                    for index in 0..count {
                        let x = (index % 32) * 110 + 40;
                        let y = (index / 32) * 220 + 32;
                        assert!(pixel(&surface, x, y)[3] > 0, "missing object {index}");
                    }
                }
                Err(error) => rejected.push(format!("{count} objects: {error}")),
            }
        }
        assert!(
            rejected.is_empty(),
            "Small-shape audit failed: {rejected:#?}"
        );
    }

    fn borrowed_render(
        visuals: &[WpfVectorVisual<'_>],
        size: PhysicalSize,
    ) -> PremultipliedRgbaSurface {
        render_wpf_vector_visuals(visuals, size, RenderLimits::default(), &NeverCancel).unwrap()
    }

    #[test]
    fn borrowed_opaque_marks_preserve_direct_canvas_rounding() {
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
        let size = PhysicalSize::new(8, 8).unwrap();
        let items = [WpfVectorVisual {
            shape: &shape,
            opacity: 255,
        }; 2];
        let expected =
            render_wpf_vector_shapes(&[shape, shape], size, RenderLimits::default(), &NeverCancel)
                .unwrap();
        assert_eq!(borrowed_render(&items, size), expected);
        assert_eq!(pixel(&expected, 2, 2), [40, 139, 55, 190]);
        assert!(std::ptr::eq(
            std::ptr::from_ref(items[0].shape),
            std::ptr::from_ref(&shape)
        ));
    }

    #[test]
    fn post_mark_opacity_scales_completed_fill_and_stroke_once() {
        let mut shape = rectangle(
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
        );
        shape.version = gif_from_screen_domain::WPF_VECTOR_SHAPE_VERSION;
        let original = shape;
        let actual = borrowed_render(
            &[WpfVectorVisual {
                shape: &shape,
                opacity: 128,
            }],
            PhysicalSize::new(8, 8).unwrap(),
        );
        assert_eq!(pixel(&actual, 2, 2), [32, 0, 64, 96]);
        assert_ne!(pixel(&actual, 2, 2), [48, 0, 64, 112]);
        assert_eq!(pixel(&actual, 3, 3), [64, 0, 0, 64]);
        assert_eq!(pixel(&actual, 1, 1), [0, 0, 64, 64]);
        assert_eq!(shape, original);
    }

    #[test]
    fn clip_precedes_opacity_with_noncommutative_integer_rounding() {
        let mut correct = [2, 0, 0, 2];
        clip_mask(&mut correct, &[16], &NeverCancel).unwrap();
        let mut budget = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        scale_opacity(&mut correct, 128, &mut budget).unwrap();
        assert_eq!(correct, [1, 0, 0, 1]);
        let mut reversed = [2, 0, 0, 2];
        scale_opacity(&mut reversed, 128, &mut budget).unwrap();
        clip_mask(&mut reversed, &[16], &NeverCancel).unwrap();
        assert_eq!(reversed, [0; 4]);
        let shape = VectorShape {
            bounds: VectorShapeBounds {
                x_hundredths: 400,
                y_hundredths: 500,
                width_hundredths: 1500,
                height_hundredths: 1125,
            },
            kind: VectorShapeKind::Triangle,
            stroke_width_hundredths: 225,
            rotation_hundredths: 3300,
            ..VectorShape::wpf_v2()
        };
        let size = PhysicalSize::new(32, 32).unwrap();
        let opaque =
            render_wpf_vector_shape(&shape, size, RenderLimits::default(), &NeverCancel).unwrap();
        let actual = borrowed_render(
            &[WpfVectorVisual {
                shape: &shape,
                opacity: 128,
            }],
            size,
        );
        let expected: Vec<_> = opaque
            .pixels()
            .iter()
            .map(|v| wpf_pixels::mul_byte(*v, 128))
            .collect();
        assert_eq!(actual.pixels(), expected);
    }

    #[test]
    fn each_mark_opacity_preserves_z_order_and_is_not_track_group_opacity() {
        let red = rectangle(
            0,
            Rgba {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 128,
            },
            Rgba::TRANSPARENT,
        );
        let blue = rectangle(
            0,
            Rgba {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 128,
            },
            Rgba::TRANSPARENT,
        );
        let size = PhysicalSize::new(8, 8).unwrap();
        let front_blue = borrowed_render(
            &[
                WpfVectorVisual {
                    shape: &red,
                    opacity: 128,
                },
                WpfVectorVisual {
                    shape: &blue,
                    opacity: 128,
                },
            ],
            size,
        );
        let front_red = borrowed_render(
            &[
                WpfVectorVisual {
                    shape: &blue,
                    opacity: 128,
                },
                WpfVectorVisual {
                    shape: &red,
                    opacity: 128,
                },
            ],
            size,
        );
        assert_eq!(pixel(&front_blue, 3, 3), [48, 0, 64, 112]);
        assert_eq!(pixel(&front_red, 3, 3), [64, 0, 48, 112]);
        let grouped =
            render_wpf_vector_shapes(&[red, blue], size, RenderLimits::default(), &NeverCancel)
                .unwrap();
        assert_eq!(
            pixel(&grouped, 3, 3).map(|v| wpf_pixels::mul_byte(v, 128)),
            [32, 0, 64, 96]
        );
    }

    #[test]
    fn zero_opacity_still_enforces_metadata_geometry_and_object_caps() {
        let mut shape = VectorShape::wpf_v2();
        let size = PhysicalSize::new(8, 8).unwrap();
        let view = WpfVectorVisual {
            shape: &shape,
            opacity: 0,
        };
        assert!(
            borrowed_render(&[view], size)
                .pixels()
                .iter()
                .all(|v| *v == 0)
        );
        assert!(matches!(
            render_wpf_vector_visuals(
                &[view; MAX_VECTOR_PREVIEW_SHAPES + 1],
                size,
                RenderLimits::default(),
                &NeverCancel
            ),
            Err(InkError::Limit(_))
        ));
        assert!(matches!(
            render_wpf_vector_visuals(
                &[view],
                size,
                RenderLimits {
                    max_surface_bytes: 256
                },
                &NeverCancel
            ),
            Err(InkError::Limit(_))
        ));
        shape.version = 3;
        assert!(matches!(
            render_wpf_vector_visuals(
                &[WpfVectorVisual {
                    shape: &shape,
                    opacity: 0
                }],
                size,
                RenderLimits::default(),
                &NeverCancel
            ),
            Err(InkError::Invalid(_))
        ));
    }

    #[test]
    fn opacity_temporary_and_tail_reservation_share_the_same_measured_ceiling() {
        let shape = rectangle(
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
        );
        let size = PhysicalSize::new(8, 8).unwrap();
        let views = [WpfVectorVisual {
            shape: &shape,
            opacity: 128,
        }];
        let mut measured = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        let expected = render_visual_canvas(views.into_iter(), size, &mut measured).unwrap();
        assert_eq!(measured.usage.pixels, 64 * 7);
        let used = measured.usage.total();
        let reservation = MAX_WORK - used;
        assert_eq!(
            render_wpf_vector_visuals_with_tail_work(
                &views,
                size,
                RenderLimits::default(),
                reservation,
                &NeverCancel
            )
            .unwrap(),
            expected
        );
        assert!(matches!(
            render_wpf_vector_visuals_with_tail_work(
                &views,
                size,
                RenderLimits::default(),
                reservation + 1,
                &NeverCancel
            ),
            Err(InkError::Limit(_))
        ));
        assert!(matches!(
            render_wpf_vector_visuals_with_tail_work(
                &views,
                size,
                RenderLimits::default(),
                u64::MAX,
                &NeverCancel
            ),
            Err(InkError::Limit(_))
        ));
        let mut reserved = Budget::new(RenderLimits::default(), &NeverCancel).unwrap();
        reserved.reserve_tail(64).unwrap();
        assert_eq!(
            render_visual_canvas(views.into_iter(), size, &mut reserved).unwrap(),
            expected
        );
        assert_eq!(reserved.usage, measured.usage);
        assert_eq!(reserved.remaining_work(), MAX_WORK - used - 64);
    }

    #[test]
    fn cancelled_opacity_path_never_returns_a_partial_visual() {
        let shape = rectangle(
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
        );
        let size = PhysicalSize::new(8, 8).unwrap();
        let views = [WpfVectorVisual {
            shape: &shape,
            opacity: 128,
        }];
        let count = CancelAt {
            calls: AtomicUsize::new(0),
            limit: usize::MAX,
        };
        render_wpf_vector_visuals(&views, size, RenderLimits::default(), &count).unwrap();
        let checkpoints = count.calls.load(Ordering::Relaxed);
        assert!(checkpoints > 20 && checkpoints < 10_000);
        for limit in 0..checkpoints {
            let cancel = CancelAt {
                calls: AtomicUsize::new(0),
                limit,
            };
            assert!(
                matches!(
                    render_wpf_vector_visuals_with_tail_work(
                        &views,
                        size,
                        RenderLimits::default(),
                        64,
                        &cancel
                    ),
                    Err(InkError::Cancelled)
                ),
                "checkpoint {limit}"
            );
        }
    }
}
