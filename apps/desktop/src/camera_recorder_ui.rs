//! Self-contained camera page; no device is opened until an explicit button press.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use gif_from_screen_application::{
    CameraCaptureOptions, CameraCaptureProgress, CameraControl, CameraDevice, CameraPhase,
    LiveRecordingOptions, enumerate_camera_devices, run_camera_capture,
};
use gif_from_screen_domain::{PhysicalSize, ProjectId, SourceProvenance, UnixTimeMs};
use gif_from_screen_project::ActiveProject;

use crate::background_task::BackgroundTask;

pub(crate) struct CameraRecorderUi {
    devices: Vec<CameraDevice>,
    selected: usize,
    width: u32,
    height: u32,
    fps: u32,
    target: String,
    settings_open: bool,
    control: Option<CameraControl>,
    task: BackgroundTask<Option<ActiveProject>, CameraCaptureProgress>,
    texture: Option<egui::TextureHandle>,
    texture_sequence: u64,
    notice: Option<String>,
}

impl Default for CameraRecorderUi {
    fn default() -> Self {
        let (devices, notice) = match enumerate_camera_devices() {
            Ok(devices) => (devices, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        Self {
            devices,
            selected: 0,
            width: 640,
            height: 480,
            fps: 15,
            target: String::new(),
            settings_open: false,
            control: None,
            task: BackgroundTask::default(),
            texture: None,
            texture_sequence: 0,
            notice,
        }
    }
}

impl Drop for CameraRecorderUi {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl CameraRecorderUi {
    pub(crate) fn is_active(&self) -> bool {
        self.task.is_running()
    }

    pub(crate) fn has_preview(&self) -> bool {
        self.is_active() && (self.texture.is_some() || self.task.progress().is_some())
    }

    /// Only an explicit Discard action calls this method.
    pub(crate) fn cancel(&self) {
        if let Some(control) = &self.control {
            control.discard();
        }
    }

    /// Closing the app or page stops the device and preserves recorded frames.
    pub(crate) fn shutdown(&self) {
        if let Some(control) = &self.control {
            control.stop();
        }
    }

    pub(crate) fn poll(&mut self) -> Option<Result<ActiveProject, String>> {
        let result = self.task.poll()?;
        let discarded = self
            .control
            .as_ref()
            .is_some_and(|control| control.phase() == CameraPhase::Discarding);
        self.control = None;
        self.texture = None;
        match result {
            Ok(Some(project)) => Some(Ok(project)),
            Ok(None) => {
                self.notice = Some(
                    if discarded {
                        "Recording discarded."
                    } else {
                        "Camera preview closed."
                    }
                    .to_owned(),
                );
                None
            }
            Err(error) => {
                self.notice = Some(error.clone());
                Some(Err(error))
            }
        }
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Camera recorder");
            ui.toggle_value(&mut self.settings_open, "Settings");
        });
        self.show_controls(ui);
        let progress = if self.is_active() {
            self.task.progress()
        } else {
            None
        };
        let details_height = if progress.is_some() {
            ui.available_height() * 0.45
        } else {
            ui.available_height()
        };
        egui::ScrollArea::vertical()
            .id_salt("camera-details")
            .max_height(details_height.max(1.0))
            .auto_shrink([false, true])
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: false,
                ..egui::scroll_area::ScrollSource::ALL
            })
            .show(ui, |ui| self.show_details(ui, progress.as_ref()));
        if let Some(progress) = progress {
            self.show_preview(ui, &progress);
        }
        if self.is_active() {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }

    fn show_details(&mut self, ui: &mut egui::Ui, progress: Option<&CameraCaptureProgress>) {
        self.show_configuration(ui);
        if let Some(control) = &self.control {
            ui.weak(match control.phase() {
                CameraPhase::Preview => "Preview only — no frames are being saved.",
                CameraPhase::Recording => "Recording — stop to open the editor.",
                CameraPhase::Paused => {
                    "Paused — preview is live; paused time is excluded from the GIF."
                }
                CameraPhase::Finishing => "Saving frames and releasing the camera…",
                CameraPhase::Discarding => "Discarding this recording and releasing the camera…",
            });
        }
        if let Some(recording) = progress.and_then(|progress| progress.recording) {
            ui.label(format!(
                "{} saved frames · {}.{:02} seconds · {} frames skipped under load",
                recording.frames,
                recording.duration_us / 1_000_000,
                (recording.duration_us % 1_000_000) / 10_000,
                progress.map_or(0, |progress| progress.dropped_frames)
            ));
            if recording.limit_reached {
                ui.weak("The safety limit was reached. Saving captured frames…");
            }
        } else if progress.is_none() && self.is_active() {
            ui.horizontal_wrapped(|ui| {
                ui.spinner();
                ui.label("Opening camera…");
            });
        }
        if let Some(notice) = &self.notice {
            ui.label(notice);
        }
    }

    fn show_configuration(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(!self.is_active(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Camera");
                crate::appearance::combo(ui, "camera-device")
                    .selected_text(self.devices.get(self.selected).map_or("No camera detected", |device| device.name.as_str()))
                    .show_ui(ui, |ui| {
                        for (index, device) in self.devices.iter().enumerate() {
                            ui.selectable_value(&mut self.selected, index, format!("{} · {}", device.name, device.path.display()));
                        }
                    });
                if ui.button("Refresh").clicked() {
                    match enumerate_camera_devices() {
                        Ok(devices) => { self.devices = devices; self.selected = 0; self.notice = None; }
                        Err(error) => self.notice = Some(error),
                    }
                }
            });
            if self.settings_open {
                ui.horizontal_wrapped(|ui| {
                    ui.add(egui::DragValue::new(&mut self.width).range(1..=4096).prefix("Width "));
                    ui.add(egui::DragValue::new(&mut self.height).range(1..=4096).prefix("Height "));
                    ui.add(egui::DragValue::new(&mut self.fps).range(1..=60).suffix(" fps"));
                });
                ui.label("New project");
                ui.add(egui::TextEdit::singleline(&mut self.target).desired_width(ui.available_width()).hint_text("Leave empty for a new camera recording in this folder"));
                ui.weak("Choose a mode supported by the camera. Up to 10,000 frames / 2 GiB of raw pixels; existing projects are never replaced. Audio is not captured.");
            }
        });
        if self.devices.is_empty() {
            ui.weak("Connect a V4L2 camera, then choose Refresh.");
        } else if !self.is_active() {
            ui.weak("The camera stays off until you choose Preview camera or Record.");
        }
    }

    fn show_controls(&mut self, ui: &mut egui::Ui) {
        if !self.is_active() {
            ui.add_enabled_ui(!self.devices.is_empty(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Preview camera").clicked()
                        && let Err(error) = self.start(false)
                    {
                        self.notice = Some(error);
                    }
                    if ui.button("Record").clicked()
                        && let Err(error) = self.start(true)
                    {
                        self.notice = Some(error);
                    }
                });
            });
            return;
        }
        let Some(control) = self.control.clone() else {
            return;
        };
        let phase = control.phase();
        ui.horizontal_wrapped(|ui| {
            let action = match phase {
                CameraPhase::Preview => ui
                    .button("Start recording")
                    .clicked()
                    .then(|| control.record()),
                CameraPhase::Recording => ui.button("Pause").clicked().then(|| control.pause()),
                CameraPhase::Paused => ui.button("Resume").clicked().then(|| control.record()),
                CameraPhase::Finishing | CameraPhase::Discarding => {
                    ui.spinner();
                    ui.label("Closing camera and finishing…");
                    None
                }
            };
            if let Some(Err(error)) = action {
                self.notice = Some(error);
            }
            if matches!(
                phase,
                CameraPhase::Preview | CameraPhase::Recording | CameraPhase::Paused
            ) {
                if ui
                    .button(if phase == CameraPhase::Preview {
                        "Close preview"
                    } else {
                        "Stop & edit"
                    })
                    .clicked()
                {
                    control.stop();
                }
                if phase != CameraPhase::Preview && ui.button("Discard").clicked() {
                    self.cancel();
                }
            }
        });
    }

    fn show_preview(&mut self, ui: &mut egui::Ui, progress: &CameraCaptureProgress) {
        let frame = &progress.preview;
        if self.texture.is_none() || frame.sequence != self.texture_sequence {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [
                    frame.size.width.get() as usize,
                    frame.size.height.get() as usize,
                ],
                &frame.pixels,
            );
            if let Some(texture) = &mut self.texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.texture = Some(ui.ctx().load_texture(
                    "camera-preview",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
            self.texture_sequence = frame.sequence;
        }
        if let Some(texture) = &self.texture {
            let size = egui::vec2(
                f32::from(u16::try_from(frame.size.width.get()).unwrap_or(u16::MAX)),
                f32::from(u16::try_from(frame.size.height.get()).unwrap_or(u16::MAX)),
            );
            let available = ui.available_size();
            let size = fit_preview(size, available);
            if size.x > 0.0 && size.y > 0.0 {
                ui.image((texture.id(), size));
            }
        }
    }

    fn start(&mut self, record: bool) -> Result<(), String> {
        let options = self.options()?;
        let control = CameraControl::default();
        if record {
            control.record()?;
        }
        let worker_control = control.clone();
        self.task.start("gfs-camera-recorder", move |context| {
            run_camera_capture(
                options,
                &worker_control,
                context.cancellation(),
                |progress| context.report(progress),
            )
        })?;
        self.control = Some(control);
        self.texture = None;
        self.texture_sequence = 0;
        self.notice = None;
        Ok(())
    }

    fn options(&self) -> Result<CameraCaptureOptions, String> {
        let device = self
            .devices
            .get(self.selected)
            .cloned()
            .ok_or_else(|| "Connect and select a camera first.".to_owned())?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?;
        let project_path = if self.target.trim().is_empty() {
            std::env::current_dir()
                .map_err(|error| error.to_string())?
                .join(format!("camera-{}.gfsproj", now.as_millis()))
        } else {
            PathBuf::from(self.target.trim())
        };
        if project_path
            .extension()
            .is_none_or(|extension| extension != "gfsproj")
        {
            return Err("The new camera project must end in .gfsproj.".to_owned());
        }
        let canvas =
            PhysicalSize::new(self.width, self.height).map_err(|error| error.to_string())?;
        Ok(CameraCaptureOptions {
            recording: LiveRecordingOptions {
                project_path,
                canvas,
                project_id: ProjectId::from_u128(uuid::Uuid::new_v4().as_u128()),
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                created_at: UnixTimeMs::new(
                    i64::try_from(now.as_millis()).map_err(|error| error.to_string())?,
                ),
                provenance: SourceProvenance::Camera {
                    device_label: Some(device.name.clone()),
                },
                provisional_duration_us: 1_000_000 / u64::from(self.fps.max(1)),
                max_frames: 10_000,
                max_frame_bytes_total: 2 * 1024 * 1024 * 1024,
            },
            device,
            fps: self.fps,
        })
    }
}

/// Fill the available media area while preserving the entire camera frame.
fn fit_preview(native: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    let width = available.x.max(0.0);
    let height = if available.y.is_finite() {
        available.y.max(0.0)
    } else {
        native.y * width / native.x
    };
    native * (width / native.x).min(height / native.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(sequence: u64) -> CameraCaptureProgress {
        CameraCaptureProgress {
            preview: gif_from_screen_application::CameraPreviewFrame {
                sequence,
                size: PhysicalSize::new(160, 90).unwrap(),
                pixels: vec![255; 160 * 90 * 4].into(),
            },
            recording: None,
            dropped_frames: 0,
        }
    }

    fn main_page_frame(
        app: &mut crate::GifFromScreenApp,
        viewport: egui::Rect,
        zoom: f32,
    ) -> egui::FullOutput {
        let context = egui::Context::default();
        crate::appearance::configure(&context);
        crate::preferences::fonts::install(&context);
        context.set_zoom_factor(zoom);
        context.style_mut(|style| style.animation_time = 0.0);
        let frame = || {
            let mut input = egui::RawInput {
                screen_rect: Some(viewport),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(1.0);
            input
        };
        let _ = context.run(frame(), |context| app.show_main_view(context));
        context.run(frame(), |context| app.show_main_view(context))
    }

    #[test]
    fn main_camera_page_keeps_live_controls_visible_and_fills_the_media_viewport() {
        for settings_open in [false, true] {
            for phase in [
                CameraPhase::Preview,
                CameraPhase::Recording,
                CameraPhase::Paused,
            ] {
                let mut app = crate::GifFromScreenApp::default();
                app.view = crate::AppView::CameraRecorder;
                app.camera_recorder.settings_open = settings_open;
                app.camera_recorder.notice = None;
                let control = CameraControl::default();
                if phase != CameraPhase::Preview {
                    control.record().unwrap();
                }
                if phase == CameraPhase::Paused {
                    control.pause().unwrap();
                }
                app.camera_recorder.control = Some(control);
                let (release, wait) = std::sync::mpsc::channel();
                app.camera_recorder
                    .task
                    .start("synthetic-camera-layout", move |context| {
                        context.report(preview(0));
                        let _ = wait.recv_timeout(Duration::from_secs(10));
                        Ok(None)
                    })
                    .unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !app.camera_recorder.has_preview() {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                }
                for (native_size, zoom) in [
                    (egui::vec2(400.0, 320.0), 1.0),
                    (egui::vec2(640.0, 480.0), 1.0),
                    (egui::vec2(960.0, 720.0), 1.5),
                    (egui::vec2(1280.0, 960.0), 2.0),
                    (egui::vec2(1920.0, 1080.0), 1.0),
                ] {
                    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, native_size / zoom);
                    let output = main_page_frame(&mut app, viewport, zoom);
                    let labels: &[&str] = match phase {
                        CameraPhase::Preview => &["Start recording", "Close preview"],
                        CameraPhase::Recording => &["Pause", "Stop & edit", "Discard"],
                        CameraPhase::Paused => &["Resume", "Stop & edit", "Discard"],
                        _ => unreachable!(),
                    };
                    for label in labels {
                        let shape = output.shapes.iter().find(|shape| matches!(
                            &shape.shape, egui::Shape::Text(text) if text.galley.text() == *label
                        )).unwrap_or_else(|| panic!("missing camera control {label}"));
                        let egui::Shape::Text(text) = &shape.shape else {
                            unreachable!()
                        };
                        let bounds = text.galley.rect.translate(text.pos.to_vec2());
                        assert!(
                            viewport.contains_rect(bounds),
                            "{label} outside {viewport:?}: {bounds:?}"
                        );
                        assert!(
                            shape.clip_rect.contains_rect(bounds),
                            "{label} clipped: {bounds:?}"
                        );
                    }
                    let texture = app.camera_recorder.texture.as_ref().unwrap().id();
                    let bounds = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Rect(rect)
                                if rect
                                    .brush
                                    .as_ref()
                                    .is_some_and(|brush| brush.fill_texture_id == texture) =>
                            {
                                Some(rect.rect)
                            }
                            _ => None,
                        })
                        .expect("main route must paint the camera frame");
                    assert!(
                        viewport.shrink(7.0).contains_rect(bounds),
                        "preview escaped {viewport:?}: {bounds:?}"
                    );
                    assert!((bounds.width() / bounds.height() - 160.0 / 90.0).abs() < 0.001);
                    assert!(
                        (bounds.right() - (viewport.right() - 8.0)).abs() < 1.5
                            || (bounds.bottom() - (viewport.bottom() - 8.0)).abs() < 1.5,
                        "main camera route left unused media space: {bounds:?} in {viewport:?}"
                    );
                }
                release.send(()).unwrap();
            }
        }
    }

    #[test]
    fn first_sequence_zero_uploads_and_preview_fills_the_available_axis() {
        for (native_viewport, zoom) in [
            (egui::vec2(400.0, 240.0), 1.0),
            (egui::vec2(1280.0, 800.0), 1.25),
            (egui::vec2(1920.0, 1080.0), 2.0),
        ] {
            let context = egui::Context::default();
            context.set_zoom_factor(zoom);
            let mut camera = CameraRecorderUi::default();
            let mut available = egui::Rect::NOTHING;
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        native_viewport / zoom,
                    )),
                    ..egui::RawInput::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        available = ui.available_rect_before_wrap();
                        camera.show_preview(ui, &preview(0));
                    });
                },
            );
            let texture = camera
                .texture
                .as_ref()
                .expect("sequence zero must upload")
                .id();
            let bounds = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Mesh(mesh) if mesh.texture_id == texture => {
                        Some(mesh.calc_bounds())
                    }
                    egui::Shape::Rect(rect)
                        if rect
                            .brush
                            .as_ref()
                            .is_some_and(|brush| brush.fill_texture_id == texture) =>
                    {
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .expect("camera preview image is painted");
            assert!(available.expand(0.5).contains_rect(bounds));
            assert!((bounds.width() / bounds.height() - 160.0 / 90.0).abs() < 0.001);
            assert!(
                (bounds.width() - available.width()).abs() < 0.5
                    || (bounds.height() - available.height()).abs() < 0.5
            );
            assert!(
                bounds.width() > 160.0,
                "preview must scale up with the window"
            );
        }
    }

    #[test]
    fn no_camera_page_is_content_sized_and_does_not_allocate_a_fake_preview() {
        let context = egui::Context::default();
        crate::appearance::configure(&context);
        let mut camera = CameraRecorderUi::default();
        camera.devices.clear();
        camera.notice = None;
        let mut used = egui::Vec2::ZERO;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 680.0),
                )),
                ..egui::RawInput::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let start = ui.cursor().min;
                    camera.show(ui);
                    used = ui.cursor().min - start;
                });
            },
        );
        assert!(
            used.y < 200.0,
            "empty camera page unexpectedly reserved a large media area: {used:?}"
        );
        assert!(camera.texture.is_none());
        assert!(!camera.has_preview());
    }

    #[test]
    fn closing_preview_does_not_resurrect_the_completed_workers_last_frame() {
        let (release, wait) = std::sync::mpsc::channel();
        let mut camera = CameraRecorderUi::default();
        camera
            .task
            .start("camera-layout-fixture", move |context| {
                context.report(preview(0));
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(None)
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !camera.has_preview() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        release.send(()).unwrap();
        while camera.is_active() {
            assert!(camera.poll().is_none());
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(
            camera.task.progress().is_some(),
            "the generic task intentionally retains progress"
        );
        assert!(!camera.has_preview());
        let context = egui::Context::default();
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| camera.show(ui));
        });
        assert!(
            camera.texture.is_none(),
            "a closed camera must remain closed on the next repaint"
        );
    }

    #[test]
    fn no_device_cannot_start_or_create_a_worker() {
        let mut ui = CameraRecorderUi::default();
        ui.devices.clear();
        assert!(ui.start(false).is_err());
        assert!(ui.start(true).is_err());
        assert!(!ui.is_active());
    }

    #[test]
    fn shutdown_stops_without_discarding_recorded_frames() {
        let control = CameraControl::default();
        control.record().unwrap();
        let mut ui = CameraRecorderUi::default();
        ui.control = Some(control.clone());
        ui.shutdown();
        assert_eq!(control.phase(), CameraPhase::Finishing);
        ui.cancel();
        assert_eq!(control.phase(), CameraPhase::Discarding);
    }
}
