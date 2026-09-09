//! Large, font-independent grab affordance on an owned ARGB window.

use x11rb::protocol::xproto::CreateGCAux;

use super::*;

impl Windows<'_, '_> {
    pub(super) fn create_handle_cursor(&mut self) -> Result<(), String> {
        let font = self.connection.generate_id().map_err(native_error)?;
        self.connection
            .open_font(font, b"cursor")
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        let cursor = self.connection.generate_id().map_err(native_error)?;
        // X11 cursorfont.h: XC_fleur (52), followed by its mask glyph (53).
        let result = self
            .connection
            .create_glyph_cursor(
                cursor,
                font,
                font,
                52,
                53,
                0,
                0,
                0,
                u16::MAX,
                u16::MAX,
                u16::MAX,
            )
            .map_err(native_error)
            .and_then(|cookie| cookie.check().map_err(native_error));
        let _ = self.connection.close_font(font);
        result?;
        self.handle_cursor = Some(cursor);
        Ok(())
    }

    pub(super) fn create_handle_gc(&mut self, ink: u32) -> Result<(), String> {
        let gc = self.connection.generate_id().map_err(native_error)?;
        self.connection
            .create_gc(
                gc,
                self.ids[HANDLE_INDEX],
                &CreateGCAux::new().foreground(ink).graphics_exposures(0),
            )
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        self.handle_gc = Some(gc);
        Ok(())
    }

    pub(super) fn paint_handle(&self) -> Result<(), String> {
        let Some(rect) = self.handle_rect else {
            return Ok(());
        };
        let window = self.ids[HANDLE_INDEX];
        let gc = self
            .handle_gc
            .ok_or("Recorder drag handle drawing context is unavailable.")?;
        self.connection
            .clear_area(false, window, 0, 0, 0, 0)
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        let width = i32::try_from(rect.size().width()).map_err(native_error)?;
        let height = i32::try_from(rect.size().height()).map_err(native_error)?;
        let unit = (width.min(height) / 12).max(1);
        let mut marks = Vec::with_capacity(110);
        let mut mark = |x, y, w, h| -> Result<(), String> {
            marks.push(Rectangle {
                x: i16::try_from(x).map_err(native_error)?,
                y: i16::try_from(y).map_err(native_error)?,
                width: u16::try_from(w).map_err(native_error)?,
                height: u16::try_from(h).map_err(native_error)?,
            });
            Ok(())
        };
        // A central move cross, with paired grip dots on either side. Portrait
        // placement rotates the dots, but the whole surface always means Move.
        let cx = width / 2;
        let cy = height / 2;
        mark(cx - 3 * unit, cy - unit / 2, 6 * unit, unit)?;
        mark(cx - unit / 2, cy - 3 * unit, unit, 6 * unit)?;
        // Four filled arrowheads distinguish Move from an Add/zoom button.
        for step in 0..2 * unit {
            mark(cx - 4 * unit + step, cy - step / 2, 1, step + 1)?;
            mark(cx + 4 * unit - step, cy - step / 2, 1, step + 1)?;
            mark(cx - step / 2, cy - 4 * unit + step, step + 1, 1)?;
            mark(cx - step / 2, cy + 4 * unit - step, step + 1, 1)?;
        }
        for side in [-1, 1] {
            for distance in [6, 9] {
                for offset in [-2, 2] {
                    let (x, y) = if width >= height {
                        (cx + side * distance * unit, cy + offset * unit)
                    } else {
                        (cx + offset * unit, cy + side * distance * unit)
                    };
                    mark(x - unit / 2, y - unit / 2, unit, unit)?;
                }
            }
        }
        self.connection
            .poly_fill_rectangle(window, gc, &marks)
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        Ok(())
    }
}
