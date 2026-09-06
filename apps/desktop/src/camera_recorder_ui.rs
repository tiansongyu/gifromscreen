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
        ui.heading("Camera recorder");
        ui.label("Preview your camera, then record only the moments you want in your GIF.");
        ui.weak(
            "The camera stays off until you choose Preview or Record. Audio is never captured.",
        );
        ui.add_space(12.0);
        self.show_configuration(ui);
        ui.add_space(8.0);
        self.show_controls(ui);
        if let Some(progress) = self.task.progress() {
            self.show_preview(ui, &progress);
            if let Some(recording) = progress.recording {
                ui.label(format!(
                    "{} saved frames · {}.{:02} seconds · {} frames skipped under load",
                    recording.frames,
                    recording.duration_us / 1_000_000,
                    (recording.duration_us % 1_000_000) / 10_000,
                    progress.dropped_frames
                ));
                if recording.limit_reached {
                    ui.weak("The safety limit was reached. Saving captured frames…");
                }
            }
        } else if self.is_active() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Opening camera…");
            });
        }
        if let Some(notice) = &self.notice {
            ui.label(notice);
        }
        if self.is_active() {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }

    fn show_configuration(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(!self.is_active(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Camera");
                egui::ComboBox::from_id_salt("camera-device")
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
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::DragValue::new(&mut self.width).range(1..=4096).prefix("Width "));
                ui.add(egui::DragValue::new(&mut self.height).range(1..=4096).prefix("Height "));
                ui.add(egui::DragValue::new(&mut self.fps).range(1..=60).suffix(" fps"));
            });
            ui.horizontal(|ui| {
                ui.label("New project");
                ui.add(egui::TextEdit::singleline(&mut self.target).desired_width(430.0).hint_text("Leave empty for a new camera recording in this folder"));
            });
            ui.weak("Choose a mode supported by the camera. Up to 10,000 frames / 2 GiB of raw pixels; existing projects are never replaced.");
        });
        if self.devices.is_empty() {
            ui.label("No Linux camera was found. Connect a V4L2 camera, then choose Refresh.");
        }
    }

    fn show_controls(&mut self, ui: &mut egui::Ui) {
        if !self.is_active() {
            ui.add_enabled_ui(!self.devices.is_empty(), |ui| {
                ui.horizontal(|ui| {
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
        ui.horizontal(|ui| {
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
        ui.weak(match phase {
            CameraPhase::Preview => "Preview only — no frames are being saved.",
            CameraPhase::Recording => {
                "Recording — pause whenever you need; stop to open the editor."
            }
            CameraPhase::Paused => {
                "Paused — preview is live; paused time is excluded from the GIF."
            }
            CameraPhase::Finishing => "Saving frames and releasing the camera…",
            CameraPhase::Discarding => "Discarding this recording and releasing the camera…",
        });
    }

    fn show_preview(&mut self, ui: &mut egui::Ui, progress: &CameraCaptureProgress) {
        let frame = &progress.preview;
        if frame.sequence != self.texture_sequence {
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
            let scale = (ui.available_width().min(720.0) / size.x)
                .min(480.0 / size.y)
                .min(1.0);
            ui.image((texture.id(), size * scale));
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

#[cfg(test)]
mod tests {
    use super::*;

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
