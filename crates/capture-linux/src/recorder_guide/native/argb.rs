//! Use an alpha visual without declaring an opaque region. Mutter 42.9's
//! `has_shadow` rejects this nonopaque texture; `_GTK_FRAME_EXTENTS` is ignored
//! on override-redirect windows. This is not a universal compositor guarantee.

use x11rb::protocol::{
    render::{PictType, Pictforminfo, QueryPictFormatsReply},
    xproto::{Screen, VisualClass, Visualid, Visualtype},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct ArgbVisual {
    pub(super) visual: Visualid,
    pub(super) pixel: u32,
    pub(super) ink_pixel: u32,
}

pub(super) fn select(
    screen: &Screen,
    screen_index: usize,
    formats: &QueryPictFormatsReply,
) -> Result<ArgbVisual, String> {
    let render_screen = formats
        .screens
        .get(screen_index)
        .ok_or("The selected X11 screen has no XRender visual information.")?;
    for visual in screen
        .allowed_depths
        .iter()
        .filter(|depth| depth.depth == 32)
        .flat_map(|depth| &depth.visuals)
        .filter(|visual| visual.class == VisualClass::TRUE_COLOR)
    {
        let format = render_screen
            .depths
            .iter()
            .filter(|depth| depth.depth == 32)
            .flat_map(|depth| &depth.visuals)
            .find(|candidate| candidate.visual == visual.visual_id)
            .and_then(|candidate| formats.formats.iter().find(|f| f.id == candidate.format));
        if let Some(pixel) = format.and_then(|format| color(visual, format)) {
            return Ok(ArgbVisual {
                visual: visual.visual_id,
                pixel,
                ink_pixel: pixel & !(visual.red_mask | visual.green_mask | visual.blue_mask),
            });
        }
    }
    Err("Recorder guides require a compatible XRender 32-bit TrueColor alpha visual; an RGB fallback could cast a shadow into the recording.".into())
}

fn color(visual: &Visualtype, format: &Pictforminfo) -> Option<u32> {
    if format.depth != 32 || format.type_ != PictType::DIRECT {
        return None;
    }
    let direct = format.direct;
    let red = shifted_mask(direct.red_mask, direct.red_shift)?;
    let green = shifted_mask(direct.green_mask, direct.green_shift)?;
    let blue = shifted_mask(direct.blue_mask, direct.blue_shift)?;
    let alpha = shifted_mask(direct.alpha_mask, direct.alpha_shift)?;
    if [red, green, blue] != [visual.red_mask, visual.green_mask, visual.blue_mask] {
        return None;
    }
    let mut occupied = 0;
    for mask in [red, green, blue, alpha] {
        if occupied & mask != 0 {
            return None;
        }
        occupied |= mask;
    }
    // Fully opaque colored strip pixels, with channel positions established by
    // XRender, not guessed from the root visual or assumed to be 0xAARRGGBB.
    let mut pixel = alpha;
    for (value, mask) in [(242_u32, red), (153, green), (74, blue)] {
        let shift = mask.trailing_zeros();
        let range = mask >> shift;
        let channel = (u64::from(value) * u64::from(range) + 127) / 255;
        pixel |= u32::try_from(channel).ok()? << shift;
    }
    Some(pixel)
}

fn shifted_mask(mask: u16, shift: u16) -> Option<u32> {
    if mask == 0 || shift >= 32 {
        return None;
    }
    let range = u32::from(mask);
    if range & (range + 1) != 0 {
        return None;
    }
    u32::try_from(u64::from(mask) << shift).ok()
}

#[cfg(test)]
mod tests;
