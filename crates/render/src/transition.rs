use gif_from_screen_domain::{Rgba, SlideDirection, TransitionKind};

use crate::{CancellationToken, RenderError, RgbaSurface};

/// A deterministic position in a transition, including both endpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionProgress {
    step: u32,
    steps: u32,
}

impl TransitionProgress {
    /// Creates a progress value in the inclusive range `0..=steps`.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::InvalidTransitionProgress`] when `steps` is zero
    /// or `step` is greater than `steps`.
    pub const fn new(step: u32, steps: u32) -> Result<Self, RenderError> {
        if steps == 0 || step > steps {
            return Err(RenderError::InvalidTransitionProgress { step, steps });
        }
        Ok(Self { step, steps })
    }

    /// Current zero-based transition step.
    pub const fn step(self) -> u32 {
        self.step
    }

    /// Final step number and interpolation denominator.
    pub const fn steps(self) -> u32 {
        self.steps
    }
}

/// Renders one transition frame between two equally sized RGBA surfaces.
///
/// Fades interpolate premultiplied-alpha components with integer arithmetic
/// and return canonical straight-alpha RGBA8. `FadeToColor` reaches the given
/// color halfway through the interval. Slides translate the outgoing surface
/// off-canvas while the incoming surface follows it without stretching.
/// Endpoint progress is exact for every transition kind.
///
/// # Errors
///
/// Returns an error when endpoint sizes differ, allocation fails, or
/// cancellation is requested.
pub fn render_transition<C: CancellationToken + ?Sized>(
    from: &RgbaSurface,
    to: &RgbaSurface,
    kind: &TransitionKind,
    progress: TransitionProgress,
    cancellation: &C,
) -> Result<RgbaSurface, RenderError> {
    ensure_compatible(from, to)?;
    ensure_not_cancelled(cancellation)?;
    if progress.step == 0 {
        return Ok(from.clone());
    }
    if progress.step == progress.steps {
        return Ok(to.clone());
    }

    match kind {
        TransitionKind::FadeToNext => fade_surfaces(
            PixelSource::Surface(from),
            PixelSource::Surface(to),
            from,
            u64::from(progress.step),
            u64::from(progress.steps),
            cancellation,
        ),
        TransitionKind::FadeToColor { color } => {
            let doubled = u64::from(progress.step) * 2;
            let steps = u64::from(progress.steps);
            if doubled <= steps {
                fade_surfaces(
                    PixelSource::Surface(from),
                    PixelSource::Color(*color),
                    from,
                    doubled,
                    steps,
                    cancellation,
                )
            } else {
                fade_surfaces(
                    PixelSource::Color(*color),
                    PixelSource::Surface(to),
                    from,
                    doubled - steps,
                    steps,
                    cancellation,
                )
            }
        }
        TransitionKind::Slide { direction } => {
            slide_surfaces(from, to, *direction, progress, cancellation)
        }
    }
}

fn ensure_compatible(from: &RgbaSurface, to: &RgbaSurface) -> Result<(), RenderError> {
    if from.size() == to.size() {
        return Ok(());
    }
    Err(RenderError::TransitionDimensionMismatch {
        from_width: from.width(),
        from_height: from.height(),
        to_width: to.width(),
        to_height: to.height(),
    })
}

#[derive(Clone, Copy)]
enum PixelSource<'a> {
    Surface(&'a RgbaSurface),
    Color(Rgba),
}

impl PixelSource<'_> {
    fn pixel(self, offset: usize) -> [u8; 4] {
        match self {
            Self::Surface(surface) => surface.pixels()[offset..offset + 4]
                .try_into()
                .expect("validated RGBA surface has complete pixels"),
            Self::Color(color) => [color.red, color.green, color.blue, color.alpha],
        }
    }
}

fn fade_surfaces<C: CancellationToken + ?Sized>(
    from: PixelSource<'_>,
    to: PixelSource<'_>,
    canvas: &RgbaSurface,
    to_weight: u64,
    denominator: u64,
    cancellation: &C,
) -> Result<RgbaSurface, RenderError> {
    debug_assert!(to_weight <= denominator);
    let from_weight = denominator - to_weight;
    let mut output = RgbaSurface::try_zeroed(canvas.size())?;
    let row_bytes =
        usize::try_from(canvas.width()).expect("validated surface width fits usize") * 4;
    for y in 0..canvas.height() {
        ensure_not_cancelled(cancellation)?;
        let row_start = usize::try_from(y).expect("surface height fits usize") * row_bytes;
        for offset in (row_start..row_start + row_bytes).step_by(4) {
            let blended = blend_straight_alpha(
                from.pixel(offset),
                to.pixel(offset),
                from_weight,
                to_weight,
                denominator,
            );
            output.pixels_mut()[offset..offset + 4].copy_from_slice(&blended);
        }
    }
    Ok(output)
}

fn blend_straight_alpha(
    from: [u8; 4],
    to: [u8; 4],
    from_weight: u64,
    to_weight: u64,
    denominator: u64,
) -> [u8; 4] {
    let alpha_sum = u64::from(from[3]) * from_weight + u64::from(to[3]) * to_weight;
    if alpha_sum == 0 {
        return [0; 4];
    }
    let mut output = [0_u8; 4];
    for channel in 0..3 {
        let premultiplied_sum = u64::from(from[channel]) * u64::from(from[3]) * from_weight
            + u64::from(to[channel]) * u64::from(to[3]) * to_weight;
        output[channel] = u8::try_from((premultiplied_sum + alpha_sum / 2) / alpha_sum)
            .expect("weighted u8 channels remain in range");
    }
    output[3] = u8::try_from((alpha_sum + denominator / 2) / denominator)
        .expect("weighted u8 alpha remains in range");
    output
}

fn slide_surfaces<C: CancellationToken + ?Sized>(
    from: &RgbaSurface,
    to: &RgbaSurface,
    direction: SlideDirection,
    progress: TransitionProgress,
    cancellation: &C,
) -> Result<RgbaSurface, RenderError> {
    let mut output = RgbaSurface::try_zeroed(from.size())?;
    let distance = match direction {
        SlideDirection::Left | SlideDirection::Right => from.width(),
        SlideDirection::Up | SlideDirection::Down => from.height(),
    };
    let shift = scaled_shift(distance, progress);

    for y in 0..from.height() {
        ensure_not_cancelled(cancellation)?;
        for x in 0..from.width() {
            let (source, source_x, source_y) = slide_source(from, to, direction, shift, x, y);
            let source_offset = source.byte_offset(source_x, source_y);
            let destination_offset = output.byte_offset(x, y);
            output.pixels_mut()[destination_offset..destination_offset + 4]
                .copy_from_slice(&source.pixels()[source_offset..source_offset + 4]);
        }
    }
    Ok(output)
}

fn scaled_shift(distance: u32, progress: TransitionProgress) -> u32 {
    let numerator = u64::from(distance) * u64::from(progress.step);
    u32::try_from((numerator + u64::from(progress.steps) / 2) / u64::from(progress.steps))
        .expect("interpolated shift does not exceed the u32 dimension")
}

fn slide_source<'a>(
    from: &'a RgbaSurface,
    to: &'a RgbaSurface,
    direction: SlideDirection,
    shift: u32,
    x: u32,
    y: u32,
) -> (&'a RgbaSurface, u32, u32) {
    match direction {
        SlideDirection::Left => {
            let outgoing_width = from.width() - shift;
            if x < outgoing_width {
                (from, x + shift, y)
            } else {
                (to, x - outgoing_width, y)
            }
        }
        SlideDirection::Right => {
            if x >= shift {
                (from, x - shift, y)
            } else {
                (to, to.width() - shift + x, y)
            }
        }
        SlideDirection::Up => {
            let outgoing_height = from.height() - shift;
            if y < outgoing_height {
                (from, x, y + shift)
            } else {
                (to, x, y - outgoing_height)
            }
        }
        SlideDirection::Down => {
            if y >= shift {
                (from, x, y - shift)
            } else {
                (to, x, to.height() - shift + y)
            }
        }
    }
}

fn ensure_not_cancelled<C: CancellationToken + ?Sized>(
    cancellation: &C,
) -> Result<(), RenderError> {
    if cancellation.is_cancelled() {
        Err(RenderError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gif_from_screen_domain::{PhysicalSize, Rgba, SlideDirection, TransitionKind};

    use super::*;
    use crate::NeverCancel;

    fn surface(width: u32, height: u32, pixels: &[[u8; 4]]) -> RgbaSurface {
        RgbaSurface::new(
            PhysicalSize::new(width, height).unwrap(),
            pixels.iter().flatten().copied().collect(),
        )
        .unwrap()
    }

    fn solid(pixel: [u8; 4]) -> RgbaSurface {
        surface(1, 1, &[pixel])
    }

    fn progress(step: u32, steps: u32) -> TransitionProgress {
        TransitionProgress::new(step, steps).unwrap()
    }

    #[test]
    fn progress_and_endpoint_dimensions_are_validated() {
        assert!(matches!(
            TransitionProgress::new(0, 0),
            Err(RenderError::InvalidTransitionProgress { .. })
        ));
        assert!(matches!(
            TransitionProgress::new(3, 2),
            Err(RenderError::InvalidTransitionProgress { .. })
        ));
        let from = solid([0, 0, 0, 255]);
        let to = surface(2, 1, &[[0, 0, 0, 255]; 2]);
        assert!(matches!(
            render_transition(
                &from,
                &to,
                &TransitionKind::FadeToNext,
                progress(1, 2),
                &NeverCancel
            ),
            Err(RenderError::TransitionDimensionMismatch { .. })
        ));
    }

    #[test]
    fn every_kind_preserves_exact_endpoints() {
        let from = solid([1, 2, 3, 4]);
        let to = solid([5, 6, 7, 8]);
        let kinds = [
            TransitionKind::FadeToNext,
            TransitionKind::FadeToColor {
                color: Rgba {
                    red: 9,
                    green: 10,
                    blue: 11,
                    alpha: 12,
                },
            },
            TransitionKind::Slide {
                direction: SlideDirection::Left,
            },
        ];
        for kind in kinds {
            assert_eq!(
                render_transition(&from, &to, &kind, progress(0, 8), &NeverCancel).unwrap(),
                from
            );
            assert_eq!(
                render_transition(&from, &to, &kind, progress(8, 8), &NeverCancel).unwrap(),
                to
            );
        }
    }

    #[test]
    fn fade_uses_premultiplied_alpha_and_canonical_transparency() {
        let opaque_red = solid([255, 0, 0, 255]);
        let transparent_blue = solid([0, 0, 255, 0]);
        let middle = render_transition(
            &opaque_red,
            &transparent_blue,
            &TransitionKind::FadeToNext,
            progress(1, 2),
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(middle.pixels(), [255, 0, 0, 128]);

        let transparent_red = solid([255, 0, 0, 0]);
        let transparent_blue = solid([0, 0, 255, 0]);
        let transparent = render_transition(
            &transparent_red,
            &transparent_blue,
            &TransitionKind::FadeToNext,
            progress(1, 2),
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(transparent.pixels(), [0, 0, 0, 0]);
    }

    #[test]
    fn fade_to_color_reaches_color_at_halfway() {
        let from = solid([0, 0, 0, 255]);
        let to = solid([255, 255, 255, 255]);
        let color = Rgba {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        let middle = render_transition(
            &from,
            &to,
            &TransitionKind::FadeToColor { color },
            progress(2, 4),
            &NeverCancel,
        )
        .unwrap();
        assert_eq!(middle.pixels(), [255, 0, 0, 255]);
    }

    #[test]
    fn slides_cover_all_four_directions_without_scaling() {
        let from = surface(
            3,
            3,
            &[
                [1, 0, 0, 255],
                [2, 0, 0, 255],
                [3, 0, 0, 255],
                [4, 0, 0, 255],
                [5, 0, 0, 255],
                [6, 0, 0, 255],
                [7, 0, 0, 255],
                [8, 0, 0, 255],
                [9, 0, 0, 255],
            ],
        );
        let to = surface(
            3,
            3,
            &[
                [11, 0, 0, 255],
                [12, 0, 0, 255],
                [13, 0, 0, 255],
                [14, 0, 0, 255],
                [15, 0, 0, 255],
                [16, 0, 0, 255],
                [17, 0, 0, 255],
                [18, 0, 0, 255],
                [19, 0, 0, 255],
            ],
        );
        let reds = |surface: RgbaSurface| {
            surface
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>()
        };
        let render = |direction| {
            render_transition(
                &from,
                &to,
                &TransitionKind::Slide { direction },
                progress(1, 3),
                &NeverCancel,
            )
            .map(reds)
            .unwrap()
        };

        assert_eq!(render(SlideDirection::Left), [2, 3, 11, 5, 6, 14, 8, 9, 17]);
        assert_eq!(
            render(SlideDirection::Right),
            [13, 1, 2, 16, 4, 5, 19, 7, 8]
        );
        assert_eq!(render(SlideDirection::Up), [4, 5, 6, 7, 8, 9, 11, 12, 13]);
        assert_eq!(render(SlideDirection::Down), [17, 18, 19, 1, 2, 3, 4, 5, 6]);
    }

    #[derive(Debug)]
    struct CancelAfterRows(AtomicUsize);

    impl CancellationToken for CancelAfterRows {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed) >= 2
        }
    }

    #[test]
    fn long_transitions_check_cancellation_by_row() {
        let pixels = vec![[0, 0, 0, 255]; 16];
        let from = surface(2, 8, &pixels);
        let to = surface(2, 8, &pixels);
        for kind in [
            TransitionKind::FadeToNext,
            TransitionKind::Slide {
                direction: SlideDirection::Left,
            },
        ] {
            assert!(matches!(
                render_transition(
                    &from,
                    &to,
                    &kind,
                    progress(1, 2),
                    &CancelAfterRows(AtomicUsize::new(0))
                ),
                Err(RenderError::Cancelled)
            ));
        }
    }
}
