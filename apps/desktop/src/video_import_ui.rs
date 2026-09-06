use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use gif_from_screen_application::{
    VideoImportLimits, VideoImportOptions, VideoImportProgress, import_video_project,
};
use gif_from_screen_domain::{PhysicalSize, ProjectId, UnixTimeMs};
use gif_from_screen_project::ActiveProject;
use uuid::Uuid;

use crate::{
    background_task::BackgroundTask,
    path_picker::{PathKind, PathPicker},
};

pub(crate) fn is_video_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            [
                "mp4", "m4v", "mov", "mkv", "webm", "avi", "mpg", "mpeg", "ts", "ogv", "flv",
                "wmv", "nut",
            ]
            .iter()
            .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

pub(crate) struct VideoImportTool {
    pub(crate) input: String,
    target: String,
    start_ms: u64,
    duration_ms: u64,
    fps: u32,
    resize: bool,
    width: u32,
    height: u32,
    picker: PathPicker,
    task: BackgroundTask<ActiveProject, VideoImportProgress>,
    notice: Option<String>,
}

impl Default for VideoImportTool {
    fn default() -> Self {
        Self {
            input: String::new(),
            target: String::new(),
            start_ms: 0,
            duration_ms: 10_000,
            fps: 15,
            resize: false,
            width: 640,
            height: 360,
            picker: PathPicker::default(),
            task: BackgroundTask::default(),
            notice: None,
        }
    }
}

impl VideoImportTool {
    pub(crate) fn is_running(&self) -> bool {
        self.task.is_running()
    }
    pub(crate) fn cancel(&self) {
        self.task.cancel();
    }
    pub(crate) fn poll(&mut self) -> Option<Result<ActiveProject, String>> {
        let mut result = self.task.poll()?;
        if self.task.is_cancelling()
            && let Ok(project) = result
        {
            result = Err(format!(
                "Import cancelled. Saved frames are available in {}.",
                project.layout().root.display()
            ));
        }
        if let Err(error) = &result {
            self.notice = Some(error.clone());
        }
        Some(result)
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        ui.heading("Import video");
        ui.label("Choose a clip, set the part you need, then edit its frames as a GIF.");
        ui.weak("Uses installed FFmpeg and ffprobe. Audio is not imported. Processing runs in the background with bounded memory.");
        ui.add_space(12.0);
        ui.add_enabled_ui(!self.is_running(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Video");
                ui.add(egui::TextEdit::singleline(&mut self.input).desired_width(410.0));
                if let Some(notice) = self.picker.show(ui, &mut self.input, PathKind::Video) { self.notice = Some(notice); }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Start");
                ui.add(egui::DragValue::new(&mut self.start_ms).suffix(" ms"));
                ui.label("Duration");
                ui.add(egui::DragValue::new(&mut self.duration_ms).range(1..=300_000).suffix(" ms"));
                ui.label("Frame rate");
                ui.add(egui::DragValue::new(&mut self.fps).range(1..=60).suffix(" fps"));
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.resize, "Resize video");
                ui.add_enabled_ui(self.resize, |ui| {
                    ui.add(egui::DragValue::new(&mut self.width).range(1..=4096).prefix("Width "));
                    ui.add(egui::DragValue::new(&mut self.height).range(1..=4096).prefix("Height "));
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("New project");
                ui.add(egui::TextEdit::singleline(&mut self.target).desired_width(450.0)
                    .hint_text("Leave empty to save beside the video"));
            });
            ui.weak("Existing projects are never replaced. Cancelling after frames have been saved keeps a recoverable partial project.");
            if ui.button("Import video").clicked() && let Err(error) = self.start() { self.notice = Some(error); }
        });
        if self.is_running() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if self.task.is_cancelling() {
                    "Cancelling…"
                } else {
                    "Importing video…"
                });
            });
            if let Some(progress) = self.task.progress() {
                ui.label(format!(
                    "{} / {} frames · {}.{:02} seconds",
                    progress.frames,
                    progress.expected_frames,
                    progress.duration_us / 1_000_000,
                    (progress.duration_us % 1_000_000) / 10_000
                ));
            }
            if ui
                .add_enabled(
                    !self.task.is_cancelling(),
                    egui::Button::new("Cancel import"),
                )
                .clicked()
            {
                self.task.cancel();
            }
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        if let Some(notice) = &self.notice {
            ui.label(notice);
        }
    }

    fn options(&self) -> Result<VideoImportOptions, String> {
        if self.input.trim().is_empty() {
            return Err("Choose a video file first.".to_owned());
        }
        let input = PathBuf::from(self.input.trim());
        let project_path = if self.target.trim().is_empty() {
            input.with_extension("gfsproj")
        } else {
            PathBuf::from(self.target.trim())
        };
        if project_path
            .extension()
            .is_none_or(|extension| extension != "gfsproj")
        {
            return Err("The target must end in .gfsproj.".to_owned());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?;
        Ok(VideoImportOptions {
            input,
            project_path,
            project_id: ProjectId::from_u128(Uuid::new_v4().as_u128()),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            created_at: UnixTimeMs::new(
                i64::try_from(now.as_millis()).map_err(|error| error.to_string())?,
            ),
            start: Duration::from_millis(self.start_ms),
            duration: Duration::from_millis(self.duration_ms),
            fps: self.fps,
            output_size: if self.resize {
                Some(
                    PhysicalSize::new(self.width, self.height)
                        .map_err(|error| error.to_string())?,
                )
            } else {
                None
            },
            limits: VideoImportLimits::default(),
        })
    }

    fn start(&mut self) -> Result<(), String> {
        let options = self.options()?;
        self.task.start("gfs-video-import", move |context| {
            import_video_project(options, context.cancellation(), |progress| {
                context.report(progress);
            })
            .map_err(|error| error.to_string())
        })?;
        self.notice = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn worker_failure_is_visible_on_the_video_page_and_clears_running_state() {
        let mut tool = VideoImportTool::default();
        tool.task
            .start("video-ui-test", |_| Err("decoder unavailable".to_owned()))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = tool.poll() {
                assert!(result.is_err());
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!tool.is_running());
        assert_eq!(tool.notice.as_deref(), Some("decoder unavailable"));
    }
    #[test]
    fn video_form_defaults_and_explicit_options_preserve_units() {
        let mut tool = VideoImportTool::default();
        assert!(tool.options().is_err());
        tool.input = "/tmp/clip.webm".to_owned();
        tool.start_ms = 1250;
        let request = tool.options().unwrap();
        assert_eq!(request.project_path, PathBuf::from("/tmp/clip.gfsproj"));
        assert_eq!(request.start, Duration::from_millis(1250));
        assert_eq!(request.duration, Duration::from_secs(10));
        assert_eq!(request.fps, 15);
        assert_eq!(request.output_size, None);
        tool.resize = true;
        assert_eq!(
            tool.options().unwrap().output_size,
            Some(PhysicalSize::new(640, 360).unwrap())
        );
        tool.target = "/tmp/wrong.gif".to_owned();
        assert!(tool.options().is_err());
    }
}
