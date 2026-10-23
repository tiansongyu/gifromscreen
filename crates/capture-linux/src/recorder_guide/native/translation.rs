//! Batch position-only ready-mode moves without rebuilding shapes or repainting.

use super::*;

impl Windows<'_, '_> {
    pub(super) fn translate(&mut self, request: GuideRequest) -> Result<bool, String> {
        let Some(previous) = self.current else {
            return Ok(false);
        };
        if !request.resize_handles
            || !previous.resize_handles
            || request.protected_region.is_some()
            || previous.protected_region.is_some()
            || request.border_width != previous.border_width
            || request.handle_scale != previous.handle_scale
        {
            return Ok(false);
        }
        let Some(old) = layout(previous, self.root_size) else {
            return Ok(false);
        };
        let Some(new) = layout(request, self.root_size) else {
            return Ok(false);
        };
        if old
            .iter()
            .zip(&new)
            .any(|(old, new)| old.size() != new.size())
            || !self.visible.iter().all(|visible| *visible)
        {
            return Ok(false);
        }
        // Queue every move before waiting for the server. Retain the opaque
        // backing, bounding/input shapes, cursor and artwork of each window.
        let mut moves = Vec::new();
        for (index, rect) in new.iter().enumerate() {
            moves.push(
                self.connection
                    .configure_window(
                        self.ids[index],
                        &ConfigureWindowAux::new()
                            .x(rect.origin().x)
                            .y(rect.origin().y),
                    )
                    .map_err(native_error)?,
            );
        }
        let geometries = self
            .ids
            .iter()
            .map(|id| self.connection.get_geometry(*id).map_err(native_error))
            .collect::<Result<Vec<_>, _>>()?;
        for (cookie, expected) in geometries.into_iter().zip(&new) {
            let actual = cookie.reply().map_err(native_error)?;
            if i32::from(actual.x) != expected.origin().x
                || i32::from(actual.y) != expected.origin().y
                || u32::from(actual.width) != expected.size().width()
                || u32::from(actual.height) != expected.size().height()
            {
                return Err("The X11 server did not accept the batched guide movement.".into());
            }
        }
        for cookie in moves {
            cookie.check().map_err(native_error)?;
        }
        let barrier = self.connection.get_input_focus().map_err(native_error)?;
        self.event_floor = barrier.sequence_number();
        barrier.reply().map_err(native_error)?;
        self.check_root()?;
        self.handle_rect = Some(new[HANDLE_INDEX]);
        self.corner_rects = std::array::from_fn(|corner| Some(new[CORNER_INDEX + corner]));
        self.current = Some(request);
        Ok(true)
    }
}

fn layout(request: GuideRequest, size: PhysicalSize) -> Option<[PhysicalRect; WINDOW_COUNT]> {
    let region = request.region?;
    let root = PhysicalRect::new(0, 0, size.width(), size.height()).ok()?;
    let strips = geometry::strips(region, request.border_width).ok()?;
    let handle = geometry::drag_handle(request, size)?;
    let corners = geometry::resize_handles(request, size);
    let [Some(tl), Some(tr), Some(br), Some(bl)] = corners else {
        return None;
    };
    let rectangles = [
        strips[0].rect,
        strips[1].rect,
        strips[2].rect,
        strips[3].rect,
        handle.rect,
        tl.rect,
        tr.rect,
        br.rect,
        bl.rect,
    ];
    rectangles
        .iter()
        .all(|rect| geometry::intersection(*rect, root) == Some(*rect))
        .then_some(rectangles)
}
