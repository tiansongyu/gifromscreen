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
    if let Some(region) = request.region {
        let pixels = strips(region, request.border_width)?
            .iter()
            .try_fold(0_u64, |total, strip| {
                total.checked_add(
                    u64::from(strip.rect.size().width()) * u64::from(strip.rect.size().height()),
                )
            })
            .ok_or_else(invalid)?;
        if pixels > 2 * 1024 * 1024 {
            return Err("Recorder guide border backing exceeds its bounded pixel budget.".into());
        }
    }
    Ok(())
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
