//! Human-readable save destination and localized, field-level validation.

use eframe::egui;
use gif_from_screen_localization::{Localizer, Message};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct OutputUi {
    picker: crate::path_picker::PathPicker,
    checked: String,
    checked_at: Option<Instant>,
    error: Option<Message>,
    dialog_error: Option<String>,
}

impl OutputUi {
    pub(crate) fn is_valid(&self) -> bool {
        self.checked_at.is_some() && self.error.is_none()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.picker.is_running()
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui, output: &mut String, localizer: Localizer) {
        ui.vertical(|ui| {
            let path = Path::new(output.trim());
            let mut name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_owned();
            let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(egui::TextEdit::singleline(&mut name).desired_width(220.0))
                    .on_hover_text(output.as_str())
                    .changed()
                {
                    *output = folder.join(&name).to_string_lossy().into_owned();
                    self.dialog_error = None;
                }
                if let Some(error) = self.picker.show_save_gif(ui, output, localizer) {
                    self.dialog_error = Some(error);
                }
            });
            let parent = Path::new(output.trim()).parent().unwrap_or(Path::new(""));
            let folder_label = if parent.as_os_str().is_empty() {
                localizer.text(Message::CurrentFolder).to_owned()
            } else {
                parent
                    .file_name()
                    .unwrap_or(parent.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            };
            ui.add(egui::Label::new(egui::RichText::new(folder_label).weak()).truncate())
                .on_hover_text(parent.display().to_string());
            if self.checked != *output
                || self
                    .checked_at
                    .is_none_or(|time| time.elapsed() >= Duration::from_secs(1))
            {
                self.checked.clone_from(output);
                self.checked_at = Some(Instant::now());
                self.error = validate(output).err();
            }
            if let Some(message) = self.error {
                ui.colored_label(ui.visuals().error_fg_color, localizer.text(message));
            }
            if let Some(error) = &self.dialog_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }
}

pub(crate) fn validate(output: &str) -> Result<(), Message> {
    let path = Path::new(output.trim());
    if path.file_name().is_none()
        || path
            .extension()
            .and_then(|value| value.to_str())
            .is_none_or(|value| !value.eq_ignore_ascii_case("gif"))
    {
        return Err(Message::OutputInvalidName);
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(Message::OutputFolderUnavailable);
    }
    if path
        .try_exists()
        .map_err(|_| Message::OutputFolderUnavailable)?
    {
        return Err(Message::OutputExists);
    }
    let project = crate::project_path_for_output(path).map_err(|_| Message::OutputInvalidName)?;
    if project
        .try_exists()
        .map_err(|_| Message::OutputFolderUnavailable)?
    {
        return Err(Message::OutputProjectExists);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_actionable_errors_without_replacing_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("演示.GIF");
        let path = output.to_str().unwrap();
        assert_eq!(validate(path), Ok(()));
        std::fs::create_dir(dir.path().join("演示.gfsproj")).unwrap();
        assert_eq!(validate(path), Err(Message::OutputProjectExists));
        std::fs::write(&output, b"original").unwrap();
        assert_eq!(validate(path), Err(Message::OutputExists));
        assert_eq!(std::fs::read(output).unwrap(), b"original");
        assert_eq!(validate(""), Err(Message::OutputInvalidName));
        assert_eq!(
            validate(dir.path().join("missing/a.gif").to_str().unwrap()),
            Err(Message::OutputFolderUnavailable)
        );
    }
}
