//! Present pointer motion on the X11 worker, even if the UI/GPU frame is stalled.

use super::{
    Context, GuideEdge, GuidePreview, GuideRequest, PhysicalPosition, PhysicalRect, Windows,
};

#[derive(Clone, Copy)]
pub(super) struct DirectDrag {
    pub(super) initial: GuideRequest,
    pub(super) start: PhysicalPosition,
    pub(super) position: PhysicalPosition,
    pub(super) edge: GuideEdge,
}

impl Windows<'_, '_> {
    pub(super) fn present_direct_drag(&mut self, context: &Context) -> Result<(), String> {
        let Some((id, drag)) = self
            .gesture
            .as_ref()
            .and_then(|gesture| gesture.direct.map(|drag| (gesture.id, drag)))
        else {
            return Ok(());
        };
        if !context.active_gesture(id) {
            return Ok(());
        }
        let Some(mut request) = self.current else {
            return Ok(());
        };
        let Some(bounds) = request.preview_bounds else {
            return Ok(());
        };
        let Some(initial) = drag.initial.region else {
            return Ok(());
        };
        let Some(region) = dragged_region(
            initial,
            drag.start,
            drag.position,
            drag.edge,
            bounds,
            request.resize_handles,
        ) else {
            return Ok(());
        };
        if request.region != Some(region) {
            request.region = Some(region);
            self.connection.stream().begin_operation();
            self.apply(request, context)?;
            self.connection.stream().registration_complete();
        }
        context.preview(GuidePreview {
            generation: drag.initial.generation,
            gesture_id: id,
            region,
            position: drag.position,
            active: true,
        });
        Ok(())
    }
}

fn dragged_region(
    initial: PhysicalRect,
    start: PhysicalPosition,
    point: PhysicalPosition,
    edge: GuideEdge,
    bounds: PhysicalRect,
    resize: bool,
) -> Option<PhysicalRect> {
    let dx = i64::from(point.x) - i64::from(start.x);
    let dy = i64::from(point.y) - i64::from(start.y);
    let min_x = i64::from(bounds.origin().x);
    let min_y = i64::from(bounds.origin().y);
    let max_x = min_x + i64::from(bounds.size().width());
    let max_y = min_y + i64::from(bounds.size().height());
    let mut left = i64::from(initial.origin().x);
    let mut top = i64::from(initial.origin().y);
    let mut right = left + i64::from(initial.size().width());
    let mut bottom = top + i64::from(initial.size().height());
    if left < min_x || top < min_y || right > max_x || bottom > max_y {
        return None;
    }
    if resize
        && matches!(
            edge,
            GuideEdge::TopLeft
                | GuideEdge::TopRight
                | GuideEdge::BottomLeft
                | GuideEdge::BottomRight
        )
    {
        if matches!(edge, GuideEdge::TopLeft | GuideEdge::BottomLeft) {
            left = (left + dx).clamp(min_x, right - 1);
        } else {
            right = (right + dx).clamp(left + 1, max_x);
        }
        if matches!(edge, GuideEdge::TopLeft | GuideEdge::TopRight) {
            top = (top + dy).clamp(min_y, bottom - 1);
        } else {
            bottom = (bottom + dy).clamp(top + 1, max_y);
        }
    } else {
        left = (left + dx).clamp(min_x, max_x - i64::from(initial.size().width()));
        top = (top + dy).clamp(min_y, max_y - i64::from(initial.size().height()));
        right = left + i64::from(initial.size().width());
        bottom = top + i64::from(initial.size().height());
    }
    PhysicalRect::new(
        i32::try_from(left).ok()?,
        i32::try_from(top).ok()?,
        u32::try_from(right - left).ok()?,
        u32::try_from(bottom - top).ok()?,
    )
    .ok()
}
