//! Ready-only, cancellable snap work owned by one recorder overlay lifetime.

use crate::{RecorderStage, recorder_geometry::RecorderGeometry};
use eframe::egui;
use gif_from_screen_capture::{CaptureSource, CaptureSourceKind, PhysicalRect};
use gif_from_screen_capture_linux::{
    DragPickerButton, DragPickerRequest, DragPickerUpdate, PickedWindow, WindowSnapBounds,
    WindowSnapCatalog, list_snap_windows, pick_snap_window, query_window_snap,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
};

pub(crate) struct WindowSnapUi {
    available: bool,
    candidates: Vec<CaptureSource>,
    selected: usize,
    bounds: WindowSnapBounds,
    pending: Option<Pending>,
    notice: Option<String>,
    closing: bool,
    native: Option<NativeDrag>,
    queued_picker: Option<ClickIntent>,
    drag_failed: bool,
    generation: u64,
    published_frame: Option<u64>,
    drawn: ButtonLayout,
    parent: Option<DragParent>,
    pixels_per_point: f32,
    launch_drag: DragLauncher,
    click_picker: ClickPicker,
}

impl Default for WindowSnapUi {
    fn default() -> Self {
        Self {
            available: false,
            candidates: Vec::new(),
            selected: 0,
            bounds: WindowSnapBounds::default(),
            pending: None,
            notice: None,
            closing: false,
            native: None,
            queued_picker: None,
            drag_failed: false,
            generation: 0,
            published_frame: None,
            drawn: ButtonLayout::default(),
            parent: None,
            pixels_per_point: 1.0,
            launch_drag: start_drag_handle,
            click_picker: pick_window,
        }
    }
}

/// Actual native GUI client identity and geometry, not the capture selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DragParent {
    pub(crate) window_id: u32,
    pub(crate) client: PhysicalRect,
}

#[derive(Clone, Copy)]
struct ClickIntent {
    geometry: RecorderGeometry,
    bounds: WindowSnapBounds,
    cancelled: bool,
}

#[derive(Default)]
struct ButtonLayout {
    hit: Option<egui::Rect>,
    enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DragBinding {
    geometry: RecorderGeometry,
    parent: DragParent,
    layout: Option<egui::Rect>,
    scale_bits: u32,
    request: DragPickerRequest,
}

impl DragBinding {
    fn same_target(self, other: Self) -> bool {
        self.geometry == other.geometry
            && self.parent == other.parent
            && self.layout == other.layout
            && self.scale_bits == other.scale_bits
            && self.request.rect == other.request.rect
            && self.request.parent_size == other.request.parent_size
            && self.request.bounds == other.request.bounds
    }
}

struct NativeDrag {
    handle: Box<dyn DragHandle>,
    binding: Option<DragBinding>,
    ready: Option<u64>,
}

impl Drop for NativeDrag {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

// This narrow boundary lets lifecycle tests inject terminal cleanup receipts
// without ever connecting a unit test to the user's DISPLAY.
trait DragHandle {
    fn request(&self, request: DragPickerRequest) -> Result<(), String>;
    fn poll(&mut self) -> DragPickerUpdate;
    fn is_picking(&self) -> bool;
    fn is_claimed(&self) -> bool;
    fn cancelled(&self) -> bool;
    fn stop(&self);
}

impl DragHandle for DragPickerButton {
    fn request(&self, request: DragPickerRequest) -> Result<(), String> {
        Self::request(self, request)
    }
    fn poll(&mut self) -> DragPickerUpdate {
        Self::poll(self)
    }
    fn is_picking(&self) -> bool {
        Self::is_picking(self)
    }
    fn is_claimed(&self) -> bool {
        Self::is_claimed(self)
    }
    fn cancelled(&self) -> bool {
        Self::cancelled(self)
    }
    fn stop(&self) {
        Self::stop(self);
    }
}

type DragLauncher = fn(u32) -> Result<Box<dyn DragHandle>, String>;
type ClickPicker = fn(WindowSnapBounds, &AtomicBool) -> Result<Option<PickedWindow>, String>;

fn pick_window(
    bounds: WindowSnapBounds,
    cancellation: &AtomicBool,
) -> Result<Option<PickedWindow>, String> {
    pick_snap_window(None, bounds, cancellation)
}

fn start_drag_handle(parent: u32) -> Result<Box<dyn DragHandle>, String> {
    DragPickerButton::start(None, parent, std::process::id())
        .map(|handle| Box::new(handle) as Box<dyn DragHandle>)
}

struct Pending {
    geometry: RecorderGeometry,
    cancellation: Arc<AtomicBool>,
    receiver: Receiver<Result<WorkResult, String>>,
    picking: bool,
}

enum WorkResult {
    Region(PhysicalRect),
    Catalog(WindowSnapCatalog),
    Picked(Option<PickedWindow>),
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Release);
    }
}

impl WindowSnapUi {
    pub(crate) fn set_candidates(&mut self, sources: &[CaptureSource]) {
        self.available = true;
        let selected = self
            .candidates
            .get(self.selected)
            .map(|source| source.id().clone());
        self.candidates = sources
            .iter()
            .filter(|source| source.kind() == CaptureSourceKind::Window)
            .take(256)
            .cloned()
            .collect();
        self.selected = selected
            .and_then(|id| self.candidates.iter().position(|source| *source.id() == id))
            .unwrap_or(0);
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.is_picking()
            || self.native_claimed()
            || self.pending.as_ref().is_some_and(|pending| {
                pending.picking || !pending.cancellation.load(Ordering::Acquire)
            })
    }

    /// Remains true through cancelled native cleanup, before controls or Start
    /// can become available again. A cancellation flag alone is not an ungrab ACK.
    pub(crate) fn is_picking(&self) -> bool {
        self.queued_picker.is_some()
            || self
                .native
                .as_ref()
                .is_some_and(|native| native.handle.is_picking())
            || self.pending.as_ref().is_some_and(|pending| pending.picking)
    }

    pub(crate) fn is_closing(&self) -> bool {
        self.closing
    }

    /// Closing the recorder is also a cancellation, but restoring an ordinary
    /// interactive window must wait for the picker's native cleanup result.
    pub(crate) fn request_close(&mut self) -> bool {
        if self.is_picking() || self.native.is_some() {
            self.closing = true;
            self.cancel();
            if let Some(native) = &self.native {
                native.handle.stop();
            }
            false
        } else {
            true
        }
    }

    pub(crate) fn close_ready(&self) -> bool {
        self.closing && !self.is_picking() && self.native.is_none()
    }

    pub(crate) fn keyboard_control(&mut self, context: &egui::Context) {
        if (self.is_picking() || self.native_claimed())
            && context.input(|input| !input.focused || input.key_pressed(egui::Key::Escape))
        {
            self.cancel();
            self.notice = Some("Window selection cancelled; waiting for native cleanup.".into());
        }
    }

    pub(crate) fn cancel(&mut self) {
        if let Some(intent) = &mut self.queued_picker {
            intent.cancelled = true;
        }
        if let Some(native) = &self.native
            && (native.handle.is_picking() || native.handle.is_claimed())
        {
            native.handle.stop();
        }
        if let Some(pending) = &self.pending {
            pending.cancellation.store(true, Ordering::Release);
        }
    }

    /// Call once before drawing. GUI rectangles below are client-local logical
    /// points; the native observer supplies parent identity and physical size.
    pub(crate) fn begin_frame(&mut self, parent: Option<DragParent>, pixels_per_point: f32) {
        self.drawn.hit = None;
        self.drawn.enabled = false;
        self.parent = parent;
        self.pixels_per_point = pixels_per_point;
    }

    fn native_claimed(&self) -> bool {
        self.native
            .as_ref()
            .is_some_and(|native| native.handle.is_claimed())
    }

    fn drag_ready(&self) -> bool {
        self.native.as_ref().is_some_and(|native| {
            !native.handle.cancelled()
                && !native.handle.is_claimed()
                && native.binding.is_some_and(|binding| {
                    binding.request.rect.is_some()
                        && native.ready == Some(binding.request.generation)
                })
        })
    }

    fn binding_current(
        &self,
        binding: DragBinding,
        geometry: RecorderGeometry,
        stage: RecorderStage,
        dragging: bool,
    ) -> bool {
        !self.closing
            && stage == RecorderStage::Ready
            && !dragging
            && !geometry.size_is_frozen()
            && binding.geometry == geometry
            && self.parent == Some(binding.parent)
            && binding.request.bounds == self.bounds
            && binding.scale_bits == self.pixels_per_point.to_bits()
    }

    /// Publish at most one request after the entire controller has drawn. Missing
    /// or clipped-away controls disable only the idle hit rectangle. A claimed
    /// press must retain its original child until the root grab or cleanup ACK.
    pub(crate) fn publish_drag_button(
        &mut self,
        geometry: RecorderGeometry,
        stage: RecorderStage,
        visible: bool,
        context: &egui::Context,
    ) {
        if self.native_claimed()
            || self
                .native
                .as_ref()
                .is_some_and(|native| native.handle.is_picking())
        {
            let valid = self
                .native
                .as_ref()
                .and_then(|native| native.binding)
                .is_some_and(|binding| {
                    self.binding_current(binding, geometry, stage, false)
                        && (!visible || self.drawn.hit == binding.layout)
                });
            if !valid {
                self.cancel();
                self.notice = Some(
                    "Window selection cancelled because its initiating layout changed.".into(),
                );
            }
            return;
        }
        if context.will_discard() || self.published_frame == Some(context.cumulative_frame_nr()) {
            return;
        }
        if self.closing
            || stage != RecorderStage::Ready
            || geometry.size_is_frozen()
            || self.parent.is_none()
        {
            if let Some(native) = &self.native {
                native.handle.stop();
            }
            return;
        }
        if self.drag_failed
            || self.queued_picker.is_some()
            || self
                .native
                .as_ref()
                .is_some_and(|native| native.handle.cancelled())
        {
            return;
        }
        let layout = (visible
            && self.available
            && self.drawn.enabled
            && self.pending.is_none()
            && !egui::Popup::is_any_open(context))
        .then_some(self.drawn.hit)
        .flatten();
        let parent = self.parent.expect("parent checked");
        let hit = match physical_hit(layout, parent, self.pixels_per_point) {
            Ok(hit) => hit,
            Err(error) => {
                self.drag_error(&error);
                return;
            }
        };
        let binding = DragBinding {
            geometry,
            parent,
            layout,
            scale_bits: self.pixels_per_point.to_bits(),
            request: DragPickerRequest {
                generation: self.generation,
                rect: hit,
                parent_size: parent.client.size(),
                bounds: self.bounds,
            },
        };
        self.install_drag_binding(binding, context.cumulative_frame_nr());
    }

    fn install_drag_binding(&mut self, mut binding: DragBinding, frame: u64) {
        if self
            .native
            .as_ref()
            .and_then(|native| native.binding)
            .is_some_and(|previous| previous.same_target(binding))
        {
            return;
        }
        if self
            .native
            .as_ref()
            .and_then(|native| native.binding)
            .is_some_and(|previous| previous.parent.window_id != binding.parent.window_id)
        {
            self.native.as_ref().unwrap().handle.stop();
            return;
        }
        if self.native.is_none() {
            if binding.request.rect.is_none() {
                return;
            }
            match (self.launch_drag)(binding.parent.window_id) {
                Ok(handle) => {
                    self.native = Some(NativeDrag {
                        handle,
                        binding: None,
                        ready: None,
                    });
                }
                Err(error) => {
                    self.drag_error(&error);
                    return;
                }
            }
        }
        let Some(generation) = self.generation.checked_add(1) else {
            self.drag_error("Drag-handle generations exhausted; reopen the recorder.");
            return;
        };
        binding.request.generation = generation;
        self.published_frame = Some(frame);
        let native = self.native.as_mut().expect("drag service prepared");
        match native.handle.request(binding.request) {
            Ok(()) => {
                native.binding = Some(binding);
                native.ready = None;
                self.generation = generation;
            }
            Err(error) => {
                // A native press may atomically claim the old request between
                // the checks above and this call. Keep its identity, cancel it,
                // and never reinterpret its result as the new layout.
                if native.handle.is_claimed() || native.handle.is_picking() {
                    native.handle.stop();
                    self.notice = Some(
                        "Window selection cancelled because its initiating layout changed.".into(),
                    );
                } else {
                    self.drag_error(&error);
                }
            }
        }
    }

    fn drag_error(&mut self, error: &str) {
        self.drag_failed = true;
        if let Some(native) = &self.native {
            native.handle.stop();
        }
        self.notice = Some(format!(
            "Native drag handle unavailable: {error} Click-to-pick is still available; use Retry drag handle to retry."
        ));
    }

    fn poll_native(
        &mut self,
        geometry: &mut RecorderGeometry,
        stage: RecorderStage,
        dragging: bool,
    ) {
        let Some(native) = &self.native else {
            return;
        };
        let claimed = native.handle.is_claimed() || native.handle.is_picking();
        if claimed
            && !native
                .binding
                .is_some_and(|binding| self.binding_current(binding, *geometry, stage, dragging))
        {
            native.handle.stop();
            self.notice = Some("Window selection cancelled because its original layout or recording target changed.".into());
        }
        let native = self.native.as_mut().expect("native retained until cleanup");
        let update = native.handle.poll();
        native.ready = update.ready;
        if let Some(active) = update.claimed.or(update.active)
            && native
                .binding
                .is_none_or(|binding| binding.request.generation != active)
        {
            native.handle.stop();
        }
        let Some(result) = update.result else {
            return;
        };
        let native = self.native.take().expect("one terminal native result");
        if native.handle.cancelled() {
            return;
        }
        match result {
            Err(error) => self.drag_error(&error),
            Ok(None) => {}
            Ok(Some(selection)) => {
                if native.binding.is_some_and(|binding| {
                    binding.request.generation == selection.generation
                        && self.binding_current(binding, *geometry, stage, dragging)
                }) {
                    self.finish_result(geometry, Ok(WorkResult::Picked(selection.picked)));
                } else {
                    self.notice = Some(
                        "Discarded a stale native window selection; current region kept.".into(),
                    );
                }
            }
        }
    }

    fn poll_queued_picker(
        &mut self,
        geometry: &RecorderGeometry,
        stage: RecorderStage,
        dragging: bool,
    ) {
        let Some(mut intent) = self.queued_picker else {
            return;
        };
        if self.closing
            || intent.geometry != *geometry
            || intent.bounds != self.bounds
            || stage != RecorderStage::Ready
            || dragging
        {
            intent.cancelled = true;
            self.queued_picker = Some(intent);
        }
        if self.native.is_none() {
            self.queued_picker = None;
            if !intent.cancelled
                && let Err(error) = self.start_click_picker(intent.geometry, intent.bounds)
            {
                self.notice = Some(error);
            }
        }
    }

    pub(crate) fn poll(
        &mut self,
        geometry: &mut RecorderGeometry,
        stage: RecorderStage,
        dragging: bool,
    ) {
        self.poll_native(geometry, stage, dragging);
        self.poll_queued_picker(geometry, stage, dragging);
        let Some(pending) = &self.pending else {
            return;
        };
        if pending.geometry != *geometry || stage != RecorderStage::Ready || dragging {
            self.cancel();
            self.notice = Some(
                "Window snap cancelled because the recording selection or stage changed.".into(),
            );
        }
        let pending = self
            .pending
            .as_ref()
            .expect("pending retained until completion");
        let result = match pending.receiver.try_recv() {
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Err("Window snap worker stopped without a result.".into())
            }
            Ok(result) => result,
        };
        let pending = self.pending.take().expect("one pending snap");
        if pending.cancellation.load(Ordering::Acquire) {
            return;
        }
        self.finish_result(geometry, result);
    }

    fn finish_result(
        &mut self,
        geometry: &mut RecorderGeometry,
        result: Result<WorkResult, String>,
    ) {
        self.notice = Some(match result {
            Ok(WorkResult::Picked(None)) => "Window selection cancelled; original region kept.".into(),
            Ok(WorkResult::Picked(Some(picked))) => match geometry.snap_to(picked.region) {
                Ok(()) => {
                    if let Some(index) = self.candidates.iter().position(|source| source.id() == picked.source.id()) {
                        self.candidates[index] = picked.source;
                        self.selected = index;
                    } else {
                        if self.candidates.len() == 256 { self.candidates.pop(); }
                        self.candidates.push(picked.source);
                        self.selected = self.candidates.len() - 1;
                    }
                    "Selected window; recording region updated. This is one-time positioning, not window tracking.".into()
                }
                Err(error) => format!("Selected window does not fit; original region kept: {error}"),
            },
            Ok(WorkResult::Region(region)) => match geometry.snap_to(region) {
                Ok(()) => "Snapped to the current window bounds. This is a one-time position, not window tracking.".into(),
                Err(error) => format!("Window snap failed; original selection kept: {error}"),
            },
            Ok(WorkResult::Catalog(catalog)) => {
                self.set_candidates(&catalog.windows);
                format!("Found {} windows. Selection geometry is unchanged.{}", self.candidates.len(), if catalog.truncated { " The discovery limit was reached; the list is incomplete." } else { "" })
            }
            Err(error) => format!("Window operation failed; selection and previous list kept: {error}"),
        });
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        geometry: &RecorderGeometry,
        stage: RecorderStage,
    ) {
        if stage != RecorderStage::Ready || geometry.size_is_frozen() || !self.available {
            return;
        }
        egui::CollapsingHeader::new("Fit region to a window…").show(ui, |ui| {
            ui.add_enabled_ui(!self.is_pending() && !self.closing && self.pending.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.selectable_value(&mut self.bounds, WindowSnapBounds::WindowFrame, "Window frame");
                    ui.selectable_value(&mut self.bounds, WindowSnapBounds::Client, "Client area");
                    ui.selectable_value(&mut self.bounds, WindowSnapBounds::Outer, "Native bounds");
                });
                let picker = ui.button("Pick window on screen…");
                let hit = picker.rect.intersect(ui.clip_rect());
                self.drawn.hit = (hit.is_finite() && hit.is_positive()).then_some(hit);
                self.drawn.enabled = picker.enabled();
                if picker.clicked()
                    && let Err(error) = self.start_picker(*geometry) { self.notice = Some(error); }
                self.show_drag_status(ui);
                ui.small("Drag this button onto a window and release, or click it then pick a window. Right-click or Escape cancels. Controls hide only while selecting; the target must fit within the selected screen.");
                if self.drag_failed && ui.button("Retry drag handle").clicked() {
                    self.drag_failed = false;
                    self.notice = None;
                }
                egui::ComboBox::from_id_salt("snap-window-choice")
                    .width(ui.available_width())
                    .truncate()
                    .selected_text(self.candidates.get(self.selected).map_or("No windows discovered", |source| source.name()))
                    .show_ui(ui, |ui| {
                        ui.set_max_width(340.0);
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        for (index, source) in self.candidates.iter().enumerate() {
                            ui.selectable_value(&mut self.selected, index, source.name()).on_hover_text(source.name());
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                if ui.button("Refresh windows").clicked()
                    && let Err(error) = self.start_work(*geometry, |cancel| list_snap_windows(None, cancel).map(WorkResult::Catalog))
                {
                    self.notice = Some(error);
                }
                if ui.add_enabled(!self.candidates.is_empty(), egui::Button::new("Snap region")).clicked() {
                    let source = self.candidates[self.selected].clone();
                    let bounds = self.bounds;
                    if let Err(error) = self.start(*geometry, move |cancel| query_window_snap(None, &source, bounds, cancel)) {
                        self.notice = Some(error);
                    }
                }
                });
            });
            if self.is_pending() || self.pending.is_some() {
                ui.spinner();
                if ui.button("Cancel window operation").clicked() {
                    self.cancel();
                    self.notice = Some("Window snap cancelled; selection unchanged.".into());
                }
            }
            ui.small("Window frame uses validated WM borders or client-side shadow hints. Native bounds may include invisible margins. Refresh discovers new or renamed windows without closing this controller.");
            if let Some(notice) = &self.notice { ui.label(notice); }
        });
    }

    fn show_drag_status(&self, ui: &mut egui::Ui) {
        let status = if self.drag_failed {
            "Drag unavailable; click-to-pick available."
        } else if self.drag_ready() {
            "Drag ready; or click to pick."
        } else {
            "Preparing drag; click-to-pick available."
        };
        // One reserved line below the fixed caption: ready changes must not
        // move its input rectangle and thereby invalidate their own generation.
        ui.add(egui::Label::new(egui::RichText::new(status).small()).truncate())
            .on_hover_text(status);
    }

    fn start(
        &mut self,
        geometry: RecorderGeometry,
        loader: impl FnOnce(&AtomicBool) -> Result<PhysicalRect, String> + Send + 'static,
    ) -> Result<(), String> {
        self.start_work(geometry, move |cancel| {
            loader(cancel).map(WorkResult::Region)
        })
    }

    fn start_picker(&mut self, geometry: RecorderGeometry) -> Result<(), String> {
        if self.is_picking() || self.native_claimed() || self.pending.is_some() {
            return Err("A window selection is already finishing.".into());
        }
        if let Some(native) = &self.native {
            native.handle.stop();
            self.queued_picker = Some(ClickIntent {
                geometry,
                bounds: self.bounds,
                cancelled: false,
            });
            self.notice = Some(
                "Preparing click-to-pick; waiting for the native drag handle to close.".into(),
            );
            return Ok(());
        }
        self.start_click_picker(geometry, self.bounds)
    }

    fn start_click_picker(
        &mut self,
        geometry: RecorderGeometry,
        bounds: WindowSnapBounds,
    ) -> Result<(), String> {
        let picker = self.click_picker;
        self.start_work(geometry, move |cancel| {
            picker(bounds, cancel).map(WorkResult::Picked)
        })?;
        self.pending.as_mut().expect("new picker job").picking = true;
        Ok(())
    }

    fn start_work(
        &mut self,
        geometry: RecorderGeometry,
        loader: impl FnOnce(&AtomicBool) -> Result<WorkResult, String> + Send + 'static,
    ) -> Result<(), String> {
        if self.pending.is_some() || self.is_picking() || self.native_claimed() || self.closing {
            return Err("A window snap is still finishing.".into());
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&cancellation);
        let (send, receiver) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("gfs-window-snap".into())
            .spawn(move || {
                let _ = send.send(loader(&cancel));
            })
            .map_err(|error| format!("Could not start window snap: {error}"))?;
        self.pending = Some(Pending {
            geometry,
            cancellation,
            receiver,
            picking: false,
        });
        self.notice = Some("Reading windows…".into());
        Ok(())
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn physical_hit(
    layout: Option<egui::Rect>,
    parent: DragParent,
    ppp: f32,
) -> Result<Option<PhysicalRect>, String> {
    if !ppp.is_finite() || ppp <= 0.0 {
        return Err("Invalid UI pixel scale.".into());
    }
    let Some(rect) = layout else {
        return Ok(None);
    };
    if !rect.is_finite() || !rect.is_positive() {
        return Ok(None);
    }
    // Round inward so a partly clipped physical pixel is never an invisible
    // input target. These are GUI client-local coordinates: do not subtract R
    // or the parent's global desktop origin.
    let scale = f64::from(ppp);
    let left = (f64::from(rect.left()) * scale).ceil().max(0.0);
    let top = (f64::from(rect.top()) * scale).ceil().max(0.0);
    let right = (f64::from(rect.right()) * scale)
        .floor()
        .min(f64::from(parent.client.size().width()));
    let bottom = (f64::from(rect.bottom()) * scale)
        .floor()
        .min(f64::from(parent.client.size().height()));
    if right <= left || bottom <= top {
        return Ok(None);
    }
    if left > f64::from(i16::MAX)
        || top > f64::from(i16::MAX)
        || right - left > f64::from(u16::MAX)
        || bottom - top > f64::from(u16::MAX)
    {
        return Err("Visible drag button exceeds the native input-child coordinate limit.".into());
    }
    PhysicalRect::new(
        left as i32,
        top as i32,
        (right - left) as u32,
        (bottom - top) as u32,
    )
    .map(Some)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::Mutex,
        time::{Duration, Instant},
    };

    fn geometry() -> RecorderGeometry {
        RecorderGeometry::new(
            PhysicalRect::new(-200, -100, 400, 300).unwrap(),
            PhysicalRect::new(0, 0, 20, 10).unwrap(),
        )
        .unwrap()
    }

    fn complete(ui: &mut WindowSnapUi, geometry: &mut RecorderGeometry, stage: RecorderStage) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while ui.pending.is_some() {
            ui.poll(geometry, stage, false);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn source(id: &str, name: &str) -> CaptureSource {
        CaptureSource::new(
            gif_from_screen_capture::CaptureSourceId::new(id).unwrap(),
            name,
            CaptureSourceKind::Window,
            Some(PhysicalRect::new(1, 2, 3, 4).unwrap()),
            1.0,
        )
        .unwrap()
    }

    #[test]
    fn recorder_close_waits_for_picker_cleanup_instead_of_restoring_an_interactive_window_early() {
        let mut ui = WindowSnapUi::default();
        let mut geometry = geometry();
        let (release, wait) = mpsc::sync_channel(1);
        ui.start_work(geometry, move |_| {
            wait.recv().unwrap();
            Ok(WorkResult::Picked(None))
        })
        .unwrap();
        ui.pending.as_mut().unwrap().picking = true;
        assert!(!ui.request_close());
        assert!(ui.is_closing() && ui.is_picking() && ui.is_pending());
        assert!(!ui.close_ready());
        release.send(()).unwrap();
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert!(ui.close_ready());
    }

    #[test]
    fn cancelled_picker_stays_busy_until_native_cleanup_result_arrives() {
        let mut ui = WindowSnapUi::default();
        let mut geometry = geometry();
        let before = geometry;
        let (release, wait) = mpsc::sync_channel(1);
        ui.start_work(geometry, move |_| {
            wait.recv().unwrap();
            Ok(WorkResult::Picked(None))
        })
        .unwrap();
        ui.pending.as_mut().unwrap().picking = true;
        let context = egui::Context::default();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |context| ui.keyboard_control(context),
        );
        assert!(
            ui.pending
                .as_ref()
                .unwrap()
                .cancellation
                .load(Ordering::Acquire)
        );
        assert!(
            ui.is_picking() && ui.is_pending(),
            "cancel requested is not native cleanup completed"
        );
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert!(ui.is_picking());
        release.send(()).unwrap();
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert!(!ui.is_picking() && !ui.is_pending());
        assert_eq!(geometry, before);
    }

    #[test]
    fn picked_window_is_applied_once_and_out_of_source_results_keep_the_old_selection() {
        let mut ui = WindowSnapUi::default();
        ui.set_candidates(&[source("old", "Old window")]);
        let mut geometry = geometry();
        let chosen = PhysicalRect::new(-100, -50, 80, 60).unwrap();
        ui.start_work(geometry, move |_| {
            Ok(WorkResult::Picked(Some(PickedWindow {
                source: source("new", "Picked window"),
                region: chosen,
            })))
        })
        .unwrap();
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert_eq!(geometry.region(), chosen);
        assert_eq!(ui.candidates[ui.selected].id().as_str(), "new");
        let before = geometry;
        let candidates = ui.candidates.clone();
        ui.start_work(geometry, |_| {
            Ok(WorkResult::Picked(Some(PickedWindow {
                source: source("outside", "Outside"),
                region: PhysicalRect::new(199, 0, 20, 10).unwrap(),
            })))
        })
        .unwrap();
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert_eq!(geometry, before);
        assert_eq!(ui.candidates, candidates);
        assert!(ui.notice.as_ref().unwrap().contains("does not fit"));
    }

    #[test]
    fn refresh_preserves_window_identity_and_geometry_and_recovers_an_empty_list() {
        let mut ui = WindowSnapUi::default();
        ui.set_candidates(&[source("one", "Old name"), source("two", "Second")]);
        ui.selected = 1;
        let mut geometry = geometry();
        let before = geometry;
        ui.start_work(geometry, |_| {
            Ok(WorkResult::Catalog(WindowSnapCatalog {
                windows: vec![source("two", "Renamed"), source("three", "New")],
                truncated: false,
            }))
        })
        .unwrap();
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert_eq!(geometry, before);
        assert_eq!(ui.candidates[ui.selected].id().as_str(), "two");
        assert_eq!(ui.candidates[ui.selected].name(), "Renamed");
        ui.set_candidates(&[]);
        assert!(ui.available && ui.candidates.is_empty());
        ui.start_work(geometry, |_| {
            Ok(WorkResult::Catalog(WindowSnapCatalog {
                windows: vec![source("new", "Discovered later")],
                truncated: true,
            }))
        })
        .unwrap();
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert_eq!(geometry, before);
        assert_eq!(ui.candidates[0].name(), "Discovered later");
        assert!(ui.notice.as_ref().unwrap().contains("incomplete"));
    }

    #[test]
    fn snap_applies_exact_negative_physical_bounds_and_rejects_clipping_or_frozen_canvas() {
        let rect = PhysicalRect::new(-100, -50, 80, 60).unwrap();
        let mut geometry = geometry();
        let mut ui = WindowSnapUi::default();
        ui.start(geometry, move |_| Ok(rect)).unwrap();
        assert!(ui.is_pending());
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert_eq!(geometry.region(), rect);
        for rect in [
            PhysicalRect::new(-201, 0, 10, 10).unwrap(),
            PhysicalRect::new(199, 0, 2, 10).unwrap(),
            PhysicalRect::new(0, 0, 500, 10).unwrap(),
        ] {
            let before = geometry;
            ui.start(geometry, move |_| Ok(rect)).unwrap();
            complete(&mut ui, &mut geometry, RecorderStage::Ready);
            assert_eq!(geometry, before);
            assert!(
                ui.notice
                    .as_ref()
                    .unwrap()
                    .contains("original selection kept")
            );
        }
        geometry.freeze_size();
        let before = geometry;
        assert!(geometry.snap_to(rect).is_err());
        assert_eq!(geometry, before);
    }

    #[test]
    fn late_result_cannot_replace_a_changed_selection_stage_or_active_gesture() {
        for change in 0..3 {
            let mut geometry = geometry();
            let mut ui = WindowSnapUi::default();
            let (release, wait) = mpsc::sync_channel(1);
            ui.start(geometry, move |_| {
                wait.recv().unwrap();
                Ok(PhysicalRect::new(-100, -50, 80, 60).unwrap())
            })
            .unwrap();
            if change == 0 {
                geometry.move_by(1, 0);
            }
            let stage = if change == 1 {
                RecorderStage::Countdown(2)
            } else {
                RecorderStage::Ready
            };
            ui.poll(&mut geometry, stage, change == 2);
            assert!(!ui.is_pending());
            if change == 0 {
                geometry.move_by(-1, 0);
            }
            let before = geometry;
            release.send(()).unwrap();
            complete(&mut ui, &mut geometry, RecorderStage::Ready);
            assert_eq!(
                geometry, before,
                "even a move away and back must invalidate old work"
            );
        }
    }

    #[test]
    fn explicit_cancel_and_overlay_drop_cancel_native_work_without_waiting() {
        let mut geometry = geometry();
        let mut ui = WindowSnapUi::default();
        let (release, wait) = mpsc::sync_channel(1);
        ui.start(geometry, move |_| {
            wait.recv().unwrap();
            Ok(PhysicalRect::new(-100, -50, 80, 60).unwrap())
        })
        .unwrap();
        ui.cancel();
        assert!(!ui.is_pending());
        let before = geometry;
        release.send(()).unwrap();
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert_eq!(geometry, before);
        let (done, observed) = mpsc::sync_channel(1);
        ui.start(geometry, move |cancel| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !cancel.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            done.send(()).unwrap();
            Err("cancelled".into())
        })
        .unwrap();
        drop(ui);
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
    }

    #[derive(Default)]
    struct FakeDragState {
        requests: Vec<DragPickerRequest>,
        claimed: Option<u64>,
        active: Option<u64>,
        cancelled: bool,
        terminal:
            Option<Result<Option<gif_from_screen_capture_linux::DragPickerSelection>, String>>,
        claim_on_request: bool,
    }

    struct FakeDrag(Arc<Mutex<FakeDragState>>);

    impl DragHandle for FakeDrag {
        fn request(&self, request: DragPickerRequest) -> Result<(), String> {
            let mut shared = self.0.lock().unwrap();
            if shared.claim_on_request {
                shared.claimed = shared.requests.last().map(|request| request.generation);
                return Err("claimed during publish".into());
            }
            if shared.claimed.is_some() || shared.cancelled {
                return Err("not idle".into());
            }
            shared.requests.push(request);
            Ok(())
        }
        fn poll(&mut self) -> DragPickerUpdate {
            let mut shared = self.0.lock().unwrap();
            let result = shared.terminal.take();
            if result.is_some() {
                shared.claimed = None;
                shared.active = None;
            }
            DragPickerUpdate {
                ready: if result.is_none() {
                    shared.requests.last().map(|request| request.generation)
                } else {
                    None
                },
                claimed: shared.claimed,
                active: shared.active,
                running: result.is_none(),
                result,
            }
        }
        fn is_picking(&self) -> bool {
            self.0.lock().unwrap().active.is_some()
        }
        fn is_claimed(&self) -> bool {
            self.0.lock().unwrap().claimed.is_some()
        }
        fn cancelled(&self) -> bool {
            self.0.lock().unwrap().cancelled
        }
        fn stop(&self) {
            self.0.lock().unwrap().cancelled = true;
        }
    }

    fn forbidden_drag_start(_: u32) -> Result<Box<dyn DragHandle>, String> {
        panic!("unit tests must not connect to DISPLAY");
    }

    fn fake_drag() -> (WindowSnapUi, Arc<Mutex<FakeDragState>>) {
        let shared = Arc::new(Mutex::new(FakeDragState::default()));
        let mut ui = WindowSnapUi {
            native: Some(NativeDrag {
                handle: Box::new(FakeDrag(Arc::clone(&shared))),
                binding: None,
                ready: None,
            }),
            launch_drag: forbidden_drag_start,
            click_picker: |_, _| Ok(None),
            ..WindowSnapUi::default()
        };
        ui.set_candidates(&[]);
        (ui, shared)
    }

    fn parent() -> DragParent {
        DragParent {
            window_id: 42,
            client: PhysicalRect::new(800, -200, 400, 300).unwrap(),
        }
    }

    fn caption() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 25.0))
    }

    fn publish(
        ui: &mut WindowSnapUi,
        context: &egui::Context,
        geometry: RecorderGeometry,
        hit: Option<egui::Rect>,
        visible: bool,
    ) {
        let _ = context.run(
            egui::RawInput {
                focused: true,
                ..egui::RawInput::default()
            },
            |context| {
                ui.begin_frame(Some(parent()), 1.0);
                ui.drawn.hit = hit;
                ui.drawn.enabled = hit.is_some();
                ui.publish_drag_button(geometry, RecorderStage::Ready, visible, context);
            },
        );
    }

    fn picked(generation: u64) -> gif_from_screen_capture_linux::DragPickerSelection {
        gif_from_screen_capture_linux::DragPickerSelection {
            generation,
            picked: Some(PickedWindow {
                source: source("native-picked", "Native picked"),
                region: PhysicalRect::new(-100, -50, 80, 60).unwrap(),
            }),
        }
    }

    #[test]
    fn client_local_hit_rounds_inward_and_never_uses_parent_desktop_or_capture_origin() {
        let layout = egui::Rect::from_min_max(egui::pos2(10.2, 20.2), egui::pos2(50.8, 40.8));
        assert_eq!(
            physical_hit(Some(layout), parent(), 1.25).unwrap(),
            Some(PhysicalRect::new(13, 26, 50, 24).unwrap())
        );
        let clipped = egui::Rect::from_min_max(egui::pos2(-10.0, -20.0), egui::pos2(20.0, 30.0));
        assert_eq!(
            physical_hit(Some(clipped), parent(), 2.0).unwrap(),
            Some(PhysicalRect::new(0, 0, 40, 60).unwrap())
        );
        let outside = egui::Rect::from_min_size(egui::pos2(500.0, 600.0), egui::vec2(1.0, 1.0));
        assert_eq!(physical_hit(Some(outside), parent(), 1.0).unwrap(), None);
        assert!(physical_hit(Some(layout), parent(), f32::NAN).is_err());
    }

    #[test]
    fn idle_armed_child_does_not_block_start_but_close_waits_for_terminal_cleanup() {
        let (mut ui, shared) = fake_drag();
        let mut geometry = geometry();
        publish(
            &mut ui,
            &egui::Context::default(),
            geometry,
            Some(caption()),
            true,
        );
        assert!(!ui.is_pending() && !ui.is_picking());
        assert!(!ui.request_close());
        assert!(shared.lock().unwrap().cancelled);
        assert!(!ui.close_ready());
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert!(!ui.close_ready(), "stop flag is not a cleanup receipt");
        shared.lock().unwrap().terminal = Some(Ok(None));
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert!(ui.close_ready());
    }

    #[test]
    fn claimed_handoff_blocks_start_without_hiding_or_replacing_its_button_request() {
        let (mut ui, shared) = fake_drag();
        let geometry = geometry();
        let context = egui::Context::default();
        publish(&mut ui, &context, geometry, Some(caption()), true);
        shared.lock().unwrap().claimed = Some(1);
        assert!(ui.is_pending());
        assert!(
            !ui.is_picking(),
            "the controller must stay visible before the root grab"
        );
        // Disabling the egui button in the pending UI must not remove its input child.
        publish(&mut ui, &context, geometry, Some(caption()), true);
        assert_eq!(shared.lock().unwrap().requests.len(), 1);
        assert!(!shared.lock().unwrap().cancelled);
        shared.lock().unwrap().active = Some(1);
        assert!(ui.is_picking());
        publish(&mut ui, &context, geometry, None, false);
        assert_eq!(shared.lock().unwrap().requests.len(), 1);
        assert!(!shared.lock().unwrap().cancelled);
    }

    #[test]
    fn every_idle_layout_bounds_or_target_change_gets_a_new_generation_and_fold_hides_once() {
        let (mut ui, shared) = fake_drag();
        let mut geometry = geometry();
        let context = egui::Context::default();
        publish(&mut ui, &context, geometry, Some(caption()), true);
        publish(&mut ui, &context, geometry, Some(caption()), true);
        assert_eq!(shared.lock().unwrap().requests.len(), 1);
        ui.bounds = WindowSnapBounds::Client;
        publish(&mut ui, &context, geometry, Some(caption()), true);
        geometry.move_by(1, 0);
        publish(&mut ui, &context, geometry, Some(caption()), true);
        publish(
            &mut ui,
            &context,
            geometry,
            Some(caption().translate(egui::vec2(0.0, 3.0))),
            true,
        );
        publish(&mut ui, &context, geometry, None, true);
        publish(&mut ui, &context, geometry, None, true);
        let shared = shared.lock().unwrap();
        assert_eq!(
            shared
                .requests
                .iter()
                .map(|request| request.generation)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        assert_eq!(shared.requests[4].rect, None);
        assert_eq!(shared.requests[0].parent_size, parent().client.size());
    }

    #[test]
    fn stale_claimed_selection_cannot_overwrite_a_moved_away_and_back_target() {
        let (mut ui, shared) = fake_drag();
        let mut geometry = geometry();
        let context = egui::Context::default();
        publish(&mut ui, &context, geometry, Some(caption()), true);
        shared.lock().unwrap().claimed = Some(1);
        geometry.move_by(1, 0);
        publish(&mut ui, &context, geometry, None, true);
        assert!(shared.lock().unwrap().cancelled);
        geometry.move_by(-1, 0);
        let before = geometry;
        shared.lock().unwrap().terminal = Some(Ok(Some(picked(1))));
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert_eq!(geometry, before);
        assert!(ui.native.is_none());
    }

    #[test]
    fn active_native_result_is_applied_once_only_after_the_cleanup_receipt() {
        let (mut ui, shared) = fake_drag();
        let mut geometry = geometry();
        publish(
            &mut ui,
            &egui::Context::default(),
            geometry,
            Some(caption()),
            true,
        );
        let before = geometry;
        {
            let mut shared = shared.lock().unwrap();
            shared.claimed = Some(1);
            shared.active = Some(1);
        }
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert_eq!(geometry, before);
        assert!(ui.is_picking());
        shared.lock().unwrap().terminal = Some(Ok(Some(picked(1))));
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert_eq!(
            geometry.region(),
            PhysicalRect::new(-100, -50, 80, 60).unwrap()
        );
        assert_eq!(ui.candidates.len(), 1);
        assert!(!ui.is_picking());
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert_eq!(ui.candidates.len(), 1);
    }

    #[test]
    fn keyboard_click_fallback_waits_for_the_idle_drag_child_to_close() {
        let (mut ui, shared) = fake_drag();
        let mut geometry = geometry();
        ui.click_picker = |_, _| {
            Ok(Some(PickedWindow {
                source: source("keyboard-picked", "Keyboard picked"),
                region: PhysicalRect::new(-100, -50, 80, 60).unwrap(),
            }))
        };
        publish(
            &mut ui,
            &egui::Context::default(),
            geometry,
            Some(caption()),
            true,
        );
        ui.start_picker(geometry).unwrap();
        assert!(ui.queued_picker.is_some() && ui.pending.is_none());
        assert!(ui.is_pending() && ui.is_picking());
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert!(ui.pending.is_none());
        shared.lock().unwrap().terminal = Some(Ok(None));
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        complete(&mut ui, &mut geometry, RecorderStage::Ready);
        assert_eq!(ui.candidates[ui.selected].id().as_str(), "keyboard-picked");
    }

    #[test]
    fn cancelling_queued_click_keeps_controls_hidden_until_idle_child_cleanup() {
        let (mut ui, shared) = fake_drag();
        let mut geometry = geometry();
        ui.click_picker = |_, _| panic!("cancelled click must never start the native picker");
        publish(
            &mut ui,
            &egui::Context::default(),
            geometry,
            Some(caption()),
            true,
        );
        ui.start_picker(geometry).unwrap();
        ui.cancel();
        assert!(ui.is_picking());
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert!(ui.is_picking());
        shared.lock().unwrap().terminal = Some(Ok(None));
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert!(!ui.is_picking() && ui.pending.is_none());
    }

    #[test]
    fn setup_failure_is_sticky_and_does_not_relaunch_until_explicit_retry() {
        let mut ui = WindowSnapUi {
            launch_drag: |_| Err("injected setup failure".into()),
            ..WindowSnapUi::default()
        };
        ui.set_candidates(&[]);
        let context = egui::Context::default();
        publish(&mut ui, &context, geometry(), Some(caption()), true);
        assert!(ui.drag_failed && ui.notice.as_ref().unwrap().contains("Click-to-pick"));
        ui.launch_drag = forbidden_drag_start;
        publish(&mut ui, &context, geometry(), Some(caption()), true);
        publish(&mut ui, &context, geometry(), None, true);
        assert!(ui.native.is_none());
        ui.drag_failed = false;
        ui.launch_drag = |_| Err("explicit retry attempted".into());
        publish(&mut ui, &context, geometry(), Some(caption()), true);
        assert!(
            ui.notice
                .as_ref()
                .unwrap()
                .contains("explicit retry attempted")
        );
    }

    #[test]
    fn press_claimed_during_publish_never_acquires_the_new_generation() {
        let (mut ui, shared) = fake_drag();
        let mut geometry = geometry();
        let context = egui::Context::default();
        publish(&mut ui, &context, geometry, Some(caption()), true);
        shared.lock().unwrap().claim_on_request = true;
        geometry.move_by(1, 0);
        publish(&mut ui, &context, geometry, Some(caption()), true);
        assert!(shared.lock().unwrap().cancelled);
        assert_eq!(
            ui.native
                .as_ref()
                .unwrap()
                .binding
                .unwrap()
                .request
                .generation,
            1
        );
        let before = geometry;
        shared.lock().unwrap().terminal = Some(Ok(Some(picked(1))));
        ui.poll(&mut geometry, RecorderStage::Ready, false);
        assert_eq!(geometry, before);
    }

    #[test]
    fn only_the_final_egui_layout_pass_can_publish_once_per_frame() {
        let (mut ui, shared) = fake_drag();
        let context = egui::Context::default();
        let _ = context.run(egui::RawInput::default(), |context| {
            ui.begin_frame(Some(parent()), 1.0);
            let first_pass = context.current_pass_index() == 0;
            ui.drawn.hit = Some(if first_pass {
                caption()
            } else {
                caption().translate(egui::vec2(20.0, 0.0))
            });
            ui.drawn.enabled = true;
            if first_pass {
                context.request_discard("test provisional picker button layout");
            }
            ui.publish_drag_button(geometry(), RecorderStage::Ready, true, context);
            // Even an accidental second shell call cannot publish a second
            // different native hit rectangle from the same completed frame.
            ui.drawn.hit = Some(caption().translate(egui::vec2(40.0, 0.0)));
            ui.publish_drag_button(geometry(), RecorderStage::Ready, true, context);
        });
        let shared = shared.lock().unwrap();
        assert_eq!(shared.requests.len(), 1);
        assert_eq!(
            shared.requests[0].rect,
            Some(PhysicalRect::new(30, 20, 100, 25).unwrap())
        );
    }

    #[test]
    fn parent_scale_and_bounds_changes_cancel_claimed_results_before_application() {
        for change in 0..5 {
            let (mut ui, shared) = fake_drag();
            let mut geometry = geometry();
            publish(
                &mut ui,
                &egui::Context::default(),
                geometry,
                Some(caption()),
                true,
            );
            shared.lock().unwrap().claimed = Some(1);
            let mut changed_parent = parent();
            let mut scale = 1.0;
            match change {
                0 => changed_parent.window_id += 1,
                1 => changed_parent.client = PhysicalRect::new(801, -200, 400, 300).unwrap(),
                2 => changed_parent.client = PhysicalRect::new(800, -200, 401, 300).unwrap(),
                3 => scale = 1.25,
                _ => ui.bounds = WindowSnapBounds::Outer,
            }
            ui.begin_frame(Some(changed_parent), scale);
            let before = geometry;
            ui.poll(&mut geometry, RecorderStage::Ready, false);
            assert!(shared.lock().unwrap().cancelled);
            shared.lock().unwrap().terminal = Some(Ok(Some(picked(1))));
            ui.poll(&mut geometry, RecorderStage::Ready, false);
            assert_eq!(geometry, before);
            assert!(ui.candidates.is_empty());
        }
    }

    #[test]
    fn actual_egui_button_clip_is_collected_and_collapsing_removes_the_idle_hit_target() {
        let (mut snap, shared) = fake_drag();
        let context = egui::Context::default();
        context.style_mut(|style| style.animation_time = 0.0);
        let closed = widget_frame(&context, &mut snap, Vec::new());
        let header = closed
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == "Fit region to a window…"
                {
                    Some(text.pos + text.galley.size() * 0.5)
                } else {
                    None
                }
            })
            .unwrap();
        click_header(&context, &mut snap, header);
        widget_frame(&context, &mut snap, Vec::new());
        let hit = snap
            .drawn
            .hit
            .expect("expanded native picker caption is visible");
        assert!(hit.right() <= 180.0 && hit.bottom() <= 120.0);
        assert_eq!(
            shared.lock().unwrap().requests.last().unwrap().rect,
            physical_hit(Some(hit), parent(), 1.0).unwrap()
        );
        let requests_before_ready = shared.lock().unwrap().requests.len();
        snap.poll(&mut geometry(), RecorderStage::Ready, false);
        assert!(snap.drag_ready());
        let ready = widget_frame(&context, &mut snap, Vec::new());
        assert!(ready.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Drag ready; or click to pick.")));
        assert_eq!(snap.drawn.hit, Some(hit));
        assert_eq!(shared.lock().unwrap().requests.len(), requests_before_ready);
        let hidden_before = shared
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.rect.is_none())
            .count();
        click_header(&context, &mut snap, header);
        widget_frame(&context, &mut snap, Vec::new());
        widget_frame(&context, &mut snap, Vec::new());
        assert!(snap.drawn.hit.is_none());
        let shared = shared.lock().unwrap();
        assert!(shared.requests.iter().any(|request| request.rect.is_some()));
        assert_eq!(shared.requests.last().unwrap().rect, None);
        assert_eq!(
            shared
                .requests
                .iter()
                .filter(|request| request.rect.is_none())
                .count(),
            hidden_before + 1
        );
    }

    #[test]
    fn disabled_claimed_button_keeps_its_visible_rect_but_folding_cancels_the_claim() {
        let (mut snap, shared) = fake_drag();
        let context = egui::Context::default();
        context.style_mut(|style| style.animation_time = 0.0);
        let first = widget_frame(&context, &mut snap, Vec::new());
        let header = first
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == "Fit region to a window…"
                {
                    Some(text.pos + text.galley.size() * 0.5)
                } else {
                    None
                }
            })
            .unwrap();
        click_header(&context, &mut snap, header);
        widget_frame(&context, &mut snap, Vec::new());
        let original_hit = snap.drawn.hit;
        let generation = shared.lock().unwrap().requests.last().unwrap().generation;
        shared.lock().unwrap().claimed = Some(generation);
        widget_frame(&context, &mut snap, Vec::new());
        assert!(!snap.drawn.enabled);
        assert_eq!(snap.drawn.hit, original_hit);
        assert!(!shared.lock().unwrap().cancelled);
        let requests = shared.lock().unwrap().requests.len();
        click_header(&context, &mut snap, header);
        widget_frame(&context, &mut snap, Vec::new());
        assert!(snap.drawn.hit.is_none());
        assert!(shared.lock().unwrap().cancelled);
        assert_eq!(shared.lock().unwrap().requests.len(), requests);
        let mut geometry = geometry();
        let before = geometry;
        shared.lock().unwrap().terminal = Some(Ok(Some(picked(generation))));
        snap.poll(&mut geometry, RecorderStage::Ready, false);
        assert_eq!(geometry, before);
    }

    #[test]
    fn actual_controller_busy_warning_never_moves_the_claimed_native_button() {
        let (mut snap, shared) = fake_drag();
        let context = egui::Context::default();
        context.style_mut(|style| style.animation_time = 0.0);
        let mut geometry = geometry();
        let mut settings = crate::RecordingSettings::default();
        let first = controller_frame(
            &context,
            &mut snap,
            &mut geometry,
            &mut settings,
            Vec::new(),
        );
        let header = first
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == "Fit region to a window…"
                {
                    Some(text.pos + text.galley.size() * 0.5)
                } else {
                    None
                }
            })
            .unwrap();
        for pressed in [true, false] {
            controller_frame(
                &context,
                &mut snap,
                &mut geometry,
                &mut settings,
                vec![
                    egui::Event::PointerMoved(header),
                    egui::Event::PointerButton {
                        pos: header,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        controller_frame(
            &context,
            &mut snap,
            &mut geometry,
            &mut settings,
            Vec::new(),
        );
        let hit = snap
            .drawn
            .hit
            .expect("real controller exposes the drag button");
        let count = shared.lock().unwrap().requests.len();
        let generation = shared.lock().unwrap().requests.last().unwrap().generation;
        shared.lock().unwrap().claimed = Some(generation);
        assert!(snap.is_pending() && !snap.is_picking());
        controller_frame(
            &context,
            &mut snap,
            &mut geometry,
            &mut settings,
            Vec::new(),
        );
        assert_eq!(snap.drawn.hit, Some(hit));
        assert!(!shared.lock().unwrap().cancelled);
        // Root active can arrive after the shell sampled hide_pixels=false.
        // Exercise that one still-visible frame through the actual whole UI.
        shared.lock().unwrap().active = Some(generation);
        controller_frame(
            &context,
            &mut snap,
            &mut geometry,
            &mut settings,
            Vec::new(),
        );
        assert_eq!(snap.drawn.hit, Some(hit));
        assert!(!shared.lock().unwrap().cancelled);
        assert_eq!(shared.lock().unwrap().requests.len(), count);
    }

    fn controller_frame(
        context: &egui::Context,
        snap: &mut WindowSnapUi,
        geometry: &mut RecorderGeometry,
        settings: &mut crate::RecordingSettings,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                focused: true,
                events,
                ..egui::RawInput::default()
            },
            |context| {
                snap.begin_frame(Some(parent()), context.pixels_per_point());
                // All other geometry gates are acknowledged; this is the actual
                // snap-busy part of RecorderOverlay::ready, not a fixed-layout stub.
                let input_ready = !snap.is_pending();
                crate::x11_controller_ui::draw(
                    context,
                    geometry,
                    RecorderStage::Ready,
                    None,
                    settings,
                    None,
                    input_ready,
                    snap,
                );
                snap.publish_drag_button(*geometry, RecorderStage::Ready, true, context);
            },
        )
    }

    fn widget_frame(
        context: &egui::Context,
        snap: &mut WindowSnapUi,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                focused: true,
                events,
                ..egui::RawInput::default()
            },
            |context| {
                snap.begin_frame(Some(parent()), context.pixels_per_point());
                egui::CentralPanel::default().show(context, |ui| {
                    ui.set_clip_rect(ui.clip_rect().intersect(egui::Rect::from_min_max(
                        egui::Pos2::ZERO,
                        egui::pos2(180.0, 120.0),
                    )));
                    snap.show(ui, &geometry(), RecorderStage::Ready);
                });
                snap.publish_drag_button(geometry(), RecorderStage::Ready, true, context);
            },
        )
    }

    fn click_header(context: &egui::Context, snap: &mut WindowSnapUi, point: egui::Pos2) {
        for pressed in [true, false] {
            widget_frame(
                context,
                snap,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
}
