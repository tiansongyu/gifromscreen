//! Application language is independent of editable projects and capture geometry.

pub(crate) mod fonts;
mod persistence;
mod store;

use eframe::egui;
use gif_from_screen_localization::{
    FallbackReason, LANGUAGE_REGISTRY, LanguagePreference, LinuxLocaleEnvironment, Localizer,
    Message, Preferences, find_language, resolve_language,
};

use persistence::{SettingsIo, Status};
use store::Store;

#[derive(Default)]
pub(crate) struct LanguageSettings {
    preferences: Preferences,
    environment: [Option<String>; 4],
    io: SettingsIo,
    open: bool,
}

impl LanguageSettings {
    /// Called only by the native entry point. Unit defaults never access the
    /// developer's environment or configuration directory.
    pub(crate) fn from_environment() -> Self {
        Self {
            environment: ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
                .map(|name| std::env::var(name).ok()),
            io: SettingsIo::new(Store::from_environment()),
            ..Self::default()
        }
    }

    fn environment(&self) -> LinuxLocaleEnvironment<'_> {
        LinuxLocaleEnvironment {
            lc_all: self.environment[0].as_deref(),
            lc_messages: self.environment[1].as_deref(),
            lang: self.environment[2].as_deref(),
            language: self.environment[3].as_deref(),
        }
    }

    pub(crate) fn localizer(&self) -> Localizer {
        Localizer::new(resolve_language(&self.preferences.language, &self.environment()).language)
    }

    pub(crate) fn poll(&mut self, context: &egui::Context) {
        if let Some(preferences) = self.io.poll(&self.preferences) {
            self.preferences = preferences;
        }
        if self.is_active() {
            context.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.io.has_pending_work()
    }

    pub(crate) fn show_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .button(self.localizer().text(Message::LanguageSettingsTitle))
            .clicked()
        {
            self.open = true;
        }
    }

    pub(crate) fn show(&mut self, context: &egui::Context) {
        let mut open = self.open;
        egui::Window::new(self.localizer().text(Message::LanguageSettingsTitle))
            .id(egui::Id::new("application-language-settings"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .show(context, |ui| self.show_contents(ui));
        self.open = open;
    }

    fn select(&mut self, preference: LanguagePreference) {
        if preference != self.preferences.language {
            self.preferences.language = preference;
            self.io.edited();
        }
    }

    fn show_contents(&mut self, ui: &mut egui::Ui) {
        let localizer = self.localizer();
        let mut choice = self.preferences.language.clone();
        ui.label(localizer.text(Message::LanguageChoice));
        egui::ComboBox::from_id_salt("application-language-choice")
            .width(340.0)
            .height(300.0)
            .selected_text(choice_label(&choice, localizer))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut choice,
                    LanguagePreference::System,
                    localizer.text(Message::LanguageSystem),
                );
                for language in LANGUAGE_REGISTRY {
                    // English names remain readable even before a script has a
                    // validated catalog/font/shaping engine. The tag is stable.
                    if let Ok(preference) = LanguagePreference::explicit(language.tag) {
                        ui.push_id(language.tag, |ui| {
                            let label = choice_label(&preference, localizer);
                            ui.selectable_value(&mut choice, preference, label);
                        });
                    }
                }
            });
        self.select(choice);
        let localizer = self.localizer();
        let resolution = resolve_language(&self.preferences.language, &self.environment());
        let effective = localizer
            .resolve(Message::LanguageSettingsTitle)
            .language_tag;
        ui.label(formatted(
            localizer,
            Message::LanguageEffective,
            &[("language", effective)],
        ));
        let mut coverage = localizer.coverage();
        if resolution.fallback == FallbackReason::UnavailableLanguage {
            // The resolver's English default is not a translated unknown locale.
            coverage.translated = 0;
        }
        if coverage.translated == 0 || resolution.fallback == FallbackReason::UnavailableLanguage {
            let requested = resolution
                .requested_tag
                .as_ref()
                .map_or(resolution.language.tag, |tag| tag.as_str());
            ui.colored_label(
                ui.visuals().warn_fg_color,
                formatted(
                    localizer,
                    Message::LanguageUnavailable,
                    &[("language", requested)],
                ),
            );
        }
        ui.label(formatted(
            localizer,
            Message::LanguageCoverage,
            &[
                ("translated", &coverage.translated.to_string()),
                ("total", &coverage.total.to_string()),
            ],
        ));
        ui.separator();
        ui.label(localizer.text(Message::LanguageSaveHint));
        self.show_status(ui, localizer);
    }

    fn show_status(&mut self, ui: &mut egui::Ui, localizer: Localizer) {
        match self.io.status() {
            Status::Unsaved => {
                ui.label(localizer.text(Message::SettingsUnsaved));
            }
            Status::Loading => {
                ui.label(localizer.text(Message::SettingsLoading));
            }
            Status::Saving => {
                ui.label(localizer.text(Message::SettingsSaving));
            }
            Status::Saved => {
                ui.label(localizer.text(Message::SettingsSaved));
            }
            Status::LoadFailed(error) | Status::SaveFailed(error) => {
                let key = if matches!(self.io.status(), Status::LoadFailed(_)) {
                    Message::SettingsLoadFailed
                } else {
                    Message::SettingsSaveFailed
                };
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    formatted(localizer, key, &[("reason", error)]),
                );
                if ui
                    .add_enabled(
                        self.io.can_reload(),
                        egui::Button::new(localizer.text(Message::SettingsReload)),
                    )
                    .clicked()
                {
                    self.io.reload();
                }
            }
            Status::Idle => {}
        }
    }
}

fn choice_label(preference: &LanguagePreference, localizer: Localizer) -> String {
    match preference {
        LanguagePreference::System => localizer.text(Message::LanguageSystem).to_owned(),
        LanguagePreference::Explicit(tag) => find_language(tag.as_str()).map_or_else(
            || tag.as_str().to_owned(),
            |language| {
                let name = if matches!(language.tag, "en" | "zh") {
                    language.autonym
                } else {
                    language.english_name
                };
                format!("{name} [{}]", language.tag)
            },
        ),
    }
}

fn formatted(localizer: Localizer, message: Message, values: &[(&str, &str)]) -> String {
    // Never panic or silently truncate user diagnostics on a formatting error.
    localizer
        .format(message, values)
        .unwrap_or_else(|error| error.to_string())
}

#[cfg(test)]
mod tests;
