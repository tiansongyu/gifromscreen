use crate::{Language, LanguagePreference, LanguageTag, find_language};

/// Maximum colon-separated `LANGUAGE` entries examined, plus the effective locale.
pub const MAX_LANGUAGE_PREFERENCES: usize = 16;
const MAX_SYSTEM_LOCALE_BYTES: usize = 256;

/// An immutable Linux environment snapshot supplied by the caller.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LinuxLocaleEnvironment<'a> {
    /// Effective category override; a nonempty value takes precedence.
    pub lc_all: Option<&'a str>,
    /// Message-category locale when `LC_ALL` is empty/unset.
    pub lc_messages: Option<&'a str>,
    /// Default locale when both category overrides are empty/unset.
    pub lang: Option<&'a str>,
    /// Preferred message languages in order, separated by colons.
    pub language: Option<&'a str>,
}

/// The input which determined a resolution, not an assertion of catalog readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionSource {
    /// Explicit application preference, stronger than all environment settings.
    ExplicitPreference,
    /// A usable entry in `LANGUAGE`.
    SystemLanguageList,
    /// The effective `LC_ALL`/`LC_MESSAGES`/`LANG` category.
    SystemLocale,
    /// No usable preference was supplied; the English baseline is selected.
    DefaultEnglish,
}

/// Why the chosen registry identity differs from the supplied preference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FallbackReason {
    /// Exact, case-insensitive registry match.
    None,
    /// A defined Chinese script/region alias selected its canonical catalog.
    LocaleAlias,
    /// A more general parent tag exists in the registry.
    ParentLanguage,
    /// A valid requested language has no applicable registered identity.
    UnavailableLanguage,
    /// Machine settings were missing or malformed.
    MissingOrInvalidSystemLocale,
    /// System mode used the explicit C/POSIX-to-English policy.
    PosixLocale,
}

/// A deterministic selection result. Registry presence is not translation readiness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedLanguage {
    /// Chosen language identity; callers must additionally check catalog/font readiness.
    pub language: &'static Language,
    /// Which preference source was used.
    pub source: ResolutionSource,
    /// Valid original requested tag, retained even when unavailable.
    pub requested_tag: Option<LanguageTag>,
    /// Explicit negotiation/fallback outcome for display and diagnostics.
    pub fallback: FallbackReason,
}

/// Resolve a language without accessing or mutating process environment/state.
///
/// System mode first honors the effective category's C/POSIX guard. Otherwise
/// `LANGUAGE` candidates take priority over `LC_ALL`, `LC_MESSAGES` and `LANG`. A
/// nonempty malformed higher category does not activate a lower category.
pub fn resolve_language(
    preference: &LanguagePreference,
    environment: &LinuxLocaleEnvironment<'_>,
) -> ResolvedLanguage {
    if let LanguagePreference::Explicit(tag) = preference {
        return resolved(tag.clone(), ResolutionSource::ExplicitPreference);
    }
    let effective = [
        environment.lc_all,
        environment.lc_messages,
        environment.lang,
    ]
    .into_iter()
    .flatten()
    .find(|value| !value.is_empty());
    let effective = effective.and_then(normalize_system_locale);
    if effective
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("C") || value.eq_ignore_ascii_case("POSIX"))
    {
        return english(
            ResolutionSource::SystemLocale,
            None,
            FallbackReason::PosixLocale,
        );
    }
    let mut first_unavailable = None;
    if let Some(list) = environment.language {
        for candidate in list.split(':').take(MAX_LANGUAGE_PREFERENCES) {
            let Some(tag) =
                normalize_system_locale(candidate).and_then(|value| LanguageTag::new(value).ok())
            else {
                continue;
            };
            let result = resolved(tag, ResolutionSource::SystemLanguageList);
            if result.fallback != FallbackReason::UnavailableLanguage {
                return result;
            }
            first_unavailable.get_or_insert(result);
        }
    }
    if let Some(tag) = effective.and_then(|value| LanguageTag::new(value).ok()) {
        let result = resolved(tag, ResolutionSource::SystemLocale);
        if result.fallback != FallbackReason::UnavailableLanguage {
            return result;
        }
        first_unavailable.get_or_insert(result);
    }
    first_unavailable.unwrap_or_else(|| {
        english(
            ResolutionSource::DefaultEnglish,
            None,
            FallbackReason::MissingOrInvalidSystemLocale,
        )
    })
}

fn resolved(tag: LanguageTag, source: ResolutionSource) -> ResolvedLanguage {
    let matched = find_language(tag.as_str())
        .map(|language| (language, FallbackReason::None))
        .or_else(|| {
            chinese_alias(tag.as_str()).map(|language| (language, FallbackReason::LocaleAlias))
        })
        .or_else(|| {
            parent_language(tag.as_str()).map(|language| (language, FallbackReason::ParentLanguage))
        });
    if let Some((language, fallback)) = matched {
        ResolvedLanguage {
            language,
            source,
            requested_tag: Some(tag),
            fallback,
        }
    } else {
        english(source, Some(tag), FallbackReason::UnavailableLanguage)
    }
}

fn english(
    source: ResolutionSource,
    requested_tag: Option<LanguageTag>,
    fallback: FallbackReason,
) -> ResolvedLanguage {
    ResolvedLanguage {
        language: find_language("en").expect("English is a fixed registry invariant"),
        source,
        requested_tag,
        fallback,
    }
}

fn parent_language(mut tag: &str) -> Option<&'static Language> {
    while let Some((parent, _)) = tag.rsplit_once('-') {
        if let Some(language) = find_language(parent) {
            return Some(language);
        }
        tag = parent;
    }
    None
}

fn chinese_alias(tag: &str) -> Option<&'static Language> {
    let mut parts = tag.split('-');
    if !parts.next()?.eq_ignore_ascii_case("zh") {
        return None;
    }
    let mut script = None;
    let mut region = None;
    for part in parts.take_while(|part| part.len() != 1) {
        if part.len() == 4 && part.bytes().all(|value| value.is_ascii_alphabetic()) {
            script = Some(part);
        } else if part.len() == 2 {
            region = Some(part);
        }
    }
    let language = match script {
        Some(value) if value.eq_ignore_ascii_case("Hant") => "zh-Hant",
        Some(value) if value.eq_ignore_ascii_case("Hans") => "zh",
        None if region.is_some_and(|value| {
            ["TW", "HK", "MO"]
                .iter()
                .any(|region| region.eq_ignore_ascii_case(value))
        }) =>
        {
            "zh-Hant"
        }
        None if region.is_some_and(|value| {
            ["CN", "SG"]
                .iter()
                .any(|region| region.eq_ignore_ascii_case(value))
        }) =>
        {
            "zh"
        }
        Some(_) | None => return None,
    };
    find_language(language)
}

fn normalize_system_locale(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > MAX_SYSTEM_LOCALE_BYTES || !value.is_ascii() {
        return None;
    }
    let base = value.split(['.', '@']).next()?;
    if base.is_empty() {
        return None;
    }
    Some(base.replace('_', "-"))
}
