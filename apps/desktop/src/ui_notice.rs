//! Presentation-only messages with stable identities and literal diagnostic data.
//!
//! Raw notices are a migration boundary, not English strings to classify. The
//! cache lets existing root views borrow text after one per-frame refresh;
//! independently rendered tools should call `render` with their own localizer.

use std::{borrow::Cow, fmt, ops::Deref};

use eframe::egui;
use gif_from_screen_localization::{Localizer, Message, find_language};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Template {
    message: Message,
    arguments: Vec<(&'static str, Argument)>,
}

/// Only application-owned, parameterless labels may be translated as values.
/// Literal user text and external diagnostics never become message lookups.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Argument {
    Literal(String),
    Message(Message),
}

impl Template {
    fn new(message: Message, arguments: &[(&'static str, &str)]) -> Self {
        Self {
            message,
            arguments: arguments
                .iter()
                .map(|&(name, value)| (name, Argument::Literal(value.to_owned())))
                .collect(),
        }
    }

    fn render(&self, localizer: Localizer) -> String {
        let values: Vec<_> = self
            .arguments
            .iter()
            .map(|(name, value)| {
                let text = match value {
                    Argument::Literal(text) => Cow::Borrowed(text.as_str()),
                    Argument::Message(message) => Cow::Owned(format(localizer, *message, &[])),
                };
                (*name, text)
            })
            .collect();
        let arguments: Vec<_> = values
            .iter()
            .map(|(name, text)| (*name, text.as_ref()))
            .collect();
        format(localizer, self.message, &arguments)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Notice {
    template: Option<Template>,
    text: String,
    // The sentence and its label arguments may fall back independently.
    // Cache by requested locale, not only the containing message's language.
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
        Self::from_template(localizer, Template::new(message, arguments))
    }

    /// Retain label identities as well as the containing sentence's identity.
    /// This is one level of parameterless messages, not recursive templates.
    pub(crate) fn with_messages(
        message: Message,
        arguments: &[(&'static str, &str)],
        messages: &[(&'static str, Message)],
    ) -> Self {
        let mut template = Template::new(message, arguments);
        template.arguments.extend(
            messages
                .iter()
                .map(|&(name, label)| (name, Argument::Message(label))),
        );
        Self::from_template(
            Localizer::new(find_language("en").expect("English baseline")),
            template,
        )
    }

    fn from_template(localizer: Localizer, template: Template) -> Self {
        Self {
            text: template.render(localizer),
            template: Some(template),
            language: localizer.requested_language().tag,
        }
    }

    pub(crate) fn message_id(&self) -> Option<Message> {
        self.template.as_ref().map(|template| template.message)
    }

    pub(crate) fn render(&self, localizer: Localizer) -> String {
        let Some(template) = &self.template else {
            return self.text.clone();
        };
        if self.language == localizer.requested_language().tag {
            return self.text.clone();
        }
        template.render(localizer)
    }

    pub(crate) fn refresh(&mut self, localizer: Localizer) {
        if self.template.is_none() {
            return;
        }
        let language = localizer.requested_language().tag;
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

    #[test]
    fn application_labels_retranslate_but_literal_paths_and_diagnostics_do_not() {
        let raw = "Delete /tmp/用户/{operation} {error}";
        let mut notice = Notice::with_messages(
            Message::EditorOperationFailed,
            &[("error", raw)],
            &[("operation", Message::EditorDelete)],
        );
        for tag in ["en", "zh", "fr", "zh", "en"] {
            let localizer = language(tag);
            let expected = localizer
                .format(
                    Message::EditorOperationFailed,
                    &[
                        ("error", raw),
                        ("operation", localizer.text(Message::EditorDelete)),
                    ],
                )
                .unwrap();
            assert_eq!(notice.render(localizer), expected);
            notice.refresh(localizer);
            assert_eq!(&*notice, expected);
            assert_eq!(notice.message_id(), Some(Message::EditorOperationFailed));
        }
    }

    #[test]
    fn invalid_message_arguments_remain_visible_diagnostics_not_recursive_templates() {
        let notice = Notice::with_messages(
            Message::EditorOperationFailed,
            &[("operation", "literal"), ("error", "raw")],
            &[("operation", Message::EditorDelete)],
        );
        assert!(notice.starts_with("editor-operation-failed:"));
        assert!(
            notice
                .render(language("zh"))
                .starts_with("editor-operation-failed:")
        );
        let notice = Notice::with_messages(
            Message::EditorOperationFailed,
            &[("error", "raw")],
            &[("operation", Message::EditorFilmstripFrame)],
        );
        assert!(notice.contains("editor-filmstrip-frame:"));
        assert!(
            notice
                .render(language("zh"))
                .contains("editor-filmstrip-frame:")
        );
    }
}
