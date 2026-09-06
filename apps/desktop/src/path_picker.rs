//! Native Linux file dialogs run independently of rendering and only fill a path field.

use crate::background_task::BackgroundTask;
use eframe::egui;
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Copy)]
pub(crate) enum PathKind {
    Video,
    Image,
    Gif,
    Project,
}

#[derive(Default)]
pub(crate) struct PathPicker {
    task: BackgroundTask<Option<PathBuf>, ()>,
    original: String,
}

impl PathPicker {
    /// Returns a notice only on failure or when a manually changed field was preserved.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        path: &mut String,
        kind: PathKind,
    ) -> Option<String> {
        let mut notice = None;
        if let Some(result) = self.task.poll() {
            match result {
                Ok(Some(selected)) if *path == self.original => {
                    if let Some(selected) = selected.to_str() {
                        selected.clone_into(path);
                    } else {
                        notice = Some("This path is not valid UTF-8. Rename it before importing; the existing path was kept.".to_owned());
                    }
                }
                Ok(Some(_)) => {
                    notice = Some(
                        "The path changed while browsing; your typed path was kept.".to_owned(),
                    );
                }
                Ok(None) => {}
                Err(error) => notice = Some(error),
            }
        }
        if ui
            .add_enabled(!self.task.is_running(), egui::Button::new("Browse…"))
            .clicked()
        {
            self.original.clone_from(path);
            let initial = PathBuf::from(path.trim());
            if let Err(error) = self.task.start("gfs-file-dialog", move |_| {
                let mut dialog = rfd::FileDialog::new();
                if let Some(parent) = initial.parent().filter(|parent| parent.is_dir()) {
                    dialog = dialog.set_directory(parent);
                }
                let selected = match kind {
                    PathKind::Video => dialog
                        .set_title("Choose video")
                        .add_filter(
                            "Video",
                            &[
                                "mp4", "m4v", "mov", "mkv", "webm", "avi", "mpg", "mpeg", "ts",
                                "ogv", "flv", "wmv", "nut",
                            ],
                        )
                        .pick_file(),
                    PathKind::Image => dialog
                        .set_title("Choose image")
                        .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "webp"])
                        .pick_file(),
                    PathKind::Gif => dialog
                        .set_title("Choose GIF")
                        .add_filter("GIF", &["gif"])
                        .pick_file(),
                    PathKind::Project => dialog
                        .set_title("Choose .gfsproj project directory")
                        .pick_folder(),
                };
                Ok(selected)
            }) {
                notice = Some(error);
            }
        }
        if self.task.is_running() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
        notice
    }
}
