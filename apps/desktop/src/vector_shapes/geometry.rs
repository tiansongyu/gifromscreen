use std::collections::{BTreeMap, BTreeSet};

use gif_from_screen_render::{InkPoint, VectorShapeGeometry, vector_shape_geometry};

use super::{
    DraftObject,
    draft::{Draft, Handle, Point, extent, pixels},
    error,
};
use crate::ui_notice::Notice;

#[derive(Default)]
pub(super) struct GeometryCache {
    generation: Option<u64>,
    entries: BTreeMap<u64, (DraftObject, VectorShapeGeometry)>,
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
                Some((original, geometry)) if original == object => geometry.clone(),
                _ => vector_shape_geometry(&object.shape).map_err(error)?,
            };
            entries.insert(object.id, (*object, geometry));
        }
        self.entries = entries;
        self.generation = Some(draft.generation);
        Ok(())
    }
    pub(super) fn get(&self, id: u64) -> Option<&VectorShapeGeometry> {
        self.entries.get(&id).map(|(_, geometry)| geometry)
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
            if self.entries[&object.id].1.hit_test(point) {
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
            if self.entries[&object.id].1.intersects_rect(low, high) {
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

/// Handles use the object's local layout box, transformed about its center.
pub(super) fn handles(object: DraftObject) -> [(Handle, InkPoint); 9] {
    let bounds = object.shape.bounds;
    let width = extent(bounds.width_hundredths);
    let height = extent(bounds.height_hundredths);
    let center = InkPoint {
        x: pixels(bounds.x_hundredths) + width / 2.0,
        y: pixels(bounds.y_hundredths) + height / 2.0,
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
