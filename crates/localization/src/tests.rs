use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::{
    Direction, FallbackReason, LANGUAGE_REGISTRY, LanguagePreference, LanguageTag,
    LanguageTagError, LinuxLocaleEnvironment, MAX_LANGUAGE_PREFERENCES, MAX_LANGUAGE_TAG_BYTES,
    Preferences, ResolutionSource, TranslationReadiness, find_language, resolve_language,
};

fn system(environment: LinuxLocaleEnvironment<'_>) -> crate::ResolvedLanguage {
    resolve_language(&LanguagePreference::System, &environment)
}

#[test]
fn registry_matches_real_reference_tags_without_claiming_translated_catalogs() {
    let expected: BTreeSet<_> = "ar zh zh-Hant cs da nl en en-GB fi fr de el he hu it ja ko pl pt pt-PT ta tr es-AR es sw sv ru uk vi"
        .split_whitespace()
        .collect();
    let actual: BTreeSet<_> = LANGUAGE_REGISTRY
        .iter()
        .map(|language| language.tag)
        .collect();
    assert_eq!(LANGUAGE_REGISTRY.len(), 29);
    assert_eq!(actual, expected);
    for language in LANGUAGE_REGISTRY {
        assert!(!language.autonym.is_empty() && !language.english_name.is_empty());
        assert_eq!(language.readiness, TranslationReadiness::NotProvided);
        assert!(LanguageTag::new(language.tag).is_ok());
        assert_eq!(
            language.direction == Direction::RightToLeft,
            matches!(language.tag, "ar" | "he")
        );
    }
    assert!(find_language("auto").is_none());
    assert_eq!(find_language("EN-gb"), find_language("en-GB"));
}

#[test]
fn defaults_are_system_and_missing_machine_values_resolve_english_explicitly() {
    let preferences = Preferences::default();
    assert_eq!(preferences.language, LanguagePreference::System);
    let resolved = resolve_language(&preferences.language, &LinuxLocaleEnvironment::default());
    assert_eq!(resolved.language.tag, "en");
    assert_eq!(resolved.source, ResolutionSource::DefaultEnglish);
    assert_eq!(
        resolved.fallback,
        FallbackReason::MissingOrInvalidSystemLocale
    );
    assert_eq!(resolved.requested_tag, None);
}

#[test]
fn explicit_override_wins_even_over_posix_and_retains_unavailable_choice() {
    let environment = LinuxLocaleEnvironment {
        lc_all: Some("C.UTF-8"),
        language: Some("de:fr"),
        ..Default::default()
    };
    let resolved = resolve_language(
        &LanguagePreference::explicit("EN-gb").unwrap(),
        &environment,
    );
    assert_eq!(resolved.language.tag, "en-GB");
    assert_eq!(resolved.fallback, FallbackReason::None);
    assert_eq!(resolved.source, ResolutionSource::ExplicitPreference);
    assert_eq!(resolved.requested_tag.unwrap().as_str(), "EN-gb");
    let preference = LanguagePreference::explicit("qaa-Qaaa-999-x-choice").unwrap();
    let before = preference.clone();
    let resolved = resolve_language(&preference, &environment);
    assert_eq!(preference, before);
    assert_eq!(resolved.language.tag, "en");
    assert_eq!(resolved.fallback, FallbackReason::UnavailableLanguage);
    assert_eq!(
        resolved.requested_tag.unwrap().as_str(),
        "qaa-Qaaa-999-x-choice"
    );
}

#[test]
fn message_category_precedence_ignores_empty_but_not_invalid_overrides() {
    let mut environment = LinuxLocaleEnvironment {
        lc_all: Some("de_DE.UTF-8"),
        lc_messages: Some("fr_FR"),
        lang: Some("ja_JP"),
        language: None,
    };
    assert_eq!(system(environment).language.tag, "de");
    environment.lc_all = Some("");
    assert_eq!(system(environment).language.tag, "fr");
    environment.lc_messages = None;
    assert_eq!(system(environment).language.tag, "ja");
    environment.lc_all = Some("bad locale!");
    let resolved = system(environment);
    assert_eq!(resolved.language.tag, "en");
    assert_eq!(
        resolved.fallback,
        FallbackReason::MissingOrInvalidSystemLocale
    );
}

#[test]
fn posix_guard_precedes_language_list_for_each_effective_category() {
    for locale in ["C", "POSIX", "C.UTF-8", "POSIX.UTF-8", "C@modifier"] {
        for category in 0..3 {
            let mut environment = LinuxLocaleEnvironment {
                language: Some("zh_TW:fr"),
                ..Default::default()
            };
            match category {
                0 => environment.lc_all = Some(locale),
                1 => environment.lc_messages = Some(locale),
                _ => environment.lang = Some(locale),
            }
            let resolved = system(environment);
            assert_eq!(resolved.language.tag, "en", "{locale}");
            assert_eq!(resolved.fallback, FallbackReason::PosixLocale);
            assert_eq!(resolved.source, ResolutionSource::SystemLocale);
        }
    }
    let resolved = system(LinuxLocaleEnvironment {
        lc_all: Some("fr_FR"),
        lc_messages: Some("C"),
        language: Some("de"),
        ..Default::default()
    });
    assert_eq!(resolved.language.tag, "de");
}

#[test]
fn language_list_precedes_effective_locale_and_unsupported_entries_do_not_block_later_choices() {
    let environment = LinuxLocaleEnvironment {
        lc_all: Some("fr_FR"),
        language: Some("::qaa:malformed value:zh_TW.UTF-8:de"),
        ..Default::default()
    };
    let resolved = system(environment);
    assert_eq!(resolved.language.tag, "zh-Hant");
    assert_eq!(resolved.source, ResolutionSource::SystemLanguageList);
    assert_eq!(resolved.requested_tag.unwrap().as_str(), "zh-TW");
    assert_eq!(resolved.fallback, FallbackReason::LocaleAlias);
    let resolved = system(LinuxLocaleEnvironment {
        language: Some("qaa:bad value"),
        lang: Some("fr_CA.UTF-8@euro"),
        ..Default::default()
    });
    assert_eq!(resolved.language.tag, "fr");
    assert_eq!(resolved.source, ResolutionSource::SystemLocale);
}

#[test]
fn only_unknown_machine_choices_fall_back_but_retain_first_valid_candidate() {
    let resolved = system(LinuxLocaleEnvironment {
        language: Some("bad value:qaa-Qaaa:zz"),
        lang: Some("qq_AA"),
        ..Default::default()
    });
    assert_eq!(resolved.language.tag, "en");
    assert_eq!(resolved.source, ResolutionSource::SystemLanguageList);
    assert_eq!(resolved.requested_tag.unwrap().as_str(), "qaa-Qaaa");
    assert_eq!(resolved.fallback, FallbackReason::UnavailableLanguage);
}

#[test]
fn chinese_script_and_region_aliases_are_deliberate_and_script_wins() {
    for (input, expected) in [
        ("zh_CN.UTF-8", "zh"),
        ("zh_SG", "zh"),
        ("zh_TW", "zh-Hant"),
        ("zh_HK", "zh-Hant"),
        ("zh_MO", "zh-Hant"),
        ("zh_Hans", "zh"),
        ("zh_Hant", "zh-Hant"),
        ("zh_Hans_TW", "zh"),
        ("zh_Hant_CN", "zh-Hant"),
        ("zh_cmn_Hant_HK", "zh-Hant"),
        ("zh", "zh"),
        ("zh_x_hant", "zh"),
    ] {
        let resolved = system(LinuxLocaleEnvironment {
            lang: Some(input),
            ..Default::default()
        });
        assert_eq!(resolved.language.tag, expected, "{input}");
    }
}

#[test]
fn exact_regional_catalogs_are_not_lost_in_parent_fallthrough() {
    for (input, expected) in [
        ("en_GB.UTF-8", "en-GB"),
        ("pt_PT@euro", "pt-PT"),
        ("es_AR", "es-AR"),
        ("en-GB-u-ca-gregory", "en-GB"),
        ("pt-PT-x-custom", "pt-PT"),
        ("es-AR-u-nu-latn", "es-AR"),
        ("pt_BR", "pt"),
        ("fr_CA", "fr"),
        ("es_419", "es"),
        ("en_US", "en"),
    ] {
        assert_eq!(
            system(LinuxLocaleEnvironment {
                lang: Some(input),
                ..Default::default()
            })
            .language
            .tag,
            expected,
            "{input}"
        );
    }
}

#[test]
fn language_list_and_tag_budgets_are_bounded_without_mutating_environment() {
    let list = std::iter::repeat_n("qaa", MAX_LANGUAGE_PREFERENCES)
        .chain(["de"])
        .collect::<Vec<_>>()
        .join(":");
    let environment = LinuxLocaleEnvironment {
        language: Some(&list),
        lang: Some("ja_JP"),
        ..Default::default()
    };
    let before = environment;
    assert_eq!(system(environment).language.tag, "ja");
    assert_eq!(environment, before);
    let oversized = "a".repeat(257);
    assert_eq!(
        system(LinuxLocaleEnvironment {
            lang: Some(&oversized),
            ..Default::default()
        })
        .fallback,
        FallbackReason::MissingOrInvalidSystemLocale
    );
}

#[test]
fn valid_bounded_unknown_tags_roundtrip_without_changing_case() {
    for tag in [
        "qaa-Qaaa-999-x-Custom",
        "de-CH-1901",
        "en-u-ca-gregory-x-private",
        "x-Site-Choice",
        "i-klingon",
        "en-GB-oed",
        "sgn-BE-FR",
        "zh-min-nan",
    ] {
        let value = LanguageTag::new(tag).unwrap();
        assert_eq!(value.as_str(), tag);
        assert_eq!(value.to_string(), tag);
        assert_eq!(
            serde_json::from_str::<LanguageTag>(&serde_json::to_string(&value).unwrap()).unwrap(),
            value
        );
    }
}

#[test]
fn malformed_tags_extensions_and_duplicates_are_rejected() {
    for tag in [
        "",
        " ",
        "en_US",
        "en.UTF-8",
        "en@euro",
        "en--US",
        "-en",
        "en-",
        "e",
        "en-a",
        "en-x",
        "en-a-b",
        "en-US-US",
        "de-1901-1901",
        "en-u-ca-u-nu",
        "x",
        "abcde123",
        "en-123456789",
        "en/US",
        "中文",
        "en\0US",
        "en\nUS",
    ] {
        assert!(LanguageTag::new(tag).is_err(), "{tag:?}");
    }
    assert!(LanguageTag::new("x-private").is_ok());
    assert!(LanguageTag::new("en-a-foo-b-bar-x-local").is_ok());
}

#[test]
fn exact_byte_boundary_is_checked_before_structural_parsing() {
    let tag = "en-abcde001-abcde002-abcde003-abcde004-abcde005-abcde006-abc123";
    assert_eq!(tag.len(), MAX_LANGUAGE_TAG_BYTES);
    assert!(LanguageTag::new(tag).is_ok());
    assert_eq!(
        LanguageTag::new(format!("{tag}x")),
        Err(LanguageTagError::TooLong {
            maximum: MAX_LANGUAGE_TAG_BYTES
        })
    );
}

#[test]
fn versioned_preferences_roundtrip_system_and_unavailable_explicit_tag() {
    let default = serde_json::to_value(Preferences::default()).unwrap();
    assert_eq!(
        default,
        json!({"format_version": 1, "language": {"mode": "system"}})
    );
    assert_eq!(
        serde_json::from_value::<Preferences>(default).unwrap(),
        Preferences::default()
    );
    let preference = Preferences {
        language: LanguagePreference::explicit("qaa-Qaaa-x-Custom").unwrap(),
    };
    let wire = serde_json::to_value(&preference).unwrap();
    assert_eq!(wire["language"]["tag"], "qaa-Qaaa-x-Custom");
    let restored: Preferences = serde_json::from_value(wire).unwrap();
    assert_eq!(restored, preference);
    assert_eq!(
        resolve_language(&restored.language, &LinuxLocaleEnvironment::default()).fallback,
        FallbackReason::UnavailableLanguage
    );
}

#[test]
fn invalid_or_newer_preference_wire_is_rejected_without_silent_defaulting() {
    for value in [
        json!({"format_version": 2, "language": {"mode": "system"}}),
        json!({"format_version": true, "language": {"mode": "system"}}),
        json!({"format_version": 1}),
        json!({"language": {"mode": "system"}}),
        json!({"format_version": 1, "language": {"mode": "unknown"}}),
        json!({"format_version": 1, "language": {"mode": "explicit", "tag": "en_US"}}),
        json!({"format_version": 1, "language": {"mode": "system"}, "unknown": true}),
        json!({"format_version": 1, "language": {"mode": "system", "extra": true}}),
        Value::Null,
    ] {
        assert!(
            serde_json::from_value::<Preferences>(value.clone()).is_err(),
            "{value}"
        );
    }
}
