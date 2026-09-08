//! Ready-only, cancellable snap work owned by one recorder overlay lifetime.

use crate::{RecorderStage, recorder_geometry::RecorderGeometry};
use eframe::egui;
use gif_from_screen_capture::{CaptureSource, CaptureSourceKind, PhysicalRect};
use gif_from_screen_capture_linux::{
    WindowSnapBounds, WindowSnapCatalog, list_snap_windows, query_window_snap,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
};

#[derive(Default)]
pub(crate) struct WindowSnapUi {
    available: bool,
    candidates: Vec<CaptureSource>,
    selected: usize,
    bounds: WindowSnapBounds,
    pending: Option<Pending>,
    notice: Option<String>,
}

struct Pending {
    geometry: RecorderGeometry,
    cancellation: Arc<AtomicBool>,
    receiver: Receiver<Result<WorkResult, String>>,
}

enum WorkResult {
    Region(PhysicalRect),
    Catalog(WindowSnapCatalog),
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
        self.pending
            .as_ref()
            .is_some_and(|pending| !pending.cancellation.load(Ordering::Acquire))
    }

    pub(crate) fn cancel(&mut self) {
        if let Some(pending) = &self.pending {
            pending.cancellation.store(true, Ordering::Release);
        }
    }

    pub(crate) fn poll(
        &mut self,
        geometry: &mut RecorderGeometry,
        stage: RecorderStage,
        dragging: bool,
    ) {
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
        self.notice = Some(match result {
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
            ui.label("Read the chosen window's current physical bounds. It must fit completely inside the selected screen.");
            ui.add_enabled_ui(self.pending.is_none(), |ui| {
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
                    ui.selectable_value(&mut self.bounds, WindowSnapBounds::WindowFrame, "Window frame");
                    ui.selectable_value(&mut self.bounds, WindowSnapBounds::Client, "Client area");
                    ui.selectable_value(&mut self.bounds, WindowSnapBounds::Outer, "Native bounds");
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
            if self.pending.is_some() {
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

    fn start(
        &mut self,
        geometry: RecorderGeometry,
        loader: impl FnOnce(&AtomicBool) -> Result<PhysicalRect, String> + Send + 'static,
    ) -> Result<(), String> {
        self.start_work(geometry, move |cancel| {
            loader(cancel).map(WorkResult::Region)
        })
    }

    fn start_work(
        &mut self,
        geometry: RecorderGeometry,
        loader: impl FnOnce(&AtomicBool) -> Result<WorkResult, String> + Send + 'static,
    ) -> Result<(), String> {
        if self.pending.is_some() {
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
        });
        self.notice = Some("Reading windows…".into());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

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
}
