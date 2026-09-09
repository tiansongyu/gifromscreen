use std::collections::{BTreeMap, BTreeSet};

use gif_from_screen_render::{
    InkPoint, VectorShapeGeometry, VectorShapeLayout, vector_shape_geometry, vector_shape_layout,
};

use super::{
    DraftObject,
    draft::{Draft, Handle, Point, pixels},
    error,
};
use crate::ui_notice::Notice;

#[derive(Default)]
pub(super) struct GeometryCache {
    generation: Option<u64>,
    entries: BTreeMap<u64, CachedGeometry>,
}

#[derive(Clone)]
struct CachedGeometry {
    object: DraftObject,
    geometry: VectorShapeGeometry,
    layout: VectorShapeLayout,
}

impl GeometryCache {
    pub(super) fn clear(&mut self) {
        self.generation = None;
        self.entries.clear();
    }
    pub(super) fn ensure(&mut self, draft: &Draft) -> Result<(), Notice> {
        if self.generation == Some(draft.generation) {
            return Ok(());
        }
        let mut entries = BTreeMap::new();
        for object in &draft.objects {
            let geometry = match self.entries.get(&object.id) {
                Some(cached) if cached.object == *object => cached.clone(),
                _ => CachedGeometry {
                    object: *object,
                    geometry: vector_shape_geometry(&object.shape).map_err(error)?,
                    layout: vector_shape_layout(&object.shape).map_err(error)?,
                },
            };
            entries.insert(object.id, geometry);
        }
        self.entries = entries;
        self.generation = Some(draft.generation);
        Ok(())
    }
    pub(super) fn get(&self, id: u64) -> Option<&VectorShapeGeometry> {
        self.entries.get(&id).map(|cached| &cached.geometry)
    }
    pub(super) fn handles(&self, id: u64) -> Option<[(Handle, InkPoint); 9]> {
        self.entries
            .get(&id)
            .map(|cached| layout_handles(cached.object, cached.layout))
    }
    pub(super) fn hit(
        &mut self,
        draft: &Draft,
        point: Point,
        budget: &mut usize,
    ) -> Result<Option<u64>, Notice> {
        self.ensure(draft)?;
        let point = ink(point);
        for object in draft.objects.iter().rev() {
            spend(budget)?;
            // Point policy remains fill containment, including transparent fill.
            // It is not a raster-alpha or layout-clip visibility query.
            if self.entries[&object.id].geometry.hit_test(point) {
                return Ok(Some(object.id));
            }
        }
        Ok(None)
    }
    pub(super) fn marquee(
        &mut self,
        draft: &Draft,
        a: Point,
        b: Point,
        budget: &mut usize,
    ) -> Result<BTreeSet<u64>, Notice> {
        self.ensure(draft)?;
        let low = ink([a[0].min(b[0]), a[1].min(b[1])]);
        let high = ink([a[0].max(b[0]), a[1].max(b[1])]);
        let mut selected = BTreeSet::new();
        for object in &draft.objects {
            spend(budget)?;
            // Deliberate Linux policy: shared fill-contour intersection. WPF's
            // VisualTreeHelper marquee also observes visual/stroke/clip details;
            // this narrower policy is not advertised as that full hit-test.
            if self.entries[&object.id].geometry.intersects_rect(low, high) {
                selected.insert(object.id);
            }
        }
        Ok(selected)
    }
}

fn spend(budget: &mut usize) -> Result<(), Notice> {
    *budget = budget
        .checked_sub(1)
        .ok_or(gif_from_screen_localization::Message::VectorInputLimit)?;
    Ok(())
}

pub(super) fn ink(point: Point) -> InkPoint {
    InkPoint {
        x: pixels(point[0]),
        y: pixels(point[1]),
    }
}

#[cfg(test)]
pub(super) fn handles(object: DraftObject) -> [(Handle, InkPoint); 9] {
    layout_handles(object, vector_shape_layout(&object.shape).unwrap())
}

/// V1 retains requested geometry. V2's shared layout reports the RenderSize
/// that WPF's base Adorner.MeasureOverride uses for the handle arrangement.
fn layout_handles(object: DraftObject, layout: VectorShapeLayout) -> [(Handle, InkPoint); 9] {
    let [width, height] = layout.render_size;
    let center = InkPoint {
        x: layout.origin.x + width / 2.0,
        y: layout.origin.y + height / 2.0,
    };
    let (sin, cos) = (f64::from(object.shape.rotation_hundredths) / 100.0)
        .to_radians()
        .sin_cos();
    [
        (Handle::TopLeft, [-0.5, -0.5]),
        (Handle::Top, [0.0, -0.5]),
        (Handle::TopRight, [0.5, -0.5]),
        (Handle::Right, [0.5, 0.0]),
        (Handle::BottomRight, [0.5, 0.5]),
        (Handle::Bottom, [0.0, 0.5]),
        (Handle::BottomLeft, [-0.5, 0.5]),
        (Handle::Left, [-0.5, 0.0]),
        (Handle::Rotate, [0.0, -0.5]),
    ]
    .map(|(handle, [x, y])| {
        (
            handle,
            InkPoint {
                x: center.x + x * width * cos - y * height * sin,
                y: center.y + x * width * sin + y * height * cos,
            },
        )
    })
}
