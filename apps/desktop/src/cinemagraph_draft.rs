//! Transient Cinemagraph ink editing. No project writes or raster buffers live here.

use std::collections::BTreeSet;

use gif_from_screen_domain::{FrameGeometryPlan, FrameId, PhysicalSize};
use gif_from_screen_render::{InkAttributes, InkPoint, InkSample, InkStroke, InkTip};

use crate::editor_workspace::{EditorWorkspace, OverlaySelectionAnchor};

mod geometry;

pub(crate) use geometry::InkBounds;

#[cfg(test)]
mod tests;

pub(crate) const MAX_CINEMAGRAPH_STROKES: usize = 256;
pub(crate) const MAX_CINEMAGRAPH_SAMPLES: usize = 16_384;
pub(crate) const MAX_CINEMAGRAPH_TARGETS: usize = 1_000;
const MAX_GESTURE_EVENTS: usize = 4_096;
const MAX_DRAFT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum CinemagraphTool {
    #[default]
    Pen,
    PointEraser,
    StrokeEraser,
    Select,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DraftStroke {
    pub(crate) id: u64,
    pub(crate) stroke: InkStroke,
}

/// A bounded, immutable worker input. The anchor remains strict at worker launch.
#[derive(Clone, Debug)]
pub(crate) struct CinemagraphRequest {
    pub(crate) anchor: OverlaySelectionAnchor,
    pub(crate) reference_frame: FrameId,
    pub(crate) reference_size: PhysicalSize,
    pub(crate) strokes: Vec<InkStroke>,
}

#[derive(Clone, Debug)]
struct Binding {
    anchor: OverlaySelectionAnchor,
    reference_frame: FrameId,
    reference_size: PhysicalSize,
}

#[derive(Clone, Debug)]
struct Gesture {
    before: Vec<DraftStroke>,
    selected_before: BTreeSet<u64>,
    kind: GestureKind,
    events: usize,
}

#[derive(Clone, Debug)]
enum GestureKind {
    Pen { id: u64 },
    Erase { previous: InkPoint, whole: bool },
    Marquee { start: InkPoint },
    Transform,
}

#[derive(Debug)]
pub(crate) struct CinemagraphDraft {
    binding: Option<Binding>,
    stale: Option<String>,
    strokes: Vec<DraftStroke>,
    selected: BTreeSet<u64>,
    gesture: Option<Gesture>,
    next_id: u64,
    generation: u64,
    pub(crate) tool: CinemagraphTool,
    pub(crate) pen: InkAttributes,
    pub(crate) eraser: InkAttributes,
}

impl Default for CinemagraphDraft {
    fn default() -> Self {
        Self {
            binding: None,
            stale: None,
            strokes: Vec::new(),
            selected: BTreeSet::new(),
            gesture: None,
            next_id: 1,
            generation: 0,
            tool: CinemagraphTool::Pen,
            pen: InkAttributes::default(),
            eraser: InkAttributes {
                width: 10.0,
                height: 10.0,
                tip: InkTip::Ellipse,
                fit_to_curve: false,
                ignore_pressure: true,
            },
        }
    }
}

impl CinemagraphDraft {
    pub(crate) fn begin(&mut self, workspace: &EditorWorkspace) -> Result<(), String> {
        if !(1..=MAX_CINEMAGRAPH_TARGETS).contains(&workspace.selection().len()) {
            return Err("Select between 1 and 1,000 target frames for Cinemagraph.".to_owned());
        }
        let anchor = workspace
            .overlay_selection_anchor()
            .map_err(|error| error.to_string())?;
        let (reference_frame, reference_size) = reference(workspace)?;
        self.cancel_gesture();
        self.strokes.clear();
        self.selected.clear();
        self.stale = None;
        self.binding = Some(Binding {
            anchor,
            reference_frame,
            reference_size,
        });
        self.bump_generation();
        Ok(())
    }

    pub(crate) fn reconcile(&mut self, workspace: &EditorWorkspace) {
        let Some(binding) = &self.binding else {
            return;
        };
        let valid = binding.anchor.matches(workspace)
            && reference(workspace)
                .is_ok_and(|current| current == (binding.reference_frame, binding.reference_size));
        if !valid && self.stale.is_none() {
            self.cancel_gesture();
            self.stale = Some(
                "The project, first frame, canvas or target selection changed. Restart Cinemagraph before applying this draft.".to_owned(),
            );
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.binding.is_some()
    }
    pub(crate) fn is_stale(&self) -> bool {
        self.stale.is_some()
    }
    pub(crate) fn stale_reason(&self) -> Option<&str> {
        self.stale.as_deref()
    }
    pub(crate) fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }
    pub(crate) fn strokes(&self) -> &[DraftStroke] {
        &self.strokes
    }
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn selected_ids(&self) -> &BTreeSet<u64> {
        &self.selected
    }
    pub(crate) fn reference_frame(&self) -> Option<FrameId> {
        self.binding.as_ref().map(|binding| binding.reference_frame)
    }
    pub(crate) fn reference_size(&self) -> Option<PhysicalSize> {
        self.binding.as_ref().map(|binding| binding.reference_size)
    }

    pub(crate) fn request(
        &self,
        workspace: &EditorWorkspace,
    ) -> Result<CinemagraphRequest, String> {
        self.ready()?;
        if self.gesture.is_some() {
            return Err("Finish the active ink gesture first.".to_owned());
        }
        let binding = self.binding.as_ref().ok_or("Start Cinemagraph first.")?;
        if !binding.anchor.matches(workspace)
            || reference(workspace)? != (binding.reference_frame, binding.reference_size)
        {
            return Err(
                "The Cinemagraph draft target is stale. Restart it before applying.".to_owned(),
            );
        }
        if self.strokes.is_empty() {
            return Err("Draw at least one motion-region stroke first.".to_owned());
        }
        validate_budget(&self.strokes)?;
        Ok(CinemagraphRequest {
            anchor: binding.anchor.clone(),
            reference_frame: binding.reference_frame,
            reference_size: binding.reference_size,
            strokes: self
                .strokes
                .iter()
                .map(|stroke| stroke.stroke.clone())
                .collect(),
        })
    }

    pub(crate) fn close(&mut self) {
        self.cancel_gesture();
        self.binding = None;
        self.stale = None;
        self.strokes.clear();
        self.selected.clear();
        self.bump_generation();
    }

    pub(crate) fn clear(&mut self) -> Result<(), String> {
        self.ready()?;
        self.cancel_gesture();
        self.strokes.clear();
        self.selected.clear();
        self.bump_generation();
        Ok(())
    }

    pub(crate) fn delete_selected(&mut self) -> Result<(), String> {
        self.ready()?;
        self.cancel_gesture();
        self.strokes
            .retain(|stroke| !self.selected.contains(&stroke.id));
        self.selected.clear();
        self.bump_generation();
        Ok(())
    }

    pub(crate) fn select_all(&mut self) -> Result<(), String> {
        self.ready()?;
        self.cancel_gesture();
        self.selected = self.strokes.iter().map(|stroke| stroke.id).collect();
        Ok(())
    }

    pub(crate) fn cancel_gesture(&mut self) {
        if let Some(gesture) = self.gesture.take() {
            self.strokes = gesture.before;
            self.selected = gesture.selected_before;
            self.bump_generation();
        }
    }

    /// Mouse input should use pressure 0.5, the WPF neutral pressure.
    pub(crate) fn pointer_down(&mut self, sample: InkSample) -> Result<(), String> {
        self.ready()?;
        validate_sample(sample)?;
        self.ensure_inside(sample.position)?;
        if self.gesture.is_some() {
            return Err("Finish or cancel the current ink gesture first.".to_owned());
        }
        validate_attributes(self.pen)?;
        validate_attributes(self.eraser)?;
        validate_budget(&self.strokes)?;
        let before = self.strokes.clone();
        let selected_before = self.selected.clone();
        let kind = match self.tool {
            CinemagraphTool::Pen => {
                if self.strokes.len() >= MAX_CINEMAGRAPH_STROKES {
                    return Err(
                        "The Cinemagraph draft has reached its 256-stroke limit.".to_owned()
                    );
                }
                let id = self.allocate_id()?;
                self.strokes.push(DraftStroke {
                    id,
                    stroke: InkStroke {
                        samples: vec![sample],
                        attributes: self.pen,
                    },
                });
                self.selected.clear();
                self.bump_generation();
                GestureKind::Pen { id }
            }
            CinemagraphTool::PointEraser | CinemagraphTool::StrokeEraser => GestureKind::Erase {
                previous: sample.position,
                whole: self.tool == CinemagraphTool::StrokeEraser,
            },
            CinemagraphTool::Select => GestureKind::Marquee {
                start: sample.position,
            },
        };
        self.gesture = Some(Gesture {
            before,
            selected_before,
            kind,
            events: 0,
        });
        self.pointer_move(sample)
    }

    pub(crate) fn pointer_move(&mut self, sample: InkSample) -> Result<(), String> {
        self.ready()?;
        if self.gesture.is_none() {
            return Ok(());
        }
        let result = self.update_pointer(sample);
        if result.is_err() {
            self.cancel_gesture();
        }
        result
    }

    pub(crate) fn pointer_up(&mut self, sample: InkSample) -> Result<(), String> {
        self.pointer_move(sample)?;
        self.gesture = None;
        Ok(())
    }

    pub(crate) fn selection_bounds(&self) -> Result<Option<InkBounds>, String> {
        geometry::bounds(
            self.strokes
                .iter()
                .filter(|stroke| self.selected.contains(&stroke.id))
                .map(|stroke| &stroke.stroke),
        )
    }

    pub(crate) fn begin_selection_transform(&mut self) -> Result<(), String> {
        self.ready()?;
        if self.gesture.is_some() {
            return Err("Finish the active ink gesture first.".to_owned());
        }
        if self.selected.is_empty() {
            return Err("Select strokes to move or resize first.".to_owned());
        }
        validate_budget(&self.strokes)?;
        self.gesture = Some(Gesture {
            before: self.strokes.clone(),
            selected_before: self.selected.clone(),
            kind: GestureKind::Transform,
            events: 0,
        });
        Ok(())
    }

    /// Affine coordinate transform from the gesture's original strokes. Tips and pressure stay fixed.
    pub(crate) fn update_selection_transform(
        &mut self,
        scale: InkPoint,
        translation: InkPoint,
    ) -> Result<(), String> {
        self.ready()?;
        let result = self.transform_selection(scale, translation);
        if result.is_err() {
            self.cancel_gesture();
        }
        result
    }

    pub(crate) fn finish_selection_transform(&mut self) -> Result<(), String> {
        if !matches!(
            self.gesture.as_ref().map(|gesture| &gesture.kind),
            Some(GestureKind::Transform)
        ) {
            return Err("There is no active selection transform.".to_owned());
        }
        self.gesture = None;
        Ok(())
    }

    fn transform_selection(
        &mut self,
        scale: InkPoint,
        translation: InkPoint,
    ) -> Result<(), String> {
        if !geometry::finite(scale)
            || !geometry::finite(translation)
            || scale.x <= 0.0
            || scale.y <= 0.0
            || scale.x > 1_000.0
            || scale.y > 1_000.0
        {
            return Err("Selection scale must be finite, positive and at most 1,000.".to_owned());
        }
        let gesture = self
            .gesture
            .as_mut()
            .ok_or("Start a selection transform first.")?;
        if !matches!(gesture.kind, GestureKind::Transform) {
            return Err("Another ink gesture is active.".to_owned());
        }
        advance_events(gesture)?;
        let mut changed = gesture.before.clone();
        for stroke in &mut changed {
            if !gesture.selected_before.contains(&stroke.id) {
                continue;
            }
            for sample in &mut stroke.stroke.samples {
                sample.position.x = sample.position.x * scale.x + translation.x;
                sample.position.y = sample.position.y * scale.y + translation.y;
                validate_sample(*sample)?;
            }
        }
        validate_budget(&changed)?;
        if self.strokes != changed {
            self.strokes = changed;
            self.bump_generation();
        }
        Ok(())
    }

    fn update_pointer(&mut self, sample: InkSample) -> Result<(), String> {
        validate_sample(sample)?;
        let Some(gesture) = self.gesture.as_mut() else {
            return Ok(());
        };
        advance_events(gesture)?;
        match gesture.kind {
            GestureKind::Pen { id } => {
                let stroke = self
                    .strokes
                    .iter_mut()
                    .find(|stroke| stroke.id == id)
                    .ok_or("The active stroke disappeared.")?;
                if stroke.stroke.samples.last() != Some(&sample) {
                    stroke.stroke.samples.push(sample);
                    self.bump_generation();
                }
            }
            GestureKind::Erase { previous, whole } => {
                let changed =
                    geometry::erase(&self.strokes, previous, sample.position, self.eraser, whole)?;
                self.install_erased(changed)?;
                if let Some(gesture) = &mut self.gesture {
                    gesture.kind = GestureKind::Erase {
                        previous: sample.position,
                        whole,
                    };
                }
            }
            GestureKind::Marquee { start } => {
                self.selected = geometry::select(&self.strokes, start, sample.position)?;
            }
            GestureKind::Transform => {
                return Err("Use the selection transform controls for this gesture.".to_owned());
            }
        }
        validate_budget(&self.strokes)
    }

    fn install_erased(&mut self, changed: Vec<(u64, Vec<InkStroke>)>) -> Result<(), String> {
        let count: usize = changed.iter().map(|(_, fragments)| fragments.len()).sum();
        if count > MAX_CINEMAGRAPH_STROKES {
            return Err(
                "Erasing would exceed the 256-stroke draft limit. Clear or delete strokes first."
                    .to_owned(),
            );
        }
        let mut replacement = Vec::with_capacity(count);
        let mut selected = BTreeSet::new();
        for (old_id, fragments) in changed {
            for (index, stroke) in fragments.into_iter().enumerate() {
                let id = if index == 0 {
                    old_id
                } else {
                    self.allocate_id()?
                };
                if self.selected.contains(&old_id) {
                    selected.insert(id);
                }
                replacement.push(DraftStroke { id, stroke });
            }
        }
        validate_budget(&replacement)?;
        self.selected = selected;
        if self.strokes != replacement {
            self.strokes = replacement;
            self.bump_generation();
        }
        Ok(())
    }

    fn ready(&self) -> Result<(), String> {
        if let Some(reason) = &self.stale {
            return Err(reason.clone());
        }
        if self.binding.is_none() {
            return Err("Start Cinemagraph first.".to_owned());
        }
        Ok(())
    }

    fn ensure_inside(&self, point: InkPoint) -> Result<(), String> {
        let size = self.reference_size().ok_or("Start Cinemagraph first.")?;
        if point.x < 0.0
            || point.y < 0.0
            || point.x > f64::from(size.width.get())
            || point.y > f64::from(size.height.get())
        {
            return Err("Start an ink gesture inside the reference image.".to_owned());
        }
        Ok(())
    }

    fn allocate_id(&mut self) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("Cinemagraph stroke identity exhausted.")?;
        Ok(id)
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

fn reference(workspace: &EditorWorkspace) -> Result<(FrameId, PhysicalSize), String> {
    let frame = workspace
        .manifest()
        .timeline
        .frames
        .first()
        .ok_or("Cinemagraph needs a first reference frame.")?;
    let asset = workspace
        .manifest()
        .assets
        .get(&frame.asset_id)
        .ok_or("The reference frame asset is missing.")?;
    let size = asset
        .kind
        .raster_size()
        .ok_or("The reference frame is not a raster image.")?;
    let geometry = FrameGeometryPlan::new(frame, size)?;
    Ok((frame.id, geometry.output_size()))
}

fn validate_attributes(attributes: InkAttributes) -> Result<(), String> {
    if !attributes.width.is_finite()
        || !attributes.height.is_finite()
        || !(1.0..=100.0).contains(&attributes.width)
        || !(1.0..=100.0).contains(&attributes.height)
    {
        return Err(
            "Pen and eraser width and height must be between 1 and 100 physical pixels.".to_owned(),
        );
    }
    Ok(())
}

fn validate_sample(sample: InkSample) -> Result<(), String> {
    if !geometry::finite(sample.position)
        || sample.position.x.abs() > 1_000_000.0
        || sample.position.y.abs() > 1_000_000.0
        || !sample.pressure.is_finite()
        || !(0.0..=1.0).contains(&sample.pressure)
    {
        return Err(
            "Ink positions and pressure must be finite and within their supported limits."
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_budget(strokes: &[DraftStroke]) -> Result<(), String> {
    let samples = strokes
        .iter()
        .try_fold(0usize, |total, stroke| {
            total.checked_add(stroke.stroke.samples.len())
        })
        .ok_or("Ink sample count overflowed.")?;
    // Account for the current draft, gesture rollback, temporary edit and worker request.
    let bytes = samples
        .checked_mul(std::mem::size_of::<InkSample>())
        .and_then(|bytes| {
            strokes
                .len()
                .checked_mul(std::mem::size_of::<DraftStroke>())
                .and_then(|overhead| bytes.checked_add(overhead))
        })
        .and_then(|bytes| bytes.checked_mul(4))
        .ok_or("Ink draft memory accounting overflowed.")?;
    if strokes.len() > MAX_CINEMAGRAPH_STROKES
        || samples > MAX_CINEMAGRAPH_SAMPLES
        || bytes > MAX_DRAFT_BYTES
    {
        return Err(
            "The Cinemagraph draft exceeds its 256-stroke, 16,384-sample or 4 MiB working limit."
                .to_owned(),
        );
    }
    Ok(())
}

fn advance_events(gesture: &mut Gesture) -> Result<(), String> {
    if gesture.events >= MAX_GESTURE_EVENTS {
        return Err("This ink gesture exceeded its bounded event limit and was rolled back. Start another gesture.".to_owned());
    }
    gesture.events += 1;
    Ok(())
}
