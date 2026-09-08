/// Paragraph direction required by a language, not a UI rendering capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Text is normally written from left to right.
    LeftToRight,
    /// Text requires right-to-left and bidirectional layout support.
    RightToLeft,
}

/// Translation status of this foundation's registry entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranslationReadiness {
    /// This crate contains no translated catalog or validated UI text engine.
    NotProvided,
}

/// An intended language identity; its presence does not certify usable UI text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Language {
    /// Canonical catalog tag matching the pinned reference's resource identity.
    pub tag: &'static str,
    /// Native-language name; rendering it still requires appropriate fonts/shaping.
    pub autonym: &'static str,
    /// English name usable before native-script rendering has been verified.
    pub english_name: &'static str,
    /// Required base text direction.
    pub direction: Direction,
    /// Honest catalog status: no translations are supplied by this foundation.
    pub readiness: TranslationReadiness,
}

const fn language(
    tag: &'static str,
    autonym: &'static str,
    english_name: &'static str,
    direction: Direction,
) -> Language {
    Language {
        tag,
        autonym,
        english_name,
        direction,
        readiness: TranslationReadiness::NotProvided,
    }
}

use Direction::{LeftToRight as LTR, RightToLeft as RTL};

/// The 29 actual catalog identities in `ScreenToGif` 2.43.2; System is a preference,
/// not a thirtieth translation. Commented-out upstream options are not included.
pub const LANGUAGE_REGISTRY: &[Language] = &[
    language("ar", "العربية", "Arabic", RTL),
    language("zh", "简体中文", "Chinese (Simplified)", LTR),
    language("zh-Hant", "繁體中文", "Chinese (Traditional)", LTR),
    language("cs", "Čeština", "Czech", LTR),
    language("da", "Dansk", "Danish", LTR),
    language("nl", "Nederlands", "Dutch", LTR),
    language("en", "English (USA)", "English (USA)", LTR),
    language("en-GB", "English (UK)", "English (UK)", LTR),
    language("fi", "Suomi", "Finnish", LTR),
    language("fr", "Français", "French", LTR),
    language("de", "Deutsch", "German", LTR),
    language("el", "Ελληνικά", "Greek", LTR),
    language("he", "עברית", "Hebrew", RTL),
    language("hu", "Magyar", "Hungarian", LTR),
    language("it", "Italiano", "Italian", LTR),
    language("ja", "日本語", "Japanese", LTR),
    language("ko", "한국어", "Korean", LTR),
    language("pl", "Polski", "Polish", LTR),
    language("pt", "Português (Brasil)", "Portuguese (Brazil)", LTR),
    language(
        "pt-PT",
        "Português (Portugal)",
        "Portuguese (Portugal)",
        LTR,
    ),
    language("ta", "தமிழ்", "Tamil", LTR),
    language("tr", "Türkçe", "Turkish", LTR),
    language("es-AR", "Español (Argentina)", "Spanish (Argentina)", LTR),
    language("es", "Español", "Spanish", LTR),
    language("sw", "Kiswahili", "Swahili", LTR),
    language("sv", "Svenska", "Swedish", LTR),
    language("ru", "Русский", "Russian", LTR),
    language("uk", "Українська", "Ukrainian", LTR),
    language("vi", "Tiếng Việt", "Vietnamese", LTR),
];

/// Find an exact registered tag, ignoring ASCII case but without negotiation.
pub fn find_language(tag: &str) -> Option<&'static Language> {
    LANGUAGE_REGISTRY
        .iter()
        .find(|language| language.tag.eq_ignore_ascii_case(tag))
}
