//! Presentation-only messages with stable identities and literal diagnostic data.
//!
//! Raw notices are a migration boundary, not English strings to classify. The
//! cache lets existing root views borrow text after one per-frame refresh;
//! independently rendered tools should call `render` with their own localizer.

use std::{fmt, ops::Deref};

use eframe::egui;
use gif_from_screen_localization::{Localizer, Message, find_language};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Template {
    message: Message,
    arguments: Vec<(&'static str, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Notice {
    template: Option<Template>,
    text: String,
    language: &'static str,
}

impl Notice {
    pub(crate) fn new(message: Message, arguments: &[(&'static str, &str)]) -> Self {
        Self::localized(
            Localizer::new(find_language("en").expect("English baseline")),
            message,
            arguments,
        )
    }

    pub(crate) fn localized(
        localizer: Localizer,
        message: Message,
        arguments: &[(&'static str, &str)],
    ) -> Self {
        let text = format(localizer, message, arguments);
        Self {
            template: Some(Template {
                message,
                arguments: arguments
                    .iter()
                    .map(|&(name, value)| (name, value.to_owned()))
                    .collect(),
            }),
            text,
            language: localizer.resolve(message).language_tag,
        }
    }

    pub(crate) fn message_id(&self) -> Option<Message> {
        self.template.as_ref().map(|template| template.message)
    }

    pub(crate) fn render(&self, localizer: Localizer) -> String {
        let Some(template) = &self.template else {
            return self.text.clone();
        };
        if self.language == localizer.resolve(template.message).language_tag {
            return self.text.clone();
        }
        let arguments: Vec<_> = template
            .arguments
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        format(localizer, template.message, &arguments)
    }

    pub(crate) fn refresh(&mut self, localizer: Localizer) {
        let Some(message) = self.message_id() else {
            return;
        };
        let language = localizer.resolve(message).language_tag;
        if self.language != language {
            self.text = self.render(localizer);
            self.language = language;
        }
    }
}

fn format(localizer: Localizer, message: Message, arguments: &[(&str, &str)]) -> String {
    localizer
        .format(message, arguments)
        .unwrap_or_else(|error| format!("{}: {error}", message.id()))
}

impl From<Message> for Notice {
    fn from(message: Message) -> Self {
        Self::new(message, &[])
    }
}

impl From<String> for Notice {
    fn from(text: String) -> Self {
        Self {
            template: None,
            text,
            language: "",
        }
    }
}

impl From<&str> for Notice {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

impl Deref for Notice {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for Notice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.text.fmt(formatter)
    }
}

impl From<&Notice> for egui::WidgetText {
    fn from(notice: &Notice) -> Self {
        notice.text.clone().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn language(tag: &str) -> Localizer {
        Localizer::new(find_language(tag).unwrap())
    }

    #[test]
    fn cached_and_explicit_rendering_keep_the_message_identity_across_switches() {
        let mut notice = Notice::new(
            Message::RecorderProjectReady,
            &[
                ("frames", "30"),
                ("seconds", "3.000"),
                ("path", "/tmp/项目/{frames}.gfsproj"),
            ],
        );
        let english = notice.to_string();
        let chinese = notice.render(language("zh"));
        assert_ne!(english, chinese);
        assert!(chinese.contains("/tmp/项目/{frames}.gfsproj"));
        notice.refresh(language("zh"));
        assert_eq!(&*notice, chinese);
        assert_eq!(notice.message_id(), Some(Message::RecorderProjectReady));
        notice.refresh(language("fr"));
        assert_eq!(&*notice, english);
        notice.refresh(language("zh"));
        assert_eq!(&*notice, chinese);
    }

    #[test]
    fn raw_diagnostics_and_strings_matching_english_ui_are_never_classified() {
        for text in ["Recording started…", "路径/{seconds}/文件\nraw OS message"] {
            let mut notice = Notice::from(text);
            notice.refresh(language("zh"));
            assert_eq!(&*notice, text);
            assert_eq!(notice.render(language("zh")), text);
            assert_eq!(notice.message_id(), None);
        }
    }

    #[test]
    fn invalid_call_site_reports_its_id_instead_of_panicking_or_interpreting_arguments() {
        let notice = Notice::new(Message::RecorderCountdownNotice, &[("秒", "3")]);
        assert!(notice.starts_with("recorder-countdown-notice:"));
        assert!(
            notice
                .render(language("zh"))
                .contains("Unknown message argument")
        );
        let notice = Notice::new(
            Message::RecorderSourcesFailed,
            &[("error", "raw {count} / {display_server}")],
        );
        assert!(
            notice
                .render(language("zh"))
                .contains("raw {count} / {display_server}")
        );
    }
}
