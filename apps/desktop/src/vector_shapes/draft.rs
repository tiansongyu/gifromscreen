use std::collections::BTreeSet;

use gif_from_screen_domain::{
    FrameId, PhysicalSize, Rgba, VectorShape, VectorShapeBounds, VectorShapeKind,
    WPF_VECTOR_SHAPE_VERSION,
};
use gif_from_screen_localization::Message;
use gif_from_screen_render::vector_shape_layout;

use super::{MAX_VECTOR_DRAFT_OBJECTS, VectorShapeRequest, error};
use crate::{
    editor_workspace::{EditorWorkspace, OverlaySelectionAnchor},
    ui_notice::Notice,
};

pub(super) type Point = [i64; 2];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ShapeTool {
    #[default]
    Insert,
    Select,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShapeStyle {
    pub(crate) stroke_width_hundredths: u32,
    pub(crate) stroke: Rgba,
    pub(crate) fill: Option<Rgba>,
    pub(crate) corner_radius_hundredths: u32,
}

impl Default for ShapeStyle {
    fn default() -> Self {
        Self::from_shape(VectorShape::default())
    }
}

impl ShapeStyle {
    pub(super) fn from_shape(shape: VectorShape) -> Self {
        Self {
            stroke_width_hundredths: shape.stroke_width_hundredths,
            stroke: shape.stroke,
            fill: shape.fill,
            corner_radius_hundredths: shape.corner_radius_hundredths,
        }
    }
    fn apply(self, shape: &mut VectorShape) {
        shape.stroke_width_hundredths = self.stroke_width_hundredths;
        shape.stroke = self.stroke;
        shape.fill = self.fill;
        shape.corner_radius_hundredths = self.corner_radius_hundredths;
    }
    fn validate(self) -> Result<(), Notice> {
        let mut shape = VectorShape::default();
        self.apply(&mut shape);
        shape.validate().map_err(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DraftObject {
    pub(crate) id: u64,
    pub(crate) shape: VectorShape,
}

struct Binding {
    anchor: OverlaySelectionAnchor,
    reference: FrameId,
    canvas: PhysicalSize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Rotate,
}

#[derive(Clone, Debug)]
pub(super) struct Gesture {
    before: Vec<DraftObject>,
    selected_before: BTreeSet<u64>,
    style_before: ShapeStyle,
    pub(super) kind: GestureKind,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum GestureKind {
    Insert {
        id: u64,
        start: Point,
        end: Point,
    },
    SelectOnly,
    Marquee {
        start: Point,
        end: Point,
        additive: bool,
    },
    Move {
        start: Point,
    },
    Resize {
        start: Point,
        handle: Handle,
    },
    Rotate {
        center: [f64; 2],
        angle: f64,
    },
}

pub(super) struct Draft {
    binding: Option<Binding>,
    pub(super) stale: bool,
    pub(super) objects: Vec<DraftObject>,
    pub(super) selected: BTreeSet<u64>,
    pub(super) gesture: Option<Gesture>,
    pub(super) style: ShapeStyle,
    pub(super) tool: ShapeTool,
    pub(super) kind: VectorShapeKind,
    pub(super) generation: u64,
    next_id: u64,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            binding: None,
            stale: false,
            objects: Vec::new(),
            selected: BTreeSet::new(),
            gesture: None,
            style: ShapeStyle::default(),
            tool: ShapeTool::Insert,
            kind: VectorShapeKind::Rectangle,
            generation: 0,
            next_id: 1,
        }
    }
}

impl Draft {
    pub(super) fn begin(
        &mut self,
        workspace: &EditorWorkspace,
        reference: FrameId,
        rendered: [u32; 2],
    ) -> Result<(), Notice> {
        if workspace.selection().is_empty() || workspace.selection().current() != Some(reference) {
            return Err(Message::VectorNeedFrames.into());
        }
        let canvas = PhysicalSize::new(rendered[0], rendered[1]).map_err(error)?;
        VectorShapeBounds {
            x_hundredths: 0,
            y_hundredths: 0,
            width_hundredths: u64::from(rendered[0]) * 100,
            height_hundredths: u64::from(rendered[1]) * 100,
        }
        .validate()
        .map_err(error)?;
        let anchor = workspace.overlay_selection_anchor().map_err(error)?;
        self.bump()?;
        self.binding = Some(Binding {
            anchor,
            reference,
            canvas,
        });
        self.stale = false;
        self.objects.clear();
        self.selected.clear();
        self.gesture = None;
        Ok(())
    }

    pub(super) fn is_active(&self) -> bool {
        self.binding.is_some()
    }
    pub(super) fn reference_frame(&self) -> Option<FrameId> {
        self.binding.as_ref().map(|b| b.reference)
    }
    pub(super) fn canvas(&self) -> Option<[u32; 2]> {
        self.binding
            .as_ref()
            .map(|b| [b.canvas.width.get(), b.canvas.height.get()])
    }
    pub(super) fn primary(&self) -> Option<DraftObject> {
        self.objects
            .iter()
            .rev()
            .find(|object| self.selected.contains(&object.id))
            .copied()
    }

    pub(super) fn reconcile(&mut self, workspace: &EditorWorkspace) {
        if self.binding.as_ref().is_some_and(|b| {
            !b.anchor.matches(workspace) || workspace.selection().current() != Some(b.reference)
        }) {
            self.cancel_gesture();
            self.stale = true;
        }
    }

    pub(super) fn ready(&self) -> Result<(), Notice> {
        if !self.is_active() {
            return Err(Message::VectorNoDraft.into());
        }
        if self.stale {
            return Err(Message::VectorStale.into());
        }
        Ok(())
    }

    pub(super) fn request(
        &self,
        workspace: &EditorWorkspace,
    ) -> Result<VectorShapeRequest, Notice> {
        self.ready()?;
        let binding = self.binding.as_ref().ok_or(Message::VectorNoDraft)?;
        if !binding.anchor.matches(workspace)
            || workspace.selection().current() != Some(binding.reference)
        {
            return Err(Message::VectorStale.into());
        }
        if self.gesture.is_some() {
            return Err(Message::VectorFinishGesture.into());
        }
        if self.objects.is_empty() {
            return Err(Message::VectorNoObjects.into());
        }
        if self.objects.len() > MAX_VECTOR_DRAFT_OBJECTS {
            return Err(object_limit());
        }
        for object in &self.objects {
            object.shape.validate().map_err(error)?;
            gif_from_screen_render::vector_shape_geometry(&object.shape).map_err(error)?;
        }
        Ok(VectorShapeRequest {
            anchor: binding.anchor.clone(),
            reference_frame: binding.reference,
            canvas_size: binding.canvas,
            shapes: self.objects.iter().map(|object| object.shape).collect(),
        })
    }

    pub(super) fn close(&mut self) {
        self.binding = None;
        self.stale = false;
        self.objects.clear();
        self.selected.clear();
        self.gesture = None;
        let _ = self.bump();
    }

    pub(super) fn set_style(&mut self, style: ShapeStyle) -> Result<(), Notice> {
        self.ready()?;
        style.validate()?;
        if self.gesture.is_some() {
            return Err(Message::VectorFinishGesture.into());
        }
        self.bump()?;
        self.style = style;
        for object in self
            .objects
            .iter_mut()
            .filter(|o| self.selected.contains(&o.id))
        {
            style.apply(&mut object.shape);
        }
        Ok(())
    }

    pub(super) fn set_rotation(&mut self, rotation: u16) -> Result<(), Notice> {
        self.ready()?;
        VectorShape {
            rotation_hundredths: rotation,
            ..VectorShape::default()
        }
        .validate()
        .map_err(error)?;
        if self.gesture.is_some() {
            return Err(Message::VectorFinishGesture.into());
        }
        self.bump()?;
        for object in self
            .objects
            .iter_mut()
            .filter(|o| self.selected.contains(&o.id))
        {
            object.shape.rotation_hundredths = rotation;
        }
        Ok(())
    }

    pub(super) fn rotate_by(&mut self, difference: i32) -> Result<(), Notice> {
        self.ready()?;
        if self.gesture.is_some() {
            return Err(Message::VectorFinishGesture.into());
        }
        self.bump()?;
        for object in self
            .objects
            .iter_mut()
            .filter(|o| self.selected.contains(&o.id))
        {
            object.shape.rotation_hundredths =
                rotation(object.shape.rotation_hundredths, difference);
        }
        Ok(())
    }

    pub(super) fn clear(&mut self) -> Result<(), Notice> {
        self.ready()?;
        self.bump()?;
        self.objects.clear();
        self.selected.clear();
        self.gesture = None;
        Ok(())
    }
    pub(super) fn select_all(&mut self) -> Result<(), Notice> {
        self.ready()?;
        self.cancel_gesture();
        self.selected = self.objects.iter().map(|o| o.id).collect();
        Ok(())
    }
    pub(super) fn delete_selected(&mut self) -> Result<(), Notice> {
        self.ready()?;
        let selected = self.selected.clone();
        self.cancel_gesture();
        self.bump()?;
        self.objects.retain(|o| !selected.contains(&o.id));
        self.selected.retain(|id| !selected.contains(id));
        Ok(())
    }

    fn snapshot(&self, kind: GestureKind) -> Gesture {
        Gesture {
            before: self.objects.clone(),
            selected_before: self.selected.clone(),
            style_before: self.style,
            kind,
        }
    }

    pub(super) fn insert(&mut self, point: Point) -> Result<(), Notice> {
        self.ready()?;
        if self.gesture.is_some() {
            return Err(Message::VectorFinishGesture.into());
        }
        if self.objects.len() >= MAX_VECTOR_DRAFT_OBJECTS {
            return Err(object_limit());
        }
        self.style.validate()?;
        let id = self.next_id;
        let next = id.checked_add(1).ok_or(Message::VectorIdsExhausted)?;
        let mut shape = VectorShape {
            kind: self.kind,
            bounds: VectorShapeBounds {
                x_hundredths: point[0],
                y_hundredths: point[1],
                width_hundredths: 1,
                height_hundredths: 1,
            },
            ..VectorShape::default()
        };
        self.style.apply(&mut shape);
        shape.validate().map_err(error)?;
        self.bump()?;
        self.gesture = Some(self.snapshot(GestureKind::Insert {
            id,
            start: point,
            end: point,
        }));
        self.next_id = next;
        self.objects.push(DraftObject { id, shape });
        self.selected.clear();
        self.selected.insert(id);
        Ok(())
    }

    pub(super) fn select(
        &mut self,
        point: Point,
        hit: Option<u64>,
        control: bool,
    ) -> Result<(), Notice> {
        self.ready()?;
        if self.gesture.is_some() {
            return Err(Message::VectorFinishGesture.into());
        }
        if hit.is_some_and(|id| !self.objects.iter().any(|object| object.id == id)) {
            return Err(Message::VectorNoObjects.into());
        }
        let kind = if hit.is_none() {
            GestureKind::Marquee {
                start: point,
                end: point,
                additive: control,
            }
        } else if control {
            GestureKind::SelectOnly
        } else {
            GestureKind::Move { start: point }
        };
        let before = self.snapshot(kind);
        if let Some(id) = hit {
            if control && self.selected.contains(&id) {
                self.selected.remove(&id);
            } else {
                if !control && !self.selected.contains(&id) {
                    self.selected.clear();
                }
                self.selected.insert(id);
                self.bump()?;
                self.objects.sort_by_key(|o| self.selected.contains(&o.id));
            }
            if let Some(primary) = self.primary() {
                self.style = ShapeStyle::from_shape(primary.shape);
            }
        } else if !control {
            self.selected.clear();
        }
        self.gesture = Some(before);
        Ok(())
    }

    pub(super) fn begin_handle(&mut self, point: Point, handle: Handle) -> Result<(), Notice> {
        self.ready()?;
        if self.gesture.is_some() {
            return Err(Message::VectorFinishGesture.into());
        }
        let primary = self.primary().ok_or(Message::VectorNoObjects)?;
        let layout = vector_shape_layout(&primary.shape).map_err(error)?;
        let center = [
            layout.origin.x + layout.render_size[0] / 2.0,
            layout.origin.y + layout.render_size[1] / 2.0,
        ];
        let kind = if handle == Handle::Rotate {
            GestureKind::Rotate {
                center,
                angle: (pixels(point[1]) - center[1]).atan2(pixels(point[0]) - center[0]),
            }
        } else {
            GestureKind::Resize {
                start: point,
                handle,
            }
        };
        self.gesture = Some(self.snapshot(kind));
        Ok(())
    }

    pub(super) fn cancel_gesture(&mut self) -> bool {
        let Some(gesture) = self.gesture.take() else {
            return false;
        };
        self.objects = gesture.before;
        self.selected = gesture.selected_before;
        self.style = gesture.style_before;
        if self.bump().is_err() {
            self.stale = true;
        }
        true
    }

    pub(super) fn update(&mut self, point: Point) -> Result<(), Notice> {
        self.ready()?;
        let Some(gesture) = self.gesture.as_ref() else {
            return Ok(());
        };
        let mut candidate = self.objects.clone();
        match gesture.kind {
            GestureKind::Insert { id, start, .. } => {
                let object = candidate
                    .iter_mut()
                    .find(|o| o.id == id)
                    .ok_or(Message::VectorNoObjects)?;
                object.shape.bounds = rectangle(start, point);
                self.gesture.as_mut().expect("gesture checked").kind = GestureKind::Insert {
                    id,
                    start,
                    end: point,
                };
            }
            GestureKind::Marquee {
                start, additive, ..
            } => {
                self.gesture.as_mut().expect("gesture checked").kind = GestureKind::Marquee {
                    start,
                    end: point,
                    additive,
                };
                return Ok(());
            }
            GestureKind::SelectOnly => return Ok(()),
            GestureKind::Move { start } => {
                let canvas = self.canvas().ok_or(Message::VectorNoDraft)?;
                candidate = moved(
                    &gesture.before,
                    &self.selected,
                    [point[0] - start[0], point[1] - start[1]],
                    canvas,
                )?;
                candidate.sort_by_key(|o| self.selected.contains(&o.id));
            }
            GestureKind::Resize { start, handle } => {
                candidate = resized(
                    &gesture.before,
                    &self.selected,
                    handle,
                    [point[0] - start[0], point[1] - start[1]],
                    self.canvas().ok_or(Message::VectorNoDraft)?,
                )?;
            }
            GestureKind::Rotate { center, angle } => {
                let current = (pixels(point[1]) - center[1]).atan2(pixels(point[0]) - center[0]);
                let degrees = (current - angle).to_degrees().round();
                #[allow(clippy::cast_possible_truncation)]
                let change = (degrees * 100.0) as i32;
                candidate.clone_from(&gesture.before);
                for object in candidate
                    .iter_mut()
                    .filter(|o| self.selected.contains(&o.id))
                {
                    object.shape.rotation_hundredths =
                        rotation(object.shape.rotation_hundredths, change);
                }
            }
        }
        for object in &candidate {
            object.shape.validate().map_err(error)?;
            if object.shape.version == WPF_VECTOR_SHAPE_VERSION {
                vector_shape_layout(&object.shape).map_err(error)?;
            }
        }
        if candidate != self.objects {
            self.bump()?;
            self.objects = candidate;
        }
        Ok(())
    }

    pub(super) fn finish(&mut self) {
        if let Some(Gesture {
            kind: GestureKind::Insert { id, start, end },
            ..
        }) = &self.gesture
            && self.objects.iter().find(|o| o.id == *id).is_some_and(|o| {
                start[0].abs_diff(end[0]) <= 120
                    || start[1].abs_diff(end[1]) <= 120
                    || o.shape.bounds.width_hundredths + o.shape.bounds.height_hundredths < 1_000
            })
        {
            self.cancel_gesture();
            return;
        }
        self.gesture = None;
    }

    pub(super) fn finish_marquee(&mut self, hits: &BTreeSet<u64>) -> Result<(), Notice> {
        let Some(Gesture {
            kind: GestureKind::Marquee { additive, .. },
            selected_before,
            ..
        }) = &self.gesture
        else {
            return Ok(());
        };
        let mut selected = if *additive {
            selected_before.clone()
        } else {
            BTreeSet::new()
        };
        selected.extend(hits);
        self.bump()?;
        self.selected = selected;
        self.objects.sort_by_key(|o| self.selected.contains(&o.id));
        self.gesture = None;
        if let Some(primary) = self.primary() {
            self.style = ShapeStyle::from_shape(primary.shape);
        }
        Ok(())
    }

    fn bump(&mut self) -> Result<(), Notice> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(Message::VectorIdsExhausted)?;
        Ok(())
    }
}

fn object_limit() -> Notice {
    Notice::new(
        Message::VectorObjectLimit,
        &[("limit", &MAX_VECTOR_DRAFT_OBJECTS.to_string())],
    )
}
pub(super) fn pixels(value: i64) -> f64 {
    f64::from(i32::try_from(value).expect("bounded draft coordinates fit i32")) / 100.0
}
pub(super) fn extent(value: u64) -> f64 {
    f64::from(u32::try_from(value).expect("bounded draft extent fits u32")) / 100.0
}
fn rotation(original: u16, change: i32) -> u16 {
    u16::try_from((i64::from(original) + i64::from(change)).rem_euclid(36_000))
        .expect("normalized angle fits u16")
}
fn rectangle(a: Point, b: Point) -> VectorShapeBounds {
    VectorShapeBounds {
        // The reference insertion rectangle is inset by 0.6 on each side.
        // This is the explicit normalized 96-DPI physical image space, not UI zoom.
        x_hundredths: a[0].min(b[0]) + 60,
        y_hundredths: a[1].min(b[1]) + 60,
        width_hundredths: a[0].abs_diff(b[0]).saturating_sub(120).max(1),
        height_hundredths: a[1].abs_diff(b[1]).saturating_sub(120).max(1),
    }
}

fn movement(
    objects: &[DraftObject],
    selected: &BTreeSet<u64>,
    dx: i64,
    dy: i64,
    canvas: [u32; 2],
) -> (i64, i64) {
    let mut low = [i64::MIN; 2];
    let mut high = [i64::MAX; 2];
    for object in objects.iter().filter(|o| selected.contains(&o.id)) {
        let b = object.shape.bounds;
        low[0] = low[0].max(-b.x_hundredths);
        low[1] = low[1].max(-b.y_hundredths);
        high[0] = high[0].min(i64::from(canvas[0]) * 100 - b.end_x_hundredths().unwrap());
        high[1] = high[1].min(i64::from(canvas[1]) * 100 - b.end_y_hundredths().unwrap());
    }
    (dx.clamp(low[0], high[0]), dy.clamp(low[1], high[1]))
}

fn legacy_resized(
    objects: &[DraftObject],
    selected: &BTreeSet<u64>,
    handle: Handle,
    delta: Point,
    canvas: [u32; 2],
) -> Vec<DraftObject> {
    let Some(primary) = objects.iter().rev().find(|o| selected.contains(&o.id)) else {
        return objects.to_vec();
    };
    if delta == [0, 0] {
        return objects.to_vec();
    }
    let (sin, cos) = angle(primary.shape);
    let local = [
        rounded_hundredths(pixels(delta[0]) * cos + pixels(delta[1]) * sin),
        rounded_hundredths(-pixels(delta[0]) * sin + pixels(delta[1]) * cos),
    ];
    let left = matches!(handle, Handle::TopLeft | Handle::Left | Handle::BottomLeft);
    let top = matches!(handle, Handle::TopLeft | Handle::Top | Handle::TopRight);
    let horizontal = !matches!(handle, Handle::Top | Handle::Bottom);
    let vertical = !matches!(handle, Handle::Left | Handle::Right);
    let mut change = [
        if horizontal {
            if left { -local[0] } else { local[0] }
        } else {
            0
        },
        if vertical {
            if top { -local[1] } else { local[1] }
        } else {
            0
        },
    ];
    // Intersect every object's legal delta interval before clamping once.
    // Repeated per-object clamping is order-dependent and can shrink an earlier
    // object below its minimum. Small inserted shapes may stay small on click.
    let mut lower = [i64::MIN; 2];
    let mut upper = [i64::MAX; 2];
    for object in objects.iter().filter(|o| selected.contains(&o.id)) {
        let bounds = object.shape.bounds;
        for (axis, dimension) in [bounds.width_hundredths, bounds.height_hundredths]
            .into_iter()
            .enumerate()
        {
            let dimension = i64::try_from(dimension).expect("bounded dimension");
            let maximum = i64::from(canvas[axis]) * 100;
            lower[axis] = lower[axis].max(dimension.min(1_000).min(maximum) - dimension);
            upper[axis] = upper[axis].min(maximum - dimension);
        }
    }
    for axis in 0..2 {
        change[axis] = change[axis].clamp(lower[axis], upper[axis]);
    }
    objects
        .iter()
        .map(|object| {
            let mut changed = *object;
            if selected.contains(&object.id) {
                let (sin, cos) = angle(object.shape);
                let shift = [
                    pixels(change[0]) / 2.0 * if left { -1.0 } else { 1.0 },
                    pixels(change[1]) / 2.0 * if top { -1.0 } else { 1.0 },
                ];
                let bounds = &mut changed.shape.bounds;
                bounds.width_hundredths =
                    u64::try_from(i64::try_from(bounds.width_hundredths).unwrap() + change[0])
                        .unwrap();
                bounds.height_hundredths =
                    u64::try_from(i64::try_from(bounds.height_hundredths).unwrap() + change[1])
                        .unwrap();
                // Keep the opposite local edge fixed as the rotated center changes.
                // At canvas boundaries the layout box is clamped back into the image.
                bounds.x_hundredths +=
                    rounded_hundredths(shift[0] * cos - shift[1] * sin - pixels(change[0]) / 2.0);
                bounds.y_hundredths +=
                    rounded_hundredths(shift[0] * sin + shift[1] * cos - pixels(change[1]) / 2.0);
                bounds.x_hundredths = bounds.x_hundredths.clamp(
                    0,
                    i64::from(canvas[0]) * 100 - i64::try_from(bounds.width_hundredths).unwrap(),
                );
                bounds.y_hundredths = bounds.y_hundredths.clamp(
                    0,
                    i64::from(canvas[1]) * 100 - i64::try_from(bounds.height_hundredths).unwrap(),
                );
            }
            changed
        })
        .collect()
}

fn primary(objects: &[DraftObject], selected: &BTreeSet<u64>) -> Option<DraftObject> {
    objects
        .iter()
        .rev()
        .find(|o| selected.contains(&o.id))
        .copied()
}

fn moved(
    objects: &[DraftObject],
    selected: &BTreeSet<u64>,
    delta: Point,
    canvas: [u32; 2],
) -> Result<Vec<DraftObject>, Notice> {
    if let Some(primary) = primary(objects, selected)
        && primary.shape.version == WPF_VECTOR_SHAPE_VERSION
    {
        if delta == [0, 0] {
            return Ok(objects.to_vec());
        }
        let desired = vector_shape_layout(&primary.shape)
            .map_err(error)?
            .desired_size;
        let mut changed = primary;
        let mut origin = [
            primary.shape.bounds.x_hundredths,
            primary.shape.bounds.y_hundredths,
        ];
        // ElementAdorner move clamps against DesiredSize with its explicit
        // one-layout-unit margin, not the larger arranged/stroked outline.
        for axis in 0..2 {
            origin[axis] = (origin[axis] + delta[axis]).max(-100);
            let far = i64::from(canvas[axis]) * 100 + 100 - rounded_hundredths(desired[axis]);
            origin[axis] = origin[axis].min(far);
        }
        changed.shape.bounds.x_hundredths = origin[0];
        changed.shape.bounds.y_hundredths = origin[1];
        return propagate_wpf(objects, selected, primary, changed, canvas);
    }
    let (dx, dy) = movement(objects, selected, delta[0], delta[1], canvas);
    let mut candidate = objects.to_vec();
    for object in candidate.iter_mut().filter(|o| selected.contains(&o.id)) {
        object.shape.bounds.x_hundredths += dx;
        object.shape.bounds.y_hundredths += dy;
    }
    Ok(candidate)
}

fn resized(
    objects: &[DraftObject],
    selected: &BTreeSet<u64>,
    handle: Handle,
    delta: Point,
    canvas: [u32; 2],
) -> Result<Vec<DraftObject>, Notice> {
    let Some(primary) = primary(objects, selected) else {
        return Ok(objects.to_vec());
    };
    if primary.shape.version != WPF_VECTOR_SHAPE_VERSION {
        return Ok(legacy_resized(objects, selected, handle, delta, canvas));
    }
    if delta == [0, 0] {
        return Ok(objects.to_vec());
    }
    let changed = wpf_resize_primary(primary, handle, delta, canvas)?;
    propagate_wpf(objects, selected, primary, changed, canvas)
}

fn wpf_resize_primary(
    primary: DraftObject,
    handle: Handle,
    delta: Point,
    canvas: [u32; 2],
) -> Result<DraftObject, Notice> {
    let layout = vector_shape_layout(&primary.shape).map_err(error)?;
    let (sin, cos) = angle(primary.shape);
    let local = [
        rounded_hundredths(pixels(delta[0]) * cos + pixels(delta[1]) * sin),
        rounded_hundredths(-pixels(delta[0]) * sin + pixels(delta[1]) * cos),
    ];
    let near = [
        matches!(handle, Handle::TopLeft | Handle::Left | Handle::BottomLeft),
        matches!(handle, Handle::TopLeft | Handle::Top | Handle::TopRight),
    ];
    let active = [
        !matches!(handle, Handle::Top | Handle::Bottom),
        !matches!(handle, Handle::Left | Handle::Right),
    ];
    let before = primary.shape.bounds;
    let old_origin = [before.x_hundredths, before.y_hundredths];
    let mut origin = old_origin;
    let mut size = [before.width_hundredths, before.height_hundredths];
    for axis in 0..2 {
        if !active[axis] {
            continue;
        }
        let desired = rounded_hundredths(layout.desired_size[axis]);
        let mut length = (desired
            + if near[axis] {
                -local[axis]
            } else {
                local[axis]
            })
        .max(1_000);
        if near[axis] {
            origin[axis] -= length - desired;
            if origin[axis] < 0 {
                length += origin[axis];
                origin[axis] = 0;
            }
        }
        // Preserve the source BottomLeft handler's extra far-X check, whose
        // reference is the original Canvas.Left, not its newly computed Left.
        if !near[axis] || (axis == 0 && handle == Handle::BottomLeft) {
            length = length.min(i64::from(canvas[axis]) * 100 - old_origin[axis]);
        }
        size[axis] = u64::try_from(length).map_err(error)?;
    }
    let mut changed = primary;
    changed.shape.bounds = VectorShapeBounds {
        x_hundredths: origin[0],
        y_hundredths: origin[1],
        width_hundredths: size[0],
        height_hundredths: size[1],
    };
    // Right/Bottom do not compensate the world-space opposite edge when the
    // rotation center moves. That V1 interaction remains in legacy_resized.
    Ok(changed)
}

fn propagate_wpf(
    objects: &[DraftObject],
    selected: &BTreeSet<u64>,
    primary: DraftObject,
    changed: DraftObject,
    canvas: [u32; 2],
) -> Result<Vec<DraftObject>, Notice> {
    let old = primary.shape.bounds;
    let new = changed.shape.bounds;
    let size_delta = [
        i64::try_from(new.width_hundredths).map_err(error)?
            - i64::try_from(old.width_hundredths).map_err(error)?,
        i64::try_from(new.height_hundredths).map_err(error)?
            - i64::try_from(old.height_hundredths).map_err(error)?,
    ];
    let origin_delta = [
        new.x_hundredths - old.x_hundredths,
        new.y_hundredths - old.y_hundredths,
    ];
    let mut candidate = objects.to_vec();
    for object in candidate.iter_mut().filter(|o| selected.contains(&o.id)) {
        if object.id == primary.id {
            *object = changed;
            continue;
        }
        let layout = vector_shape_layout(&object.shape).map_err(error)?;
        let bounds = object.shape.bounds;
        let mut size = [bounds.width_hundredths, bounds.height_hundredths];
        let mut origin = [bounds.x_hundredths, bounds.y_hundredths];
        // Source DrawingCanvas.Adorner_Manipulated applies the primary's
        // requested-size delta conditionally to each secondary. This is not
        // V1's shared clamp, nor independent DesiredSize rounding per object.
        for axis in 0..2 {
            let actual = rounded_hundredths(layout.render_size[axis]);
            let maximum = i64::from(canvas[axis]) * 100;
            if size_delta[axis].abs() > 10
                && actual + size_delta[axis] > 1_000
                && actual + size_delta[axis] <= maximum
            {
                size[axis] =
                    u64::try_from(i64::try_from(size[axis]).map_err(error)? + size_delta[axis])
                        .map_err(error)?;
            }
            let position = origin[axis] + origin_delta[axis];
            if position >= 0 && position + actual < maximum {
                origin[axis] = position;
            }
        }
        object.shape.bounds = VectorShapeBounds {
            x_hundredths: origin[0],
            y_hundredths: origin[1],
            width_hundredths: size[0],
            height_hundredths: size[1],
        };
    }
    Ok(candidate)
}

fn angle(shape: VectorShape) -> (f64, f64) {
    (f64::from(shape.rotation_hundredths) / 100.0)
        .to_radians()
        .sin_cos()
}

fn rounded_hundredths(value: f64) -> i64 {
    // Only finite bounded canvas deltas pass here, including the rotated sum.
    #[allow(clippy::cast_possible_truncation)]
    {
        (value * 100.0).round() as i64
    }
}
