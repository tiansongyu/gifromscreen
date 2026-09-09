use gif_from_screen_capture::PhysicalRect;
#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
use gif_from_screen_capture::PhysicalSize;

use super::GuideRequest;

#[derive(Clone, Copy)]
pub(super) struct Strip {
    pub rect: PhysicalRect,
}

pub(super) fn validate(request: GuideRequest) -> Result<(), String> {
    if request.generation == 0 {
        return Err("Recorder guide generation must be nonzero.".into());
    }
    if !(1..=16).contains(&request.border_width) {
        return Err("Recorder guide border must be 1–16 physical pixels.".into());
    }
    if !(100..=400).contains(&request.handle_scale) {
        return Err("Recorder drag handle scale must be 100–400 percent.".into());
    }
    if let Some(region) = request.region {
        let pixels = strips(region, request.border_width)?
            .iter()
            .try_fold(0_u64, |total, strip| {
                total.checked_add(
                    u64::from(strip.rect.size().width()) * u64::from(strip.rect.size().height()),
                )
            })
            .ok_or_else(invalid)?;
        let (handle_width, handle_height) = handle_size(request.handle_scale);
        if pixels + u64::from(handle_width) * u64::from(handle_height) > 2 * 1024 * 1024 {
            return Err("Recorder guide border backing exceeds its bounded pixel budget.".into());
        }
    }
    Ok(())
}

fn handle_size(scale: u16) -> (u32, u32) {
    (
        (144 * u32::from(scale)).div_ceil(100),
        (36 * u32::from(scale)).div_ceil(100),
    )
}

/// Prefer the top center, then other edges/anchors without entering capture or
/// controller pixels. If the root has no safe space, the ordinary border stays.
#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
pub(super) fn drag_handle(request: GuideRequest, root_size: PhysicalSize) -> Option<Strip> {
    let region = request.region?;
    let root = PhysicalRect::new(0, 0, root_size.width(), root_size.height()).ok()?;
    intersection(region, root)?;
    let (long, short) = handle_size(request.handle_scale);
    let left = i64::from(region.origin().x);
    let top = i64::from(region.origin().y);
    let right = left + i64::from(region.size().width());
    let bottom = top + i64::from(region.size().height());
    let border = i64::from(request.border_width);
    for side in 0..4 {
        let horizontal = side == 0 || side == 3;
        let width = if horizontal { long } else { short }.min(root_size.width());
        // A short recording near the top edge still needs a side grip that
        // fits above its controller; do not force the full portrait length.
        let height = if horizontal {
            short
        } else {
            long.min(region.size().height().max(short))
        }
        .min(root_size.height());
        for anchor in 0..3 {
            let along = |start, end, extent| match anchor {
                0 => (start + end - extent) / 2,
                1 => start,
                _ => end - extent,
            };
            let x = if horizontal {
                along(left, right, i64::from(width)).clamp(0, i64::from(root_size.width() - width))
            } else if side == 1 {
                left - border - i64::from(width)
            } else {
                right + border
            };
            let y = if !horizontal {
                along(top, bottom, i64::from(height))
                    .clamp(0, i64::from(root_size.height() - height))
            } else if side == 0 {
                top - border - i64::from(height)
            } else {
                bottom + border
            };
            let (Ok(x), Ok(y)) = (i16::try_from(x), i16::try_from(y)) else {
                continue;
            };
            let rect = PhysicalRect::new(i32::from(x), i32::from(y), width, height).ok()?;
            if intersection(rect, root) == Some(rect)
                && intersection(rect, region).is_none()
                && request
                    .handle_avoid
                    .is_none_or(|avoid| intersection(rect, avoid).is_none())
                && request
                    .protected_region
                    .is_none_or(|old| intersection(rect, old).is_none())
            {
                return Some(Strip { rect });
            }
        }
    }
    None
}

pub(super) fn strips(region: PhysicalRect, border: u16) -> Result<[Strip; 4], String> {
    let x = i64::from(region.origin().x);
    let y = i64::from(region.origin().y);
    let width = i64::from(region.size().width());
    let height = i64::from(region.size().height());
    let border = i64::from(border);
    let strip = |x: i64, y: i64, width: i64, height: i64| {
        // Shapes use signed 16-bit origins. Bounding each strip dimension to
        // i16::MAX also makes every clipped/protected local origin representable.
        let x = i16::try_from(x).map_err(|_| invalid())?;
        let y = i16::try_from(y).map_err(|_| invalid())?;
        if !(1..=i64::from(i16::MAX)).contains(&width)
            || !(1..=i64::from(i16::MAX)).contains(&height)
        {
            return Err(invalid());
        }
        let rect = PhysicalRect::new(
            i32::from(x),
            i32::from(y),
            u32::try_from(width).map_err(|_| invalid())?,
            u32::try_from(height).map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?;
        Ok(Strip { rect })
    };
    Ok([
        strip(x - border, y - border, width + border * 2, border)?,
        strip(x - border, y + height, width + border * 2, border)?,
        strip(x - border, y, border, height)?,
        strip(x + width, y, border, height)?,
    ])
}

fn invalid() -> String {
    "Recorder guide strips exceed supported X11 coordinate/dimension limits.".into()
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
pub(super) fn intersection(a: PhysicalRect, b: PhysicalRect) -> Option<PhysicalRect> {
    let left = i64::from(a.origin().x).max(i64::from(b.origin().x));
    let top = i64::from(a.origin().y).max(i64::from(b.origin().y));
    let right = (i64::from(a.origin().x) + i64::from(a.size().width()))
        .min(i64::from(b.origin().x) + i64::from(b.size().width()));
    let bottom = (i64::from(a.origin().y) + i64::from(a.size().height()))
        .min(i64::from(b.origin().y) + i64::from(b.size().height()));
    if right <= left || bottom <= top {
        return None;
    }
    PhysicalRect::new(
        i32::try_from(left).ok()?,
        i32::try_from(top).ok()?,
        u32::try_from(right - left).ok()?,
        u32::try_from(bottom - top).ok()?,
    )
    .ok()
}

#[cfg(any(all(target_os = "linux", feature = "native-x11"), test))]
pub(super) fn covers_root(region: PhysicalRect, size: PhysicalSize) -> bool {
    region.origin().x <= 0
        && region.origin().y <= 0
        && i64::from(region.origin().x) + i64::from(region.size().width())
            >= i64::from(size.width())
        && i64::from(region.origin().y) + i64::from(region.size().height())
            >= i64::from(size.height())
}
