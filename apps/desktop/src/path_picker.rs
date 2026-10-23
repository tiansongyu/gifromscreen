//! Native Linux file dialogs run independently of rendering and only fill a path field.

use crate::background_task::BackgroundTask;
use eframe::egui;
use gif_from_screen_localization::{Localizer, Message};
use std::{path::PathBuf, time::Duration};

#[derive(Clone, Copy)]
pub(crate) enum PathKind {
    Video,
    Image,
    Gif,
    SaveGif,
    Project,
}

#[derive(Default)]
pub(crate) struct PathPicker {
    task: BackgroundTask<Option<PathBuf>, ()>,
    original: String,
}

impl PathPicker {
    /// Reserve the browse button before the growing path editor. A nested
    /// enabled UI at the end of a full wrapping row cannot reflow atomically.
    pub(crate) fn show_field(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        path: &mut String,
        kind: PathKind,
    ) -> Option<String> {
        ui.label(label);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let notice = self.show(ui, path, kind);
                ui.add(egui::TextEdit::singleline(path).desired_width(ui.available_width()));
                notice
            },
        )
        .inner
    }

    /// Returns a notice only on failure or when a manually changed field was preserved.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        path: &mut String,
        kind: PathKind,
    ) -> Option<String> {
        self.show_named(ui, path, kind, "Browse…", "Output GIF")
    }

    pub(crate) fn is_running(&self) -> bool {
        self.task.is_running()
    }

    pub(crate) fn show_save_gif(
        &mut self,
        ui: &mut egui::Ui,
        path: &mut String,
        localizer: Localizer,
    ) -> Option<String> {
        self.show_named(
            ui,
            path,
            PathKind::SaveGif,
            localizer.text(Message::ChooseSaveLocation),
            localizer.text(Message::RecorderOutputGif),
        )
    }

    fn show_named(
        &mut self,
        ui: &mut egui::Ui,
        path: &mut String,
        kind: PathKind,
        button: &'static str,
        save_title: &'static str,
    ) -> Option<String> {
        let mut notice = self.task.poll().and_then(|result| match result {
            Ok(selected) => accept_selection(path, &self.original, selected, kind).err(),
            Err(error) => Some(error),
        });
        if ui
            .add_enabled(!self.task.is_running(), egui::Button::new(button))
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
                    PathKind::SaveGif => {
                        let filename = initial
                            .file_name()
                            .and_then(|name| name.to_str())
                            .filter(|name| !name.is_empty())
                            .unwrap_or("gif-from-screen.gif");
                        dialog
                            .set_title(save_title)
                            .set_file_name(filename)
                            .add_filter("GIF", &["gif"])
                            .save_file()
                    }
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

/// A cancelled dialog never changes the field, and an outstanding result must
/// not replace a newer manual edit. Choosing a destination does not create it.
fn accept_selection(
    path: &mut String,
    original: &str,
    selected: Option<PathBuf>,
    kind: PathKind,
) -> Result<(), String> {
    let Some(mut selected) = selected else {
        return Ok(());
    };
    if path != original {
        return Err("The path changed while browsing; your typed path was kept.".into());
    }
    if matches!(kind, PathKind::SaveGif)
        && !selected
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"))
    {
        let mut name = selected.file_name().unwrap_or_default().to_os_string();
        name.push(".gif");
        selected.set_file_name(name);
    }
    let selected = selected.to_str().ok_or(
        "This path is not valid UTF-8. Rename it before selecting it; the existing path was kept.",
    )?;
    selected.clone_into(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_selection_accepts_new_unicode_names_and_adds_only_missing_gif_suffix() {
        for (selected, expected) in [
            ("/tmp/演示 目录/新录制", "/tmp/演示 目录/新录制.gif"),
            ("/tmp/demo.GIF", "/tmp/demo.GIF"),
            ("/tmp/demo.png", "/tmp/demo.png.gif"),
        ] {
            let mut path = "old.gif".to_owned();
            accept_selection(
                &mut path,
                "old.gif",
                Some(selected.into()),
                PathKind::SaveGif,
            )
            .unwrap();
            assert_eq!(path, expected);
        }
    }

    #[test]
    fn cancel_and_late_dialog_results_preserve_the_current_destination() {
        let mut path = "newer.gif".to_owned();
        accept_selection(&mut path, "old.gif", None, PathKind::SaveGif).unwrap();
        assert_eq!(path, "newer.gif");
        assert!(
            accept_selection(
                &mut path,
                "old.gif",
                Some("dialog.gif".into()),
                PathKind::SaveGif
            )
            .is_err()
        );
        assert_eq!(path, "newer.gif");
    }

    #[test]
    fn import_selection_keeps_original_extension_and_save_selection_writes_no_file() {
        let directory = tempfile::tempdir().unwrap();
        let selected = directory.path().join("new recording");
        let mut path = String::new();
        accept_selection(&mut path, "", Some(selected.clone()), PathKind::SaveGif).unwrap();
        assert!(!PathBuf::from(&path).exists());
        assert!(!selected.exists());
        path.clear();
        accept_selection(&mut path, "", Some(selected.clone()), PathKind::Image).unwrap();
        assert_eq!(path, selected.to_str().unwrap());
    }
}
